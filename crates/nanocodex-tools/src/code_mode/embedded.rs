use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{Arc, mpsc as std_mpsc},
    thread,
    time::Instant,
};

use cljrs_async::{
    await_value, cancel_future, eval_async::eval_async, isolate::Isolate, spawn_future,
    task_scope::FutureTaskScope,
};
use cljrs_env::{
    env::Env,
    error::EvalError,
    gc_roots::{ValueRootGuard, root_value},
    policy::{CodeModePolicy, CodeModePolicyGuard},
};
use cljrs_gc::GcPtr;
use cljrs_interop::Registry;
use cljrs_reader::Parser;
use cljrs_types::{error::CljxError, span::Span};
use cljrs_value::{
    Arity, CljxFuture, ExceptionInfo, FutureState, Keyword, MapValue, NativeFn, PersistentVector,
    Value, ValueError,
};
use num_bigint::BigInt;
use serde_json::{Map, Number, Value as JsonValue, json};
use tokio::sync::mpsc;

use super::{
    CellDiagnostic, CellDiagnosticKind, CellExceptionInfo, CellLocationPrecision,
    CellSourceLocation, RuntimeEvent, ToolOutputContent,
};

pub(super) struct EmbeddedHost {
    command_tx: mpsc::UnboundedSender<HostCommand>,
    events: mpsc::UnboundedReceiver<RuntimeEvent>,
    policy: Arc<CodeModePolicy>,
    worker: Option<thread::JoinHandle<()>>,
}

enum HostCommand {
    Start(StartExecution),
    ToolResult {
        execution_id: u64,
        id: u64,
        value: JsonValue,
        success: bool,
    },
    Shutdown,
}

struct StartExecution {
    execution_id: u64,
    parent_call_id: String,
    source: String,
    tools: Vec<JsonValue>,
    stored: HashMap<String, JsonValue>,
}

struct PendingTool {
    _root: ValueRootGuard,
    value: Box<Value>,
    call_id: String,
    name: String,
    input: JsonValue,
    started_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScopePolicy {
    CancelPending,
    KeepRunning,
}

struct ToolScope {
    id: u64,
    on_error: ScopePolicy,
    on_exit: ScopePolicy,
    tool_ids: Vec<u64>,
}

struct ExecutionState {
    execution_id: u64,
    parent_call_id: String,
    event_tx: mpsc::UnboundedSender<RuntimeEvent>,
    pending_tools: HashMap<u64, PendingTool>,
    next_tool_id: u64,
    tool_scopes: Vec<ToolScope>,
    next_scope_id: u64,
    content: Vec<ToolOutputContent>,
    stored: HashMap<String, JsonValue>,
    stored_writes: HashMap<String, JsonValue>,
    tools: Vec<JsonValue>,
    exit_requested: bool,
}

struct CellEvalFailure {
    error: EvalError,
    form_index: usize,
    location: CellSourceLocation,
}

enum WorkerControl {
    Continue,
    Shutdown,
}

impl EmbeddedHost {
    pub(super) fn spawn() -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, events) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        let policy = CodeModePolicy::new();
        let worker_policy = Arc::clone(&policy);

        let worker = Isolate::new("nanocodex-code-mode-cljrs")
            .try_spawn(move || async move {
                run_worker(command_rx, event_tx, ready_tx, worker_policy).await;
            })
            .map_err(|error| format!("failed to start embedded cljrs Code Mode host: {error}"))?;

        ready_rx
            .recv()
            .map_err(|_| "embedded cljrs Code Mode host ended during startup".to_owned())??;

        Ok(Self {
            command_tx,
            events,
            policy,
            worker: Some(worker),
        })
    }

    pub(super) fn start_cell(
        &self,
        execution_id: u64,
        parent_call_id: &str,
        source: &str,
        stored: HashMap<String, JsonValue>,
        tools: Vec<JsonValue>,
    ) -> Result<(), String> {
        self.command_tx
            .send(HostCommand::Start(StartExecution {
                execution_id,
                parent_call_id: parent_call_id.to_owned(),
                source: source.to_owned(),
                tools,
                stored,
            }))
            .map_err(|_| "embedded cljrs Code Mode host is unavailable".to_owned())
    }

    pub(super) async fn read_event(&mut self) -> Result<RuntimeEvent, String> {
        self.events
            .recv()
            .await
            .ok_or_else(|| "embedded cljrs Code Mode host ended before a result".to_owned())
    }

    pub(super) fn send_tool_result(
        &self,
        execution_id: u64,
        id: u64,
        value: JsonValue,
        success: bool,
    ) -> Result<(), String> {
        self.command_tx
            .send(HostCommand::ToolResult {
                execution_id,
                id,
                value,
                success,
            })
            .map_err(|_| {
                "embedded cljrs Code Mode host closed before accepting a tool result".to_owned()
            })
    }

    pub(super) async fn terminate(&mut self) {
        self.policy.cancel();
        let _ = self.command_tx.send(HostCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = tokio::task::spawn_blocking(move || worker.join()).await;
        }
    }
}

impl Drop for EmbeddedHost {
    fn drop(&mut self) {
        self.policy.cancel();
        let _ = self.command_tx.send(HostCommand::Shutdown);
    }
}

