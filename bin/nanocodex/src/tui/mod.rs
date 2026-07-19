mod app;
mod terminal;
mod view;

use std::time::Duration;

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use eyre::{Result, WrapErr};
use futures_util::StreamExt;
use nanocodex::Nanocodex;
use tokio::{
    sync::mpsc,
    time::{MissedTickBehavior, interval},
};

use self::{app::App, terminal::TerminalSession};
use crate::config::AgentArgs;

enum WorkerEvent {
    TurnFinished { error: Option<String> },
}

enum TerminalAction {
    Redraw,
    Ignore,
    Quit,
}

pub(crate) async fn run(config: AgentArgs, initial_prompt: Option<String>) -> Result<()> {
    let cwd = config
        .cwd()
        .canonicalize()
        .wrap_err("failed to resolve the working directory")?;
    let (agent, mut agent_events) = config.build()?;
    let (prompt_tx, prompt_rx) = mpsc::unbounded_channel();
    let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
    spawn_prompt_worker(agent, prompt_rx, worker_tx);

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
        submit(&mut app, &prompt_tx)?;
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
                match handle_terminal_event(event, &mut app, &prompt_tx)? {
                    TerminalAction::Redraw => needs_draw = true,
                    TerminalAction::Ignore => {}
                    TerminalAction::Quit => return Ok(()),
                }
            }
            event = agent_events.recv(), if agent_events_open => {
                let Some(event) = event else {
                    app.transcript.push(app::TranscriptItem::Error(
                        "agent event stream closed".to_owned(),
                    ));
                    app.running = false;
                    "Agent stopped".clone_into(&mut app.status);
                    agent_events_open = false;
                    needs_draw = true;
                    continue;
                };
                needs_draw |= app.on_agent_event(&event);
            }
            update = worker_rx.recv() => {
                if let Some(WorkerEvent::TurnFinished { error }) = update {
                    app.turn_finished(error);
                    needs_draw = true;
                }
            }
            _ = ticker.tick(), if app.running => {
                app.on_tick();
                needs_draw = true;
            }
        }
    }
}

fn spawn_prompt_worker(
    agent: Nanocodex,
    mut prompts: mpsc::UnboundedReceiver<String>,
    updates: mpsc::UnboundedSender<WorkerEvent>,
) {
    tokio::spawn(async move {
        while let Some(prompt) = prompts.recv().await {
            match agent.prompt(prompt).await {
                Ok(turn) => {
                    let updates = updates.clone();
                    tokio::spawn(async move {
                        let error = turn.result().await.err().map(|error| error.to_string());
                        drop(updates.send(WorkerEvent::TurnFinished { error }));
                    });
                }
                Err(error) => {
                    drop(updates.send(WorkerEvent::TurnFinished {
                        error: Some(error.to_string()),
                    }));
                }
            }
        }
    });
}

fn handle_terminal_event(
    event: Event,
    app: &mut App,
    prompts: &mpsc::UnboundedSender<String>,
) -> Result<TerminalAction> {
    match event {
        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            if handle_key(key, app, prompts)? {
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
    prompts: &mpsc::UnboundedSender<String>,
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
        KeyCode::Enter => submit(app, prompts)?,
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
        KeyCode::Tab
        | KeyCode::BackTab
        | KeyCode::Insert
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

fn submit(app: &mut App, prompts: &mpsc::UnboundedSender<String>) -> Result<()> {
    if let Some(prompt) = app.take_submission() {
        prompts
            .send(prompt)
            .map_err(|_| eyre::eyre!("agent prompt worker stopped"))?;
    }
    Ok(())
}
