mod config;
mod mcp;
mod mpp;
mod observability;
mod run;
mod subagents;
mod tui;
mod update;

use clap::{Args, Parser, Subcommand, builder::NonEmptyStringValueParser};
use eyre::Result;

use config::AgentArgs;
use observability::ObservabilityArgs;

#[derive(Parser)]
#[command(
    version,
    about = "An interactive coding agent and headless JSONL runner",
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    agent: AgentArgs,

    #[command(flatten)]
    observability: ObservabilityArgs,

    /// Submit an initial prompt immediately after the TUI opens.
    #[arg(long, value_parser = NonEmptyStringValueParser::new())]
    prompt: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Run one prompt and stream JSONL events to stdout.
    Run(Box<RunCommand>),
    /// Update this executable to the latest GitHub release.
    Update(update::Update),
}

#[derive(Args)]
struct RunCommand {
    #[command(flatten)]
    run: run::Run,

    #[command(flatten)]
    agent: AgentArgs,

    #[command(flatten)]
    observability: ObservabilityArgs,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Keep direct `cargo run` behavior consistent with the Justfile without
    // requiring shell-specific syntax to load the repository's `.env` file.
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();
    match cli.command {
        Some(Command::Run(command)) => {
            let _observability = command.observability.install(false, command.agent.cwd())?;
            command.run.run(command.agent).await
        }
        Some(Command::Update(command)) => command.run().await,
        None => {
            let _observability = cli.observability.install(true, cli.agent.cwd())?;
            tui::run(cli.agent, cli.prompt).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIVATE_KEY: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";

    #[test]
    fn mpp_flags_select_the_tui_transport() {
        let cli = Cli::try_parse_from([
            "nanocodex",
            "--api-key",
            "test-key",
            "--mpp",
            "--tempo-private-key",
            PRIVATE_KEY,
        ])
        .unwrap();

        assert!(cli.command.is_none());
        assert!(cli.agent.uses_mpp());
    }

    #[test]
    fn mpp_flags_select_the_one_shot_transport() {
        let cli = Cli::try_parse_from([
            "nanocodex",
            "--api-key",
            "test-key",
            "run",
            "reply with ok",
            "--mpp",
            "--tempo-private-key",
            PRIVATE_KEY,
        ])
        .unwrap();

        assert!(matches!(cli.command, Some(Command::Run(_))));
        assert!(cli.agent.uses_mpp());
    }
}
