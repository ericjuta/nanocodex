use std::{process::Command, time::Instant};

pub(crate) struct ShellOutput {
    pub(crate) status: &'static str,
    pub(crate) return_code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) duration_ns: u64,
    pub(crate) succeeded: bool,
}

pub(crate) fn execute(command: &str, cwd: &str) -> ShellOutput {
    let started_at = Instant::now();
    let output = Command::new("/bin/sh")
        .args(["-lc", command])
        .current_dir(cwd)
        .output();
    let duration_ns = u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX);

    match output {
        Ok(output) => ShellOutput {
            status: if output.status.success() {
                "completed"
            } else {
                "failed"
            },
            return_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            duration_ns,
            succeeded: output.status.success(),
        },
        Err(error) => ShellOutput {
            status: "error",
            return_code: None,
            stdout: String::new(),
            stderr: error.to_string(),
            duration_ns,
            succeeded: false,
        },
    }
}
