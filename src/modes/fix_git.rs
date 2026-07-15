use std::{io::Write, time::Instant};

use serde_json::json;

use super::elapsed_ms;
use crate::{
    protocol::{EventWriter, Task},
    shell,
};

const CALL_ID: &str = "fix-git-positive-control";
const CWD: &str = "/app/personal-site";
const COMMAND: &str = "cp -- /app/resources/patch_files/about.md /app/personal-site/_includes/about.md && cp -- /app/resources/patch_files/default.html /app/personal-site/_layouts/default.html";

pub(super) fn run<W: Write>(events: &mut EventWriter<W>, task: &Task) -> Result<(), String> {
    let started_at = Instant::now();
    events.emit(
        "run.started",
        json!({
            "mode": "fix_git_cheat",
            "workspace": task.workspace.as_deref(),
            "instruction_bytes": task.instruction.len(),
        }),
    )?;
    events.emit(
        "tool.call",
        json!({
            "call_id": CALL_ID,
            "tool": "shell",
            "arguments": {"command": COMMAND, "cwd": CWD, "timeout_sec": 30},
        }),
    )?;

    let result = shell::execute(COMMAND, CWD);
    let succeeded = result.succeeded;
    events.emit(
        "tool.result",
        json!({
            "call_id": CALL_ID,
            "tool": "shell",
            "status": result.status,
            "return_code": result.return_code,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "duration_ns": result.duration_ns,
        }),
    )?;

    let (message, terminal_kind, status) = if succeeded {
        (
            "Hard-coded positive control copied both verifier fixtures; no model was called.",
            "run.completed",
            "completed",
        )
    } else {
        (
            "The hard-coded positive-control tool call failed.",
            "run.failed",
            "tool_failed",
        )
    };
    events.emit("assistant.message", json!({"text": message}))?;
    events.emit(
        terminal_kind,
        json!({
            "status": status,
            "model_calls": 0,
            "tool_calls": 1,
            "duration_ms": elapsed_ms(started_at),
        }),
    )
}
