// git-annex-browser - TUI for exploring git-annex repositories, drives, trust levels, and file locations.

use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

mod annex;
mod app;
mod node;
mod ui;
mod util;
mod worker;

use app::{Command, ListItem, ViewSnapshot};
use std::sync::atomic::Ordering;
use ui::{keyboard::map_key, tui};
use worker::{WorkerMsg, WorkerOut};

#[derive(Parser)]
#[command(
    version,
    about = "TUI for git-annex repos & drives. Caches data so you can quickly browse the state of many (often offline) drives via a clear interface."
)]
struct Config {
    /// Directory to recursively scan for git-annex repositories
    #[arg(default_value = ".")]
    dir: String,

    /// UI tick interval ms
    #[arg(long, default_value_t = 100)]
    tick_ms: u64,

    /// Dump textual summary (no TUI) — useful for scripting or quick inspection
    #[arg(long)]
    dump: bool,

    /// Scan repos and update the local cache (no TUI/GUI).
    /// The tool's main value is caching git-annex metadata so that viewing
    /// the overall state of many repositories and drives is fast and clear
    /// in the interactive interface, instead of running slow commands repeatedly.
    #[arg(long)]
    scan: bool,

    /// Suppress progress and summary output (useful with --scan for cron jobs).
    #[arg(long)]
    quiet: bool,
}

fn run_scan(cfg: &Config, scan_root: &Path) -> Result<()> {
    let quiet = cfg.quiet;

    if !quiet {
        eprintln!(
            "Scanning for git-annex repos under {} ...",
            scan_root.display()
        );
    }

    let repos = annex::find_annex_repos(scan_root);

    if !quiet {
        eprintln!("Found {} repos", repos.len());
    }

    let mut found = std::collections::HashMap::new();

    let total = repos.len();
    for (i, r) in repos.iter().enumerate() {
        let idx = i + 1;

        if !quiet {
            let mut name = r
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| r.display().to_string());
            if name.len() > 40 {
                name = format!("...{}", &name[name.len() - 37..]);
            }
            let pct = (idx * 100).checked_div(total).unwrap_or(100);
            let bar_width = 30;
            let filled = (pct * bar_width) / 100;
            let bar = "=".repeat(filled) + &" ".repeat(bar_width - filled);
            eprint!("\r[{}] {}/{} ({}%) {}", bar, idx, total, pct, name);
            let _ = std::io::Write::flush(&mut std::io::stderr());
        }

        match annex::load_metadata(r) {
            Ok(m) => {
                found.insert(r.to_string_lossy().to_string(), m);
            }
            Err(e) => {
                if !quiet {
                    eprintln!("\n  Warning: failed to load {}: {}", r.display(), e);
                }
            }
        }
    }

    if !quiet {
        eprintln!(); // finish the progress line
    }

    if let Err(e) = annex::merge_scan_into_cache(scan_root, found) {
        if !quiet {
            eprintln!("Failed to save cache: {}", e);
        }
        return Err(e);
    }

    if !quiet {
        let p = annex::cache_path();
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        eprintln!("Cache updated: {} ({} bytes)", p.display(), size);
    }

    Ok(())
}