async fn run_worker(
    mut command_rx: mpsc::UnboundedReceiver<HostCommand>,
    event_tx: mpsc::UnboundedSender<RuntimeEvent>,
    ready_tx: std_mpsc::SyncSender<Result<(), String>>,
    policy: Arc<CodeModePolicy>,
) {
    let globals = cljrs_stdlib::standard_env_no_ir();
    cljrs_async::init(&globals);
    let _policy = CodeModePolicyGuard::install(policy);

    if ready_tx.send(Ok(())).is_err() {
        return;
    }

    while let Some(command) = command_rx.recv().await {
        match command {
            HostCommand::Start(start) => {
                if matches!(
                    run_execution(start, &globals, &event_tx, &mut command_rx).await,
                    WorkerControl::Shutdown
                ) {
                    break;
                }
            }
            HostCommand::ToolResult { .. } => {}
            HostCommand::Shutdown => break,
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one loop owns the embedded cell lifecycle and command multiplexing"
)]
async fn run_execution(
    start: StartExecution,
    globals: &Arc<cljrs_env::env::GlobalEnv>,
    event_tx: &mpsc::UnboundedSender<RuntimeEvent>,
    command_rx: &mut mpsc::UnboundedReceiver<HostCommand>,
) -> WorkerControl {
    let cell_ns = format!("nanocodex.cell.{}", start.execution_id);
    globals.get_or_create_ns(&cell_ns);
    globals.refer_all(&cell_ns, "clojure.core");

    let state = Rc::new(RefCell::new(ExecutionState {
        execution_id: start.execution_id,
        parent_call_id: start.parent_call_id,
        event_tx: event_tx.clone(),
        pending_tools: HashMap::new(),
        next_tool_id: 1,
        tool_scopes: Vec::new(),
        next_scope_id: 1,
        content: Vec::new(),
        stored: start.stored,
        stored_writes: HashMap::new(),
        tools: start.tools,
        exit_requested: false,
    }));
    install_helpers(globals, &cell_ns, &state);

    let cell_source = start.source.clone();
    let mut parser = Parser::new(start.source, format!("<{cell_ns}>"));
    let forms = match parser.parse_all() {
        Ok(forms) => forms,
        Err(error) => {
            finish_with_error(globals, &cell_ns, &state, read_diagnostic(error));
            return WorkerControl::Continue;
        }
    };

    let mut env = Env::new(Arc::clone(globals), &cell_ns);
    let _env_root = cljrs_env::gc_roots::push_env_root(&env);
    let scope = FutureTaskScope::new();
    let scope_guard = scope.install();
    let result = {
        let evaluation = async {
            let mut value = Value::Nil;
            for (index, form) in forms.iter().enumerate() {
                value = match eval_async(form, &mut env).await {
                    Ok(value) => value,
                    Err(error) => {
                        return Err(CellEvalFailure {
                            error,
                            form_index: index.saturating_add(1),
                            location: form_location(&form.span, &cell_source),
                        });
                    }
                };
            }
            Ok::<Value, CellEvalFailure>(value)
        };
        tokio::pin!(evaluation);

        loop {
            tokio::select! {
                result = &mut evaluation => break Some(result),
                command = command_rx.recv() => {
                    match command {
                        Some(HostCommand::ToolResult {
                            execution_id,
                            id,
                            value,
                            success,
                        }) if execution_id == start.execution_id => {
                            settle_tool_result(&state, id, value, success);
                        }
                        Some(HostCommand::Shutdown) | None => break None,
                        Some(HostCommand::Start(other)) => {
                            let _ = event_tx.send(RuntimeEvent::Error {
                                cell_id: other.execution_id,
                                diagnostic: CellDiagnostic::runtime(
                                    "embedded cljrs host received a cell while another cell was active",
                                ),
                                content: Vec::new(),
                                stored: HashMap::new(),
                            });
                        }
                        Some(HostCommand::ToolResult { .. }) => {}
                    }
                }
            }
        }
    };

    scope.cancel_all();
    scope.join_all().await;
    drop(scope_guard);
    cancel_pending_tools(&state);

    if let Some(result) = result {
        let exit_requested = state.borrow().exit_requested;
        match result {
            Ok(_) => finish_with_success(globals, &cell_ns, &state),
            Err(CellEvalFailure {
                error: EvalError::Thrown(_),
                ..
            }) if exit_requested => {
                finish_with_success(globals, &cell_ns, &state);
            }
            Err(failure) => {
                finish_with_error(globals, &cell_ns, &state, eval_diagnostic(failure));
            }
        }
        WorkerControl::Continue
    } else {
        remove_cell_namespaces(globals, &cell_ns);
        WorkerControl::Shutdown
    }
}

#[allow(clippy::too_many_lines)]
fn install_helpers(
    globals: &Arc<cljrs_env::env::GlobalEnv>,
    cell_ns: &str,
    state: &Rc<RefCell<ExecutionState>>,
) {
    let registry = Registry::new(Arc::clone(globals));

    let tool_state = Rc::clone(state);
    registry.define(
        "nanocodex.tools/call",
        NativeFn::with_closure("nanocodex/tools-call", Arity::Fixed(2), move |args| {
            start_tool_call(&tool_state, args)
        }),
    );

    let alias_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "tool-call",
        NativeFn::with_closure("nanocodex/tool-call", Arity::Fixed(2), move |args| {
            start_tool_call(&alias_state, args)
        }),
    );

    let text_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "text",
        NativeFn::with_closure("nanocodex/text", Arity::Fixed(1), move |args| {
            let text = stringify_value(&args[0]);
            text_state
                .borrow_mut()
                .content
                .push(ToolOutputContent::InputText { text });
            Ok(Value::Nil)
        }),
    );

    let notify_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "notify",
        NativeFn::with_closure("nanocodex/notify", Arity::Fixed(1), move |args| {
            let text = stringify_value(&args[0]);
            if text.trim().is_empty() {
                return Err(ValueError::Other(
                    "notify expects non-empty text".to_owned(),
                ));
            }
            let state = notify_state.borrow();
            state
                .event_tx
                .send(RuntimeEvent::Notify {
                    cell_id: state.execution_id,
                    text,
                })
                .map_err(|_| ValueError::Other("Code Mode observer closed".to_owned()))?;
            Ok(Value::Nil)
        }),
    );

    let image_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "image",
        NativeFn::with_closure("nanocodex/image", Arity::Variadic { min: 1 }, move |args| {
            append_image(&image_state, args)?;
            Ok(Value::Nil)
        }),
    );

    let generated_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "generated-image",
        NativeFn::with_closure("nanocodex/generated-image", Arity::Fixed(1), move |args| {
            let value = value_to_json(&args[0]).map_err(ValueError::Other)?;
            let object = value.as_object().ok_or_else(|| {
                ValueError::Other(
                    "generated-image expects an image generation result map".to_owned(),
                )
            })?;
            let hint = object.get("output_hint").cloned();
            append_image_json(&generated_state, value, None)?;
            if let Some(hint) = hint {
                let hint = hint.as_str().ok_or_else(|| {
                    ValueError::Other(
                        "generated-image output_hint must be a string when provided".to_owned(),
                    )
                })?;
                generated_state
                    .borrow_mut()
                    .content
                    .push(ToolOutputContent::InputText {
                        text: hint.to_owned(),
                    });
            }
            Ok(Value::Nil)
        }),
    );

    let store_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "store",
        NativeFn::with_closure("nanocodex/store", Arity::Fixed(2), move |args| {
            let key = storage_key(&args[0])?;
            let value = value_to_json(&args[1]).map_err(ValueError::Other)?;
            let mut state = store_state.borrow_mut();
            state.stored.insert(key.clone(), value.clone());
            state.stored_writes.insert(key, value);
            Ok(Value::Nil)
        }),
    );

    let load_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "load",
        NativeFn::with_closure("nanocodex/load", Arity::Fixed(1), move |args| {
            let key = storage_key(&args[0])?;
            let value = load_state
                .borrow()
                .stored
                .get(&key)
                .cloned()
                .unwrap_or(JsonValue::Null);
            json_to_value(value).map_err(ValueError::Other)
        }),
    );

    let yield_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "yield-control",
        NativeFn::with_closure("nanocodex/yield-control", Arity::Fixed(0), move |_args| {
            let mut state = yield_state.borrow_mut();
            let content = std::mem::take(&mut state.content);
            state
                .event_tx
                .send(RuntimeEvent::Yielded {
                    cell_id: state.execution_id,
                    content,
                })
                .map_err(|_| ValueError::Other("Code Mode observer closed".to_owned()))?;
            Ok(Value::Nil)
        }),
    );

    let exit_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "exit",
        NativeFn::with_closure("nanocodex/exit", Arity::Fixed(0), move |_args| {
            exit_state.borrow_mut().exit_requested = true;
            Err(ValueError::Thrown(Value::Nil))
        }),
    );

    let metadata_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "all-tools",
        NativeFn::with_closure(
            "nanocodex/all-tools",
            Arity::Variadic { min: 0 },
            move |args| all_tools(&metadata_state, args),
        ),
    );

    let info_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "tool-info",
        NativeFn::with_closure("nanocodex/tool-info", Arity::Fixed(1), move |args| {
            tool_info(&info_state, args)
        }),
    );

    let pending_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "pending-tools",
        NativeFn::with_closure("nanocodex/pending-tools", Arity::Fixed(0), move |_args| {
            pending_tools(&pending_state)
        }),
    );

    let cancel_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "cancel-tool",
        NativeFn::with_closure("nanocodex/cancel-tool", Arity::Fixed(1), move |args| {
            cancel_tool(&cancel_state, args)
        }),
    );

    let status_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "tool-status",
        NativeFn::with_closure("nanocodex/tool-status", Arity::Fixed(1), move |args| {
            Ok(tool_status(&status_state, args))
        }),
    );

    let await_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "await-tool",
        NativeFn::with_closure("nanocodex/await-tool", Arity::Fixed(1), move |args| {
            await_tool(&await_state, args)
        }),
    );

    let scope_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "with-tool-scope",
        NativeFn::with_closure("nanocodex/with-tool-scope", Arity::Fixed(2), move |args| {
            with_tool_scope(&scope_state, args)
        }),
    );

    let runtime_info_state = Rc::clone(state);
    registry.define_in(
        cell_ns,
        "code-mode-info",
        NativeFn::with_closure("nanocodex/code-mode-info", Arity::Fixed(0), move |_args| {
            Ok(code_mode_info(&runtime_info_state))
        }),
    );
}

