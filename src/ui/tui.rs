/*!
TUI rendering using ratatui. Styled after zfs-browser.
*/

use crate::app::{ViewSnapshot, VisualRemoteKind, VisualRepo, VisualRepoDetail, VisualReport};
use crate::util::human_bytes;
use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{
        Clear as CrosstermClear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
        disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem as RatListItem, ListState, Paragraph, Wrap},
};
use std::{
    io::{Result as IoResult, Stdout, stdout},
    panic,
};

static HELP_TEXT: &str = r#"
  ↑ / k          up
  ↓ / j          down
  PgUp/PgDn      page
  ⇧PgUp/PgDn     scroll details
  g / G          top / bottom
  → / Enter / l  descend
  ← / Back / h   back
  r / F5         refresh (re-scan)
  /              filter current list
  x              toggle raw / hex-ish view of selection
  z              zoom visual to full screen
  ? / F1         toggle help
  q / Esc        quit

Focus on drives (special remotes), trust, last fsck, groups/wanted, numcopies, file locations per drive.
  Global report and per-repo visual: size bars + copy-health vs numcopies.
  Enter on the report (or z) zooms it full screen; h/z back.
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
        execute!(
            out,
            EnterAlternateScreen,
            CrosstermClear(ClearType::All),
            Hide
        )?;
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
    term.size()
        .map(|s| s.height.saturating_sub(LIST_CHROME_ROWS + 2) as usize)
        .unwrap_or(20)
}

#[allow(clippy::too_many_arguments)]
pub fn draw(
    frame: &mut Frame,
    snap: Option<&ViewSnapshot>,
    busy: bool,
    show_help: bool,
    show_raw: bool,
    detail_scroll: usize,
    filter: &str,
    filter_editing: bool,
    zoom: bool,
) {
    // Clear the screen on every frame to prevent old terminal content from showing through.
    frame.render_widget(Clear, frame.area());

    let Some(snap) = snap else {
        let msg = Paragraph::new("scanning for annex repos…").block(
            Block::default()
                .borders(Borders::ALL)
                .title(" git-annex-browser "),
        );
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
    ])
    .areas(frame.area());

    render_breadcrumb(frame, crumb_area, snap);

    let zoomed = zoom && visual_for(snap) != VisualKind::None;
    if zoomed {
        render_details(frame, main_area, snap, detail_scroll, show_raw, true);
    } else {
        let [list_area, detail_area] =
            Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
                .areas(main_area);
        render_list(frame, list_area, snap, filter);
        render_details(frame, detail_area, snap, detail_scroll, show_raw, false);
    }
    render_status(frame, status_area, snap, busy, filter, filter_editing, zoom);
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage(50),
        Constraint::Length(height),
        Constraint::Percentage(50),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}

fn render_breadcrumb(frame: &mut Frame, area: Rect, snap: &ViewSnapshot) {
    let text = snap.crumb.join("  ▸  ");
    let p = Paragraph::new(text).style(Style::default().fg(Color::Cyan));
    frame.render_widget(p, area);
}

fn render_list(frame: &mut Frame, area: Rect, snap: &ViewSnapshot, filter: &str) {
    let f = filter.to_lowercase();
    let visible: Vec<(usize, &crate::app::ListItem)> = snap
        .list
        .iter()
        .enumerate()
        .filter(|(_, it)| {
            f.is_empty()
                || it.label.to_lowercase().contains(&f)
                || it.kind.to_lowercase().contains(&f)
        })
        .collect();
    let selected_vis = visible.iter().position(|(i, _)| *i == snap.selected);

    let items: Vec<RatListItem> = visible
        .iter()
        .map(|(i, it)| {
            let kind_style = match it.kind.as_str() {
                "drive" => Style::default().fg(Color::Blue),
                "here" => Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                "repo" => Style::default().fg(Color::Magenta),
                "file" => Style::default().fg(Color::Gray),
                "report" | "viz" => Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                _ => Style::default(),
            };
            let sel_marker = if *i == snap.selected { "▶ " } else { "  " };
            let label_style = if it.anomalous {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else if let Some(t) = it.trust {
                Style::default().fg(crate::util::trust_color(t))
            } else {
                Style::default()
            };
            RatListItem::new(Line::from(vec![
                Span::raw(sel_marker),
                Span::styled(format!("[{}] ", it.kind), kind_style),
                Span::styled(&it.label, label_style),
            ]))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" git-annex-browser ({}) ", snap.total_repos)),
    );
    let mut state = ListState::default();
    state.select(selected_vis);
    frame.render_stateful_widget(list, area, &mut state);
}

