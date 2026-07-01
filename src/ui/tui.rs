/*!
TUI rendering using ratatui. Styled after zfs-browser.
*/

use crate::app::ViewSnapshot;
use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{Clear as CrosstermClear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem as RatListItem, ListState, Paragraph, Wrap},
};
use std::{io::{Result as IoResult, Stdout, stdout}, panic};

static HELP_TEXT: &str = r#"
  ↑ / k          up
  ↓ / j          down
  PgUp/PgDn      page
  ⇧PgUp/PgDn     scroll details
  g / G          top / bottom
  → / Enter / l  descend
  ← / Back / h   back
  r / F5         refresh (re-scan)
  x              toggle raw / hex-ish view of selection
  ? / F1         toggle help
  q / Esc        quit

Focus on drives (special remotes), trust, last fsck, groups/wanted, numcopies, file locations per drive.
"#;

pub const LIST_CHROME_ROWS: u16 = 2;

pub struct TerminalGuard {
    pub term: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    pub fn new() -> IoResult<Self> {
        panic::set_hook(Box::new(|info| {
            let _ = disable_raw_mode();
            let _ = execute!(stdout(), LeaveAlternateScreen, Show);
            eprintln!("panic: {}", info);
        }));
        enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen, CrosstermClear(ClearType::All), Hide)?;
        let mut term = Terminal::new(CrosstermBackend::new(out))?;
        term.clear()?;
        Ok(Self { term })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, Show);
    }
}

pub fn page_size(term: &Terminal<CrosstermBackend<Stdout>>) -> usize {
    term.size().map(|s| s.height.saturating_sub(LIST_CHROME_ROWS + 2) as usize).unwrap_or(20)
}

pub fn draw(
    frame: &mut Frame,
    snap: Option<&ViewSnapshot>,
    busy: bool,
    show_help: bool,
    show_raw: bool,
    detail_scroll: usize,
) {
    // Clear the screen on every frame to prevent old terminal content from showing through.
    frame.render_widget(Clear, frame.area());

    let Some(snap) = snap else {
        let msg = Paragraph::new("scanning for annex repos…")
            .block(Block::default().borders(Borders::ALL).title(" git-annex-browser "));
        frame.render_widget(msg, centered_rect(40, 3, frame.area()));
        return;
    };

    if show_help {
        let p = Paragraph::new(HELP_TEXT)
            .block(Block::default().borders(Borders::ALL).title(" help "))
            .wrap(Wrap { trim: true });
        frame.render_widget(p, centered_rect(70, 22, frame.area()));
        return;
    }

    let [crumb_area, main_area, status_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ]).areas(frame.area());

    let [list_area, detail_area] = Layout::horizontal([
        Constraint::Percentage(48),
        Constraint::Percentage(52),
    ]).areas(main_area);

    render_breadcrumb(frame, crumb_area, snap);
    render_list(frame, list_area, snap);
    render_details(frame, detail_area, snap, detail_scroll, show_raw);
    render_status(frame, status_area, snap, busy);
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage(50),
        Constraint::Length(height),
        Constraint::Percentage(50),
    ]).split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ]).split(popup_layout[1])[1]
}

fn render_breadcrumb(frame: &mut Frame, area: Rect, snap: &ViewSnapshot) {
    let text = snap.crumb.join("  ▸  ");
    let p = Paragraph::new(text).style(Style::default().fg(Color::Cyan));
    frame.render_widget(p, area);
}

fn render_list(frame: &mut Frame, area: Rect, snap: &ViewSnapshot) {
    let items: Vec<RatListItem> = snap.list.iter().enumerate().map(|(i, it)| {
        let kind_style = match it.kind.as_str() {
            "drive" => Style::default().fg(Color::Blue),
            "here" => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            "repo" => Style::default().fg(Color::Magenta),
            "file" => Style::default().fg(Color::Gray),
            "report" => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            _ => Style::default(),
        };
        let sel_marker = if i == snap.selected { "▶ " } else { "  " };
        let label_style = if it.anomalous {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        RatListItem::new(Line::from(vec![
            Span::raw(sel_marker),
            Span::styled(format!("[{}] ", it.kind), kind_style),
            Span::styled(&it.label, label_style),
        ]))
    }).collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(format!(" git-annex-browser ({}) ", snap.total_repos)));
    let mut state = ListState::default();
    state.select(Some(snap.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_details(frame: &mut Frame, area: Rect, snap: &ViewSnapshot, scroll: usize, show_raw: bool) {
    let content = if show_raw {
        snap.raw.clone().unwrap_or_else(|| "no raw data for selection".into())
    } else {
        snap.details.join("\n")
    };

    let title = if show_raw { " details (raw) " } else { " details " };

    let p = Paragraph::new(content)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));

    frame.render_widget(p, area);
}

fn render_status(frame: &mut Frame, area: Rect, snap: &ViewSnapshot, busy: bool) {
    let busy_str = if busy { " ⏳" } else { "" };
    let text = format!("{}{}  •  {} repos  •  ↑↓ nav  → descend  ← back  r refresh  x raw  q quit", snap.status, busy_str, snap.total_repos);
    let p = Paragraph::new(text).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(p, area);
}