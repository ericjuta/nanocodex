use serde::{Deserialize, Serialize};
use serde_json::{json, value::RawValue};
use sha2::{Digest, Sha256};

use super::response::Caller;
use crate::{
    ResponsesError,
    model::{MAX_CONCURRENT_SUBAGENTS, ModelConfig},
    protocol::Task,
    shell::ExecCommandResult,
};

const CACHE_PROFILE_VERSION: &str = "openai-coding-v13";
const PROJECT_CONTEXT_HEADER: &str = "# Project context";

pub(in crate::model) struct RequestProfile {
    prompt_cache_key: String,
    tools: Box<RawValue>,
}

impl RequestProfile {
    pub(in crate::model) fn new(config: &ModelConfig) -> Result<Self, ResponsesError> {
        let tools = tool_catalog(config)?;
        let mut hasher = Sha256::new();
        hasher.update(config.model.as_bytes());
        hasher.update([0]);
        hasher.update(CACHE_PROFILE_VERSION.as_bytes());
        hasher.update([0]);
        hasher.update(ModelConfig::system_prompt().as_bytes());
        hasher.update([0]);
        hasher.update(tools.get().as_bytes());
        let mut digest = format!("{:x}", hasher.finalize());
        digest.truncate(48);
        Ok(Self {
            prompt_cache_key: format!("harness:{digest}"),
            tools,
        })
    }

    pub(in crate::model) fn prompt_cache_key(&self) -> &str {
        &self.prompt_cache_key
    }

    fn tools(&self) -> &RawValue {
        &self.tools
    }
}

#[derive(Serialize)]
#[serde(untagged)]
pub(in crate::model) enum InputItem {
    Message(MessageInput),
    FunctionCallOutput(FunctionCallOutput),
}

#[derive(Serialize)]
pub(in crate::model) struct MessageInput {
    #[serde(rename = "type")]
    kind: &'static str,
    role: &'static str,
    content: Vec<InputText>,
}

impl MessageInput {
    fn project_context(workspace: &str, project_instructions: Option<&str>) -> Self {
        let mut content = vec![InputText::stable(PROJECT_CONTEXT_HEADER, true)];
        if let Some(project_instructions) = project_instructions {
            content.push(InputText::new(format!(
                "# AGENTS.md instructions for {workspace}\n\n<INSTRUCTIONS>\n{project_instructions}\n</INSTRUCTIONS>"
            )));
        }
        content.push(InputText::new(format!(
            "<environment_context>\n<cwd>{workspace}</cwd>\n<shell>/bin/sh</shell>\n</environment_context>"
        )));
        Self {
            kind: "message",
            role: "user",
            content,
        }
    }

