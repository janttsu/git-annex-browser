//! UI-thread state and key handling, kept free of terminal I/O so it can be unit tested.

use crate::app::{Command, ViewSnapshot, visible_indices};
use crate::ui::keyboard::map_key;
use crate::ui::tui;
use crate::worker::WorkerOut;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// What the main loop should do after a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    /// Nothing for the worker; redraw if state changed.
    None,
    /// Forward a navigation command to the worker.
    Send(Command),
    /// Stop the program.
    Quit,
}

#[derive(Default)]
pub struct UiState {
    pub snapshot: Option<ViewSnapshot>,
    /// Navigation commands sent to the worker without a reply yet.
    pub pending: usize,
    pub show_help: bool,
    pub show_raw: bool,
    pub zoom: bool,
    pub filter: String,
    pub filter_editing: bool,
    pub detail_scroll: usize,
}

impl UiState {
    pub fn busy(&self) -> bool {
        self.pending > 0 || self.snapshot.as_ref().is_some_and(|s| s.scanning)
    }

    fn selected_kind(&self) -> Option<crate::node::NodeKind> {
        let s = self.snapshot.as_ref()?;
        s.list.get(s.selected).map(|i| i.kind)
    }

    /// Apply a message from the worker.
    pub fn on_worker(&mut self, msg: WorkerOut) {
        match msg {
            WorkerOut::Nav(s) => {
                self.pending = self.pending.saturating_sub(1);
                self.replace_snapshot(s);
            }
            WorkerOut::Background(s) => self.apply_background(s),
        }
    }

    fn replace_snapshot(&mut self, s: ViewSnapshot) {
        let same = self
            .snapshot
            .as_ref()
            .is_some_and(|o| o.crumb == s.crumb && o.selected == s.selected);
        if !same {
            self.detail_scroll = 0;
        }
        self.snapshot = Some(s);
    }

    /// A background refresh must not undo a selection the user just moved locally.
    /// The worker's reply to the pending navigation always carries the newest state,
    /// so while navigation is in flight only list data, status and scan state are taken.
    fn apply_background(&mut self, incoming: ViewSnapshot) {
        if self.pending == 0 {
            self.replace_snapshot(incoming);
            return;
        }
        let Some(cur) = self.snapshot.as_mut() else {
            self.snapshot = Some(incoming);
            return;
        };
        cur.status = incoming.status;
        cur.scanning = incoming.scanning;
        cur.total_repos = incoming.total_repos;
        cur.visual = incoming.visual;
        cur.repo_visuals = incoming.repo_visuals;
        if cur.crumb != incoming.crumb {
            return;
        }
        let sel = cur.selected.min(incoming.list.len().saturating_sub(1));
        if sel == incoming.selected {
            cur.details = incoming.details;
            cur.raw = incoming.raw;
            cur.usage = incoming.usage;
        }
        cur.list = incoming.list;
        cur.selected = sel;
    }

