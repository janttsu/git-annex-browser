use crate::app::Command;
use crossterm::event::{KeyCode, KeyEvent};

/// Keys that become worker commands. UI-only keys (`/`, `x`, `z`, Esc, detail
/// scrolling) are handled in `UiState::handle_key` before this is called.
pub fn map_key(key: KeyEvent) -> Command {
    match key.code {
        KeyCode::Char('q') => Command::Quit,
        KeyCode::Up | KeyCode::Char('k') => Command::Up,
        KeyCode::Down | KeyCode::Char('j') => Command::Down,
        KeyCode::PageUp => Command::PageUp,
        KeyCode::PageDown => Command::PageDown,
        KeyCode::Home | KeyCode::Char('g') => Command::Top,
        KeyCode::End | KeyCode::Char('G') => Command::Bottom,
        KeyCode::Right | KeyCode::Enter | KeyCode::Char('l') => Command::Descend,
        KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => Command::Back,
        KeyCode::Char('r') | KeyCode::F(5) => Command::Refresh,
        KeyCode::Char('?') | KeyCode::F(1) => Command::ToggleHelp,
        _ => Command::None,
    }
}
