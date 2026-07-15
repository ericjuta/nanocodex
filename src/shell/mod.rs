mod output;
mod process;

use std::path::Path;

use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct ShellExecution {
    pub(crate) max_output_length: u64,
    pub(crate) output: Vec<ShellCommandOutput>,
}

#[derive(Serialize)]
pub(crate) struct ShellCommandOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) outcome: ShellOutcome,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ShellOutcome {
    Exit { exit_code: i32 },
    Timeout,
}

pub(crate) async fn execute_action(
    commands: Vec<String>,
    timeout_ms: Option<i64>,
    max_output_length: Option<i64>,
    workspace: &str,
) -> ShellExecution {
    let command_timeout = process::effective_timeout(timeout_ms);
    let output_limit = output::effective_limit(max_output_length);
    let (environment, secrets) = process::sanitized_environment();
    let mut remaining_output = output_limit;
    let mut output = Vec::with_capacity(commands.len());

    for script in commands {
        let command_output = process::execute(
            &script,
            Path::new(workspace),
            command_timeout,
            remaining_output,
            &environment,
            &secrets,
        )
        .await;
        let timed_out = matches!(&command_output.outcome, ShellOutcome::Timeout);
        remaining_output = remaining_output.saturating_sub(command_output.character_count());
        output.push(command_output);
        if timed_out {
            break;
        }
    }

    ShellExecution {
        max_output_length: u64::try_from(output_limit).unwrap_or(u64::MAX),
        output,
    }
}

impl ShellCommandOutput {
    fn exit(stdout: String, stderr: String, exit_code: i32) -> Self {
        Self {
            stdout,
            stderr,
            outcome: ShellOutcome::Exit { exit_code },
        }
    }

    fn character_count(&self) -> usize {
        self.stdout
            .chars()
            .count()
            .saturating_add(self.stderr.chars().count())
    }
}
