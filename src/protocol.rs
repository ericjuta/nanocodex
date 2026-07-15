use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const VERSION: u32 = 1;

#[derive(Deserialize)]
struct InputEnvelope {
    protocol_version: u32,
    request_id: String,
    seq: u64,
    #[serde(rename = "type")]
    kind: String,
    payload: Task,
}

#[derive(Serialize)]
struct OutputEnvelope<'a> {
    protocol_version: u32,
    request_id: &'a str,
    seq: u64,
    #[serde(rename = "type")]
    kind: &'a str,
    payload: Value,
}

pub(crate) struct TaskRequest {
    pub(crate) request_id: String,
    pub(crate) task: Task,
}

#[derive(Deserialize)]
pub(crate) struct Task {
    pub(crate) instruction: String,
    #[serde(default)]
    pub(crate) workspace: Option<String>,
}

pub(crate) struct EventWriter<W> {
    output: W,
    request_id: String,
    next_seq: u64,
}

impl<W: Write> EventWriter<W> {
    pub(crate) fn new(output: W, request_id: String) -> Self {
        Self {
            output,
            request_id,
            next_seq: 1,
        }
    }

    pub(crate) fn emit(&mut self, kind: &str, payload: Value) -> Result<(), String> {
        let envelope = OutputEnvelope {
            protocol_version: VERSION,
            request_id: &self.request_id,
            seq: self.next_seq,
            kind,
            payload,
        };
        serde_json::to_writer(&mut self.output, &envelope)
            .map_err(|error| format!("failed to encode stdout event: {error}"))?;
        self.output
            .write_all(b"\n")
            .and_then(|()| self.output.flush())
            .map_err(|error| format!("failed to flush stdout event: {error}"))?;
        self.next_seq += 1;
        Ok(())
    }
}

pub(crate) fn read_task_start(mut input: impl BufRead) -> Result<TaskRequest, String> {
    let mut line = String::new();
    loop {
        line.clear();
        let bytes_read = input
            .read_line(&mut line)
            .map_err(|error| format!("failed to read stdin: {error}"))?;
        if bytes_read == 0 {
            return Err("stdin ended before task.start".to_owned());
        }
        if !line.trim().is_empty() {
            break;
        }
    }

    let request: InputEnvelope =
        serde_json::from_str(&line).map_err(|error| format!("invalid input JSONL: {error}"))?;
    if request.protocol_version != VERSION {
        return Err(format!(
            "unsupported protocol_version {}; expected {VERSION}",
            request.protocol_version
        ));
    }
    if request.request_id.trim().is_empty() {
        return Err("request_id must not be empty".to_owned());
    }
    if request.seq != 1 || request.kind != "task.start" {
        return Err("first input event must be task.start with seq 1".to_owned());
    }
    if request.payload.instruction.trim().is_empty() {
        return Err("task.start instruction must not be empty".to_owned());
    }
    Ok(TaskRequest {
        request_id: request.request_id,
        task: request.payload,
    })
}