fn all_tools(state: &Rc<RefCell<ExecutionState>>, args: &[Value]) -> Result<Value, ValueError> {
    if args.len() > 1 {
        return Err(ValueError::Other(
            "all-tools accepts at most one string or map query".to_owned(),
        ));
    }
    match args.first() {
        None | Some(Value::Nil) => {
            let tools = state.borrow().tools.clone();
            json_to_value(JsonValue::Array(tools)).map_err(ValueError::Other)
        }
        Some(Value::Map(query)) => all_tools_map_query(state, query),
        Some(other) => {
            let query = tool_name(other)?.to_lowercase();
            let tools = state
                .borrow()
                .tools
                .iter()
                .filter(|tool| {
                    tool.get("name")
                        .and_then(JsonValue::as_str)
                        .is_some_and(|name| name.to_lowercase().contains(&query))
                        || tool
                            .get("description")
                            .and_then(JsonValue::as_str)
                            .is_some_and(|description| description.to_lowercase().contains(&query))
                })
                .cloned()
                .collect();
            json_to_value(JsonValue::Array(tools)).map_err(ValueError::Other)
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "map-query option parsing stays local to one discovery helper"
)]
fn all_tools_map_query(
    state: &Rc<RefCell<ExecutionState>>,
    query: &MapValue,
) -> Result<Value, ValueError> {
    let mut unknown = Vec::new();
    query.for_each(|key, _| {
        if let Value::Keyword(k) = key {
            let name = k.get().name.as_ref();
            if !matches!(
                name,
                "query" | "kind" | "dynamic" | "limit" | "cursor" | "include-schema"
            ) {
                unknown.push(name.to_owned());
            }
        }
    });
    if !unknown.is_empty() {
        unknown.sort();
        return Err(ValueError::Other(format!(
            "all-tools unknown options: {}",
            unknown.join(", ")
        )));
    }

    let text_query = query
        .get(&Value::keyword(Keyword::simple("query")))
        .and_then(|value| match value {
            Value::Str(s) => Some(s.get().to_lowercase()),
            Value::Keyword(k) => Some(k.get().full_name().to_lowercase()),
            _ => None,
        });
    let kind_filter = query
        .get(&Value::keyword(Keyword::simple("kind")))
        .and_then(|value| match value {
            Value::Str(s) => Some(s.get().clone()),
            Value::Keyword(k) => Some(k.get().name.as_ref().to_owned()),
            _ => None,
        });
    let dynamic_filter = query
        .get(&Value::keyword(Keyword::simple("dynamic")))
        .and_then(|value| match value {
            Value::Bool(b) => Some(b),
            _ => None,
        });
    let limit = query
        .get(&Value::keyword(Keyword::simple("limit")))
        .and_then(|value| match value {
            Value::Long(n) if n >= 0 => usize::try_from(n).ok(),
            _ => None,
        });
    let cursor = query
        .get(&Value::keyword(Keyword::simple("cursor")))
        .and_then(|value| match value {
            Value::Str(s) => s.get().parse::<usize>().ok(),
            Value::Long(n) if n >= 0 => usize::try_from(n).ok(),
            _ => None,
        })
        .unwrap_or(0);
    let include_schema = match query.get(&Value::keyword(Keyword::simple("include-schema"))) {
        Some(Value::Bool(b)) => b,
        Some(Value::Nil) => false,
        _ => true,
    };

    let mut tools = state.borrow().tools.clone();
    tools.sort_by(|left, right| {
        left.get("name")
            .and_then(JsonValue::as_str)
            .cmp(&right.get("name").and_then(JsonValue::as_str))
    });
    let filtered = tools
        .into_iter()
        .filter(|tool| {
            text_query.as_ref().is_none_or(|query| {
                tool.get("name")
                    .and_then(JsonValue::as_str)
                    .is_some_and(|name| name.to_lowercase().contains(query))
                    || tool
                        .get("description")
                        .and_then(JsonValue::as_str)
                        .is_some_and(|description| description.to_lowercase().contains(query))
            }) && kind_filter.as_ref().is_none_or(|kind| {
                tool.get("kind").and_then(JsonValue::as_str) == Some(kind.as_str())
            }) && dynamic_filter.is_none_or(|dynamic| {
                tool.get("dynamic").and_then(JsonValue::as_bool) == Some(dynamic)
            })
        })
        .collect::<Vec<_>>();
    let page = filtered
        .into_iter()
        .skip(cursor)
        .take(limit.unwrap_or(usize::MAX))
        .map(|mut tool| {
            if !include_schema && let Some(object) = tool.as_object_mut() {
                object.remove("input_schema");
                object.remove("output_schema");
            }
            tool
        })
        .collect::<Vec<_>>();
    let next_cursor = cursor.saturating_add(page.len());
    json_to_value(json!({
        "tools": page,
        "next_cursor": next_cursor,
    }))
    .map_err(ValueError::Other)
}

fn tool_info(state: &Rc<RefCell<ExecutionState>>, args: &[Value]) -> Result<Value, ValueError> {
    let name = tool_name(&args[0])?;
    let tool = state
        .borrow()
        .tools
        .iter()
        .find(|tool| tool.get("name").and_then(JsonValue::as_str) == Some(name.as_str()))
        .cloned();
    tool.map_or(Ok(Value::Nil), |tool| {
        json_to_value(tool).map_err(ValueError::Other)
    })
}

fn tool_name(value: &Value) -> Result<String, ValueError> {
    match value {
        Value::Str(value) => Ok(value.get().clone()),
        Value::Keyword(value) => Ok(value.get().full_name()),
        other => Err(ValueError::Other(format!(
            "tool name or query must be a string or keyword, got {}",
            other.type_name()
        ))),
    }
}

fn pending_tools(state: &Rc<RefCell<ExecutionState>>) -> Result<Value, ValueError> {
    let mut tools = state
        .borrow()
        .pending_tools
        .iter()
        .map(|(id, tool)| {
            json!({
                "id": id,
                "call_id": tool.call_id,
                "name": tool.name,
                "input": tool.input,
                "state": future_state_name(tool.value.as_ref()),
                "started_at_ms": u64::try_from(tool.started_at.elapsed().as_millis())
                    .unwrap_or(u64::MAX),
            })
        })
        .collect::<Vec<_>>();
    tools.sort_unstable_by_key(|tool| tool.get("id").and_then(JsonValue::as_u64));
    json_to_value(JsonValue::Array(tools)).map_err(ValueError::Other)
}

fn cancel_tool(state: &Rc<RefCell<ExecutionState>>, args: &[Value]) -> Result<Value, ValueError> {
    let Some(id) = resolve_tool_id(state, &args[0]) else {
        return Ok(Value::Bool(false));
    };
    cancel_tool_id(state, id)
}

fn cancel_tool_id(state: &Rc<RefCell<ExecutionState>>, id: u64) -> Result<Value, ValueError> {
    let cancellation = {
        let state = state.borrow();
        state.pending_tools.get(&id).map(|tool| {
            (
                RuntimeEvent::CancelTool {
                    cell_id: state.execution_id,
                    id,
                    call_id: tool.call_id.clone(),
                    name: tool.name.clone(),
                    input: tool.input.clone(),
                },
                state.event_tx.clone(),
            )
        })
    };
    let Some((event, event_tx)) = cancellation else {
        return Ok(Value::Bool(false));
    };
    event_tx
        .send(event)
        .map_err(|_| ValueError::Other("Code Mode observer closed".to_owned()))?;
    if let Some(tool) = state.borrow_mut().pending_tools.remove(&id) {
        cancel_tool_future(&tool);
    }
    Ok(Value::Bool(true))
}

fn cancel_tool_future(tool: &PendingTool) {
    if let Value::Future(future) = tool.value.as_ref() {
        cancel_future(future);
    }
}

fn tool_status(state: &Rc<RefCell<ExecutionState>>, args: &[Value]) -> Value {
    let Some(id) = resolve_tool_id(state, &args[0]) else {
        return Value::Nil;
    };
    let id_value = Value::Long(i64::try_from(id).unwrap_or(i64::MAX));
    let state = state.borrow();
    let Some(tool) = state.pending_tools.get(&id) else {
        return Value::Map(MapValue::from_pairs(vec![
            (Value::keyword(Keyword::simple("id")), id_value),
            (
                Value::keyword(Keyword::simple("state")),
                Value::keyword(Keyword::simple("unknown")),
            ),
        ]));
    };
    Value::Map(MapValue::from_pairs(vec![
        (Value::keyword(Keyword::simple("id")), id_value),
        (
            Value::keyword(Keyword::simple("call-id")),
            Value::Str(GcPtr::new(tool.call_id.clone())),
        ),
        (
            Value::keyword(Keyword::simple("name")),
            Value::Str(GcPtr::new(tool.name.clone())),
        ),
        (
            Value::keyword(Keyword::simple("state")),
            Value::keyword(Keyword::simple(future_state_name(tool.value.as_ref()))),
        ),
        (
            Value::keyword(Keyword::simple("elapsed-ms")),
            Value::Long(i64::try_from(tool.started_at.elapsed().as_millis()).unwrap_or(i64::MAX)),
        ),
    ]))
}

fn await_tool(state: &Rc<RefCell<ExecutionState>>, args: &[Value]) -> Result<Value, ValueError> {
    let Some(id) = resolve_tool_id(state, &args[0]) else {
        return Err(ValueError::Other(
            "await-tool expects a pending tool future or id".to_owned(),
        ));
    };
    let future = {
        let state = state.borrow();
        let Some(tool) = state.pending_tools.get(&id) else {
            return Err(ValueError::Other(format!(
                "await-tool: no pending tool with id {id}"
            )));
        };
        tool.value.as_ref().clone()
    };
    Ok(future)
}

fn with_tool_scope(
    state: &Rc<RefCell<ExecutionState>>,
    args: &[Value],
) -> Result<Value, ValueError> {
    let opts = match args.first() {
        Some(Value::Map(map)) => map.clone(),
        Some(Value::Nil) | None => MapValue::empty(),
        Some(other) => {
            return Err(ValueError::Other(format!(
                "with-tool-scope expects an options map, got {}",
                other.type_name()
            )));
        }
    };
    let thunk = args.get(1).cloned().unwrap_or(Value::Nil);
    let on_error_opt = opts.get(&Value::keyword(Keyword::simple("on-error")));
    let on_exit_opt = opts.get(&Value::keyword(Keyword::simple("on-exit")));
    let on_error = scope_policy(on_error_opt.as_ref(), ScopePolicy::KeepRunning)?;
    let on_exit = scope_policy(on_exit_opt.as_ref(), ScopePolicy::CancelPending)?;
    let state = Rc::clone(state);
    let (globals, ns) = cljrs_env::callback::capture_eval_context().ok_or_else(|| {
        ValueError::Other("with-tool-scope called outside an eval context".to_owned())
    })?;
    Ok(spawn_future(async move {
        let scope_id = {
            let mut state = state.borrow_mut();
            let id = state.next_scope_id;
            state.next_scope_id = state.next_scope_id.saturating_add(1);
            state.tool_scopes.push(ToolScope {
                id,
                on_error,
                on_exit,
                tool_ids: Vec::new(),
            });
            id
        };
        let mut env = Env::new(globals, &ns);
        let applied = cljrs_env::apply::apply_value(&thunk, Vec::new(), &mut env);
        let result = match applied {
            Ok(value) => await_value(value).await,
            Err(error) => Err(error),
        };
        finish_tool_scope(&state, scope_id, result.is_err());
        result
    }))
}

fn scope_policy(value: Option<&Value>, default: ScopePolicy) -> Result<ScopePolicy, ValueError> {
    match value {
        None | Some(Value::Nil) => Ok(default),
        Some(Value::Keyword(k)) if k.get().name.as_ref() == "cancel-pending" => {
            Ok(ScopePolicy::CancelPending)
        }
        Some(Value::Keyword(k)) if k.get().name.as_ref() == "keep-running" => {
            Ok(ScopePolicy::KeepRunning)
        }
        Some(other) => Err(ValueError::Other(format!(
            "scope policy must be :cancel-pending or :keep-running, got {}",
            other.type_name()
        ))),
    }
}

fn finish_tool_scope(state: &Rc<RefCell<ExecutionState>>, scope_id: u64, errored: bool) {
    let (policy, tool_ids) = {
        let mut state = state.borrow_mut();
        let Some(index) = state
            .tool_scopes
            .iter()
            .position(|scope| scope.id == scope_id)
        else {
            return;
        };
        let scope = state.tool_scopes.remove(index);
        let policy = if errored {
            scope.on_error
        } else {
            scope.on_exit
        };
        (policy, scope.tool_ids)
    };
    if policy == ScopePolicy::CancelPending {
        for id in tool_ids {
            let _ = cancel_tool_id(state, id);
        }
    }
}

fn code_mode_info(state: &Rc<RefCell<ExecutionState>>) -> Value {
    let pending = state.borrow().pending_tools.len();
    let tool_count = state.borrow().tools.len();
    json_to_value(json!({
        "runtime": "cljrs",
        "cljrs_async": "0.1.228",
        "nanocodex_tools": env!("CARGO_PKG_VERSION"),
        "supported_async_forms": [
            "await", "do", "if", "let", "loop", "try", "binding", "and", "or", "letfn", "with-out-str"
        ],
        "supported_combinators": [
            "timeout", "alts", "join-all", "join-all-settled", "race", "await-with-timeout", "with-tool-scope"
        ],
        "pending_tools": pending,
        "enabled_tools": tool_count,
        "budgets": {
            "gas": "policy-owned",
            "cell_local_handles": true
        }
    }))
    .unwrap_or(Value::Nil)
}

fn resolve_tool_id(state: &Rc<RefCell<ExecutionState>>, value: &Value) -> Option<u64> {
    match value {
        Value::Long(id) if *id >= 0 => u64::try_from(*id).ok(),
        Value::Future(target) => state.borrow().pending_tools.iter().find_map(|(id, tool)| {
            let Value::Future(candidate) = tool.value.as_ref() else {
                return None;
            };
            GcPtr::ptr_eq(candidate, target).then_some(*id)
        }),
        _ => None,
    }
}

fn future_state_name(value: &Value) -> &'static str {
    let Value::Future(future) = value else {
        return "unknown";
    };
    match &*future
        .get()
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        FutureState::Running => "running",
        FutureState::Done(_) => "done",
        FutureState::Failed(_) => "failed",
        FutureState::GasExhausted => "gas-exhausted",
        FutureState::Cancelled => "cancelled",
    }
}

