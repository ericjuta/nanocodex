use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ModelConfig;
use crate::{protocol::Task, shell::ShellCommandOutput};

const BASE_INSTRUCTIONS: &str = r"You are a coding agent running non-interactively inside an isolated evaluation container.

Complete the user's task autonomously. Local shell access is available only through Programmatic Tool Calling: write hosted JavaScript that invokes tools.shell. Continue until the requested change is implemented and checked; do not merely explain what should be done. You have full permission inside the container and must not ask for approval.

<tool_orchestration>
Treat each generated JavaScript program as one bounded semantic phase. Within a phase, continue through every mechanically predictable step before emitting text. Return control to the model only when the next action requires semantic judgment, the phase is complete, or the phase cannot proceed safely.

Run independent read-only shell actions concurrently with Promise.all. Sequence dependent actions and all mutations. Never mutate the same workspace concurrently. Put sequential commands that share one timeout and output budget in a single shell action. If an expected command fails, gather the diagnostics needed to decide what to do next in the same program. After a successful mutation, run its mechanically determined verification in the same program.

Process and reduce intermediate results in JavaScript instead of forwarding every raw result. Emit one compact JSON result containing the phase status, relevant evidence, verification, and any failure that needs model judgment. Do not repeat completed calls. Retry a transient failure at most once.
</tool_orchestration>

Shell actions always run from the task workspace. Keep commands scoped to the task. When finished, give a concise summary of the changes and verification.";
const PROMPT_CACHE_KEY: &str = "harness-openai-coding-v1";
const PROGRAMMATIC_CALLER: [&str; 1] = ["programmatic"];

#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum InputItem {
    Message(MessageInput),
    ShellCallOutput(ShellCallOutput),
}

#[derive(Serialize)]
pub(super) struct MessageInput {
    #[serde(rename = "type")]
    kind: &'static str,
    role: &'static str,
    content: [InputText; 1],
}

#[derive(Serialize)]
pub(super) struct ShellCallOutput {
    #[serde(rename = "type")]
    kind: &'static str,
    call_id: String,
    max_output_length: u64,
    output: Vec<ShellCommandOutput>,
    caller: Caller,
}

#[derive(Serialize)]
pub(super) struct ResponseCreate<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    model: &'a str,
    instructions: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
    input: &'a [InputItem],
    tools: ToolDefinitions,
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    reasoning: ReasoningControls,
    store: bool,
    stream: bool,
    prompt_cache_key: &'static str,
    text: TextControls,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Usage {
    pub(super) input_tokens: u64,
    pub(super) input_tokens_details: InputTokenDetails,
    pub(super) output_tokens: u64,
    pub(super) output_tokens_details: OutputTokenDetails,
    pub(super) total_tokens: u64,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct InputTokenDetails {
    pub(super) cached_tokens: u64,
    pub(super) cache_write_tokens: u64,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct OutputTokenDetails {
    pub(super) reasoning_tokens: u64,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(super) enum ServerEvent {
    #[serde(rename = "response.created")]
    Created { response: CreatedResponse },
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta { delta: String },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta { delta: String },
    #[serde(rename = "response.reasoning_summary.delta")]
    ReasoningSummaryDelta { delta: String },
    #[serde(rename = "response.output_item.done")]
    OutputItemDone { item: OutputItem },
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
    pub(super) const fn is_output(&self) -> bool {
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
pub(super) struct CreatedResponse {
    pub(super) id: String,
}

#[derive(Deserialize)]
pub(super) struct CompletedResponse {
    pub(super) id: String,
    pub(super) status: String,
    pub(super) output: Vec<OutputItem>,
    pub(super) usage: Usage,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(super) enum OutputItem {
    #[serde(rename = "shell_call")]
    ShellCall {
        call_id: String,
        action: ShellAction,
        caller: Caller,
        #[serde(default)]
        created_by: Option<Value>,
    },
    #[serde(rename = "message")]
    Message {
        #[serde(default)]
        content: Vec<OutputContent>,
    },
    #[serde(rename = "program")]
    Program,
    #[serde(rename = "program_output")]
    ProgramOutput,
    #[serde(other)]
    Other,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct ShellAction {
    pub(super) commands: Vec<String>,
    #[serde(default)]
    pub(super) timeout_ms: Option<i64>,
    #[serde(default)]
    pub(super) max_output_length: Option<i64>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub(super) enum Caller {
    #[serde(rename = "program")]
    Program { caller_id: String },
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(super) enum OutputContent {
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(other)]
    Other,
}

#[derive(Clone, Serialize)]
pub(super) struct InputText {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

#[derive(Clone, Copy, Serialize)]
struct ReasoningControls {
    effort: &'static str,
    summary: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct TextControls {
    verbosity: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct ShellTool {
    #[serde(rename = "type")]
    kind: &'static str,
    environment: LocalEnvironment,
    allowed_callers: [&'static str; 1],
}

#[derive(Clone, Copy, Serialize)]
struct LocalEnvironment {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct ToolDefinitions(ShellTool, ProgrammaticTool);

#[derive(Clone, Copy, Serialize)]
struct ProgrammaticTool {
    #[serde(rename = "type")]
    kind: &'static str,
}

const SHELL_TOOL: ShellTool = ShellTool {
    kind: "shell",
    environment: LocalEnvironment { kind: "local" },
    allowed_callers: PROGRAMMATIC_CALLER,
};

const PROGRAMMATIC_TOOL: ProgrammaticTool = ProgrammaticTool {
    kind: "programmatic_tool_calling",
};

impl InputItem {
    pub(super) fn initial_task(task: &Task, workspace: &str) -> Vec<Self> {
        vec![Self::Message(MessageInput {
            kind: "message",
            role: "user",
            content: [InputText {
                kind: "input_text",
                text: format!(
                    "{}\n\n<environment_context>\n<cwd>{workspace}</cwd>\n<shell>/bin/sh</shell>\n</environment_context>",
                    task.instruction
                ),
            }],
        })]
    }
}

impl ShellCallOutput {
    pub(super) fn new(
        call_id: String,
        max_output_length: u64,
        output: Vec<ShellCommandOutput>,
        caller: Caller,
    ) -> Self {
        Self {
            kind: "shell_call_output",
            call_id,
            max_output_length,
            output,
            caller,
        }
    }

    pub(super) fn call_id(&self) -> &str {
        &self.call_id
    }
}

impl From<ShellCallOutput> for InputItem {
    fn from(output: ShellCallOutput) -> Self {
        Self::ShellCallOutput(output)
    }
}

impl<'a> ResponseCreate<'a> {
    pub(super) fn new(
        config: &'a ModelConfig,
        input: &'a [InputItem],
        previous_response_id: Option<&'a str>,
    ) -> Self {
        Self {
            kind: "response.create",
            model: &config.model,
            instructions: BASE_INSTRUCTIONS,
            previous_response_id,
            input,
            tools: ToolDefinitions(SHELL_TOOL, PROGRAMMATIC_TOOL),
            tool_choice: "auto",
            parallel_tool_calls: true,
            reasoning: ReasoningControls {
                effort: config.effort.as_str(),
                summary: "auto",
            },
            store: false,
            stream: true,
            prompt_cache_key: PROMPT_CACHE_KEY,
            text: TextControls { verbosity: "low" },
        }
    }
}
