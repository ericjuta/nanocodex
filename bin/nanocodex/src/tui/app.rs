use std::{collections::VecDeque, path::PathBuf};

use nanocodex::{AgentEvent, AgentEventKind};
use serde::Deserialize;
use serde_json::Value;

const MAX_REASONING_STATUS_CHARS: usize = 160;
const MAX_TOOL_ARGUMENT_CHARS: usize = 180;

pub(super) enum TranscriptItem {
    User(String),
    Assistant(String),
    Tool {
        call_id: String,
        name: String,
        arguments: String,
        status: ToolStatus,
    },
    Error(String),
}

#[derive(Clone, Copy)]
pub(super) enum ToolStatus {
    Running,
    Completed,
    Failed,
}

pub(super) struct App {
    pub(super) cwd: PathBuf,
    pub(super) transcript: Vec<TranscriptItem>,
    pub(super) input: String,
    pub(super) cursor: usize,
    pub(super) pending_turns: usize,
    pub(super) running: bool,
    pub(super) status: String,
    pub(super) scroll_from_bottom: usize,
    pub(super) frame: usize,
    streamed_this_turn: bool,
    reasoning: String,
    history: Vec<String>,
    history_cursor: Option<usize>,
    history_draft: String,
    queued_prompts: VecDeque<String>,
}