fn start_tool_call(
    state: &Rc<RefCell<ExecutionState>>,
    args: &[Value],
) -> Result<Value, ValueError> {
    let name = match &args[0] {
        Value::Str(name) => name.get().clone(),
        other => {
            return Err(ValueError::Other(format!(
                "tool call expects a string tool name, got {}",
                other.type_name()
            )));
        }
    };
    let input = value_to_json(&args[1]).map_err(ValueError::Other)?;
    validate_tool_input(state, &name, &input)?;
    let future = GcPtr::new(CljxFuture::new());
    let rooted = Box::new(Value::Future(future.clone()));
    let root = root_value(rooted.as_ref());

    let mut state = state.borrow_mut();
    let id = state.next_tool_id;
    state.next_tool_id = state.next_tool_id.saturating_add(1);
    let call_id = format!("{}/code-{id}", state.parent_call_id);
    if let Some(scope) = state.tool_scopes.last_mut() {
        scope.tool_ids.push(id);
    }
    state.pending_tools.insert(
        id,
        PendingTool {
            _root: root,
            value: rooted,
            call_id,
            name: name.clone(),
            input: input.clone(),
            started_at: Instant::now(),
        },
    );
    state
        .event_tx
        .send(RuntimeEvent::ToolCall {
            cell_id: state.execution_id,
            id,
            name,
            input,
        })
        .map_err(|_| ValueError::Other("Code Mode observer closed".to_owned()))?;
    Ok(Value::Future(future))
}

