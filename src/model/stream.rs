use std::{
    collections::{HashSet, VecDeque},
    future::Future,
    io::Write,
    pin::Pin,
    time::Instant,
};

use futures_util::{StreamExt, stream::FuturesUnordered};
use serde::Serialize;
use serde_json::Value;

use super::{
    ApiEvent, RunStats, TRANSPORT, elapsed_ns,
    wire::{
        Agent, CompletedResponse, ExecCommandArguments, FunctionCallOutput, InputItem,
        MessagePhase, OutputContent, OutputItem, ResponseInject, ResponseInjectError,
        ResponseInjectErrorCode, ServerEvent, Usage,
    },
};
use crate::{
    AgentError, ResponsesError, Result,
    protocol::EventWriter,
    responses::{EncodedRequest, ResponsesSocket, decode_event, parse_raw_json},
    shell,
};

const ROOT_AGENT: &str = "/root";
const EXEC_COMMAND: &str = "exec_command";

type ToolFuture = Pin<Box<dyn Future<Output = Result<CompletedToolCall>> + Send>>;

pub(super) struct TurnResult {
    pub(super) id: String,
    pub(super) status: String,
    pub(super) final_message: Option<String>,
    pub(super) next_input: Vec<InputItem>,
    pub(super) usage: Usage,
    pub(super) time_to_first_event_ns: u64,
    pub(super) time_to_first_output_ns: Option<u64>,
    pub(super) tool_calls: usize,
}

struct CompletedToolCall {
    output: FunctionCallOutput,
    duration_ns: u64,
}

struct PendingInjection {
    started_at: Instant,
}

struct ResponseDriver<'a, W> {
    socket: &'a mut ResponsesSocket,
    events: &'a mut EventWriter<W>,
    stats: &'a mut RunStats,
    workspace: &'a str,
    call_index: u32,
    started_at: Instant,
    response_id: Option<String>,
    completed: Option<CompletedResponse>,
    final_message: Option<String>,
    next_input: Vec<InputItem>,
    tool_tasks: FuturesUnordered<ToolFuture>,
    tool_batch_started_at: Option<Instant>,
    seen_tool_calls: HashSet<String>,
    pending_injections: VecDeque<PendingInjection>,
    live_injection: bool,
    first_event_ns: Option<u64>,
    first_output_ns: Option<u64>,
}

#[derive(Serialize)]
struct TextDelta<'a> {
    model_call_index: u32,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<&'a Agent>,
}

#[derive(Serialize)]
struct ToolCallEvent<'a> {
    call_id: &'a str,
    tool: &'static str,
    arguments: &'a ExecCommandArguments,
    model_call_index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    caller: Option<&'a super::wire::Caller>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<&'a Agent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_by: Option<&'a Value>,
}

#[derive(Serialize)]
struct ToolResultEvent<'a> {
    call_id: &'a str,
    tool: &'static str,
    status: &'static str,
    duration_ns: u64,
    result: &'a FunctionCallOutput,
}

#[derive(Serialize)]
struct InjectionCompleted<'a> {
    response_id: &'a str,
    duration_ns: u64,
    status: &'static str,
}

pub(super) async fn receive<W: Write>(
    socket: &mut ResponsesSocket,
    events: &mut EventWriter<W>,
    stats: &mut RunStats,
    workspace: &str,
    call_index: u32,
    started_at: Instant,
    live_injection: bool,
) -> Result<TurnResult> {
    ResponseDriver::new(
        socket,
        events,
        stats,
        workspace,
        call_index,
        started_at,
        live_injection,
    )
    .drive()
    .await
}

impl<'a, W: Write> ResponseDriver<'a, W> {
    fn new(
        socket: &'a mut ResponsesSocket,
        events: &'a mut EventWriter<W>,
        stats: &'a mut RunStats,
        workspace: &'a str,
        call_index: u32,
        started_at: Instant,
        live_injection: bool,
    ) -> Self {
        Self {
            socket,
            events,
            stats,
            workspace,
            call_index,
            started_at,
            response_id: None,
            completed: None,
            final_message: None,
            next_input: Vec::new(),
            tool_tasks: FuturesUnordered::new(),
            tool_batch_started_at: None,
            seen_tool_calls: HashSet::new(),
            pending_injections: VecDeque::new(),
            live_injection,
            first_event_ns: None,
            first_output_ns: None,
        }
    }

