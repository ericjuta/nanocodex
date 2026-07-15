mod modes;
mod protocol;
mod shell;

use std::io::{BufRead, Write};

pub use modes::Mode;
use protocol::{EventWriter, read_task_start};

/// Run one harness request from JSONL input to JSONL output.
///
/// # Errors
///
/// Returns an error when the input envelope is invalid, a mode fails, or an
/// output event cannot be written.
pub fn run(input: impl BufRead, output: impl Write, mode: Mode) -> Result<(), String> {
    let request = read_task_start(input)?;
    let mut events = EventWriter::new(output, request.request_id);
    modes::run(mode, &mut events, &request.task)
}
