mod app;
mod terminal;
mod view;

use std::time::Duration;

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use eyre::{Result, WrapErr};
use futures_util::StreamExt;
use nanocodex::{AgentError, AgentEvent, AgentEvents, Nanocodex, NanocodexError};
use tokio::{
    sync::mpsc,
    time::{MissedTickBehavior, interval},
};

use self::{
    app::{App, PaneId, TranscriptItem},
    terminal::TerminalSession,
};
use crate::config::AgentArgs;

const BTW_BOUNDARY: &str = r"You are answering an ephemeral BTW side question.
Treat inherited conversation history only as reference context. Do not resume or complete an
earlier task. Answer only the question after this boundary. Do not modify the workspace unless
that side question explicitly requests a mutation.

BTW question:
";

enum WorkerCommand {
    Prompt { target: PaneId, prompt: String },
    Steer { target: PaneId, prompt: String },
    OpenBtw { id: u64, prompt: Option<String> },
    CloseBtw { id: u64 },
}

enum WorkerEvent {
    TurnFinished {
        target: PaneId,
        error: Option<String>,
    },
    SteerAccepted {
        target: PaneId,
        prompt: String,
    },
    SteerQueued {
        target: PaneId,
        prompt: String,
    },
    SteerFailed {
        target: PaneId,
        error: String,
    },
    BtwOpened {
        id: u64,
    },
    BtwOpenFailed {
        id: u64,
        error: String,
    },
    BtwAgentEvent {
        id: u64,
        event: AgentEvent,
    },
    BtwEventStreamClosed {
        id: u64,
    },
}

struct BtwWorker {
    id: u64,
    agent: Nanocodex,
    first_prompt: bool,
}

impl BtwWorker {
    fn prepare_prompt(&mut self, prompt: String) -> String {
        prepare_btw_prompt(&mut self.first_prompt, prompt)
    }
}

fn prepare_btw_prompt(first_prompt: &mut bool, prompt: String) -> String {
    if *first_prompt {
        *first_prompt = false;
        format!("{BTW_BOUNDARY}{prompt}")
    } else {
        prompt
    }
}

enum TerminalAction {
    Redraw,
    Ignore,
    Quit,
}

#[derive(Clone, Copy)]
enum SubmitIntent {
    Immediate,
    Queue,
}

#[derive(Debug, Eq, PartialEq)]
enum Submission {
    Prompt(String),
    Btw(Option<String>),
    CloseBtw,
}

