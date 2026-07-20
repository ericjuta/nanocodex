use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use clap::{ArgAction, Args, builder::NonEmptyStringValueParser};
use eyre::{Result, eyre};
use nanocodex::{AgentEvents, Nanocodex, Responses, Thinking, Tools};

use crate::mcp::McpArgs;
use crate::mpp::{MppAdapter, MppArgs};
use crate::subagents::{self, ChildAgents};

pub(crate) struct ConfiguredAgent {
    pub(crate) handle: Nanocodex,
    pub(crate) events: AgentEvents,
    pub(crate) child_agents: Option<Arc<ChildAgents>>,
    pub(crate) mpp_adapter: Option<MppAdapter>,
}

#[derive(Args)]
pub(crate) struct AgentArgs {
    /// `OpenAI` API key. Prefer `OPENAI_API_KEY` or the repository `.env` file.
    #[arg(
        long,
        env = "OPENAI_API_KEY",
        hide_env_values = true,
        value_parser = NonEmptyStringValueParser::new()
    )]
    api_key: Option<String>,

    /// Working directory exposed to the coding tools.
    #[arg(long, default_value = ".")]
    cwd: PathBuf,

    /// Reasoning effort used by the model.
    #[arg(long, env = "OPENAI_REASONING_EFFORT", default_value_t)]
    thinking: Thinking,

    /// Replace the standard system/developer instructions.
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    instructions: Option<String>,

    /// Whether standalone web search is exposed to the model.
    #[arg(
        long,
        env = "NANOCODEX_WEB_SEARCH",
        default_value_t = true,
        action = ArgAction::Set
    )]
    web_search: bool,

    /// Whether image generation is exposed to the model.
    #[arg(
        long,
        env = "NANOCODEX_IMAGE_GENERATION",
        default_value_t = true,
        action = ArgAction::Set
    )]
    image_generation: bool,

    /// Expose reusable clean, forked, and follow-up child agents in Code Mode.
    #[arg(
        long,
        env = "NANOCODEX_SUBAGENTS",
        default_value_t = false,
        action = ArgAction::Set
    )]
    subagents: bool,

    /// Responses API WebSocket endpoint.
    #[arg(
        long,
        env = "OPENAI_RESPONSES_WEBSOCKET_URL",
        default_value = "wss://api.openai.com/v1/responses"
    )]
    websocket_url: String,

    /// `OpenAI` HTTP API base used by standalone web search.
    #[arg(
        long,
        env = "OPENAI_API_BASE_URL",
        default_value = "https://api.openai.com/v1"
    )]
    api_base_url: String,

    #[command(flatten)]
    mcp: McpArgs,

    #[command(flatten)]
    mpp: MppArgs,
}

impl AgentArgs {
    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    #[cfg(test)]
    pub(crate) const fn uses_mpp(&self) -> bool {
        self.mpp.is_enabled()
    }

    pub(crate) fn build(self) -> Result<ConfiguredAgent> {
        let mpp_enabled = self.mpp.is_enabled();
        let api_key = match self.api_key {
            Some(api_key) => api_key,
            None if mpp_enabled => String::new(),
            None => return Err(eyre!("--api-key or OPENAI_API_KEY is required")),
        };
        let (websocket_url, mpp_adapter) = self.mpp.start(self.websocket_url)?;
        let responses = Responses::builder()
            .websocket_url(websocket_url)
            .api_base_url(self.api_base_url)
            .build();
        let mut tools = Tools::builder()
            .web_search(self.web_search)
            .image_generation(self.image_generation);
        if let Some(mcp) = self.mcp.build()? {
            tools = tools.provider(mcp);
        }
        let tools = tools.build()?;
        let child_agents = self.subagents.then(|| Arc::new(ChildAgents::default()));
        let builder = Nanocodex::builder(api_key)
            .thinking(self.thinking)
            .workspace(self.cwd)
            .responses(responses);
        let builder = if let Some(child_agents) = &child_agents {
            let tools = tools.clone();
            let child_agents = Arc::downgrade(child_agents);
            builder.tools_factory(move |agent| {
                subagents::with_subagents(tools.clone(), agent, child_agents.clone())
            })
        } else {
            builder.tools(tools)
        };
        let builder = if let Some(instructions) = self.instructions {
            builder.instructions(instructions)
        } else {
            builder
        };
        let (handle, events) = builder.build()?;
        Ok(ConfiguredAgent {
            handle,
            events,
            child_agents,
            mpp_adapter,
        })
    }
}