fn validate_tool_input(
    state: &Rc<RefCell<ExecutionState>>,
    name: &str,
    input: &JsonValue,
) -> Result<(), ValueError> {
    let schema = state.borrow().tools.iter().find_map(|tool| {
        (tool.get("name").and_then(JsonValue::as_str) == Some(name))
            .then(|| tool.get("input_schema").cloned())
            .flatten()
    });
    let Some(schema) = schema else {
        return Ok(());
    };
    if schema.is_null() {
        return Ok(());
    }
    if let Err(error) = jsonschema::validate(&schema, input) {
        let path = error.instance_path().to_string();
        let schema_path = error.schema_path().to_string();
        let message = format!("tool input invalid for `{name}`: {error}");
        let data = MapValue::from_pairs(vec![
            (
                Value::keyword(Keyword::simple("type")),
                Value::keyword(Keyword::simple("tool-input-invalid")),
            ),
            (
                Value::keyword(Keyword::simple("tool")),
                Value::Str(GcPtr::new(name.to_owned())),
            ),
            (
                Value::keyword(Keyword::simple("instance-path")),
                Value::Str(GcPtr::new(path)),
            ),
            (
                Value::keyword(Keyword::simple("schema-path")),
                Value::Str(GcPtr::new(schema_path)),
            ),
            (
                Value::keyword(Keyword::simple("expected")),
                json_to_value(schema).unwrap_or(Value::Nil),
            ),
            (
                Value::keyword(Keyword::simple("value")),
                json_to_value(input.clone()).unwrap_or(Value::Nil),
            ),
        ]);
        return Err(ValueError::Thrown(Value::Error(GcPtr::new(
            ExceptionInfo::new(
                ValueError::Other(message.clone()),
                message,
                Some(data),
                None,
            ),
        ))));
    }
    Ok(())
}