pub(crate) async fn run(config: AgentArgs, initial_prompt: Option<String>) -> Result<()> {
    let cwd = config
        .cwd()
        .canonicalize()
        .wrap_err("failed to resolve the working directory")?;
    let configured = config.build()?;
    let agent = configured.handle;
    let mut agent_events = configured.events;
    let _child_agents = configured.child_agents;
    let (worker_tx, worker_rx) = mpsc::unbounded_channel();
    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    spawn_agent_worker(agent, worker_rx, update_tx);

    let mut terminal = TerminalSession::enter().wrap_err("failed to initialize the terminal")?;
    let mut input_events = EventStream::new();
    let mut ticker = interval(Duration::from_millis(80));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut app = App::new(cwd);
    let mut agent_events_open = true;
    let mut needs_draw = true;

    if let Some(prompt) = initial_prompt {
        app.input = prompt;
        app.cursor = app.input.len();
        submit(&mut app, &worker_tx, SubmitIntent::Immediate)?;
    }

    loop {
        if needs_draw {
            terminal
                .terminal()
                .draw(|frame| view::render(frame, &app))?;
            needs_draw = false;
        }
        tokio::select! {
            event = input_events.next() => {
                let event = event.transpose()?.ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "terminal input closed")
                })?;
                match handle_terminal_event(event, &mut app, &worker_tx)? {
                    TerminalAction::Redraw => needs_draw = true,
                    TerminalAction::Ignore => {}
                    TerminalAction::Quit => return Ok(()),
                }
            }
            event = agent_events.recv(), if agent_events_open => {
                let Some(event) = event else {
                    app.main.transcript.push(TranscriptItem::Error(
                        "agent event stream closed".to_owned(),
                    ));
                    app.main.running = false;
                    "Agent stopped".clone_into(&mut app.main.status);
                    agent_events_open = false;
                    needs_draw = true;
                    continue;
                };
                needs_draw |= app.on_agent_event(PaneId::Main, &event);
            }
            update = update_rx.recv() => {
                let Some(update) = update else {
                    app.main.transcript.push(TranscriptItem::Error(
                        "agent worker stopped".to_owned(),
                    ));
                    needs_draw = true;
                    continue;
                };
                match update {
                    WorkerEvent::TurnFinished { target, error } => {
                        app.turn_finished(target, error);
                    }
                    WorkerEvent::SteerAccepted { target, prompt } => {
                        app.steer_accepted(target, prompt);
                    }
                    WorkerEvent::SteerQueued { target, prompt } => {
                        app.steer_queued(target, prompt);
                    }
                    WorkerEvent::SteerFailed { target, error } => {
                        app.steer_failed(target, error);
                    }
                    WorkerEvent::BtwOpened { id } => app.btw_opened(id),
                    WorkerEvent::BtwOpenFailed { id, error } => app.btw_failed(id, error),
                    WorkerEvent::BtwAgentEvent { id, event } => {
                        let _ = app.on_agent_event(PaneId::Btw(id), &event);
                    }
                    WorkerEvent::BtwEventStreamClosed { id } => {
                        if app.btw_id() == Some(id) {
                            app.btw_failed(id, "BTW event stream closed".to_owned());
                        }
                    }
                }
                needs_draw = true;
            }
            _ = ticker.tick(), if app.main.running || app.btw.as_ref().is_some_and(|btw| btw.conversation.running) => {
                app.on_tick();
                needs_draw = true;
            }
        }
    }
}

fn spawn_agent_worker(
    root: Nanocodex,
    mut commands: mpsc::UnboundedReceiver<WorkerCommand>,
    updates: mpsc::UnboundedSender<WorkerEvent>,
) {
    tokio::spawn(async move {
        let mut btw: Option<BtwWorker> = None;
        while let Some(command) = commands.recv().await {
            match command {
                WorkerCommand::Prompt { target, prompt } => match target {
                    PaneId::Main => start_turn(&root, target, prompt, &updates).await,
                    PaneId::Btw(id) => {
                        if let Some(branch) = btw.as_mut().filter(|branch| branch.id == id) {
                            let prompt = branch.prepare_prompt(prompt);
                            start_turn(&branch.agent, target, prompt, &updates).await;
                        } else {
                            drop(updates.send(WorkerEvent::TurnFinished {
                                target,
                                error: Some("BTW branch is not available".to_owned()),
                            }));
                        }
                    }
                },
                WorkerCommand::Steer { target, prompt } => match target {
                    PaneId::Main => steer_turn(&root, target, prompt, &updates).await,
                    PaneId::Btw(id) => {
                        if let Some(branch) = btw.as_ref().filter(|branch| branch.id == id) {
                            steer_turn(&branch.agent, target, prompt, &updates).await;
                        } else {
                            drop(updates.send(WorkerEvent::SteerFailed {
                                target,
                                error: "BTW branch is not available".to_owned(),
                            }));
                        }
                    }
                },
                WorkerCommand::OpenBtw { id, prompt } => {
                    btw = None;
                    match root.fork().await {
                        Ok((agent, events)) => {
                            forward_btw_events(id, events, updates.clone());
                            drop(updates.send(WorkerEvent::BtwOpened { id }));
                            let mut branch = BtwWorker {
                                id,
                                agent,
                                first_prompt: true,
                            };
                            if let Some(prompt) = prompt {
                                let prompt = branch.prepare_prompt(prompt);
                                start_turn(&branch.agent, PaneId::Btw(id), prompt, &updates).await;
                            }
                            btw = Some(branch);
                        }
                        Err(error) => {
                            drop(updates.send(WorkerEvent::BtwOpenFailed {
                                id,
                                error: error.to_string(),
                            }));
                        }
                    }
                }
                WorkerCommand::CloseBtw { id } => {
                    if btw.as_ref().is_some_and(|branch| branch.id == id) {
                        btw = None;
                    }
                }
            }
        }
    });
}