fn selected_kind(snap: &ViewSnapshot) -> &str {
    snap.list
        .get(snap.selected)
        .map(|it| it.kind.as_str())
        .unwrap_or("")
}

fn selected_repo_visual(snap: &ViewSnapshot) -> Option<&VisualRepoDetail> {
    let key = snap.list.get(snap.selected)?.repo_name.as_deref()?;
    snap.repo_visuals.iter().find(|v| v.name == key)
}

fn visual_for(snap: &ViewSnapshot) -> VisualKind {
    match selected_kind(snap) {
        "report" if snap.visual.is_some() => VisualKind::Global,
        "repo" | "viz" if selected_repo_visual(snap).is_some() => VisualKind::Repo,
        _ => VisualKind::None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VisualKind {
    None,
    Global,
    Repo,
}

fn render_details(
    frame: &mut Frame,
    area: Rect,
    snap: &ViewSnapshot,
    scroll: usize,
    show_raw: bool,
    zoomed: bool,
) {
    if !show_raw {
        match visual_for(snap) {
            VisualKind::Global => {
                if let Some(vis) = &snap.visual {
                    render_visual_report(frame, area, vis, snap.scanning, scroll, zoomed);
                    return;
                }
            }
            VisualKind::Repo => {
                if let Some(vis) = selected_repo_visual(snap) {
                    render_repo_visual(frame, area, vis, snap.scanning, scroll, zoomed);
                    return;
                }
            }
            VisualKind::None => {}
        }
    }

    let content = if show_raw {
        snap.raw
            .clone()
            .unwrap_or_else(|| "no raw data for selection".into())
    } else {
        snap.details.join("\n")
    };

    let title = if show_raw {
        " details (raw) "
    } else {
        " details "
    };

    let p = Paragraph::new(content)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));

    frame.render_widget(p, area);
}

fn render_status(
    frame: &mut Frame,
    area: Rect,
    snap: &ViewSnapshot,
    busy: bool,
    filter: &str,
    filter_editing: bool,
    zoom: bool,
) {
    let busy_str = if busy { " [busy]" } else { "" };
    let filter_str = if filter_editing {
        format!("  filter: {filter}_")
    } else if !filter.is_empty() {
        format!("  filter: {filter}")
    } else {
        String::new()
    };
    let text = format!(
        "{}{}{}  •  {} repos  •  / filter  z zoom  ↑↓ nav  → descend  ← back  r refresh  q quit{}",
        snap.status,
        busy_str,
        filter_str,
        snap.total_repos,
        if zoom { "  [zoomed]" } else { "" }
    );
    let p = Paragraph::new(text).style(Style::default().fg(Color::Gray));
    frame.render_widget(p, area);
}

fn health_color(kind: &str) -> Color {
    match kind {
        "ok" => Color::Green,
        "mixed" => Color::Yellow,
        "poor" => Color::Red,
        _ => Color::DarkGray,
    }
}

fn block_bar(filled: usize, width: usize, color: Color) -> Vec<Span<'static>> {
    let filled = filled.min(width);
    vec![
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled(
            "░".repeat(width - filled),
            Style::default().fg(Color::DarkGray),
        ),
    ]
}

fn copy_mini(repo: &VisualRepo, width: usize) -> Vec<Span<'static>> {
    if repo.keys_tracked == 0 || width == 0 {
        return vec![Span::styled(
            "░".repeat(width),
            Style::default().fg(Color::DarkGray),
        )];
    }
    let t = repo.keys_tracked as f64;
    let mut n_under = ((repo.keys_under as f64 / t) * width as f64).round() as usize;
    let mut n_ok = ((repo.keys_ok as f64 / t) * width as f64).round() as usize;
    let mut n_over = ((repo.keys_over as f64 / t) * width as f64).round() as usize;
    while n_under + n_ok + n_over > width {
        if n_over > 0 {
            n_over -= 1;
        } else if n_ok > 0 {
            n_ok -= 1;
        } else {
            n_under = n_under.saturating_sub(1);
        }
    }
    while n_under + n_ok + n_over < width {
        n_ok += 1;
    }
    let mut spans = Vec::new();
    if n_under > 0 {
        spans.push(Span::styled(
            "█".repeat(n_under),
            Style::default().fg(Color::Red),
        ));
    }
    if n_ok > 0 {
        spans.push(Span::styled(
            "█".repeat(n_ok),
            Style::default().fg(Color::Green),
        ));
    }
    if n_over > 0 {
        spans.push(Span::styled(
            "█".repeat(n_over),
            Style::default().fg(Color::Cyan),
        ));
    }
    spans
}

