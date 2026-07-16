use std::io;

use tokio::io::{AsyncRead, AsyncReadExt};

const DEFAULT_MAX_OUTPUT_LENGTH: usize = 4_096;
const MAX_OUTPUT_LENGTH: usize = 1024 * 1024;
const CHARACTERS_PER_TOKEN: usize = 4;
const READ_BUFFER_LENGTH: usize = 8 * 1024;
const REDACTION: &str = "[REDACTED]";

pub(super) struct CapturedOutput {
    stdout: BoundedBytes,
    stderr: BoundedBytes,
    error: Option<String>,
}

impl CapturedOutput {
    fn new(limit: usize) -> Self {
        Self {
            stdout: BoundedBytes::new(limit),
            stderr: BoundedBytes::new(limit),
            error: None,
        }
    }

    pub(super) fn empty() -> Self {
        Self::new(0)
    }

    pub(super) fn error(error: String) -> Self {
        let mut captured = Self::new(0);
        captured.error = Some(error);
        captured
    }
}

pub(super) fn effective_token_limit(requested: Option<i64>) -> usize {
    requested.map_or(DEFAULT_MAX_OUTPUT_LENGTH, |value| {
        usize::try_from(value)
            .unwrap_or(0)
            .saturating_mul(CHARACTERS_PER_TOKEN)
            .min(MAX_OUTPUT_LENGTH)
    })
}

pub(super) async fn drain_pipes(
    mut stdout: Option<impl AsyncRead + Unpin>,
    mut stderr: Option<impl AsyncRead + Unpin>,
    capture_limit: usize,
) -> CapturedOutput {
    let mut captured = CapturedOutput::new(capture_limit);
    let mut stdout_buffer = [0_u8; READ_BUFFER_LENGTH];
    let mut stderr_buffer = [0_u8; READ_BUFFER_LENGTH];

    while stdout.is_some() || stderr.is_some() {
        tokio::select! {
            read = read_pipe(&mut stdout, &mut stdout_buffer), if stdout.is_some() => {
                capture_read(
                    read,
                    &mut stdout,
                    &stdout_buffer,
                    &mut captured.stdout,
                    &mut captured.error,
                );
            }
            read = read_pipe(&mut stderr, &mut stderr_buffer), if stderr.is_some() => {
                capture_read(
                    read,
                    &mut stderr,
                    &stderr_buffer,
                    &mut captured.stderr,
                    &mut captured.error,
                );
            }
        }
    }

    captured
}

pub(super) fn render(
    captured: CapturedOutput,
    wait_error: Option<String>,
    secrets: &[String],
    limit: usize,
) -> (String, String) {
    let mut stderr = String::from_utf8_lossy(&captured.stderr.bytes).into_owned();
    if let Some(error) = captured.error {
        append_diagnostic(&mut stderr, &error);
    }
    if let Some(error) = wait_error {
        append_diagnostic(&mut stderr, &error);
    }

    let stdout = redact_and_limit(
        String::from_utf8_lossy(&captured.stdout.bytes).into_owned(),
        secrets,
        limit,
    );
    let stderr_limit = limit.saturating_sub(stdout.chars().count());
    let stderr = redact_and_limit(stderr, secrets, stderr_limit);
    (stdout, stderr)
}

pub(super) fn redact_and_limit(mut output: String, secrets: &[String], limit: usize) -> String {
    for secret in secrets {
        output = output.replace(secret, REDACTION);
        truncate_characters(&mut output, limit.saturating_mul(4));
    }
    truncate_characters(&mut output, limit);
    output
}

async fn read_pipe<R: AsyncRead + Unpin>(
    pipe: &mut Option<R>,
    buffer: &mut [u8],
) -> io::Result<usize> {
    match pipe {
        Some(pipe) => pipe.read(buffer).await,
        None => Ok(0),
    }
}

fn capture_read<R>(
    read: io::Result<usize>,
    pipe: &mut Option<R>,
    read_buffer: &[u8],
    captured: &mut BoundedBytes,
    error: &mut Option<String>,
) {
    match read {
        Ok(0) => *pipe = None,
        Ok(length) => captured.push(&read_buffer[..length]),
        Err(read_error) => {
            *pipe = None;
            append_optional_diagnostic(
                error,
                &format!("failed to read command output: {read_error}"),
            );
        }
    }
}

struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedBytes {
    const fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        self.bytes
            .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
    }
}

fn truncate_characters(output: &mut String, limit: usize) {
    if let Some((byte_index, _)) = output.char_indices().nth(limit) {
        output.truncate(byte_index);
    }
}

fn append_diagnostic(output: &mut String, diagnostic: &str) {
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(diagnostic);
}

fn append_optional_diagnostic(output: &mut Option<String>, diagnostic: &str) {
    append_diagnostic(output.get_or_insert_default(), diagnostic);
}