    async fn drive(mut self) -> Result<TurnResult> {
        while !self.is_complete() {
            let needs_server_event =
                self.completed.is_none() || !self.pending_injections.is_empty();
            let has_tool_task = !self.tool_tasks.is_empty();

            match (needs_server_event, has_tool_task) {
                (true, true) => {
                    tokio::select! {
                        raw_event = self.socket.next_text() => {
                            let text = raw_event?;
                            self.handle_raw_event(text.as_str())?;
                        }
                        completed = self.tool_tasks.next() => {
                            let completed = completed.ok_or(AgentError::MalformedResponse {
                                detail: "tool task stream ended while work remained",
                            })??;
                            self.handle_tool_completion(completed).await?;
                        }
                    }
                }
                (true, false) => {
                    let text = self.socket.next_text_or_idle_timeout().await?;
                    self.handle_raw_event(text.as_str())?;
                }
                (false, true) => {
                    let completed =
                        self.tool_tasks
                            .next()
                            .await
                            .ok_or(AgentError::MalformedResponse {
                                detail: "tool task stream ended while work remained",
                            })??;
                    self.handle_tool_completion(completed).await?;
                }
                (false, false) => {
                    return Err(AgentError::MalformedResponse {
                        detail: "response driver stopped before response.completed",
                    }
                    .into());
                }
            }
        }

        self.finish()
    }

    fn is_complete(&self) -> bool {
        self.completed.is_some() && self.tool_tasks.is_empty() && self.pending_injections.is_empty()
    }

    fn handle_raw_event(&mut self, text: &str) -> Result<()> {
        let raw_event = parse_raw_json(text)?;
        let elapsed = elapsed_ns(self.started_at);
        self.first_event_ns.get_or_insert(elapsed);
        self.events.emit(
            "api.event",
            ApiEvent {
                direction: "inbound",
                transport: TRANSPORT,
                phase: "generation",
                model_call_index: Some(self.call_index),
                event: raw_event,
            },
        )?;
        let event = decode_event::<ServerEvent>(raw_event)?;
        if event.is_output() {
            self.first_output_ns.get_or_insert(elapsed);
        }

        match event {
            ServerEvent::Created { response } => {
                self.response_id = Some(response.id);
            }
            ServerEvent::OutputTextDelta { delta, agent } => {
                self.events.emit(
                    "assistant.delta",
                    TextDelta {
                        model_call_index: self.call_index,
                        text: &delta,
                        agent: agent.as_ref(),
                    },
                )?;
            }
            ServerEvent::ReasoningSummaryTextDelta { delta }
            | ServerEvent::ReasoningSummaryDelta { delta } => {
                self.events.emit(
                    "reasoning.summary.delta",
                    TextDelta {
                        model_call_index: self.call_index,
                        text: &delta,
                        agent: None,
                    },
                )?;
            }
            ServerEvent::OutputItemDone { item, agent } => {
                self.handle_output_item(item, agent, true)?;
            }
            ServerEvent::InjectCreated { response_id } => {
                self.handle_injection_created(&response_id)?;
            }
            ServerEvent::InjectFailed {
                response_id,
                input,
                error,
            } => {
                self.handle_injection_failed(raw_event.get(), &response_id, input, &error)?;
            }
            ServerEvent::Completed { mut response } => {
                self.response_id = Some(response.id.clone());
                for item in std::mem::take(&mut response.output) {
                    self.handle_output_item(item, None, false)?;
                }
                self.completed = Some(response);
            }
            ServerEvent::Error | ServerEvent::Failed | ServerEvent::Incomplete => {
                return Err(ResponsesError::Api {
                    event: raw_event.get().to_owned(),
                }
                .into());
            }
            ServerEvent::Other => {}
        }
        Ok(())
    }