    fn user(task: &Task) -> Self {
        Self {
            kind: "message",
            role: "user",
            content: vec![InputText::new(task.instruction.clone())],
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(in crate::model) struct FunctionCallOutput {
    #[serde(rename = "type")]
    kind: FunctionCallOutputKind,
    call_id: String,
    output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    caller: Option<Caller>,
}

impl FunctionCallOutput {
    pub(in crate::model) fn new(
        call_id: String,
        result: &ExecCommandResult,
        caller: Option<Caller>,
    ) -> serde_json::Result<Self> {
        Ok(Self {
            kind: FunctionCallOutputKind::FunctionCallOutput,
            call_id,
            output: serde_json::to_string(result)?,
            caller,
        })
    }

    pub(in crate::model) fn call_id(&self) -> &str {
        &self.call_id
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
enum FunctionCallOutputKind {
    #[serde(rename = "function_call_output")]
    FunctionCallOutput,
}

impl From<FunctionCallOutput> for InputItem {
    fn from(output: FunctionCallOutput) -> Self {
        Self::FunctionCallOutput(output)
    }
}

impl InputItem {
    pub(in crate::model) fn for_task(
        task: &Task,
        workspace: &str,
        project_instructions: Option<&str>,
    ) -> Vec<Self> {
        vec![
            Self::Message(MessageInput::project_context(
                workspace,
                project_instructions,
            )),
            Self::Message(MessageInput::user(task)),
        ]
    }
}

#[derive(Serialize)]
pub(in crate::model) struct ResponseCreate<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    model: &'a str,
    instructions: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
    input: &'a [InputItem],
    tools: &'a RawValue,
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    reasoning: ReasoningControls,
    context_management: [CompactionControl; 1],
    store: bool,
    generate: bool,
    prompt_cache_key: &'a str,
    prompt_cache_options: PromptCacheOptions,
    text: TextControls,
    #[serde(skip_serializing_if = "Option::is_none")]
    multi_agent: Option<MultiAgentControls>,
}

impl<'a> ResponseCreate<'a> {
    pub(in crate::model) fn warmup(
        config: &'a ModelConfig,
        input: &'a [InputItem],
        profile: &'a RequestProfile,
    ) -> Self {
        Self::new(config, input, None, false, profile)
    }

    pub(in crate::model) fn continued(
        config: &'a ModelConfig,
        input: &'a [InputItem],
        previous_response_id: &'a str,
        profile: &'a RequestProfile,
    ) -> Self {
        Self::new(config, input, Some(previous_response_id), true, profile)
    }

    fn new(
        config: &'a ModelConfig,
        input: &'a [InputItem],
        previous_response_id: Option<&'a str>,
        generate: bool,
        profile: &'a RequestProfile,
    ) -> Self {
        Self {
            kind: "response.create",
            model: &config.model,
            instructions: ModelConfig::system_prompt(),
            previous_response_id,
            input,
            tools: profile.tools(),
            tool_choice: "auto",
            parallel_tool_calls: true,
            reasoning: ReasoningControls {
                effort: config.effort.as_str(),
                mode: "standard",
                context: "all_turns",
            },
            context_management: [CompactionControl {
                kind: "compaction",
                compact_threshold: config.compact_threshold,
            }],
            store: true,
            generate,
            prompt_cache_key: profile.prompt_cache_key(),
            prompt_cache_options: PromptCacheOptions {
                mode: "explicit",
                ttl: "30m",
            },
            text: TextControls { verbosity: "low" },
            multi_agent: config.multi_agent.then_some(MultiAgentControls {
                enabled: true,
                max_concurrent_subagents: MAX_CONCURRENT_SUBAGENTS,
            }),
        }
    }
}

#[derive(Serialize)]
pub(in crate::model) struct ResponseInject<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    response_id: &'a str,
    input: &'a [FunctionCallOutput],
}

impl<'a> ResponseInject<'a> {
    pub(in crate::model) const fn new(
        response_id: &'a str,
        input: &'a [FunctionCallOutput],
    ) -> Self {
        Self {
            kind: "response.inject",
            response_id,
            input,
        }
    }
}

#[derive(Clone, Serialize)]
struct InputText {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_breakpoint: Option<CacheBreakpoint>,
}

impl InputText {
    fn new(text: String) -> Self {
        Self {
            kind: "input_text",
            text,
            prompt_cache_breakpoint: None,
        }
    }

    fn stable(text: &'static str, breakpoint: bool) -> Self {
        Self {
            kind: "input_text",
            text: text.to_owned(),
            prompt_cache_breakpoint: breakpoint.then_some(CacheBreakpoint { mode: "explicit" }),
        }
    }
}

#[derive(Clone, Copy, Serialize)]
struct CacheBreakpoint {
    mode: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct PromptCacheOptions {
    mode: &'static str,
    ttl: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct ReasoningControls {
    effort: &'static str,
    mode: &'static str,
    context: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct CompactionControl {
    #[serde(rename = "type")]
    kind: &'static str,
    compact_threshold: u64,
}

#[derive(Clone, Copy, Serialize)]
struct TextControls {
    verbosity: &'static str,
}

#[derive(Clone, Copy, Serialize)]
struct MultiAgentControls {
    enabled: bool,
    max_concurrent_subagents: u32,
}

fn tool_catalog(config: &ModelConfig) -> Result<Box<RawValue>, ResponsesError> {
    let caller = if config.multi_agent {
        "direct"
    } else {
        "programmatic"
    };
    let mut tools = vec![json!({
        "type": "function",
        "name": "exec_command",
        "description": "Runs a shell command to completion, returning bounded output and timing.",
        "strict": false,
        "parameters": {
            "type": "object",
            "properties": {
                "cmd": {
                    "type": "string",
                    "description": "Shell command to execute."
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory for the command. Defaults to the task workspace."
                },
                "login": {
                    "type": "boolean",
                    "description": "True runs with login-shell semantics; false disables them. Defaults to true."
                },
                "max_output_tokens": {
                    "type": "integer",
                    "description": "Approximate output token budget. Defaults to 1024 tokens."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Maximum command runtime. Defaults to 120000 ms."
                }
            },
            "required": ["cmd"],
            "additionalProperties": false
        },
        "output_schema": {
            "type": "object",
            "properties": {
                "wall_time_seconds": {
                    "type": "number",
                    "description": "Elapsed wall time spent executing the command."
                },
                "exit_code": {
                    "type": "integer",
                    "description": "Process exit code, omitted when the command timed out."
                },
                "output": {
                    "type": "string",
                    "description": "Combined stdout and stderr, possibly truncated."
                }
            },
            "required": ["wall_time_seconds", "output"],
            "additionalProperties": false
        },
        "allowed_callers": [caller]
    })];
    if !config.multi_agent {
        tools.push(json!({ "type": "programmatic_tool_calling" }));
    }
    serde_json::value::to_raw_value(&tools).map_err(ResponsesError::EncodeRequest)
}