fn main() -> Result<()> {
    let cfg = Config::parse();
    let scan_root: PathBuf = PathBuf::from(&cfg.dir)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&cfg.dir));

    if cfg.scan {
        return run_scan(&cfg, &scan_root);
    }

    if cfg.dump {
        // Non-interactive dump mode
        let repos = annex::find_annex_repos(&scan_root);
        println!("git-annex-browser dump for {}", scan_root.display());
        println!("found {} annex repos\n", repos.len());

        let mut loaded: Vec<(PathBuf, annex::AnnexMetadata)> = Vec::new();
        for r in &repos {
            match annex::load_metadata(r) {
                Ok(m) => loaded.push((r.clone(), m)),
                Err(e) => eprintln!("  load {} failed: {}", r.display(), e),
            }
        }

        let total_unique: u64 = loaded.iter().map(|(_, m)| m.unique_size).sum();
        let total_consumed: u64 = loaded.iter().map(|(_, m)| m.consumed_size).sum();
        let total_files: usize = loaded.iter().map(|(_, m)| m.files.len()).sum();
        println!("REPORT:");
        println!(
            "  total unique data (1 copy per file): {}",
            util::human_bytes(total_unique)
        );
        println!(
            "  total storage across all drives (with copies): {}",
            util::human_bytes(total_consumed)
        );
        println!("  total working tree files: {}\n", total_files);

        for (r, m) in &loaded {
            let clean = r
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| r.display().to_string());
            let desc_note = if !m.description.is_empty() && m.description != clean {
                format!(" ({})", m.description)
            } else {
                String::new()
            };
            println!("=== {}{} ===", clean, desc_note);
            println!("  path: {}", r.display());
            println!("  uuid: {}", m.uuid);
            println!("  files in tree: {}, keys: {}", m.files.len(), m.total_keys);
            println!(
                "  unique size (1 copy): {}",
                util::human_bytes(m.unique_size)
            );
            println!(
                "  consumed across drives: {}",
                util::human_bytes(m.consumed_size)
            );
            println!("  remotes/drives:");
            let mut rems: Vec<_> = m.remotes.values().collect();
            rems.sort_by_key(|r| {
                (
                    std::cmp::Reverse(r.last_fsck.unwrap_or(0)),
                    r.name().to_string(),
                )
            });
            for rem in rems {
                let marker = if rem.uuid == m.uuid { " [HERE]" } else { "" };
                let fs = rem
                    .last_fsck
                    .map(|t| format!(" fsck={}", util::fmt_unix(t)))
                    .unwrap_or_default();
                let sp = rem
                    .available_space
                    .map(|b| format!(" {} free", util::human_bytes(b)))
                    .unwrap_or_default();
                println!(
                    "    - {} ({}){} trust={} present={} keys{}{}",
                    rem.name(),
                    rem.rtype(),
                    marker,
                    rem.trust.as_str(),
                    rem.present_count,
                    fs,
                    sp
                );
            }
            println!();
        }
        if !loaded.is_empty() {
            let cache_repos = loaded
                .into_iter()
                .map(|(r, m)| (r.to_string_lossy().to_string(), m))
                .collect();
            let _ = annex::merge_scan_into_cache(&scan_root, cache_repos);
        }
        return Ok(());
    }

    let cancel = Arc::new(AtomicBool::new(false));

    let (cmd_tx, snap_rx) = worker::spawn(scan_root, Arc::clone(&cancel));

    let mut guard = tui::TerminalGuard::new()?;
    let tick = Duration::from_millis(cfg.tick_ms.clamp(10, 1000));
    let mut snapshot: Option<ViewSnapshot> = None;
    let mut pending: usize = 0;
    let mut show_help = false;
    let mut show_raw = false;
    let mut detail_scroll: usize = 0;
    let mut filter = String::new();
    let mut filter_editing = false;

    loop {
        while let Ok(msg) = snap_rx.try_recv() {
            match msg {
                WorkerOut::Nav(s) => {
                    pending = pending.saturating_sub(1);
                    let same = snapshot
                        .as_ref()
                        .map(|o| o.crumb == s.crumb && o.selected == s.selected)
                        .unwrap_or(false);
                    if !same {
                        detail_scroll = 0;
                    }
                    snapshot = Some(s);
                }
                WorkerOut::Background(s) => {
                    if pending == 0 {
                        let same = snapshot
                            .as_ref()
                            .map(|o| o.crumb == s.crumb && o.selected == s.selected)
                            .unwrap_or(false);
                        if !same {
                            detail_scroll = 0;
                        }
                        snapshot = Some(s);
                    }
                }
            }
        }

        guard.term.draw(|frame| {
            tui::draw(
                frame,
                snapshot.as_ref(),
                pending > 0,
                show_help,
                show_raw,
                detail_scroll,
                &filter,
                filter_editing,
            )
        })?;

        if !event::poll(tick)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            request_quit(&cancel, &cmd_tx);
            break;
        }

        if key.kind != KeyEventKind::Press {
            continue;
        }

        if filter_editing {
            match key.code {
                KeyCode::Esc => {
                    filter_editing = false;
                    filter.clear();
                }
                KeyCode::Enter => filter_editing = false,
                KeyCode::Backspace => {
                    filter.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    filter.clear();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    filter.push(c);
                }
                _ => {}
            }
            continue;
        }

        if key.code == KeyCode::Char('/') && !show_help {
            filter_editing = true;
            continue;
        }

        if key.code == KeyCode::Char('x') && !show_help {
            show_raw = !show_raw;
            continue;
        }

        if key.modifiers.contains(KeyModifiers::SHIFT)
            && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
        {
            let page = tui::page_size(&guard.term).max(1);
            let rows = snapshot.as_ref().map(|s| s.details.len()).unwrap_or(0);
            let max = rows.saturating_sub(page);
            detail_scroll = match key.code {
                KeyCode::PageDown => (detail_scroll + page).min(max),
                _ => detail_scroll.saturating_sub(page),
            };
            continue;
        }

        let cmd = map_key(key);
        match cmd {
            Command::Quit => {
                request_quit(&cancel, &cmd_tx);
                break;
            }
            Command::ToggleHelp => show_help = !show_help,
            Command::None => (),
            _ if show_help => {
                show_help = false;
            }
            Command::Up
            | Command::Down
            | Command::PageUp
            | Command::PageDown
            | Command::Top
            | Command::Bottom => {
                if let Some(s) = &mut snapshot {
                    apply_nav(s, cmd, tui::page_size(&guard.term).max(1), &filter);
                }
                let send = if filter.is_empty() {
                    cmd
                } else {
                    Command::Select(snapshot.as_ref().map(|s| s.selected).unwrap_or(0))
                };
                let page = tui::page_size(&guard.term);
                if cmd_tx.send(WorkerMsg::Nav(send, page)).is_err() {
                    break;
                }
                pending += 1;
            }
            Command::Descend | Command::Back | Command::Refresh => {
                if matches!(cmd, Command::Descend | Command::Back | Command::Refresh) {
                    filter.clear();
                    filter_editing = false;
                }
                let page = tui::page_size(&guard.term);
                if cmd_tx.send(WorkerMsg::Nav(cmd, page)).is_err() {
                    break;
                }
                pending += 1;
            }
            cmd => {
                let page = tui::page_size(&guard.term);
                if cmd_tx.send(WorkerMsg::Nav(cmd, page)).is_err() {
                    break;
                }
                pending += 1;
            }
        }
    }
    Ok(())
}

fn request_quit(cancel: &Arc<AtomicBool>, cmd_tx: &std::sync::mpsc::Sender<WorkerMsg>) {
    cancel.store(true, Ordering::Relaxed);
    let _ = cmd_tx.send(WorkerMsg::Nav(Command::Quit, 0));
}

fn visible_indices(list: &[ListItem], filter: &str) -> Vec<usize> {
    if filter.is_empty() {
        return (0..list.len()).collect();
    }
    let f = filter.to_lowercase();
    list.iter()
        .enumerate()
        .filter(|(_, it)| {
            it.label.to_lowercase().contains(&f) || it.kind.to_lowercase().contains(&f)
        })
        .map(|(i, _)| i)
        .collect()
}

fn apply_nav(s: &mut ViewSnapshot, cmd: Command, page_size: usize, filter: &str) {
    let vis = visible_indices(&s.list, filter);
    if vis.is_empty() {
        return;
    }
    let cur = vis.iter().position(|&i| i == s.selected).unwrap_or(0);
    let last = vis.len().saturating_sub(1);
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