fn settle_tool_result(
    state: &Rc<RefCell<ExecutionState>>,
    id: u64,
    value: JsonValue,
    success: bool,
) {
    let pending = {
        let mut state = state.borrow_mut();
        let Some(pending) = state.pending_tools.remove(&id) else {
            return;
        };
        pending
    };
    let Value::Future(future) = pending.value.as_ref() else {
        return;
    };
    let next = if success {
        match json_to_value(value) {
            Ok(value) => FutureState::Done(value),
            Err(error) => FutureState::Failed(EvalError::Runtime(error).to_error_value()),
        }
    } else {
        FutureState::Failed(tool_failure_value(&pending, value))
    };
    let mut future_state = future
        .get()
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if matches!(*future_state, FutureState::Running) {
        *future_state = next;
        drop(future_state);
        future.get().cond.notify_all();
    }
}

fn tool_failure_value(pending: &PendingTool, output: JsonValue) -> Value {
    let message = output
        .as_str()
        .or_else(|| output.get("error").and_then(JsonValue::as_str))
        .or_else(|| output.get("message").and_then(JsonValue::as_str))
        .map_or_else(
            || format!("nested tool `{}` failed", pending.name),
            str::to_owned,
        );
    let input =
        json_to_value(pending.input.clone()).unwrap_or_else(|error| Value::Str(GcPtr::new(error)));
    let output = json_to_value(output).unwrap_or_else(|error| Value::Str(GcPtr::new(error)));
    let data = MapValue::from_pairs(vec![
        (
            Value::keyword(Keyword::simple("type")),
            Value::keyword(Keyword::simple("nested-tool-failure")),
        ),
        (
            Value::keyword(Keyword::simple("tool")),
            Value::Str(GcPtr::new(pending.name.clone())),
        ),
        (Value::keyword(Keyword::simple("input")), input),
        (
            Value::keyword(Keyword::simple("call-id")),
            Value::Str(GcPtr::new(pending.call_id.clone())),
        ),
        (Value::keyword(Keyword::simple("output")), output),
    ]);
    Value::Error(GcPtr::new(ExceptionInfo::new(
        ValueError::Other(message.clone()),
        message,
        Some(data),
        None,
    )))
}

fn cancel_pending_tools(state: &Rc<RefCell<ExecutionState>>) {
    let pending = std::mem::take(&mut state.borrow_mut().pending_tools);
    for tool in pending.into_values() {
        cancel_tool_future(&tool);
    }
}

fn append_image(state: &Rc<RefCell<ExecutionState>>, args: &[Value]) -> Result<(), ValueError> {
    let value = value_to_json(&args[0]).map_err(ValueError::Other)?;
    let detail = args
        .get(1)
        .filter(|value| !matches!(value, Value::Nil))
        .map(value_to_json)
        .transpose()
        .map_err(ValueError::Other)?;
    append_image_json(state, value, detail)
}

fn append_image_json(
    state: &Rc<RefCell<ExecutionState>>,
    value: JsonValue,
    explicit_detail: Option<JsonValue>,
) -> Result<(), ValueError> {
    let (image_url, embedded_detail) = match value {
        JsonValue::String(image_url) => (image_url, None),
        JsonValue::Object(object) => {
            let image_url = object
                .get("image_url")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| ValueError::Other(image_helper_error().to_owned()))?
                .to_owned();
            (image_url, object.get("detail").cloned())
        }
        _ => return Err(ValueError::Other(image_helper_error().to_owned())),
    };
    if image_url.is_empty() {
        return Err(ValueError::Other(image_helper_error().to_owned()));
    }
    let Some((scheme, _)) = image_url.split_once(':') else {
        return Err(ValueError::Other(
            "invalid image output; pass a base64 data URI".to_owned(),
        ));
    };
    if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
        return Err(ValueError::Other(
            "remote image URLs are not supported in tool outputs; pass a base64 data URI"
                .to_owned(),
        ));
    }
    if !scheme.eq_ignore_ascii_case("data") {
        return Err(ValueError::Other(
            "invalid image output; pass a base64 data URI".to_owned(),
        ));
    }

    let detail = explicit_detail
        .or(embedded_detail)
        .unwrap_or_else(|| JsonValue::String("high".to_owned()));
    let detail = detail.as_str().ok_or_else(|| {
        ValueError::Other("image detail must be one of: auto, low, high, original".to_owned())
    })?;
    let detail = detail.to_ascii_lowercase();
    if !matches!(detail.as_str(), "auto" | "low" | "high" | "original") {
        return Err(ValueError::Other(
            "image detail must be one of: auto, low, high, original".to_owned(),
        ));
    }

    let content = serde_json::from_value(json!({
        "type": "input_image",
        "image_url": image_url,
        "detail": detail,
    }))
    .map_err(|error| ValueError::Other(format!("invalid image output: {error}")))?;
    state.borrow_mut().content.push(content);
    Ok(())
}

