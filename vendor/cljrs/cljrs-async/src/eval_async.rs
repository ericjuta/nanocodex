//! Asynchronous tree-walking evaluation for `^:async` function bodies.
//!
//! [`eval_async`] yields at every `(await …)` and at every subtree that may
//! contain one: control specials (`do`/`if`/`let`/`loop`/`try`/`binding`/
//! `and`/`or`/`letfn`/`with-out-str`), collection literals, ordinary call
//! arguments, and form-intercepted natives (`apply`, `swap!`, …). Sync-only
//! specials that cannot safely contain `await` reject it with a located
//! diagnostic instead of blocking the `LocalSet`.
//!
//! Forms that the sync evaluator macro-expands (`when`, `cond`, `->`, …) are
//! expanded here first via [`cljrs_interp::macros::macroexpand`] so their
//! desugared `if`/`do`/`let` shapes are handled with proper yielding.

#![allow(
    clippy::result_large_err,
    reason = "EvalResult is the public cljrs async evaluator contract"
)]

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use cljrs_env::env::Env;
use cljrs_env::error::{EvalError, EvalResult};
use cljrs_gc::GcPtr;
use cljrs_interp::apply::{
    bind_fn_params, eval_alter_var_root, eval_reset_bang, eval_send_to_agent, eval_swap_bang,
    eval_vary_meta, eval_volatile, eval_vreset_bang, eval_vswap_bang, eval_with_bindings_star,
    make_delay_from_fn, make_lazy_seq_from_fn, select_arity,
};
use cljrs_interp::destructure::{bind_pattern, value_to_seq_vec};
use cljrs_interp::eval::{eval, is_special_form};
use cljrs_interp::macros::macroexpand;
use cljrs_reader::Form;
use cljrs_reader::form::FormKind;
use cljrs_value::value::SetValue;
use cljrs_value::{
    Atom, BoundFn, CljxFnArity, CljxFuture, FutureState, MapValue, PersistentHashSet,
    PersistentList, PersistentVector, Symbol, Value, Var,
};

/// Spawn `task` on the current `LocalSet` and return a `Value::Future` that the
/// task settles on completion. The single delivery point for every async
/// primitive (`^:async` calls, `timeout`, `alts`).
///
/// Public so other native crates (e.g. `cljrs-io`) can drive their own async
/// work onto the shared executor and deliver results through the same `Future`
/// machinery.
///
/// Must be called from within a Tokio `LocalSet` context.
pub fn spawn_future<F>(task: F) -> Value
where
    F: Future<Output = EvalResult> + 'static,
{
    // GC builds: heap-promotion fallback — the task's captured environment is
    // opaque to the publish-barrier scan, and the task may run after any
    // bump-region scope active right now has closed.  Poison the active
    // regions so they are retired (kept alive) instead of reset; a no-op (one
    // thread-local read) when no region is open, which is the common case.
    cljrs_gc::region::poison_active_regions();
    let future = GcPtr::new(CljxFuture::new());
    let task_future = future.clone();
    let gas_meters = cljrs_env::gas::active_meters();
    let handle = tokio::task::spawn_local(async move {
        // Root the result future across GC cycles: the spawning scope's alloc
        // frame may have dropped before the task gets to run.
        let anchor = Value::Future(task_future.clone());
        let _root = cljrs_env::gc_roots::root_value(&anchor);
        let mut task = Box::pin(task);
        let result = std::future::poll_fn(|cx| {
            // LocalSet tasks share an OS thread, so TLS state must be scoped
            // to one poll and removed before another task can run.
            let _gas_guards = cljrs_env::gas::install_meters(&gas_meters);
            task.as_mut().poll(cx)
        })
        .await;
        settle_future(&task_future, result);
    });
    crate::cancel::register_task(future.clone(), handle);
    crate::task_scope::register(future.clone());
    Value::Future(future)
}

/// Write a completed result into a future and wake blocking `deref` waiters.
/// The first terminal state wins; a racing cancel leaves the future cancelled.
pub(crate) fn settle_future(future: &GcPtr<CljxFuture>, result: EvalResult) {
    let mut state = future.get().state.lock().unwrap();
    if !matches!(*state, FutureState::Running) {
        drop(state);
        crate::cancel::clear(future);
        return;
    }
    *state = match result {
        Ok(v) => FutureState::Done(v),
        // Preserve the thrown value (and any non-Thrown error as a fresh
        // Value::Error) so `await` can re-throw it with ex-data/ex-cause intact.
        Err(EvalError::GasExhausted) => FutureState::GasExhausted,
        Err(e) => FutureState::Failed(e.to_error_value()),
    };
    drop(state);
    future.get().cond.notify_all();
    crate::cancel::clear(future);
}