    /// Handle a key. `page` is the number of visible list rows.
    pub fn handle_key(&mut self, key: KeyEvent, page: usize) -> UiAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return UiAction::Quit;
        }
        if key.kind != KeyEventKind::Press {
            return UiAction::None;
        }

        if self.filter_editing {
            match key.code {
                KeyCode::Esc => {
                    self.filter_editing = false;
                    self.filter.clear();
                }
                KeyCode::Enter => self.filter_editing = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char('u') if ctrl => self.filter.clear(),
                KeyCode::Char(c) if !ctrl => self.filter.push(c),
                _ => {}
            }
            return UiAction::None;
        }

        // Any key closes the help overlay; it never quits or navigates.
        if self.show_help {
            self.show_help = false;
            return UiAction::None;
        }

        let page = page.max(1);
        match key.code {
            // Esc closes the topmost mode, then goes up a level. Only `q` quits.
            KeyCode::Esc => {
                if self.zoom {
                    self.zoom = false;
                    return UiAction::None;
                }
                if !self.filter.is_empty() {
                    self.filter.clear();
                    return UiAction::None;
                }
                return self.nav_level(Command::Back);
            }
            KeyCode::Char('/') => {
                self.filter_editing = true;
                return UiAction::None;
            }
            KeyCode::Char('x') => {
                self.show_raw = !self.show_raw;
                return UiAction::None;
            }
            KeyCode::Char('z') => {
                self.zoom = !self.zoom;
                return UiAction::None;
            }
            KeyCode::PageUp | KeyCode::PageDown if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.scroll_details(key.code == KeyCode::PageDown, page);
                return UiAction::None;
            }
            KeyCode::Char('J') => {
                self.scroll_details(true, 1);
                return UiAction::None;
            }
            KeyCode::Char('K') => {
                self.scroll_details(false, 1);
                return UiAction::None;
            }
            KeyCode::Char('d') if ctrl => {
                self.scroll_details(true, page / 2);
                return UiAction::None;
            }
            KeyCode::Char('u') if ctrl => {
                self.scroll_details(false, page / 2);
                return UiAction::None;
            }
            _ => {}
        }

        match map_key(key) {
            Command::None => UiAction::None,
            Command::Quit if self.zoom => {
                self.zoom = false;
                UiAction::None
            }
            Command::Quit => UiAction::Quit,
            Command::ToggleHelp => {
                self.show_help = true;
                UiAction::None
            }
            cmd @ (Command::Up
            | Command::Down
            | Command::PageUp
            | Command::PageDown
            | Command::Top
            | Command::Bottom) => self.nav_move(cmd, page),
            Command::Back if self.zoom => {
                self.zoom = false;
                UiAction::None
            }
            Command::Descend if self.selected_kind().is_some_and(|k| k.is_visual()) => {
                self.zoom = true;
                UiAction::None
            }
            cmd @ (Command::Descend | Command::Back | Command::Refresh) => self.nav_level(cmd),
            cmd => self.send(cmd),
        }
    }

    /// Move the selection immediately (so the list stays snappy) and tell the worker.
    fn nav_move(&mut self, cmd: Command, page: usize) -> UiAction {
        let mut send = cmd;
        if let Some(s) = self.snapshot.as_mut() {
            apply_nav(s, cmd, page, &self.filter);
            if !self.filter.is_empty() {
                send = Command::Select(s.selected);
            }
        }
        self.send(send)
    }

    fn nav_level(&mut self, cmd: Command) -> UiAction {
        self.filter.clear();
        self.filter_editing = false;
        if cmd == Command::Refresh {
            self.zoom = false;
        }
        self.send(cmd)
    }

    fn send(&mut self, cmd: Command) -> UiAction {
        self.pending += 1;
        UiAction::Send(cmd)
    }

    pub fn scroll_details(&mut self, down: bool, by: usize) {
        let by = by.max(1);
        if down {
            let rows = self
                .snapshot
                .as_ref()
                .map(tui::detail_scroll_rows)
                .unwrap_or(0);
            self.detail_scroll = (self.detail_scroll + by).min(rows.saturating_sub(1));
        } else {
            self.detail_scroll = self.detail_scroll.saturating_sub(by);
        }
    }
}