fn image_helper_error() -> &'static str {
    "image expects a non-empty data URI string or a map with :image_url and optional :detail"
}

fn storage_key(value: &Value) -> Result<String, ValueError> {
    match value {
        Value::Str(value) => Ok(value.get().clone()),
        Value::Keyword(value) => Ok(value.get().full_name()),
        other => Err(ValueError::Other(format!(
            "storage key must be a string or keyword, got {}",
            other.type_name()
        ))),
    }
}

fn stringify_value(value: &Value) -> String {
    if let Value::Str(value) = value {
        return value.get().clone();
    }
    value_to_json(value)
        .and_then(|value| serde_json::to_string(&value).map_err(|error| error.to_string()))
        .unwrap_or_else(|_| format!("{value}"))
}

fn read_diagnostic(error: CljxError) -> CellDiagnostic {
    match error {
        CljxError::ReadError { message, span, src }
        | CljxError::EvalError { message, span, src } => {
            let location = span.map(|span| {
                exact_source_location(src.name(), src.inner(), span.offset(), span.len())
            });
            CellDiagnostic {
                kind: CellDiagnosticKind::Read,
                message,
                form_index: None,
                location,
                exception: None,
            }
        }
        CljxError::Io(error) => CellDiagnostic {
            kind: CellDiagnosticKind::Read,
            message: error.to_string(),
            form_index: None,
            location: None,
            exception: None,
        },
        CljxError::SerializationError { message } => CellDiagnostic {
            kind: CellDiagnosticKind::Read,
            message,
            form_index: None,
            location: None,
            exception: None,
        },
    }
}

fn eval_diagnostic(failure: CellEvalFailure) -> CellDiagnostic {
    let kind = match &failure.error {
        EvalError::Runtime(_) => CellDiagnosticKind::Runtime,
        EvalError::GasExhausted => CellDiagnosticKind::GasExhausted,
        EvalError::ForbiddenEffect(_) => CellDiagnosticKind::ForbiddenEffect,
        EvalError::UnboundSymbol(_) => CellDiagnosticKind::UnboundSymbol,
        EvalError::Arity { .. } => CellDiagnosticKind::Arity,
        EvalError::NotCallable(_) => CellDiagnosticKind::NotCallable,
        EvalError::Thrown(_) => CellDiagnosticKind::Thrown,
        EvalError::Read(_) => CellDiagnosticKind::Read,
        EvalError::Recur(_) => CellDiagnosticKind::InternalRecur,
        EvalError::CommitSignatureVerificationFailed { .. } => {
            CellDiagnosticKind::CommitSignatureVerificationFailed
        }
    };
    let exception = match &failure.error {
        EvalError::Thrown(Value::Error(exception)) => Some(exception_info(exception.get(), 0)),
        _ => None,
    };
    CellDiagnostic {
        kind,
        message: failure.error.to_string(),
        form_index: Some(failure.form_index),
        location: Some(failure.location),
        exception,
    }
}

fn exception_info(exception: &ExceptionInfo, depth: usize) -> CellExceptionInfo {
    let (data, data_unavailable) = match exception.data() {
        Some(data) => match value_to_json(&Value::Map(data)) {
            Ok(data) => (Some(data), None),
            Err(error) => (None, Some(error)),
        },
        None => (None, None),
    };
    let cause = if depth < 31 {
        exception
            .cause()
            .map(|cause| Box::new(exception_info(cause.get(), depth.saturating_add(1))))
    } else {
        None
    };
    CellExceptionInfo {
        message: exception.message(),
        data,
        data_unavailable,
        cause,
    }
}

fn form_location(span: &Span, source: &str) -> CellSourceLocation {
    let (excerpt, caret_column) = source_excerpt(source, span.start, span.col);
    CellSourceLocation {
        source: span.file.as_ref().clone(),
        byte_start: span.start,
        byte_end: span.end,
        line: span.line,
        column: span.col,
        precision: CellLocationPrecision::EnclosingTopLevelForm,
        excerpt,
        caret_column,
    }
}

fn exact_source_location(
    source_name: &str,
    source: &str,
    byte_start: usize,
    byte_len: usize,
) -> CellSourceLocation {
    let (line, column) = byte_line_column(source, byte_start);
    let (excerpt, caret_column) = source_excerpt(source, byte_start, column);
    CellSourceLocation {
        source: source_name.to_owned(),
        byte_start,
        byte_end: byte_start.saturating_add(byte_len),
        line,
        column,
        precision: CellLocationPrecision::Exact,
        excerpt,
        caret_column,
    }
}

fn source_excerpt(source: &str, byte_start: usize, column: u32) -> (Option<String>, Option<u32>) {
    const MAX_EXCERPT_CHARS: usize = 160;
    let offset = byte_start.min(source.len());
    let line_start = source[..offset].rfind('\n').map_or(0, |idx| idx + 1);
    let line_end = source[offset..]
        .find('\n')
        .map_or(source.len(), |idx| offset + idx);
    let mut line = source[line_start..line_end].to_owned();
    if line.chars().count() > MAX_EXCERPT_CHARS {
        line = line.chars().take(MAX_EXCERPT_CHARS).collect();
        line.push('…');
    }
    if line.is_empty() {
        return (None, None);
    }
    (Some(line), Some(column.max(1)))
}

