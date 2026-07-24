use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{Arc, mpsc as std_mpsc},
    thread,
};

use cljrs_async::{eval_async::eval_async, isolate::Isolate, task_scope::FutureTaskScope};
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
}

struct ExecutionState {
    execution_id: u64,
    parent_call_id: String,
    event_tx: mpsc::UnboundedSender<RuntimeEvent>,
    pending_tools: HashMap<u64, PendingTool>,
    next_tool_id: u64,
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
        content: Vec::new(),
        stored: start.stored,
        stored_writes: HashMap::new(),
        tools: start.tools,
        exit_requested: false,
    }));
    install_helpers(globals, &cell_ns, &state);

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
                            location: form_location(&form.span),
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
}

fn all_tools(state: &Rc<RefCell<ExecutionState>>, args: &[Value]) -> Result<Value, ValueError> {
    if args.len() > 1 {
        return Err(ValueError::Other(
            "all-tools accepts at most one string query".to_owned(),
        ));
    }
    let query = args
        .first()
        .map(tool_name)
        .transpose()?
        .map(|query| query.to_lowercase());
    let tools = state
        .borrow()
        .tools
        .iter()
        .filter(|tool| {
            query.as_ref().is_none_or(|query| {
                tool.get("name")
                    .and_then(JsonValue::as_str)
                    .is_some_and(|name| name.to_lowercase().contains(query))
                    || tool
                        .get("description")
                        .and_then(JsonValue::as_str)
                        .is_some_and(|description| description.to_lowercase().contains(query))
            })
        })
        .cloned()
        .collect();
    json_to_value(JsonValue::Array(tools)).map_err(ValueError::Other)
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
            })
        })
        .collect::<Vec<_>>();
    tools.sort_unstable_by_key(|tool| tool.get("id").and_then(JsonValue::as_u64));
    json_to_value(JsonValue::Array(tools)).map_err(ValueError::Other)
}

fn cancel_tool(state: &Rc<RefCell<ExecutionState>>, args: &[Value]) -> Result<Value, ValueError> {
    let Value::Future(target) = &args[0] else {
        return Err(ValueError::Other(format!(
            "cancel-tool expects a nested tool future, got {}",
            args[0].type_name()
        )));
    };
    let cancellation = {
        let state = state.borrow();
        state.pending_tools.iter().find_map(|(id, tool)| {
            let Value::Future(candidate) = tool.value.as_ref() else {
                return None;
            };
            GcPtr::ptr_eq(candidate, target).then(|| {
                (
                    *id,
                    RuntimeEvent::CancelTool {
                        cell_id: state.execution_id,
                        id: *id,
                        call_id: tool.call_id.clone(),
                        name: tool.name.clone(),
                        input: tool.input.clone(),
                    },
                    state.event_tx.clone(),
                )
            })
        })
    };
    let Some((id, event, event_tx)) = cancellation else {
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
        let mut future_state = future
            .get()
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *future_state = FutureState::Cancelled;
        future.get().cond.notify_all();
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
    let future = GcPtr::new(CljxFuture::new());
    let rooted = Box::new(Value::Future(future.clone()));
    let root = root_value(rooted.as_ref());

    let mut state = state.borrow_mut();
    let id = state.next_tool_id;
    state.next_tool_id = state.next_tool_id.saturating_add(1);
    let call_id = format!("{}/code-{id}", state.parent_call_id);
    state.pending_tools.insert(
        id,
        PendingTool {
            _root: root,
            value: rooted,
            call_id,
            name: name.clone(),
            input: input.clone(),
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
    *future_state = next;
    future.get().cond.notify_all();
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

fn form_location(span: &Span) -> CellSourceLocation {
    CellSourceLocation {
        source: span.file.as_ref().clone(),
        byte_start: span.start,
        byte_end: span.end,
        line: span.line,
        column: span.col,
        precision: CellLocationPrecision::EnclosingTopLevelForm,
    }
}

fn exact_source_location(
    source_name: &str,
    source: &str,
    byte_start: usize,
    byte_len: usize,
) -> CellSourceLocation {
    let (line, column) = byte_line_column(source, byte_start);
    CellSourceLocation {
        source: source_name.to_owned(),
        byte_start,
        byte_end: byte_start.saturating_add(byte_len),
        line,
        column,
        precision: CellLocationPrecision::Exact,
    }
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
