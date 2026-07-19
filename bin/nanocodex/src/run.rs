use std::io;

use clap::{Args, builder::NonEmptyStringValueParser};
use eyre::Result;

use crate::config::AgentArgs;

#[derive(Args)]
pub(crate) struct Run {
    /// Prompt submitted to the agent.
    #[arg(value_parser = NonEmptyStringValueParser::new())]
    prompt: String,
}

impl Run {
    pub(crate) async fn run(self, config: AgentArgs) -> Result<()> {
        let (handle, mut events) = config.build()?;
        let turn = handle.prompt(self.prompt).await?;
        events.write_turn_jsonl(io::stdout()).await?;
        turn.result().await?;
        Ok(())
    }
}