impl App {
    pub(super) fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            transcript: Vec::new(),
            input: String::new(),
            cursor: 0,
            pending_turns: 0,
            running: false,
            status: "Ready".to_owned(),
            scroll_from_bottom: 0,
            frame: 0,
            streamed_this_turn: false,
            reasoning: String::new(),
            history: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
            queued_prompts: VecDeque::new(),
        }
    }

    pub(super) fn insert_char(&mut self, character: char) {
        self.detach_history();
        self.input.insert(self.cursor, character);
        self.cursor += character.len_utf8();
    }

    pub(super) fn insert_str(&mut self, text: &str) {
        self.detach_history();
        self.input.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub(super) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.detach_history();
        let previous = self.input[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
        self.input.drain(previous..self.cursor);
        self.cursor = previous;
    }

    pub(super) fn delete(&mut self) {
        if self.cursor == self.input.len() {
            return;
        }
        self.detach_history();
        let next = self.input[self.cursor..]
            .chars()
            .next()
            .map_or(self.input.len(), |character| {
                self.cursor + character.len_utf8()
            });
        self.input.drain(self.cursor..next);
    }

    pub(super) fn move_left(&mut self) {
        self.cursor = self.input[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
    }

    pub(super) fn move_right(&mut self) {
        if let Some(character) = self.input[self.cursor..].chars().next() {
            self.cursor += character.len_utf8();
        }
    }

    pub(super) fn move_home(&mut self) {
        self.cursor = self.input[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
    }

    pub(super) fn move_end(&mut self) {
        self.cursor = self.input[self.cursor..]
            .find('\n')
            .map_or(self.input.len(), |index| self.cursor + index);
    }

    pub(super) fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.history_cursor = None;
        self.history_draft.clear();
    }

    pub(super) fn previous_history(&mut self) {
        if self.history.is_empty() || self.input.contains('\n') {
            return;
        }
        let index = if let Some(index) = self.history_cursor {
            index.saturating_sub(1)
        } else {
            self.history_draft.clone_from(&self.input);
            self.history.len() - 1
        };
        self.history_cursor = Some(index);
        self.input.clone_from(&self.history[index]);
        self.cursor = self.input.len();
    }

    pub(super) fn next_history(&mut self) {
        let Some(index) = self.history_cursor else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_cursor = Some(index + 1);
            self.input.clone_from(&self.history[index + 1]);
        } else {
            self.history_cursor = None;
            self.input.clone_from(&self.history_draft);
            self.history_draft.clear();
        }
        self.cursor = self.input.len();
    }

    pub(super) fn take_submission(&mut self) -> Option<String> {
        if self.input.chars().all(char::is_whitespace) {
            return None;
        }
        self.cursor = 0;
        let prompt = std::mem::take(&mut self.input);
        self.history.push(prompt.clone());
        self.history_cursor = None;
        self.history_draft.clear();
        self.queued_prompts.push_back(prompt.clone());
        self.pending_turns += 1;
        self.status = if self.running {
            "Prompt queued".to_owned()
        } else {
            "Starting".to_owned()
        };
        self.scroll_from_bottom = 0;
        Some(prompt)
    }

    pub(super) fn turn_finished(&mut self, error: Option<String>) {
        self.pending_turns = self.pending_turns.saturating_sub(1);
        if let Some(error) = error {
            self.transcript.push(TranscriptItem::Error(error));
        }
    }

    pub(super) fn scroll_up(&mut self, rows: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(rows);
    }

    pub(super) fn scroll_down(&mut self, rows: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(rows);
    }

    pub(super) fn on_tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    pub(super) fn on_agent_event(&mut self, event: &AgentEvent) -> bool {
        match event.kind {
            AgentEventKind::RunStarted => {
                if let Some(prompt) = self.queued_prompts.pop_front() {
                    self.transcript.push(TranscriptItem::User(prompt));
                }
                self.running = true;
                self.streamed_this_turn = false;
                self.reasoning.clear();
                "Thinking".clone_into(&mut self.status);
            }
            AgentEventKind::AssistantDelta => {
                if let Ok(payload) = event.decode_payload::<TextPayload>() {
                    self.push_assistant_delta(&payload.text);
                }
            }
            AgentEventKind::AssistantMessage => {
                if let Ok(payload) = event.decode_payload::<TextPayload>()
                    && !self.streamed_this_turn
                {
                    self.transcript
                        .push(TranscriptItem::Assistant(payload.text));
                }
            }
            AgentEventKind::ReasoningSummaryDelta => {
                if let Ok(payload) = event.decode_payload::<TextPayload>() {
                    self.reasoning.push_str(&payload.text);
                    self.status = reasoning_tail(&self.reasoning);
                }
            }
            AgentEventKind::ToolCall => {
                if let Ok(payload) = event.decode_payload::<ToolCallPayload>() {
                    let arguments = compact_arguments(&payload.arguments);
                    self.status = format!("Running {}", payload.tool);
                    self.transcript.push(TranscriptItem::Tool {
                        call_id: payload.call_id,
                        name: payload.tool,
                        arguments,
                        status: ToolStatus::Running,
                    });
                }
            }
            AgentEventKind::ToolResult => {
                if let Ok(payload) = event.decode_payload::<ToolResultPayload>() {
                    let status = if payload.status == "completed" {
                        ToolStatus::Completed
                    } else {
                        ToolStatus::Failed
                    };
                    if let Some(TranscriptItem::Tool {
                        status: current, ..
                    }) = self.transcript.iter_mut().rev().find(|item| {
                        matches!(item, TranscriptItem::Tool { call_id, .. } if call_id == &payload.call_id)
                    }) {
                        *current = status;
                    }
                    "Working".clone_into(&mut self.status);
                }
            }
            AgentEventKind::RunError => {
                if let Ok(payload) = event.decode_payload::<ErrorPayload>() {
                    self.transcript.push(TranscriptItem::Error(payload.message));
                }
            }
            AgentEventKind::RunCompleted => {
                self.running = false;
                "Ready".clone_into(&mut self.status);
            }
            AgentEventKind::RunFailed => {
                self.running = false;
                "Turn failed".clone_into(&mut self.status);
            }
            AgentEventKind::ApiEvent
            | AgentEventKind::ModelWarmupStarted
            | AgentEventKind::ModelWarmupCompleted
            | AgentEventKind::ModelWarmupFailed
            | AgentEventKind::ModelCallStarted
            | AgentEventKind::ModelCallCompleted
            | AgentEventKind::ModelCallFailed
            | AgentEventKind::ModelCompactionStarted
            | AgentEventKind::ModelCompactionCompleted
            | AgentEventKind::ModelCompactionFailed
            | AgentEventKind::ModelAttemptStarted
            | AgentEventKind::ModelAttemptFailed
            | AgentEventKind::ModelAttemptRetrying
            | AgentEventKind::ModelConnectionStarted
            | AgentEventKind::ModelConnectionCompleted
            | AgentEventKind::ModelConnectionFailed => return false,
        }
        true
    }

    fn push_assistant_delta(&mut self, delta: &str) {
        let append_to_current = self.streamed_this_turn;
        self.streamed_this_turn = true;
        if append_to_current
            && let Some(TranscriptItem::Assistant(message)) = self.transcript.last_mut()
        {
            message.push_str(delta);
        } else {
            self.transcript
                .push(TranscriptItem::Assistant(delta.to_owned()));
        }
    }

    fn detach_history(&mut self) {
        self.history_cursor = None;
        self.history_draft.clear();
    }
}

#[derive(Deserialize)]
struct TextPayload {
    text: String,
}

#[derive(Deserialize)]
struct ErrorPayload {
    message: String,
}

#[derive(Deserialize)]
struct ToolCallPayload {
    call_id: String,
    tool: String,
    arguments: Value,
}

#[derive(Deserialize)]
struct ToolResultPayload {
    call_id: String,
    status: String,
}

fn compact_arguments(arguments: &Value) -> String {
    let value = match arguments {
        Value::String(value) => value.clone(),
        _ => arguments.to_string(),
    };
    if value.chars().count() <= MAX_TOOL_ARGUMENT_CHARS {
        return value;
    }
    let mut output: String = value.chars().take(MAX_TOOL_ARGUMENT_CHARS).collect();
    output.push('…');
    output
}

fn reasoning_tail(reasoning: &str) -> String {
    let compact = reasoning.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = compact.chars().count();
    if count <= MAX_REASONING_STATUS_CHARS {
        return compact;
    }
    let mut tail: String = compact
        .chars()
        .skip(count - MAX_REASONING_STATUS_CHARS)
        .collect();
    tail.insert(0, '…');
    tail
}
