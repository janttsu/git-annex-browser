use crossterm::event::{KeyCode, KeyEvent};
use crate::app::Command;

pub fn map_key(key: KeyEvent) -> Command {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Command::Quit,
        KeyCode::Up | KeyCode::Char('k') => Command::Up,
        KeyCode::Down | KeyCode::Char('j') => Command::Down,
        KeyCode::PageUp => Command::PageUp,
        KeyCode::PageDown => Command::PageDown,
        KeyCode::Char('g') => Command::Top,
        KeyCode::Char('G') => Command::Bottom,
        KeyCode::Right | KeyCode::Enter | KeyCode::Char('l') => Command::Descend,
        KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => Command::Back,
        KeyCode::Char('r') | KeyCode::F(5) => Command::Refresh,
        KeyCode::Char('?') | KeyCode::F(1) => Command::ToggleHelp,
        KeyCode::Char('x') => Command::ToggleRaw, // handled specially in main sometimes
        _ => Command::None,
    }
}