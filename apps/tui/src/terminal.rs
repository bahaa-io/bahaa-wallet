use std::io::{self, stdout};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use ratatui::{DefaultTerminal, Frame};

pub(crate) struct Session(DefaultTerminal);

impl Session {
    pub(crate) fn start() -> io::Result<Self> {
        let session = Self(ratatui::try_init()?);
        let restore = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
            restore(info);
        }));
        execute!(stdout(), EnableMouseCapture, EnableBracketedPaste)?;
        Ok(session)
    }

    pub(crate) fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) -> io::Result<()> {
        self.0.draw(render).map(drop)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
        let _ = ratatui::try_restore();
    }
}
