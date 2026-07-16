use std::{
    env,
    ffi::{OsStr, OsString},
    io,
    os::unix::process::ExitStatusExt,
    path::Path,
    process::Stdio,
    time::Duration,
};

use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use tokio::{
    process::{Child, Command},
    task::JoinHandle,
    time::timeout,
};

use super::{ShellCommandOutput, ShellOutcome, output};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 60 * 60 * 1_000;
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);
const SENSITIVE_ENV_PARTS: [&str; 11] = [
    "AUTH",
    "AUTHORIZATION",
    "COOKIE",
    "CREDENTIAL",
    "CREDENTIALS",
    "KEY",
    "PASS",
    "PASSWD",
    "PASSWORD",
    "SECRET",
    "TOKEN",
];

pub(super) fn effective_timeout(requested_ms: Option<i64>) -> Duration {
    let milliseconds = requested_ms.map_or(DEFAULT_TIMEOUT_MS, |value| {
        u64::try_from(value).unwrap_or(1)
    });
    Duration::from_millis(milliseconds.clamp(1, MAX_TIMEOUT_MS))
}

pub(super) async fn execute(
    script: &str,
    workspace: &Path,
    login: bool,
    command_timeout: Duration,
    output_limit: usize,
    environment: &[(OsString, OsString)],
    secrets: &[String],
) -> ShellCommandOutput {
    let mut command = Command::new("/bin/sh");
    command
        .args([if login { "-lc" } else { "-c" }, script])
        .current_dir(workspace)
        .env_clear()
        .envs(environment.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ShellCommandOutput::exit(
                String::new(),
                output::redact_and_limit(
                    format!("failed to spawn /bin/sh: {error}"),
                    secrets,
                    output_limit,
                ),
                1,
            );
        }
    };
    let Some(pid) = child.id() else {
        return ShellCommandOutput::exit(
            String::new(),
            "spawned /bin/sh without a process identifier".to_owned(),
            1,
        );
    };
    let mut process_group = ProcessGroupGuard::new(pid);
    let capture_limit = output_limit.saturating_mul(4);
    let mut drain = tokio::spawn(output::drain_pipes(
        child.stdout.take(),
        child.stderr.take(),
        capture_limit,
    ));

    let (outcome, wait_error) = wait_for_child(&mut child, command_timeout, &process_group).await;
    let captured = finish_drain(&mut drain).await;
    process_group.disarm();
    let (stdout, stderr) = output::render(captured, wait_error, secrets, output_limit);

    ShellCommandOutput {
        stdout,
        stderr,
        outcome,
    }
}

async fn wait_for_child(
    child: &mut Child,
    command_timeout: Duration,
    process_group: &ProcessGroupGuard,
) -> (ShellOutcome, Option<String>) {
    match timeout(command_timeout, child.wait()).await {
        Ok(Ok(status)) => {
            let exit_code = status
                .code()
                .or_else(|| status.signal().map(|signal| 128_i32.saturating_add(signal)))
                .unwrap_or(1);
            (ShellOutcome::Exit { exit_code }, None)
        }
        Ok(Err(error)) => (
            ShellOutcome::Exit { exit_code: 1 },
            Some(format!("failed to wait for /bin/sh: {error}")),
        ),
        Err(_) => {
            let mut cleanup_errors = Vec::new();
            if let Err(error) = process_group.terminate() {
                cleanup_errors.push(format!(
                    "failed to terminate command process group: {error}"
                ));
                if let Err(error) = child.kill().await {
                    cleanup_errors.push(format!("failed to terminate /bin/sh: {error}"));
                }
            }
            if let Err(error) = child.wait().await {
                cleanup_errors.push(format!("failed to reap /bin/sh: {error}"));
            }
            (
                ShellOutcome::Timeout,
                (!cleanup_errors.is_empty()).then(|| cleanup_errors.join("; ")),
            )
        }
    }
}

async fn finish_drain(drain: &mut JoinHandle<output::CapturedOutput>) -> output::CapturedOutput {
    if let Ok(result) = timeout(PIPE_DRAIN_GRACE, &mut *drain).await {
        return joined_output(result);
    }

    drain.abort();
    output::CapturedOutput::empty()
}

fn joined_output(
    result: Result<output::CapturedOutput, tokio::task::JoinError>,
) -> output::CapturedOutput {
    result.unwrap_or_else(|error| {
        output::CapturedOutput::error(format!("command output drain task failed: {error}"))
    })
}

struct ProcessGroupGuard {
    process_group: Option<Pid>,
}

impl ProcessGroupGuard {
    fn new(pid: u32) -> Self {
        Self {
            process_group: i32::try_from(pid).ok().map(Pid::from_raw),
        }
    }

    fn terminate(&self) -> io::Result<()> {
        let Some(process_group) = self.process_group else {
            return Err(io::Error::other("process identifier exceeds i32::MAX"));
        };
        match killpg(process_group, Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
        }
    }

    fn disarm(&mut self) {
        self.process_group = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

pub(super) fn sanitized_environment() -> (Vec<(OsString, OsString)>, Vec<String>) {
    let mut environment = Vec::new();
    let mut secrets = Vec::new();
    for (name, value) in env::vars_os() {
        if is_sensitive_name(&name) {
            if let Some(value) = value.to_str().filter(|value| value.len() >= 8) {
                secrets.push(value.to_owned());
            }
        } else {
            environment.push((name, value));
        }
    }
    secrets.sort_unstable_by_key(|secret| std::cmp::Reverse(secret.len()));
    secrets.dedup();
    (environment, secrets)
}

fn is_sensitive_name(name: &OsStr) -> bool {
    name.to_string_lossy()
        .to_ascii_uppercase()
        .split('_')
        .any(|part| SENSITIVE_ENV_PARTS.contains(&part))
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        path::Path,
        time::{Duration, SystemTime},
    };

    use super::{ShellOutcome, execute};

    #[tokio::test]
    async fn successful_command_leaves_background_process_running()
    -> Result<(), Box<dyn std::error::Error>> {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "harness-background-process-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&directory)?;
        let marker = directory.join("survived");
        let mut environment: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        environment.push((
            OsString::from("HARNESS_BACKGROUND_MARKER"),
            marker.as_os_str().to_owned(),
        ));

        let result = execute(
            "(sleep 3; printf survived > \"$HARNESS_BACKGROUND_MARKER\") &",
            Path::new("/"),
            false,
            Duration::from_secs(10),
            4_096,
            &environment,
            &[],
        )
        .await;

        for _ in 0..20 {
            if marker.is_file() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let survived = marker.is_file();
        fs::remove_dir_all(directory)?;

        assert!(matches!(
            result.outcome,
            ShellOutcome::Exit { exit_code: 0 }
        ));
        assert!(survived, "background process was killed after shell exit");
        Ok(())
    }
}
