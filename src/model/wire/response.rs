use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::request::FunctionCallOutput;

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::model) struct Usage {
    pub(in crate::model) input_tokens: u64,
    pub(in crate::model) input_tokens_details: InputTokenDetails,
    pub(in crate::model) output_tokens: u64,
    pub(in crate::model) output_tokens_details: OutputTokenDetails,
    pub(in crate::model) total_tokens: u64,
}

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::model) struct InputTokenDetails {
    pub(in crate::model) cached_tokens: u64,
    pub(in crate::model) cache_write_tokens: u64,
}

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::model) struct OutputTokenDetails {
    pub(in crate::model) reasoning_tokens: u64,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(in crate::model) enum ServerEvent {
    #[serde(rename = "response.created")]
    Created { response: CreatedResponse },
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta {
        delta: String,
        #[serde(default)]
        agent: Option<Agent>,
    },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta { delta: String },
    #[serde(rename = "response.reasoning_summary.delta")]
    ReasoningSummaryDelta { delta: String },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone {
        item: OutputItem,
        #[serde(default)]
        agent: Option<Agent>,
    },
    #[serde(rename = "response.inject.created")]
    InjectCreated { response_id: String },
    #[serde(rename = "response.inject.failed")]
    InjectFailed {
        response_id: String,
        input: Vec<FunctionCallOutput>,
        error: ResponseInjectError,
    },
    #[serde(rename = "response.completed")]
    Completed { response: CompletedResponse },
    #[serde(rename = "response.failed")]
    Failed,
    #[serde(rename = "response.incomplete")]
    Incomplete,
    #[serde(rename = "error")]
    Error,
    #[serde(other)]
    Other,
}

impl ServerEvent {
    pub(in crate::model) const fn is_output(&self) -> bool {
        matches!(
            self,
            Self::OutputTextDelta { .. }
                | Self::ReasoningSummaryTextDelta { .. }
                | Self::ReasoningSummaryDelta { .. }
                | Self::OutputItemDone { .. }
        )
    }
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(in crate::model) enum WarmupServerEvent {
    #[serde(rename = "response.created")]
    Created { response: WarmupResponse },
    #[serde(rename = "response.completed")]
    Completed { response: WarmupResponse },
    #[serde(rename = "response.failed")]
    Failed,
    #[serde(rename = "response.incomplete")]
    Incomplete,
    #[serde(rename = "error")]
    Error,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
pub(in crate::model) struct WarmupResponse {
    pub(in crate::model) id: String,
    #[serde(default)]
    pub(in crate::model) usage: Option<Usage>,
}

#[derive(Deserialize)]
pub(in crate::model) struct CreatedResponse {
    pub(in crate::model) id: String,
}

#[derive(Deserialize)]
pub(in crate::model) struct CompletedResponse {
    pub(in crate::model) id: String,
    pub(in crate::model) status: String,
    pub(in crate::model) output: Vec<OutputItem>,
    pub(in crate::model) usage: Usage,
}

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::model) struct Agent {
    pub(in crate::model) agent_name: String,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::model) enum MessagePhase {
    Commentary,
    FinalAnswer,
}

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::model) struct ResponseInjectError {
    pub(in crate::model) code: ResponseInjectErrorCode,
    pub(in crate::model) message: String,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::model) enum ResponseInjectErrorCode {
    ResponseAlreadyCompleted,
    ResponseNotFound,
    InvalidInput,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(in crate::model) enum OutputItem {
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
        #[serde(default)]
        caller: Option<Caller>,
        #[serde(default)]
        agent: Option<Agent>,
        #[serde(default)]
        created_by: Option<Value>,
    },
    #[serde(rename = "message")]
    Message {
        #[serde(default)]
        content: Vec<OutputContent>,
        #[serde(default)]
        agent: Option<Agent>,
        #[serde(default)]
        phase: Option<MessagePhase>,
    },
    #[serde(rename = "multi_agent_call")]
    MultiAgentCall,
    #[serde(rename = "multi_agent_call_output")]
    MultiAgentCallOutput,
    #[serde(rename = "agent_message")]
    AgentMessage,
    #[serde(rename = "compaction")]
    Compaction,
    #[serde(rename = "program")]
    Program,
    #[serde(rename = "program_output")]
    ProgramOutput,
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::model) struct ExecCommandArguments {
    pub(in crate::model) cmd: String,
    #[serde(default)]
    pub(in crate::model) workdir: Option<String>,
    #[serde(default)]
    pub(in crate::model) login: Option<bool>,
    #[serde(default)]
    pub(in crate::model) timeout_ms: Option<i64>,
    #[serde(default)]
    pub(in crate::model) max_output_tokens: Option<i64>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub(in crate::model) enum Caller {
    #[serde(rename = "program")]
    Program { caller_id: String },
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(in crate::model) enum OutputContent {
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(other)]
    Other,
}