    fn handle_output_item(
        &mut self,
        item: OutputItem,
        event_agent: Option<Agent>,
        count_hosted_item: bool,
    ) -> Result<()> {
        match item {
            OutputItem::FunctionCall {
                call_id,
                name,
                arguments,
                caller,
                agent,
                created_by,
            } => {
                if !self.seen_tool_calls.insert(call_id.clone()) {
                    return Ok(());
                }
                if name != EXEC_COMMAND {
                    return Err(AgentError::UnsupportedFunction { name, call_id }.into());
                }
                let arguments =
                    serde_json::from_str::<ExecCommandArguments>(&arguments).map_err(|source| {
                        ResponsesError::InvalidToolArguments {
                            call_id: call_id.clone(),
                            source,
                        }
                    })?;
                let agent = agent.or(event_agent);
                self.events.emit(
                    "tool.call",
                    ToolCallEvent {
                        call_id: &call_id,
                        tool: EXEC_COMMAND,
                        arguments: &arguments,
                        model_call_index: self.call_index,
                        caller: caller.as_ref(),
                        agent: agent.as_ref(),
                        created_by: created_by.as_ref(),
                    },
                )?;
                self.stats.tool_calls += 1;
                if self.tool_tasks.is_empty() {
                    self.tool_batch_started_at = Some(Instant::now());
                }
                let workspace = self.workspace.to_owned();
                let command = shell::ExecCommand::new(
                    arguments.cmd,
                    arguments.workdir,
                    arguments.login,
                    arguments.timeout_ms,
                    arguments.max_output_tokens,
                );
                self.tool_tasks.push(Box::pin(async move {
                    let started_at = Instant::now();
                    let execution = shell::execute(command, &workspace).await;
                    let duration_ns = elapsed_ns(started_at);
                    let output = FunctionCallOutput::new(call_id.clone(), &execution, caller)
                        .map_err(|source| ResponsesError::EncodeToolOutput { call_id, source })?;
                    Ok(CompletedToolCall {
                        output,
                        duration_ns,
                    })
                }));
            }
            OutputItem::Message {
                content,
                agent,
                phase,
            } => {
                let agent = agent.or(event_agent);
                if is_final_message(self.live_injection, agent.as_ref(), phase) {
                    self.final_message = Some(message_text(content));
                }
            }
            OutputItem::MultiAgentCall => {
                if count_hosted_item {
                    self.stats.hosted_multi_agent_calls += 1;
                }
            }
            OutputItem::AgentMessage => {
                if count_hosted_item {
                    self.stats.agent_messages += 1;
                }
            }
            OutputItem::Compaction => {
                if count_hosted_item {
                    self.stats.compactions += 1;
                }
            }
            OutputItem::MultiAgentCallOutput
            | OutputItem::Program
            | OutputItem::ProgramOutput
            | OutputItem::Other => {}
        }
        Ok(())
    }

    async fn handle_tool_completion(&mut self, completed: CompletedToolCall) -> Result<()> {
        self.stats.tool_work_duration_ns += completed.duration_ns;
        if self.tool_tasks.is_empty() {
            let batch_started_at =
                self.tool_batch_started_at
                    .take()
                    .ok_or(AgentError::MalformedResponse {
                        detail: "tool batch completed without a start timestamp",
                    })?;
            self.stats.tool_wall_duration_ns += elapsed_ns(batch_started_at);
        }
        self.events.emit(
            "tool.result",
            ToolResultEvent {
                call_id: completed.output.call_id(),
                tool: EXEC_COMMAND,
                status: "completed",
                duration_ns: completed.duration_ns,
                result: &completed.output,
            },
        )?;

        if !self.live_injection {
            self.stats.continuations_queued += 1;
            self.next_input.push(completed.output.into());
            return Ok(());
        }

        if self.completed.is_some() {
            self.stats.injections_deferred += 1;
            self.stats.continuations_queued += 1;
            self.next_input.push(completed.output.into());
            return Ok(());
        }

        let response_id = self
            .response_id
            .clone()
            .ok_or(AgentError::MalformedResponse {
                detail: "tool call completed before response.created",
            })?;
        let input = [completed.output];
        let started_at = Instant::now();
        self.send_injection(&response_id, &input, "injection")
            .await?;
        self.stats.injections_sent += 1;
        self.pending_injections
            .push_back(PendingInjection { started_at });
        Ok(())
    }

