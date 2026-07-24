//! Dynamic execution policies for restricted evaluator profiles.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{EvalError, EvalResult};

thread_local! {
    static TRANSACTION_DEPTH: Cell<usize> = const { Cell::new(0) };
    static TRANSACTION_GENSYM: Cell<u64> = const { Cell::new(0) };
}

thread_local! {
    static CODE_MODE_POLICIES: RefCell<Vec<Arc<CodeModePolicy>>> =
        const { RefCell::new(Vec::new()) };
}

/// Isolate-local policy used by embedders that evaluate untrusted Code Mode cells.
#[derive(Debug)]
pub struct CodeModePolicy {
    cancelled: AtomicBool,
}

impl CodeModePolicy {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
        })
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Installs a Code Mode policy for its dynamic extent on the isolate thread.
#[must_use = "dropping the guard removes the Code Mode execution policy"]
pub struct CodeModePolicyGuard;

impl CodeModePolicyGuard {
    pub fn install(policy: Arc<CodeModePolicy>) -> Self {
        CODE_MODE_POLICIES.with(|policies| policies.borrow_mut().push(policy));
        Self
    }
}

impl Drop for CodeModePolicyGuard {
    fn drop(&mut self) {
        CODE_MODE_POLICIES.with(|policies| {
            policies.borrow_mut().pop();
        });
    }
}

pub fn code_mode_policy_active() -> bool {
    CODE_MODE_POLICIES.with(|policies| !policies.borrow().is_empty())
}

pub fn code_mode_cancelled() -> bool {
    CODE_MODE_POLICIES.with(|policies| policies.borrow().iter().any(|policy| policy.is_cancelled()))
}

/// Return whether a namespace is visible to a restricted Code Mode cell.
pub fn namespace_visible(name: &str) -> bool {
    if !code_mode_policy_active() {
        return true;
    }
    matches!(
        name,
        "clojure.core" | "clojure.core.async" | "clojure.rust.error" | "nanocodex.tools"
    ) || name.starts_with("nanocodex.cell.")
}

/// Installs the side-effect-free transaction policy for its dynamic extent.
#[must_use = "dropping the guard removes the transaction execution policy"]
pub struct TransactionPolicyGuard;

impl TransactionPolicyGuard {
    pub fn install() -> Self {
        TRANSACTION_DEPTH.with(|depth| {
            if depth.get() == 0 {
                TRANSACTION_GENSYM.with(|counter| counter.set(0));
            }
            depth.set(depth.get() + 1);
        });
        Self
    }
}

impl Drop for TransactionPolicyGuard {
    fn drop(&mut self) {
        TRANSACTION_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

pub fn transaction_policy_active() -> bool {
    TRANSACTION_DEPTH.with(|depth| depth.get() != 0)
}

/// Return an invocation-local deterministic gensym sequence number when the
/// transaction policy is active.
pub fn next_transaction_gensym() -> Option<u64> {
    if !transaction_policy_active() {
        return None;
    }
    Some(TRANSACTION_GENSYM.with(|counter| {
        let current = counter.get();
        counter.set(current.wrapping_add(1));
        current
    }))
}

fn forbidden(operation: &str) -> EvalError {
    EvalError::ForbiddenEffect(operation.to_string())
}

fn check_code_mode_cancelled() -> EvalResult<()> {
    if code_mode_cancelled() {
        Err(EvalError::Runtime(
            "Code Mode execution cancelled".to_string(),
        ))
    } else {
        Ok(())
    }
}

/// Check a native builtin at its final call boundary.
///
/// The transaction environment registers only clojurust's builtins, so this
/// denylist is the capability surface: filesystem, output, clocks, randomness,
/// process-global state, blocking/concurrency, and Rust object construction.
pub fn check_native(name: &str) -> EvalResult<()> {
    check_code_mode_cancelled()?;
    const TRANSACTION_DENIED: &[&str] = &[
        "print",
        "println",
        "pr",
        "prn",
        "printf",
        "newline",
        "flush",
        "spit",
        "slurp",
        "close",
        "nanotime",
        "sleep",
        "rand",
        "rand-int",
        "random-sample",
        "shuffle",
        "random-uuid",
        "gensym",
        "add-tap",
        "remove-tap",
        "tap>",
        "shared-atom",
        "promise",
        "deliver",
        "send",
        "send-off",
        "new",
        "Exception.",
        "push-precision!",
        "pop-precision!",
    ];
    const CODE_MODE_DENIED: &[&str] = &[
        "print",
        "println",
        "pr",
        "prn",
        "printf",
        "newline",
        "flush",
        "spit",
        "slurp",
        "close",
        "nanotime",
        "sleep",
        "rand",
        "rand-int",
        "random-sample",
        "shuffle",
        "random-uuid",
        "gensym",
        "add-tap",
        "remove-tap",
        "tap>",
        "shared-atom",
        "promise",
        "deliver",
        "send",
        "send-off",
        "new",
        "Exception.",
        "push-precision!",
        "pop-precision!",
        "find-ns",
        "create-ns",
        "remove-ns",
        "all-ns",
        "ns-map",
        "ns-interns",
        "ns-publics",
        "ns-refers",
        "ns-aliases",
        "ns-resolve",
        "resolve",
        "alter-var-root",
        "intern",
        "load-string",
    ];
    if (transaction_policy_active() && TRANSACTION_DENIED.contains(&name))
        || (code_mode_policy_active() && CODE_MODE_DENIED.contains(&name))
    {
        Err(forbidden(name))
    } else {
        Ok(())
    }
}

/// Check special forms that can reach outside the invocation environment.
pub fn check_special(name: &str) -> EvalResult<()> {
    check_code_mode_cancelled()?;
    const TRANSACTION_DENIED: &[&str] = &[
        ".",
        "ns",
        "require",
        "in-ns",
        "alias",
        "load-file",
        "with-out-str",
        "await",
    ];
    const CODE_MODE_DENIED: &[&str] = &[
        ".",
        "ns",
        "require",
        "in-ns",
        "alias",
        "load-file",
        "with-out-str",
        "set!",
    ];
    if (transaction_policy_active() && TRANSACTION_DENIED.contains(&name))
        || (code_mode_policy_active() && CODE_MODE_DENIED.contains(&name))
    {
        Err(forbidden(name))
    } else {
        Ok(())
    }
}

pub fn check_versioned_lookup() -> EvalResult<()> {
    if transaction_policy_active() || code_mode_policy_active() {
        Err(forbidden("versioned namespace lookup"))
    } else {
        Ok(())
    }
}
