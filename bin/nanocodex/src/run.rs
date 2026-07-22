use std::io;

use clap::{Args, builder::NonEmptyStringValueParser};
use eyre::Result;

use crate::config::AgentArgs;

#[derive(Args)]
pub(crate) struct Run {
    /// Prompt submitted to the agent.
    #[arg(value_parser = NonEmptyStringValueParser::new())]
    prompt: String,

    /// Submit the same prompt as sequential follow-on turns on one owned session.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=100))]
    repeat: u16,
}

impl Run {
    pub(crate) async fn run(self, config: AgentArgs) -> Result<()> {
        let configured = config.build()?;
        let handle = configured.handle;
        let mut events = configured.events;
        let result = async {
            for _ in 0..self.repeat {
                let turn = handle.prompt(self.prompt.clone()).await?;
                events.write_turn_jsonl(io::stdout()).await?;
                turn.result().await?;
            }
            Ok(())
        }
        .await;
        drop(handle);
        let cleanup = if let Some(child_agents) = configured.child_agents {
            child_agents.shutdown().await
        } else {
            Ok(())
        };
        preserve_primary_with_cleanup(result, cleanup)
    }
}

pub(crate) fn preserve_primary_with_cleanup<T>(
    result: Result<T>,
    cleanup: std::result::Result<(), io::Error>,
) -> Result<T> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup)) => Err(cleanup.into()),
        (Err(primary), Ok(())) => Err(primary),
        (Err(primary), Err(cleanup)) => {
            let context = format!("{primary}; child-agent cleanup also failed: {cleanup}");
            Err(primary.wrap_err(context))
        }
    }
}

#[cfg(test)]
mod tests {
    use eyre::eyre;

    use super::preserve_primary_with_cleanup;

    #[test]
    fn primary_run_and_cleanup_failures_are_both_retained() {
        let error = preserve_primary_with_cleanup::<()>(
            Err(eyre!("primary run failed")),
            Err(std::io::Error::other("cleanup failed")),
        )
        .expect_err("dual failure unexpectedly succeeded");
        assert!(error.to_string().contains("primary run failed"));
        assert!(error.to_string().contains("cleanup failed"));
        assert_eq!(error.root_cause().to_string(), "primary run failed");
    }
}
