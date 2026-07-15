use std::io;

use clap::{Parser, Subcommand};
use eyre::Result;
use harness::Mode;

#[derive(Parser)]
#[command(version, about = "A Harbor-first OpenAI coding harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read one task request from stdin and stream JSONL events to stdout.
    Run {
        #[arg(long, value_enum, default_value_t)]
        mode: Mode,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Run { mode } => harness::run(io::stdin().lock(), io::stdout().lock(), mode),
    }
    .map_err(eyre::Report::msg)
}