fn trunc_name(name: &str, width: usize) -> String {
    if name.chars().count() <= width {
        format!("{name:<width$}")
    } else {
        let mut s: String = name.chars().take(width.saturating_sub(1)).collect();
        s.push('…');
        format!("{s:<width$}")
    }
}

pub fn detail_scroll_rows(snap: &ViewSnapshot) -> usize {
    match visual_for(snap) {
        VisualKind::Global => snap
            .visual
            .as_ref()
            .map(|v| {
                4 + v.repos.len()
                    + if v.remotes.is_empty() {
                        0
                    } else {
                        2 + v.remotes.len()
                    }
            })
            .unwrap_or(snap.details.len()),
        VisualKind::Repo => selected_repo_visual(snap)
            .map(|v| 7 + v.remotes.len())
            .unwrap_or(snap.details.len()),
        VisualKind::None => snap.details.len(),
    }
}

fn render_visual_report(
    frame: &mut Frame,
    area: Rect,
    vis: &VisualReport,
    scanning: bool,
    scroll: usize,
    zoomed: bool,
) {
    let title = if scanning {
        " visual report (updating)  z=zoom "
    } else if zoomed {
        " visual report  z/h=back to list "
    } else {
        " visual report  z=full screen "
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 24 || inner.height == 0 {
        return;
    }

    let w = inner.width as usize;
    let name_w = (w / 4).clamp(14, 24);
    let (bar_w, show_counts) = bar_budget(w, name_w);
    let max_sz = vis.repos.iter().map(|r| r.unique_size).max().unwrap_or(0);
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::raw("unique "),
        Span::styled(
            human_bytes(vis.unique_size),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   stored "),
        Span::styled(
            human_bytes(vis.consumed_size),
            Style::default().fg(Color::White),
        ),
        Span::raw(format!("   {} repos", vis.repos.len())),
    ]));
    lines.push(Line::from(vec![
        Span::styled("↓under ", Style::default().fg(Color::Red)),
        Span::styled("✓ok ", Style::default().fg(Color::Green)),
        Span::styled("↑extra ", Style::default().fg(Color::Cyan)),
        Span::styled(
            "bar color = copy health vs numcopies",
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "data by repo",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));

    for repo in &vis.repos {
        lines.push(repo_row(repo, max_sz, name_w, bar_w, show_counts));
    }

    if !vis.remotes.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "special remotes",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        let max_r = vis.remotes.iter().map(|(_, b)| *b).max().unwrap_or(0);
        for (name, bytes) in &vis.remotes {
            lines.push(remote_row(
                name,
                *bytes,
                None,
                max_r,
                name_w.clamp(16, 28),
                bar_w,
                Color::Magenta,
            ));
        }
    }

    let p = Paragraph::new(lines).scroll((scroll as u16, 0));
    frame.render_widget(p, inner);
}