fn byte_line_column(source: &str, offset: usize) -> (u32, u32) {
    let mut line = 1_u32;
    let mut column = 1_u32;
    for byte in source.as_bytes().iter().take(offset.min(source.len())) {
        if *byte == b'\n' {
            line = line.saturating_add(1);
            column = 1;
        } else {
            column = column.saturating_add(1);
        }
    }
    (line, column)
}

fn finish_with_success(
    globals: &Arc<cljrs_env::env::GlobalEnv>,
    cell_ns: &str,
    state: &Rc<RefCell<ExecutionState>>,
) {
    let mut state = state.borrow_mut();
    let event = RuntimeEvent::Done {
        cell_id: state.execution_id,
        content: std::mem::take(&mut state.content),
        stored: std::mem::take(&mut state.stored_writes),
    };
    remove_cell_namespaces(globals, cell_ns);
    let _ = state.event_tx.send(event);
}

fn finish_with_error(
    globals: &Arc<cljrs_env::env::GlobalEnv>,
    cell_ns: &str,
    state: &Rc<RefCell<ExecutionState>>,
    diagnostic: CellDiagnostic,
) {
    let mut state = state.borrow_mut();
    let event = RuntimeEvent::Error {
        cell_id: state.execution_id,
        diagnostic,
        content: std::mem::take(&mut state.content),
        stored: std::mem::take(&mut state.stored_writes),
    };
    remove_cell_namespaces(globals, cell_ns);
    let _ = state.event_tx.send(event);
}

fn remove_cell_namespaces(globals: &cljrs_env::env::GlobalEnv, cell_ns: &str) {
    let mut namespaces = globals
        .namespaces
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    namespaces.remove(cell_ns);
    namespaces.remove("nanocodex.tools");
}

fn json_to_value(value: JsonValue) -> Result<Value, String> {
    match value {
        JsonValue::Null => Ok(Value::Nil),
        JsonValue::Bool(value) => Ok(Value::Bool(value)),
        JsonValue::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(Value::Long(value))
            } else if let Some(value) = value.as_u64() {
                Ok(Value::BigInt(GcPtr::new(BigInt::from(value))))
            } else {
                value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .map(Value::Double)
                    .ok_or_else(|| format!("unsupported JSON number: {value}"))
            }
        }
        JsonValue::String(value) => Ok(Value::Str(GcPtr::new(value))),
        JsonValue::Array(values) => values
            .into_iter()
            .map(json_to_value)
            .collect::<Result<PersistentVector, _>>()
            .map(|value| Value::Vector(GcPtr::new(value))),
        JsonValue::Object(values) => values
            .into_iter()
            .map(|(key, value)| {
                Ok((
                    Value::Keyword(GcPtr::new(Keyword::simple(key))),
                    json_to_value(value)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()
            .map(MapValue::from_pairs)
            .map(Value::Map),
    }
}

fn value_to_json(value: &Value) -> Result<JsonValue, String> {
    match value {
        Value::Nil => Ok(JsonValue::Null),
        Value::Bool(value) => Ok(JsonValue::Bool(*value)),
        Value::Long(value) => Ok(JsonValue::Number(Number::from(*value))),
        Value::BigInt(value) => bigint_to_json(value.get()),
        Value::Double(value) => Number::from_f64(*value)
            .map(JsonValue::Number)
            .ok_or_else(|| format!("non-finite number cannot cross the tool boundary: {value}")),
        Value::Char(value) => Ok(JsonValue::String(value.to_string())),
        Value::Str(value) => Ok(JsonValue::String(value.get().clone())),
        Value::Keyword(value) => Ok(JsonValue::String(value.get().full_name())),
        Value::List(value) => value
            .get()
            .iter()
            .map(value_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(JsonValue::Array),
        Value::Vector(value) => value
            .get()
            .iter()
            .map(value_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(JsonValue::Array),
        Value::Map(value) => {
            let mut output = Map::with_capacity(value.count());
            for (key, value) in value.iter() {
                let key = match key {
                    Value::Str(key) => key.get().clone(),
                    Value::Keyword(key) => key.get().full_name(),
                    other => {
                        return Err(format!(
                            "Clojure map keys crossing the tool boundary must be strings or keywords, got {}",
                            other.type_name()
                        ));
                    }
                };
                if output.contains_key(&key) {
                    return Err(format!(
                        "Clojure map contains duplicate JSON object key `{key}` after string/keyword normalization"
                    ));
                }
                output.insert(key, value_to_json(value)?);
            }
            Ok(JsonValue::Object(output))
        }
        Value::WithMeta(value, _) => value_to_json(value),
        other => Err(format!(
            "unsupported Clojure value at the tool boundary: {}",
            other.type_name()
        )),
    }
}

fn bigint_to_json(value: &BigInt) -> Result<JsonValue, String> {
    let encoded = value.to_string();
    let number = if encoded.starts_with('-') {
        encoded.parse::<i64>().map(Number::from)
    } else {
        encoded.parse::<u64>().map(Number::from)
    }
    .map_err(|_| format!("big integer is outside JSON's exact integer range: {encoded}"))?;
    Ok(JsonValue::Number(number))
}

#[cfg(test)]
mod boundary_tests {
    use super::{json_to_value, value_to_json};
    use cljrs_gc::GcPtr;
    use cljrs_value::{Keyword, MapValue, Value};
    use serde_json::json;

    #[test]
    fn large_unsigned_json_integers_round_trip_exactly() {
        for original in [
            json!(9_007_199_254_740_993_u64),
            json!(i64::MAX),
            json!(u64::MAX),
        ] {
            let value = json_to_value(original.clone()).expect("JSON integer should convert");
            assert_eq!(
                value_to_json(&value).expect("Clojure integer should convert"),
                original
            );
        }
        let value = json_to_value(json!(u64::MAX)).expect("u64::MAX should convert");
        assert!(matches!(value, Value::BigInt(_)));
    }

    #[test]
    fn colliding_string_and_keyword_map_keys_are_rejected() {
        let value = Value::Map(MapValue::from_pairs(vec![
            (Value::Str(GcPtr::new("x".to_owned())), Value::Long(1)),
            (
                Value::Keyword(GcPtr::new(Keyword::simple("x"))),
                Value::Long(2),
            ),
        ]));
        let error = value_to_json(&value).expect_err("colliding keys should fail");
        assert!(error.contains("duplicate JSON object key `x`"));
    }
}
