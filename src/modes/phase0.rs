use std::{io::Write, time::Instant};

use serde_json::json;

use super::elapsed_ms;
use crate::protocol::{EventWriter, Task};

pub(super) fn run<W: Write>(events: &mut EventWriter<W>, task: &Task) -> Result<(), String> {
    let started_at = Instant::now();
    events.emit(
        "run.started",
        json!({
            "mode": "phase0_no_model",
            "workspace": task.workspace.as_deref(),
            "instruction_bytes": task.instruction.len(),
        }),
    )?;
    events.emit(
        "assistant.message",
        json!({"text": "Phase 0 transport probe completed; no model or tools were run."}),
    )?;
    events.emit(
        "run.completed",
        json!({
            "status": "not_attempted",
            "model_calls": 0,
            "tool_calls": 0,
            "duration_ms": elapsed_ms(started_at),
        }),
    )
}