fn render_repo_visual(
    frame: &mut Frame,
    area: Rect,
    vis: &VisualRepoDetail,
    scanning: bool,
    scroll: usize,
    zoomed: bool,
) {
    let title = if scanning {
        format!(" {}  (updating)  z=zoom ", vis.name)
    } else if zoomed {
        format!(" {}  z/h=back to list ", vis.name)
    } else {
        format!(" {}  z=full screen ", vis.name)
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 24 || inner.height == 0 {
        return;
    }

    let w = inner.width as usize;
    let name_w = (w / 3).clamp(14, 28);
    let (bar_w, _) = bar_budget(w, name_w);
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::raw("unique "),
        Span::styled(
            human_bytes(vis.unique_size),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   stored "),
        Span::styled(
            human_bytes(vis.consumed_size),
            Style::default().fg(Color::White),
        ),
        Span::raw(format!("   numcopies {}", vis.numcopies)),
    ]));

    let health = vis.health_kind();
    lines.push(Line::from(vec![
        Span::raw("copy health  "),
        Span::styled(
            health,
            Style::default()
                .fg(health_color(health))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
    ]));
    let mut health_line = vec![Span::raw("             ")];
    health_line.extend(copy_mini_counts(
        vis.keys_tracked,
        vis.keys_under,
        vis.keys_ok,
        vis.keys_over,
        bar_w.max(16),
    ));
    health_line.push(Span::styled(
        format!(
            "  ↓{}  ✓{}  ↑{}",
            vis.keys_under, vis.keys_ok, vis.keys_over
        ),
        Style::default().fg(Color::Gray),
    ));
    lines.push(Line::from(health_line));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "on remotes / drives",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));

    let max_r = vis.remotes.iter().map(|r| r.size).max().unwrap_or(0);
    if vis.remotes.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no location data yet",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for r in &vis.remotes {
            let color = match r.kind {
                VisualRemoteKind::Here => Color::Green,
                VisualRemoteKind::Special => Color::Magenta,
                VisualRemoteKind::Other => Color::Blue,
            };
            lines.push(remote_row(
                &r.name,
                r.size,
                Some(r.count),
                max_r,
                name_w,
                bar_w,
                color,
            ));
        }
    }

    let p = Paragraph::new(lines).scroll((scroll as u16, 0));
    frame.render_widget(p, inner);
}

fn bar_budget(width: usize, name_w: usize) -> (usize, bool) {
    // name + bar + size + n= + health [+ counts]
    let show_counts = width >= 88;
    let extra = 9 + 4 + 8 + if show_counts { 16 } else { 0 } + 4;
    let bar = width.saturating_sub(name_w + extra).clamp(8, 64);
    (bar, show_counts)
}

fn repo_row(
    repo: &VisualRepo,
    max_sz: u64,
    name_w: usize,
    bar_w: usize,
    show_counts: bool,
) -> Line<'static> {
    let filled = if max_sz == 0 {
        0
    } else {
        ((repo.unique_size as f64 / max_sz as f64) * bar_w as f64).round() as usize
    };
    let color = health_color(repo.health_kind());
    let mut spans = vec![Span::styled(
        trunc_name(&repo.name, name_w),
        Style::default().fg(Color::White),
    )];
    spans.push(Span::raw(" "));
    spans.extend(block_bar(filled, bar_w, color));
    spans.push(Span::styled(
        format!(" {:>9}", human_bytes(repo.unique_size)),
        Style::default().fg(Color::Gray),
    ));
    spans.push(Span::styled(
        format!(" n={}", repo.numcopies),
        Style::default().fg(Color::DarkGray),
    ));
    spans.push(Span::raw(" "));
    spans.extend(copy_mini(repo, 8));
    if repo.keys_tracked == 0 {
        spans.push(Span::styled(
            " pending",
            Style::default().fg(Color::DarkGray),
        ));
    } else if show_counts {
        spans.push(Span::styled(
            format!(
                " ↓{} ✓{} ↑{}",
                repo.keys_under, repo.keys_ok, repo.keys_over
            ),
            Style::default().fg(Color::Gray),
        ));
    }
    Line::from(spans)
}

fn remote_row(
    name: &str,
    bytes: u64,
    count: Option<usize>,
    max: u64,
    name_w: usize,
    bar_w: usize,
    color: Color,
) -> Line<'static> {
    let filled = if max == 0 {
        0
    } else {
        ((bytes as f64 / max as f64) * bar_w as f64).round() as usize
    };
    let mut spans = vec![Span::styled(
        trunc_name(name, name_w),
        Style::default().fg(color),
    )];
    spans.push(Span::raw(" "));
    spans.extend(block_bar(filled, bar_w, color));
    spans.push(Span::styled(
        format!(" {:>9}", human_bytes(bytes)),
        Style::default().fg(Color::Gray),
    ));
    if let Some(c) = count {
        spans.push(Span::styled(
            format!(" {c} keys"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

fn copy_mini_counts(
    tracked: usize,
    under: usize,
    ok: usize,
    over: usize,
    width: usize,
) -> Vec<Span<'static>> {
    copy_mini(
        &VisualRepo {
            name: String::new(),
            unique_size: 0,
            numcopies: 1,
            keys_tracked: tracked,
            keys_under: under,
            keys_ok: ok,
            keys_over: over,
        },
        width,
    )
}