async fn start_turn(
    agent: &Nanocodex,
    target: PaneId,
    prompt: String,
    updates: &mpsc::UnboundedSender<WorkerEvent>,
) {
    match agent.prompt(prompt).await {
        Ok(turn) => {
            let updates = updates.clone();
            tokio::spawn(async move {
                let error = turn.result().await.err().map(|error| error.to_string());
                drop(updates.send(WorkerEvent::TurnFinished { target, error }));
            });
        }
        Err(error) => {
            drop(updates.send(WorkerEvent::TurnFinished {
                target,
                error: Some(error.to_string()),
            }));
        }
    }
}

async fn steer_turn(
    agent: &Nanocodex,
    target: PaneId,
    prompt: String,
    updates: &mpsc::UnboundedSender<WorkerEvent>,
) {
    match agent.steer(prompt.clone()).await {
        Ok(()) => {
            drop(updates.send(WorkerEvent::SteerAccepted { target, prompt }));
        }
        Err(NanocodexError::Agent(AgentError::NoActiveTurnToSteer)) => {
            // The turn may finish between the TUI observing it and the driver
            // accepting the steer. Preserve the user's input as the next turn.
            drop(updates.send(WorkerEvent::SteerQueued {
                target,
                prompt: prompt.clone(),
            }));
            start_turn(agent, target, prompt, updates).await;
        }
        Err(error) => {
            drop(updates.send(WorkerEvent::SteerFailed {
                target,
                error: error.to_string(),
            }));
        }
    }
}

fn forward_btw_events(
    id: u64,
    mut events: AgentEvents,
    updates: mpsc::UnboundedSender<WorkerEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if updates
                .send(WorkerEvent::BtwAgentEvent { id, event })
                .is_err()
            {
                return;
            }
        }
        drop(updates.send(WorkerEvent::BtwEventStreamClosed { id }));
    });
}

fn handle_terminal_event(
    event: Event,
    app: &mut App,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<TerminalAction> {
    match event {
        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            if handle_key(key, app, commands)? {
                Ok(TerminalAction::Quit)
            } else {
                Ok(TerminalAction::Redraw)
            }
        }
        Event::Paste(text) => {
            app.insert_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
            Ok(TerminalAction::Redraw)
        }
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => {
                app.scroll_up(3);
                Ok(TerminalAction::Redraw)
            }
            MouseEventKind::ScrollDown => {
                app.scroll_down(3);
                Ok(TerminalAction::Redraw)
            }
            _ => Ok(TerminalAction::Ignore),
        },
        Event::Resize(_, _) => Ok(TerminalAction::Redraw),
        Event::FocusGained | Event::FocusLost | Event::Key(_) => Ok(TerminalAction::Ignore),
    }
}

fn handle_key(
    key: KeyEvent,
    app: &mut App,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
) -> Result<bool> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') => return Ok(true),
            KeyCode::Char('d') if app.input.is_empty() => return Ok(true),
            KeyCode::Char('j') => app.insert_char('\n'),
            KeyCode::Char('a') => app.move_home(),
            KeyCode::Char('e') => app.move_end(),
            KeyCode::Char('p') => app.previous_history(),
            KeyCode::Char('n') => app.next_history(),
            _ => {}
        }
        return Ok(false);
    }

    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            app.insert_char('\n');
        }
        KeyCode::Enter => submit(app, commands, SubmitIntent::Immediate)?,
        KeyCode::Char(character) => app.insert_char(character),
        KeyCode::Backspace => app.backspace(),
        KeyCode::Delete => app.delete(),
        KeyCode::Left => app.move_left(),
        KeyCode::Right => app.move_right(),
        KeyCode::Home => app.move_home(),
        KeyCode::End => app.move_end(),
        KeyCode::Up => app.previous_history(),
        KeyCode::Down => app.next_history(),
        KeyCode::PageUp => app.scroll_up(12),
        KeyCode::PageDown => app.scroll_down(12),
        KeyCode::Esc => app.clear_input(),
        KeyCode::Tab if app.has_input() => submit(app, commands, SubmitIntent::Queue)?,
        KeyCode::Tab | KeyCode::BackTab => app.toggle_focus(),
        KeyCode::Insert
        | KeyCode::F(_)
        | KeyCode::Null
        | KeyCode::CapsLock
        | KeyCode::ScrollLock
        | KeyCode::NumLock
        | KeyCode::PrintScreen
        | KeyCode::Pause
        | KeyCode::Menu
        | KeyCode::KeypadBegin
        | KeyCode::Media(_)
        | KeyCode::Modifier(_) => {}
    }
    Ok(false)
}

