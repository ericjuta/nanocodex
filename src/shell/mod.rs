mod output;
mod process;

use std::{path::PathBuf, time::Duration};

use serde::Serialize;

pub(crate) struct ExecCommand {
    script: String,
    workdir: Option<String>,
    login: Option<bool>,
    timeout_ms: Option<i64>,
    max_output_tokens: Option<i64>,
}

impl ExecCommand {
    pub(crate) const fn new(
        script: String,
        workdir: Option<String>,
        login: Option<bool>,
        timeout_ms: Option<i64>,
        max_output_tokens: Option<i64>,
    ) -> Self {
        Self {
            script,
            workdir,
            login,
            timeout_ms,
            max_output_tokens,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct ExecCommandResult {
    wall_time_seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    output: String,
}

struct ShellCommandOutput {
    stdout: String,
    stderr: String,
    outcome: ShellOutcome,
}

enum ShellOutcome {
    Exit { exit_code: i32 },
    Timeout,
}

pub(crate) async fn execute(command: ExecCommand, workspace: &str) -> ExecCommandResult {
    let started_at = std::time::Instant::now();
    let command_timeout = process::effective_timeout(command.timeout_ms);
    let output_limit = output::effective_token_limit(command.max_output_tokens);
    let (environment, secrets) = process::sanitized_environment();
    let requested_workdir = command
        .workdir
        .filter(|workdir| !workdir.is_empty())
        .map_or_else(|| PathBuf::from(workspace), PathBuf::from);
    let workdir = if requested_workdir.is_absolute() {
        requested_workdir
    } else {
        PathBuf::from(workspace).join(requested_workdir)
    };
    let output = process::execute(
        &command.script,
        &workdir,
        command.login.unwrap_or(true),
        command_timeout,
        output_limit,
        &environment,
        &secrets,
    )
    .await;
    output.into_result(started_at.elapsed())
}

impl ShellCommandOutput {
    fn exit(stdout: String, stderr: String, exit_code: i32) -> Self {
        Self {
            stdout,
            stderr,
            outcome: ShellOutcome::Exit { exit_code },
        }
    }

    fn into_result(self, wall_time: Duration) -> ExecCommandResult {
        let mut output = self.stdout;
        if !self.stderr.is_empty() {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&self.stderr);
        }
        let exit_code = match self.outcome {
            ShellOutcome::Exit { exit_code } => Some(exit_code),
            ShellOutcome::Timeout => {
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str("Command timed out.");
                None
            }
        };
        ExecCommandResult {
            wall_time_seconds: wall_time.as_secs_f64(),
            exit_code,
            output,
        }
    }
}