    async fn send_injection(
        &mut self,
        response_id: &str,
        input: &[FunctionCallOutput],
        phase: &'static str,
    ) -> Result<()> {
        let request = EncodedRequest::new(&ResponseInject::new(response_id, input))?;
        self.events.emit(
            "api.event",
            ApiEvent {
                direction: "outbound",
                transport: TRANSPORT,
                phase,
                model_call_index: Some(self.call_index),
                event: request.raw(),
            },
        )?;
        self.socket.send(&request).await.map_err(Into::into)
    }

    fn handle_injection_created(&mut self, response_id: &str) -> Result<()> {
        self.validate_injection_response(response_id)?;
        let pending = self
            .pending_injections
            .pop_front()
            .ok_or(AgentError::MalformedResponse {
                detail: "response.inject.created had no pending injection",
            })?;
        let duration_ns = elapsed_ns(pending.started_at);
        self.stats.injections_accepted += 1;
        self.stats.injection_ack_wait_ns += duration_ns;
        self.events.emit(
            "model.injection.completed",
            InjectionCompleted {
                response_id,
                duration_ns,
                status: "accepted",
            },
        )
    }

    fn handle_injection_failed(
        &mut self,
        raw_event: &str,
        response_id: &str,
        input: Vec<FunctionCallOutput>,
        error: &ResponseInjectError,
    ) -> Result<()> {
        self.validate_injection_response(response_id)?;
        let pending = self
            .pending_injections
            .pop_front()
            .ok_or(AgentError::MalformedResponse {
                detail: "response.inject.failed had no pending injection",
            })?;
        let duration_ns = elapsed_ns(pending.started_at);
        self.stats.injection_ack_wait_ns += duration_ns;
        if error.code != ResponseInjectErrorCode::ResponseAlreadyCompleted {
            return Err(ResponsesError::Api {
                event: raw_event.to_owned(),
            }
            .into());
        }

        self.stats.injections_deferred += 1;
        self.stats.continuations_queued += 1;
        self.next_input.extend(input.into_iter().map(Into::into));
        self.events.emit(
            "model.injection.completed",
            InjectionCompleted {
                response_id,
                duration_ns,
                status: "deferred",
            },
        )
    }

    fn validate_injection_response(&self, response_id: &str) -> Result<()> {
        if self.response_id.as_deref() == Some(response_id) {
            return Ok(());
        }
        Err(AgentError::MalformedResponse {
            detail: "injection acknowledgement referenced another response",
        }
        .into())
    }

    fn finish(mut self) -> Result<TurnResult> {
        let response = self.completed.take().ok_or(AgentError::MalformedResponse {
            detail: "response driver finished without response.completed",
        })?;
        Ok(TurnResult {
            id: response.id,
            status: response.status,
            final_message: self.final_message,
            next_input: self.next_input,
            usage: response.usage,
            time_to_first_event_ns: self.first_event_ns.unwrap_or_default(),
            time_to_first_output_ns: self.first_output_ns,
            tool_calls: self.seen_tool_calls.len(),
        })
    }
}

fn is_final_message(multi_agent: bool, agent: Option<&Agent>, phase: Option<MessagePhase>) -> bool {
    !multi_agent
        || (agent.is_some_and(|agent| agent.agent_name == ROOT_AGENT)
            && matches!(phase, Some(MessagePhase::FinalAnswer)))
}

fn message_text(content: Vec<OutputContent>) -> String {
    content
        .into_iter()
        .filter_map(|content| match content {
            OutputContent::OutputText { text } => Some(text),
            OutputContent::Other => None,
        })
        .collect()
}