fn submit(
    app: &mut App,
    commands: &mpsc::UnboundedSender<WorkerCommand>,
    intent: SubmitIntent,
) -> Result<()> {
    let Some(input) = app.take_submission() else {
        return Ok(());
    };
    match classify_submission(input) {
        Submission::Prompt(prompt) => {
            let target = app.focus;
            if matches!(intent, SubmitIntent::Immediate) && app.is_running(target) {
                if app.queue_steer(target, prompt.clone()) {
                    send_command(commands, WorkerCommand::Steer { target, prompt })?;
                }
            } else if app.queue_prompt(target, prompt.clone()) {
                send_command(commands, WorkerCommand::Prompt { target, prompt })?;
            }
        }
        Submission::Btw(prompt) => {
            if let Some(id) = app.btw_id() {
                app.focus_btw();
                if let Some(prompt) = prompt {
                    let target = PaneId::Btw(id);
                    if app.queue_prompt(target, prompt.clone()) {
                        send_command(commands, WorkerCommand::Prompt { target, prompt })?;
                    }
                }
            } else {
                let id = app.begin_btw();
                if let Some(prompt) = prompt.as_ref() {
                    let _ = app.queue_prompt(PaneId::Btw(id), prompt.clone());
                }
                send_command(commands, WorkerCommand::OpenBtw { id, prompt })?;
            }
        }
        Submission::CloseBtw => {
            if let Some(id) = app.btw_id() {
                if app.btw_busy() {
                    app.reject_btw_close_while_busy();
                } else {
                    app.close_btw(id);
                    send_command(commands, WorkerCommand::CloseBtw { id })?;
                }
            }
        }
    }
    Ok(())
}

fn send_command(
    commands: &mpsc::UnboundedSender<WorkerCommand>,
    command: WorkerCommand,
) -> Result<()> {
    commands
        .send(command)
        .map_err(|_| eyre::eyre!("agent worker stopped"))
}

fn classify_submission(input: String) -> Submission {
    let trimmed = input.trim();
    if trimmed == "/btw" {
        return Submission::Btw(None);
    }
    if let Some(prompt) = trimmed.strip_prefix("/btw ") {
        let prompt = prompt.trim();
        return Submission::Btw((!prompt.is_empty()).then(|| prompt.to_owned()));
    }
    if trimmed == "/close" {
        return Submission::CloseBtw;
    }
    Submission::Prompt(input)
}

#[cfg(test)]
mod tests {
    use super::{BTW_BOUNDARY, Submission, classify_submission, prepare_btw_prompt};

    #[test]
    fn parses_btw_and_close_without_capturing_similar_prompts() {
        assert_eq!(
            classify_submission("/btw".to_owned()),
            Submission::Btw(None)
        );
        assert_eq!(
            classify_submission(" /btw   inspect the cache  ".to_owned()),
            Submission::Btw(Some("inspect the cache".to_owned()))
        );
        assert_eq!(
            classify_submission("/close".to_owned()),
            Submission::CloseBtw
        );
        assert_eq!(
            classify_submission("/btw-not-a-command".to_owned()),
            Submission::Prompt("/btw-not-a-command".to_owned())
        );
    }

    #[test]
    fn side_boundary_wraps_only_the_first_btw_prompt() {
        let mut first = true;
        assert_eq!(
            prepare_btw_prompt(&mut first, "first".to_owned()),
            format!("{BTW_BOUNDARY}first")
        );
        assert_eq!(
            prepare_btw_prompt(&mut first, "follow-up".to_owned()),
            "follow-up"
        );
    }
}
