use std::io::{self, IsTerminal, Stdout, stdin, stdout};

use crossterm::{
    cursor::{Hide, Show},
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

pub(super) type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

pub(super) struct TerminalSession {
    terminal: TuiTerminal,
}

struct RestoreOnDrop {
    armed: bool,
}

impl TerminalSession {
    pub(super) fn enter() -> io::Result<Self> {
        if !stdin().is_terminal() || !stdout().is_terminal() {
            return Err(io::Error::other(
                "interactive mode requires terminal stdin and stdout; use `nanocodex run` for JSONL",
            ));
        }
        enable_raw_mode()?;
        let mut restore = RestoreOnDrop { armed: true };
        let mut output = stdout();
        execute!(
            output,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            Hide
        )?;
        drop(execute!(
            output,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        ));
        let terminal = Terminal::new(CrosstermBackend::new(output))?;
        restore.armed = false;
        Ok(Self { terminal })
    }

    pub(super) fn terminal(&mut self) -> &mut TuiTerminal {
        &mut self.terminal
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        drop(self.terminal.show_cursor());
        restore(self.terminal.backend_mut());
    }
}

impl Drop for RestoreOnDrop {
    fn drop(&mut self) {
        if self.armed {
            restore(&mut stdout());
        }
    }
}

fn restore(output: &mut impl io::Write) {
    drop(disable_raw_mode());
    drop(execute!(
        output,
        Show,
        PopKeyboardEnhancementFlags,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    ));
}