/// Run the body of an `^:async` function to completion, yielding at every
/// `await`. Returns the value of the last body form.
///
/// `callee` must be a `Value::Fn`; `args` are the already-evaluated call
/// arguments. A fresh environment is built from the function's closure with
/// `is_async = true` so nested `await`s take the yielding path.
pub async fn run_async_fn(callee: Value, args: Vec<Value>, base: &Env) -> EvalResult {
    let f = match &callee {
        Value::Fn(f) => f.get().clone(),
        other => {
            return Err(EvalError::Runtime(format!(
                "async dispatch expected a fn, got {}",
                other.type_name()
            )));
        }
    };
    let arity = select_arity(&f, args.len())?.clone();
    let mut env = Env::with_closure(base.globals.clone(), &f.defining_ns, &f);
    env.is_async = true;

    // Keep callee and the local env alive across GC cycles at async yield points.
    let _callee_root = cljrs_env::gc_roots::root_value(&callee);
    let _env_root = cljrs_env::gc_roots::push_env_root(&env);

    let mut current_args = args;
    loop {
        env.push_frame();
        bind_fn_params(&arity, &current_args, &mut env)?;
        if let Some(name) = &f.name {
            env.bind(name.clone(), callee.clone());
        }

        let mut result = Ok(Value::Nil);
        for form in &arity.body {
            result = Box::pin(eval_async(form, &mut env)).await;
            if result.is_err() {
                break;
            }
        }
        env.pop_frame();

        match result {
            Ok(v) => return Ok(v),
            Err(EvalError::Recur(new_args)) => {
                current_args = flatten_recur_args(&arity, new_args);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Asynchronously evaluate a single form.
pub async fn eval_async(form: &Form, env: &mut Env) -> EvalResult {
    if !cljrs_env::gas::charge(1) {
        return Err(EvalError::GasExhausted);
    }
    // Collection literals may contain `await` sub-expressions (e.g. the
    // return value of a ^:async fn is `[(await x) (await y)]`). Evaluate
    // each element with eval_async so awaits yield cooperatively instead of
    // blocking the LocalSet thread via the sync condvar path.
    match &form.kind {
        FormKind::Vector(elems) => {
            let elems = elems.clone();
            let mut vals: Vec<Value> = Vec::with_capacity(elems.len());
            for f in &elems {
                vals.push(Box::pin(eval_async(f, env)).await?);
            }
            return Ok(Value::Vector(GcPtr::new(PersistentVector::from_iter(vals))));
        }
        FormKind::Map(elems) => {
            if elems.len() % 2 != 0 {
                return Err(EvalError::Runtime(
                    "map literal must have an even number of forms".into(),
                ));
            }
            let elems = elems.clone();
            let mut pairs: Vec<Value> = Vec::with_capacity(elems.len());
            for f in &elems {
                pairs.push(Box::pin(eval_async(f, env)).await?);
            }
            let kv_pairs: Vec<(Value, Value)> = pairs
                .chunks(2)
                .map(|pair| (pair[0].clone(), pair[1].clone()))
                .collect();
            return Ok(Value::Map(MapValue::from_pairs(kv_pairs)));
        }
        FormKind::Set(elems) => {
            let elems = elems.clone();
            let mut vals: Vec<Value> = Vec::with_capacity(elems.len());
            for f in &elems {
                vals.push(Box::pin(eval_async(f, env)).await?);
            }
            return Ok(Value::Set(SetValue::Hash(GcPtr::new(
                PersistentHashSet::from_iter(vals),
            ))));
        }
        FormKind::List(_) => {} // fall through to list handling below
        _ => return eval(form, env),
    }

    // Reduce control-flow macros (when, cond, ->, …) to their special-form core
    // so awaits nested inside them take the yielding path.
    let expanded = macroexpand(form, env)?;
    let forms = match &expanded.kind {
        FormKind::List(forms) if !forms.is_empty() => forms,
        _ => return eval(&expanded, env),
    };

    if let FormKind::Symbol(s) = &forms[0].kind {
        if is_special_form(s) {
            cljrs_env::policy::check_special(s)?;
        }
        match s.as_str() {
            "await" => return eval_await_async(&forms[1..], env).await,
            "do" => return eval_body_async(&forms[1..], env).await,
            "if" => return eval_if_async(&forms[1..], env).await,
            "let*" | "let" => return eval_let_async(&forms[1..], env).await,
            "loop*" | "loop" => return eval_loop_async(&forms[1..], env).await,
            "try" => return eval_try_async(&forms[1..], env).await,
            "binding" => return eval_binding_async(&forms[1..], env).await,
            "and" => return eval_and_async(&forms[1..], env).await,
            "or" => return eval_or_async(&forms[1..], env).await,
            "letfn" => return eval_letfn_async(&forms[1..], env).await,
            "with-out-str" => return eval_with_out_str_async(&forms[1..], env).await,
            // Definition forms that only *capture* bodies (`fn`/`defn`/…) may
            // contain `await` textually without evaluating it. Sync-eval those.
            // Forms that evaluate their arguments/body now must reject await
            // rather than block the LocalSet on the sync condvar path.
            other if is_special_form(other) => {
                if !special_form_defers_body(other) && form_contains_await(&expanded) {
                    return Err(EvalError::Runtime(format!(
                        "await is not allowed inside special form `{other}`"
                    )));
                }
                return eval(&expanded, env);
            }
            _ => {}
        }
        return eval_call_async(&forms[0], &forms[1..], &expanded, env).await;
    }

    // Callable data structures and computed call heads still need async argument
    // evaluation: `(:key (await future))` must not take the blocking sync path.
    eval_call_async(&forms[0], &forms[1..], &expanded, env).await
}

/// `(await x)` — evaluate `x`, then yield until the resulting future/promise
/// resolves.
async fn eval_await_async(args: &[Form], env: &mut Env) -> EvalResult {
    let Some(arg) = args.first() else {
        return Err(EvalError::Runtime("await requires one argument".into()));
    };
    let val = Box::pin(eval_async(arg, env)).await?;
    await_value(val).await
}

/// Cooperatively await a Clojure value. Futures and promises yield to the
/// executor until resolved; any other value is returned as-is.
pub async fn await_value(val: Value) -> EvalResult {
    match val {
        Value::Future(f) => {
            // Root the future across GC cycles: the alloc frame of the scope that
            // produced it may have dropped before this task reached a yield point.
            let anchor = Value::Future(f.clone());
            let _root = cljrs_env::gc_roots::root_value(&anchor);
            loop {
                {
                    let guard = f.get().state.lock().unwrap();
                    match &*guard {
                        FutureState::Done(v) => {
                            f.get().mark_observed();
                            return Ok(v.clone());
                        }
                        FutureState::Failed(v) => {
                            f.get().mark_observed();
                            return Err(EvalError::Thrown(v.clone()));
                        }
                        FutureState::GasExhausted => {
                            f.get().mark_observed();
                            return Err(EvalError::GasExhausted);
                        }
                        FutureState::Cancelled => {
                            return Err(EvalError::Runtime("future was cancelled".into()));
                        }
                        FutureState::Running => {}
                    }
                }
                cljrs_env::gc_roots::async_gc_collect();
                tokio::task::yield_now().await;
            }
        }
        Value::Promise(p) => {
            let anchor = Value::Promise(p.clone());
            let _root = cljrs_env::gc_roots::root_value(&anchor);
            loop {
                {
                    if let Some(v) = p.get().value.lock().unwrap().as_ref() {
                        return Ok(v.clone());
                    }
                }
                cljrs_env::gc_roots::async_gc_collect();
                tokio::task::yield_now().await;
            }
        }
        other => Ok(other),
    }
}

/// Evaluate a sequence of body forms, returning the value of the last.
async fn eval_body_async(forms: &[Form], env: &mut Env) -> EvalResult {
    let mut result = Value::Nil;
    for form in forms {
        result = Box::pin(eval_async(form, env)).await?;
    }
    Ok(result)
}

/// `(try body... (catch Type e handler...)... (finally cleanup...))` with
/// yielding bodies. Mirrors the synchronous `eval_try` (`cljrs-interp`) exactly,
/// but evaluates the body, catch handlers, and finally block with `eval_async`
/// so an `await`/`<?` inside any of them cooperates with the executor instead of
/// falling back to the blocking sync path.
async fn eval_try_async(args: &[Form], env: &mut Env) -> EvalResult {
    let (body, catches, fin_body) = cljrs_interp::special::parse_try_args(args);

    let mut result = eval_body_async(body, env).await;

    // Handle catch: never intercept Recur (loop/fn trampoline signal).
    let err_opt = match std::mem::replace(&mut result, Ok(Value::Nil)) {
        Ok(v) => {
            result = Ok(v);
            None
        }
        Err(EvalError::Recur(recur_args)) => {
            result = Err(EvalError::Recur(recur_args));
            None
        }
        Err(EvalError::GasExhausted) => {
            result = Err(EvalError::GasExhausted);
            None
        }
        Err(other) => Some(other),
    };

    if let Some(err) = err_opt {
        let thrown_val = match err {
            EvalError::Thrown(v) => v,
            ref other => cljrs_interp::special::eval_error_to_value(other),
        };
        let mut handled = false;
        for c in &catches {
            if cljrs_interp::special::catch_type_matches(c.type_sym, &thrown_val) {
                env.push_frame();
                env.bind(std::sync::Arc::from(c.binding), thrown_val.clone());
                result = eval_body_async(c.body, env).await;
                env.pop_frame();
                handled = true;
                break;
            }
        }
        if !handled {
            // No matching catch — re-throw.
            result = Err(EvalError::Thrown(thrown_val));
        }
    }

    // Always run finally (its value is discarded).
    if !fin_body.is_empty() {
        let _ = eval_body_async(fin_body, env).await;
    }

    result
}

/// `(if test then else?)` with a yielding test and selected branch.
async fn eval_if_async(args: &[Form], env: &mut Env) -> EvalResult {
    let Some(test_form) = args.first() else {
        return Err(EvalError::Runtime("if requires a test".into()));
    };
    let test = Box::pin(eval_async(test_form, env)).await?;
    let truthy = !matches!(test, Value::Nil | Value::Bool(false));
    if truthy {
        match args.get(1) {
            Some(then) => Box::pin(eval_async(then, env)).await,
            None => Ok(Value::Nil),
        }
    } else {
        match args.get(2) {
            Some(els) => Box::pin(eval_async(els, env)).await,
            None => Ok(Value::Nil),
        }
    }
}

/// `(let* [bindings] body…)` with yielding binding inits and body. Destructuring
/// patterns are bound via the shared [`bind_pattern`] helper.
async fn eval_let_async(args: &[Form], env: &mut Env) -> EvalResult {
    let bindings = match args.first().map(|f| &f.kind) {
        Some(FormKind::Vector(v)) => v.clone(),
        _ => return Err(EvalError::Runtime("let* requires a binding vector".into())),
    };
    if bindings.len() % 2 != 0 {
        return Err(EvalError::Runtime(
            "let* binding vector must have even length".into(),
        ));
    }

    env.push_frame();
    for pair in bindings.chunks(2) {
        let val = match Box::pin(eval_async(&pair[1], env)).await {
            Ok(v) => v,
            Err(e) => {
                env.pop_frame();
                return Err(e);
            }
        };
        if let Err(e) = bind_pattern(&pair[0], val, env) {
            env.pop_frame();
            return Err(e);
        }
    }
    let result = eval_body_async(&args[1..], env).await;
    env.pop_frame();
    result
}

/// `(loop* [bindings] body…)` / `(loop [bindings] body…)` — an async-aware
/// loop: binding inits are evaluated with [`eval_async`], the body runs with
/// [`eval_body_async`], and `recur` restarts the iteration.
async fn eval_loop_async(args: &[Form], env: &mut Env) -> EvalResult {
    let bindings = match args.first().map(|f| &f.kind) {
        Some(FormKind::Vector(v)) => v.clone(),
        _ => return Err(EvalError::Runtime("loop requires a binding vector".into())),
    };
    if bindings.len() % 2 != 0 {
        return Err(EvalError::Runtime(
            "loop binding vector must have even length".into(),
        ));
    }

    let patterns: Vec<Form> = bindings.iter().step_by(2).cloned().collect();
    let init_forms: Vec<Form> = bindings.iter().skip(1).step_by(2).cloned().collect();

    // Evaluate initial binding values.
    let mut current_vals: Vec<Value> = Vec::with_capacity(patterns.len());
    for form in &init_forms {
        current_vals.push(Box::pin(eval_async(form, env)).await?);
    }

    let body = &args[1..];
    loop {
        env.push_frame();
        for (pat, val) in patterns.iter().zip(current_vals.iter()) {
            if let Err(e) = bind_pattern(pat, val.clone(), env) {
                env.pop_frame();
                return Err(e);
            }
        }

        let result = eval_body_async(body, env).await;
        env.pop_frame();

        match result {
            Ok(v) => return Ok(v),
            Err(EvalError::Recur(new_vals)) => {
                if new_vals.len() != patterns.len() {
                    return Err(EvalError::Arity {
                        name: "recur".into(),
                        expected: patterns.len().to_string(),
                        got: new_vals.len(),
                    });
                }
                current_vals = new_vals;
            }
            Err(e) => return Err(e),
        }
    }
}

/// A function call whose arguments may contain `await`s. Arguments are
/// evaluated with [`eval_async`] (so awaits yield), then the callee is applied.
///
/// Form-intercepted natives (`apply`, `swap!`, …) evaluate arguments on the
/// async path, then dispatch through value-level handlers that preserve their
/// spreading/atom/dynamic-binding semantics.
async fn eval_call_async(head: &Form, args: &[Form], whole: &Form, env: &mut Env) -> EvalResult {
    let callee = eval(head, env)?;
    match &callee {
        Value::NativeFunction(nf) if is_form_intercepted(&nf.get().name) => {
            let name = nf.get().name.clone();
            let mut argv: Vec<Value> = Vec::with_capacity(args.len());
            for a in args {
                let _args_root = cljrs_env::gc_roots::root_values(&argv);
                argv.push(Box::pin(eval_async(a, env)).await?);
            }
            return dispatch_intercepted(&name, argv, env);
        }
        // A macro head should have been expanded already, but guard regardless.
        Value::Macro(_) => {
            if form_contains_await(whole) {
                // Re-expand after async-capable evaluation of the expanded form.
                let expanded = macroexpand(whole, env)?;
                return Box::pin(eval_async(&expanded, env)).await;
            }
            return eval(whole, env);
        }
        _ => {}
    }

    let _callee_root = cljrs_env::gc_roots::root_value(&callee);
    let mut argv: Vec<Value> = Vec::with_capacity(args.len());
    for a in args {
        let _args_root = cljrs_env::gc_roots::root_values(&argv);
        argv.push(Box::pin(eval_async(a, env)).await?);
    }
    cljrs_env::apply::apply_value(&callee, argv, env)
}

/// Native functions that `cljrs_interp::eval_call` intercepts at the form level.
fn is_form_intercepted(name: &str) -> bool {
    matches!(
        name,
        "apply"
            | "atom"
            | "reset!"
            | "swap!"
            | "volatile!"
            | "vreset!"
            | "agent"
            | "make-lazy-seq"
            | "make-delay"
            | "vswap!"
            | "send"
            | "send-off"
            | "with-bindings*"
            | "alter-var-root"
            | "vary-meta"
            | "find-ns"
            | "the-ns"
            | "ns-interns"
            | "ns-publics"
            | "ns-refers"
            | "ns-map"
            | "all-ns"
            | "create-ns"
            | "ns-aliases"
            | "remove-ns"
            | "alter-meta!"
            | "ns-resolve"
            | "resolve"
            | "intern"
            | "bound-fn*"
    )
}

fn dispatch_intercepted(name: &str, args: Vec<Value>, env: &mut Env) -> EvalResult {
    match name {
        "apply" => eval_apply_values(args, env),
        "atom" => eval_atom_values(args, env),
        "reset!" => eval_reset_bang(args, env),
        "swap!" => eval_swap_bang(args, env),
        "volatile!" => eval_volatile(args),
        "vreset!" => eval_vreset_bang(args),
        "vswap!" => eval_vswap_bang(args, env),
        "agent" => Err(EvalError::Runtime("agent is not yet implemented".into())),
        "make-lazy-seq" => {
            let Some(f) = args.first() else {
                return Err(EvalError::Arity {
                    name: "make-lazy-seq".into(),
                    expected: "1".into(),
                    got: 0,
                });
            };
            make_lazy_seq_from_fn(f, env.globals.clone(), env.current_ns.clone())
        }
        "make-delay" => {
            let Some(f) = args.first() else {
                return Err(EvalError::Arity {
                    name: "make-delay".into(),
                    expected: "1".into(),
                    got: 0,
                });
            };
            make_delay_from_fn(f, env.globals.clone(), env.current_ns.clone())
        }
        "send" | "send-off" => eval_send_to_agent(args, env),
        "with-bindings*" => eval_with_bindings_star(args, env),
        "alter-var-root" => eval_alter_var_root(args, env),
        "vary-meta" => eval_vary_meta(args, env),
        "find-ns" | "the-ns" => eval_find_ns_values(args, env),
        "ns-interns" | "ns-publics" => eval_ns_interns_values(args, env),
        "ns-refers" => eval_ns_refers_values(args, env),
        "ns-map" => eval_ns_map_values(args, env),
        "all-ns" => eval_all_ns_values(env),
        "create-ns" => eval_create_ns_values(args, env),
        "ns-aliases" => eval_ns_aliases_values(args, env),
        "remove-ns" => eval_remove_ns_values(args, env),
        "alter-meta!" => eval_alter_meta_values(args, env),
        "ns-resolve" => eval_ns_resolve_values(args, env),
        "resolve" => eval_resolve_values(args, env),
        "intern" => eval_intern_values(args, env),
        "bound-fn*" => eval_bound_fn_star_values(args),
        other => Err(EvalError::Runtime(format!(
            "async interceptor missing handler for `{other}`"
        ))),
    }
}

/// `(binding [sym val …] body…)` with yielding inits and body.
async fn eval_binding_async(args: &[Form], env: &mut Env) -> EvalResult {
    let pairs = match args.first().map(|f| &f.kind) {
        Some(FormKind::Vector(v)) => v.clone(),
        _ => return Err(EvalError::Runtime("binding requires a vector".into())),
    };
    if pairs.len() % 2 != 0 {
        return Err(EvalError::Runtime(
            "binding vector must have even count".into(),
        ));
    }

    let mut frame: HashMap<usize, Value> = HashMap::new();
    for pair in pairs.chunks(2) {
        let sym_str = match &pair[0].kind {
            FormKind::Symbol(s) => s.clone(),
            _ => return Err(EvalError::Runtime("binding targets must be symbols".into())),
        };
        let parsed = Symbol::parse(&sym_str);
        let ns_part: Arc<str> = match parsed.namespace.as_deref() {
            Some(ns_part) => env
                .globals
                .resolve_alias(&env.current_ns, ns_part)
                .unwrap_or_else(|| Arc::from(ns_part)),
            None => env.current_ns.clone(),
        };
        let var_ptr = env
            .globals
            .lookup_var_in_ns(&ns_part, &parsed.name)
            .ok_or_else(|| EvalError::UnboundSymbol(sym_str.clone()))?;
        let val = Box::pin(eval_async(&pair[1], env)).await?;
        frame.insert(cljrs_env::dynamics::var_key_of(&var_ptr), val);
    }

    let _guard = cljrs_env::dynamics::push_frame(frame);
    eval_body_async(&args[1..], env).await
}

async fn eval_and_async(args: &[Form], env: &mut Env) -> EvalResult {
    let mut result = Value::Bool(true);
    for form in args {
        result = Box::pin(eval_async(form, env)).await?;
        if matches!(result, Value::Nil | Value::Bool(false)) {
            return Ok(result);
        }
    }
    Ok(result)
}

async fn eval_or_async(args: &[Form], env: &mut Env) -> EvalResult {
    let mut last = Value::Nil;
    for form in args {
        last = Box::pin(eval_async(form, env)).await?;
        if !matches!(last, Value::Nil | Value::Bool(false)) {
            return Ok(last);
        }
    }
    Ok(last)
}

/// `letfn` builds fn values synchronously (bodies are not executed), then
/// evaluates the body with yielding.
async fn eval_letfn_async(args: &[Form], env: &mut Env) -> EvalResult {
    let bindings = match args.first().map(|f| &f.kind) {
        Some(FormKind::Vector(v)) => v.clone(),
        _ => return Err(EvalError::Runtime("letfn requires a binding vector".into())),
    };

    env.push_frame();
    for binding in &bindings {
        let FormKind::List(parts) = &binding.kind else {
            continue;
        };
        if parts.is_empty() {
            continue;
        }
        let mut fn_forms = vec![Form::new(
            FormKind::Symbol("fn".to_string()),
            binding.span.clone(),
        )];
        fn_forms.extend(parts.iter().cloned());
        let fn_form = Form::new(FormKind::List(fn_forms), binding.span.clone());
        let fn_val = match eval(&fn_form, env) {
            Ok(v) => v,
            Err(e) => {
                env.pop_frame();
                return Err(e);
            }
        };
        let name = match &parts[0].kind {
            FormKind::Symbol(s) => s.clone(),
            _ => {
                env.pop_frame();
                return Err(EvalError::Runtime(
                    "letfn binding name must be a symbol".into(),
                ));
            }
        };
        env.bind(Arc::from(name.as_str()), fn_val);
    }

    let result = eval_body_async(&args[1..], env).await;
    env.pop_frame();
    result
}

async fn eval_with_out_str_async(body: &[Form], env: &mut Env) -> EvalResult {
    cljrs_builtins::builtins::push_output_capture();
    let result = eval_body_async(body, env).await;
    let captured = cljrs_builtins::builtins::pop_output_capture().unwrap_or_default();
    result?;
    Ok(Value::string(captured))
}

/// Special forms whose nested forms are captured, not evaluated, at this call.
fn special_form_defers_body(name: &str) -> bool {
    matches!(
        name,
        "fn" | "fn*" | "defn" | "defn-" | "defmacro" | "quote" | "var"
    )
}

fn form_contains_await(form: &Form) -> bool {
    match &form.kind {
        FormKind::List(forms) => {
            if matches!(forms.first().map(|f| &f.kind), Some(FormKind::Symbol(s)) if s == "await") {
                return true;
            }
            forms.iter().any(form_contains_await)
        }
        FormKind::Vector(forms)
        | FormKind::Map(forms)
        | FormKind::Set(forms)
        | FormKind::AnonFn(forms) => forms.iter().any(form_contains_await),
        FormKind::Quote(inner)
        | FormKind::SyntaxQuote(inner)
        | FormKind::Unquote(inner)
        | FormKind::UnquoteSplice(inner)
        | FormKind::Deref(inner)
        | FormKind::Var(inner)
        | FormKind::TaggedLiteral(_, inner) => form_contains_await(inner),
        FormKind::Meta(a, b) => form_contains_await(a) || form_contains_await(b),
        FormKind::ReaderCond { clauses, .. } => clauses.iter().any(form_contains_await),
        _ => false,
    }
}

fn eval_apply_values(mut evaled: Vec<Value>, env: &mut Env) -> EvalResult {
    if evaled.len() < 2 {
        return Err(EvalError::Arity {
            name: "apply".into(),
            expected: "2+".into(),
            got: evaled.len(),
        });
    }
    let f = evaled.remove(0);
    let last = evaled.pop().unwrap();
    let _f_root = cljrs_env::gc_roots::root_value(&f);
    let _last_root = cljrs_env::gc_roots::root_value(&last);
    let _evaled_root = cljrs_env::gc_roots::root_values(&evaled);
    evaled.extend(value_to_seq_vec(&last));
    cljrs_env::apply::apply_value(&f, evaled, env)
}

fn eval_atom_values(args: Vec<Value>, env: &mut Env) -> EvalResult {
    if args.is_empty() {
        return Err(EvalError::Arity {
            name: "atom".into(),
            expected: "1+".into(),
            got: 0,
        });
    }
    let initial = args[0].clone();
    let options = &args[1..];
    let mut meta_opt: Option<Value> = None;
    let mut validator_opt: Option<Value> = None;
    let mut i = 0;
    while i + 1 < options.len() {
        match &options[i] {
            Value::Keyword(k) if k.get().name.as_ref() == "meta" => {
                meta_opt = Some(options[i + 1].clone());
                i += 2;
            }
            Value::Keyword(k) if k.get().name.as_ref() == "validator" => {
                let vf = options[i + 1].clone();
                validator_opt = if vf == Value::Nil { None } else { Some(vf) };
                i += 2;
            }
            _ => {
                i += 2;
            }
        }
    }
    if let Some(ref m) = meta_opt
        && !matches!(m, Value::Nil | Value::Map(_))
    {
        return Err(EvalError::Thrown(Value::string(
            "Atom metadata must be a map or nil".to_string(),
        )));
    }
    if let Some(ref vf) = validator_opt {
        let result = cljrs_env::apply::apply_value(vf, vec![initial.clone()], env)?;
        if result == Value::Nil || result == Value::Bool(false) {
            return Err(EvalError::Thrown(Value::string(
                "Invalid initial value for atom".to_string(),
            )));
        }
    }
    let atom = GcPtr::new(Atom::new(initial));
    if let Some(m) = meta_opt {
        atom.get()
            .set_meta(if m == Value::Nil { None } else { Some(m) });
    }
    if let Some(vf) = validator_opt {
        atom.get().set_validator(Some(vf));
    }
    Ok(Value::Atom(atom))
}

fn ns_name_from_val(v: &Value) -> Result<String, EvalError> {
    match v {
        Value::Symbol(s) => Ok(s.get().name.as_ref().to_string()),
        Value::Str(s) => Ok(s.get().clone()),
        Value::Namespace(ns) => Ok(ns.get().name.as_ref().to_string()),
        Value::Keyword(k) => Ok(k.get().name.as_ref().to_string()),
        other => Err(EvalError::Runtime(format!(
            "expected symbol, string, or namespace, got {}",
            other.type_name()
        ))),
    }
}

fn the_ns_value(v: &Value, env: &Env) -> Result<GcPtr<cljrs_value::Namespace>, EvalError> {
    if let Value::Namespace(ns) = v {
        return Ok(ns.clone());
    }
    let name = ns_name_from_val(v)?;
    let map = env.globals.namespaces.read().unwrap();
    match map.get(name.as_str()) {
        Some(ns) => Ok(ns.clone()),
        None => Err(EvalError::Runtime(format!("No namespace: {name} found"))),
    }
}

fn eval_find_ns_values(args: Vec<Value>, env: &Env) -> EvalResult {
    if args.is_empty() {
        return Err(EvalError::Arity {
            name: "find-ns".into(),
            expected: "1".into(),
            got: 0,
        });
    }
    let name = ns_name_from_val(&args[0])?;
    let map = env.globals.namespaces.read().unwrap();
    match map.get(name.as_str()) {
        Some(ns) => Ok(Value::Namespace(ns.clone())),
        None => Ok(Value::Nil),
    }
}

fn eval_ns_interns_values(args: Vec<Value>, env: &Env) -> EvalResult {
    if args.is_empty() {
        return Err(EvalError::Arity {
            name: "ns-interns".into(),
            expected: "1".into(),
            got: 0,
        });
    }
    let ns = the_ns_value(&args[0], env)?;
    cljrs_builtins::builtins::builtin_ns_interns(&[Value::Namespace(ns)])
        .map_err(cljrs_env::error::value_error_to_eval_error)
}

fn eval_ns_refers_values(args: Vec<Value>, env: &Env) -> EvalResult {
    if args.is_empty() {
        return Err(EvalError::Arity {
            name: "ns-refers".into(),
            expected: "1".into(),
            got: 0,
        });
    }
    let ns = the_ns_value(&args[0], env)?;
    cljrs_builtins::builtins::builtin_ns_refers(&[Value::Namespace(ns)])
        .map_err(cljrs_env::error::value_error_to_eval_error)
}

fn eval_ns_map_values(args: Vec<Value>, env: &Env) -> EvalResult {
    if args.is_empty() {
        return Err(EvalError::Arity {
            name: "ns-map".into(),
            expected: "1".into(),
            got: 0,
        });
    }
    let ns = the_ns_value(&args[0], env)?;
    cljrs_builtins::builtins::builtin_ns_map(&[Value::Namespace(ns)])
        .map_err(cljrs_env::error::value_error_to_eval_error)
}

fn eval_all_ns_values(env: &Env) -> EvalResult {
    let map = env.globals.namespaces.read().unwrap();
    let items: Vec<Value> = map
        .values()
        .map(|ns| Value::Namespace(ns.clone()))
        .collect();
    drop(map);
    Ok(Value::List(GcPtr::new(PersistentList::from_iter(items))))
}

fn eval_create_ns_values(args: Vec<Value>, env: &Env) -> EvalResult {
    if args.is_empty() {
        return Err(EvalError::Arity {
            name: "create-ns".into(),
            expected: "1".into(),
            got: 0,
        });
    }
    let name = ns_name_from_val(&args[0])?;
    Ok(Value::Namespace(env.globals.get_or_create_ns(&name)))
}

fn eval_ns_aliases_values(args: Vec<Value>, env: &Env) -> EvalResult {
    if args.is_empty() {
        return Err(EvalError::Arity {
            name: "ns-aliases".into(),
            expected: "1".into(),
            got: 0,
        });
    }
    let ns_name = ns_name_from_val(&args[0])?;
    let map = env.globals.namespaces.read().unwrap();
    let Some(ns) = map.get(ns_name.as_str()).cloned() else {
        return Ok(Value::Map(MapValue::empty()));
    };
    let aliases = ns.get().aliases.lock().unwrap().clone();
    drop(map);
    let mut m = MapValue::empty();
    for (alias, full_ns_name) in &aliases {
        let sym = Value::symbol(Symbol::simple(alias.clone()));
        let nsmap = env.globals.namespaces.read().unwrap();
        if let Some(target_ns) = nsmap.get(full_ns_name.as_ref()) {
            m = m.assoc(sym, Value::Namespace(target_ns.clone()));
        }
    }
    Ok(Value::Map(m))
}

fn eval_remove_ns_values(args: Vec<Value>, env: &Env) -> EvalResult {
    if args.is_empty() {
        return Err(EvalError::Arity {
            name: "remove-ns".into(),
            expected: "1".into(),
            got: 0,
        });
    }
    let name = ns_name_from_val(&args[0])?;
    env.globals
        .namespaces
        .write()
        .unwrap()
        .remove(name.as_str());
    Ok(Value::Nil)
}

fn eval_alter_meta_values(mut args: Vec<Value>, env: &mut Env) -> EvalResult {
    if args.len() < 2 {
        return Err(EvalError::Arity {
            name: "alter-meta!".into(),
            expected: "2+".into(),
            got: args.len(),
        });
    }
    let obj = args.remove(0);
    let f = args.remove(0);
    let current_meta = match &obj {
        Value::Var(vp) => vp.get().get_meta().unwrap_or(Value::Map(MapValue::empty())),
        _ => Value::Map(MapValue::empty()),
    };
    let mut call_args = vec![current_meta];
    call_args.extend(args);
    let new_meta = cljrs_env::apply::apply_value(&f, call_args, env)?;
    if let Value::Var(vp) = &obj {
        vp.get().set_meta(new_meta.clone());
    }
    Ok(new_meta)
}

fn eval_ns_resolve_values(args: Vec<Value>, env: &Env) -> EvalResult {
    if args.len() < 2 {
        return Err(EvalError::Arity {
            name: "ns-resolve".into(),
            expected: "2".into(),
            got: args.len(),
        });
    }
    let ns_name = ns_name_from_val(&args[0])?;
    let sym_name = match &args[1] {
        Value::Symbol(s) => s.get().name.as_ref().to_string(),
        Value::Str(s) => s.get().clone(),
        other => {
            return Err(EvalError::Runtime(format!(
                "ns-resolve: second arg must be symbol or string, got {}",
                other.type_name()
            )));
        }
    };
    Ok(match env.globals.lookup_var(&ns_name, &sym_name) {
        Some(var_ptr) => Value::Var(var_ptr),
        None => Value::Nil,
    })
}

fn resolve_current_ns(env: &Env) -> Arc<str> {
    if let Some(var) = env.globals.lookup_var("clojure.core", "*ns*") {
        let val = cljrs_env::dynamics::deref_var(&var);
        if let Some(Value::Namespace(ns_ptr)) = val {
            return ns_ptr.get().name.clone();
        }
    }
    env.current_ns.clone()
}

fn eval_resolve_values(args: Vec<Value>, env: &Env) -> EvalResult {
    if args.len() != 1 {
        return Err(EvalError::Arity {
            name: "resolve".into(),
            expected: "1".into(),
            got: args.len(),
        });
    }
    let resolve_ns = resolve_current_ns(env);
    match &args[0] {
        Value::Symbol(s) => {
            let sym = s.get();
            if let Some(ns) = &sym.namespace {
                let full_ns = env
                    .globals
                    .resolve_alias(&resolve_ns, ns.as_ref())
                    .unwrap_or_else(|| ns.clone());
                return Ok(
                    match env.globals.lookup_var_in_ns(&full_ns, sym.name.as_ref()) {
                        Some(var_ptr) => Value::Var(var_ptr),
                        None => Value::Nil,
                    },
                );
            }
            Ok(
                match env.globals.lookup_var_in_ns(&resolve_ns, sym.name.as_ref()) {
                    Some(var_ptr) => Value::Var(var_ptr),
                    None => Value::Nil,
                },
            )
        }
        Value::Str(s) => Ok(
            match env.globals.lookup_var_in_ns(&resolve_ns, s.get().as_str()) {
                Some(var_ptr) => Value::Var(var_ptr),
                None => Value::Nil,
            },
        ),
        other => Err(EvalError::Runtime(format!(
            "resolve: arg must be symbol or string, got {}",
            other.type_name()
        ))),
    }
}

fn eval_intern_values(args: Vec<Value>, env: &Env) -> EvalResult {
    if !(2..=3).contains(&args.len()) {
        return Err(EvalError::Runtime("intern expects 2 or 3 arguments".into()));
    }
    let ns_name: Arc<str> = match &args[0] {
        Value::Symbol(s) => s.get().name.clone(),
        Value::Namespace(ns) => ns.get().name.clone(),
        other => {
            return Err(EvalError::Runtime(format!(
                "intern: first arg must be namespace or symbol, got {}",
                other.type_name()
            )));
        }
    };
    let var_name: Arc<str> = match &args[1] {
        Value::Symbol(s) => s.get().name.clone(),
        other => {
            return Err(EvalError::Runtime(format!(
                "intern: second arg must be symbol, got {}",
                other.type_name()
            )));
        }
    };
    let ns = {
        let map = env.globals.namespaces.read().unwrap();
        map.get(ns_name.as_ref()).cloned()
    };
    let ns = ns.ok_or_else(|| EvalError::Runtime(format!("No namespace: {ns_name} found")))?;
    let var = if args.len() == 3 {
        let val = args[2].clone();
        let mut interns = ns.get().interns.lock().unwrap();
        if let Some(var) = interns.get(&var_name) {
            var.get().bind(val);
            var.clone()
        } else {
            let var = GcPtr::new(Var::new(ns_name.clone(), var_name.clone()));
            var.get().bind(val);
            interns.insert(var_name, var.clone());
            var
        }
    } else {
        let mut interns = ns.get().interns.lock().unwrap();
        if let Some(var) = interns.get(&var_name) {
            var.clone()
        } else {
            let var = GcPtr::new(Var::new(ns_name.clone(), var_name.clone()));
            interns.insert(var_name, var.clone());
            var
        }
    };
    Ok(Value::Var(var))
}

fn eval_bound_fn_star_values(args: Vec<Value>) -> EvalResult {
    if args.len() != 1 {
        return Err(EvalError::Arity {
            name: "bound-fn*".into(),
            expected: "1".into(),
            got: args.len(),
        });
    }
    let frames = cljrs_env::dynamics::capture_current();
    let mut merged = HashMap::new();
    for frame in &frames {
        merged.extend(frame.iter().map(|(k, v)| (*k, v.clone())));
    }
    Ok(Value::BoundFn(GcPtr::new(BoundFn {
        wrapped: args[0].clone(),
        captured_bindings: merged,
    })))
}

/// Flatten `recur` arguments for a variadic arity so the rest collection is
/// spread back into individual positional arguments (mirrors `call_cljrs_fn`).
fn flatten_recur_args(arity: &CljxFnArity, new_args: Vec<Value>) -> Vec<Value> {
    if arity.rest_param.is_some() {
        let n = arity.params.len();
        if new_args.len() == n + 1 {
            let mut flat = new_args[..n].to_vec();
            match &new_args[n] {
                Value::Nil => {}
                rest_val => flat.extend(value_to_seq_vec(rest_val)),
            }
            return flat;
        }
    }
    new_args
}