pub fn apply_nav(s: &mut ViewSnapshot, cmd: Command, page_size: usize, filter: &str) {
    let vis = visible_indices(&s.list, filter);
    if vis.is_empty() {
        return;
    }
    let cur = vis.iter().position(|&i| i == s.selected).unwrap_or(0);
    let last = vis.len() - 1;
    let next = match cmd {
        Command::Up => cur.saturating_sub(1),
        Command::Down => (cur + 1).min(last),
        Command::PageUp => cur.saturating_sub(page_size),
        Command::PageDown => (cur + page_size).min(last),
        Command::Top => 0,
        Command::Bottom => last,
        _ => cur,
    };
    s.selected = vis[next];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ListItem;
    use crate::node::NodeKind;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn item(label: &str, kind: NodeKind) -> ListItem {
        ListItem {
            label: label.into(),
            kind,
            anomalous: false,
            trust: None,
            repo_name: None,
            size: None,
            missing: false,
            under_copies: false,
        }
    }

    fn snap(items: Vec<ListItem>, crumb: &[&str]) -> ViewSnapshot {
        ViewSnapshot {
            crumb: crumb.iter().map(|s| s.to_string()).collect(),
            list: items,
            selected: 0,
            details: vec![],
            raw: None,
            visual: None,
            repo_visuals: vec![],
            usage: None,
            status: "s".into(),
            total_repos: 0,
            scanning: false,
        }
    }

    fn state() -> UiState {
        UiState {
            snapshot: Some(snap(
                vec![
                    item("alpha", NodeKind::Repo),
                    item("beta", NodeKind::Repo),
                    item("gamma", NodeKind::Repo),
                ],
                &["root"],
            )),
            ..Default::default()
        }
    }

    #[test]
    fn q_in_help_closes_help_instead_of_quitting() {
        let mut ui = state();
        assert_eq!(ui.handle_key(key(KeyCode::Char('?')), 10), UiAction::None);
        assert!(ui.show_help);
        assert_eq!(ui.handle_key(key(KeyCode::Char('q')), 10), UiAction::None);
        assert!(!ui.show_help);
        assert_eq!(ui.handle_key(key(KeyCode::Char('q')), 10), UiAction::Quit);
    }

    #[test]
    fn esc_closes_modes_then_goes_back_but_never_quits() {
        let mut ui = state();
        ui.zoom = true;
        ui.filter = "al".into();
        assert_eq!(ui.handle_key(key(KeyCode::Esc), 10), UiAction::None);
        assert!(!ui.zoom);
        assert_eq!(ui.handle_key(key(KeyCode::Esc), 10), UiAction::None);
        assert!(ui.filter.is_empty());
        assert_eq!(
            ui.handle_key(key(KeyCode::Esc), 10),
            UiAction::Send(Command::Back)
        );
    }

    #[test]
    fn esc_while_editing_filter_clears_it() {
        let mut ui = state();
        ui.handle_key(key(KeyCode::Char('/')), 10);
        ui.handle_key(key(KeyCode::Char('b')), 10);
        assert_eq!(ui.filter, "b");
        ui.handle_key(key(KeyCode::Esc), 10);
        assert!(!ui.filter_editing);
        assert!(ui.filter.is_empty());
    }

    #[test]
    fn filtered_navigation_sends_absolute_selection() {
        let mut ui = state();
        ui.filter = "a".into(); // alpha, beta, gamma all contain "a"
        ui.filter = "ma".into(); // only gamma
        let act = ui.handle_key(key(KeyCode::Down), 10);
        assert_eq!(act, UiAction::Send(Command::Select(2)));
        assert_eq!(ui.snapshot.as_ref().unwrap().selected, 2);
    }

    #[test]
    fn background_snapshot_keeps_local_selection_while_pending() {
        let mut ui = state();
        ui.handle_key(key(KeyCode::Down), 10);
        assert_eq!(ui.pending, 1);
        let mut bg = snap(
            vec![
                item("alpha", NodeKind::Repo),
                item("beta", NodeKind::Repo),
                item("gamma", NodeKind::Repo),
            ],
            &["root"],
        );
        bg.status = "scanning 3/4".into();
        bg.scanning = true;
        ui.on_worker(WorkerOut::Background(bg));
        let s = ui.snapshot.as_ref().unwrap();
        assert_eq!(s.selected, 1);
        assert_eq!(s.status, "scanning 3/4");
        assert!(ui.busy());
    }

    #[test]
    fn background_snapshot_for_other_view_still_updates_status() {
        let mut ui = state();
        ui.handle_key(key(KeyCode::Right), 10);
        let mut bg = snap(vec![item("x", NodeKind::Repo)], &["root", "other"]);
        bg.status = "cache updated".into();
        ui.on_worker(WorkerOut::Background(bg));
        let s = ui.snapshot.as_ref().unwrap();
        assert_eq!(s.crumb, vec!["root".to_string()]);
        assert_eq!(s.status, "cache updated");
    }

    #[test]
    fn enter_on_report_zooms_locally() {
        let mut ui = UiState {
            snapshot: Some(snap(
                vec![item("Global report", NodeKind::Report)],
                &["root"],
            )),
            ..Default::default()
        };
        assert_eq!(ui.handle_key(key(KeyCode::Enter), 10), UiAction::None);
        assert!(ui.zoom);
        assert_eq!(ui.handle_key(key(KeyCode::Char('h')), 10), UiAction::None);
        assert!(!ui.zoom);
    }
}
