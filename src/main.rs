// git-annex-browser - TUI for exploring git-annex repositories, drives, trust levels, and file locations.

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

mod annex;
mod app;
mod node;
mod ui;
mod util;
mod worker;

use app::{Command, ViewSnapshot};
use ui::{keyboard::map_key, tui};
use worker::WorkerMsg;

#[derive(Parser)]
#[command(version, about = "TUI browser for git-annex metadata: repos, remotes/drives, trust, fsck, file locations")]
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

    /// Scan all repos under the directory and update the cache (no TUI/GUI).
    /// Intended for periodic/cron use.
    #[arg(long)]
    scan: bool,

    /// Suppress progress and summary output (useful with --scan for cron).
    #[arg(long)]
    quiet: bool,
}

fn run_scan(cfg: &Config, scan_root: &PathBuf) -> Result<()> {
    let quiet = cfg.quiet;

    if !quiet {
        eprintln!("Scanning for git-annex repos under {} ...", scan_root.display());
    }

    let repos = annex::find_annex_repos(scan_root);

    if !quiet {
        eprintln!("Found {} repos", repos.len());
    }

    let mut cache = annex::AnnexCache {
        version: 1,
        updated: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
        repos: std::collections::HashMap::new(),
    };

    let total = repos.len();
    for (i, r) in repos.iter().enumerate() {
        let idx = i + 1;

        if !quiet {
            let mut name = r
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| r.display().to_string());
            if name.len() > 40 {
                name = format!("...{}", &name[name.len()-37..]);
            }
            let pct = if total == 0 { 100 } else { (idx * 100) / total };
            let bar_width = 30;
            let filled = (pct * bar_width) / 100;
            let bar = "=".repeat(filled) + &" ".repeat(bar_width - filled);
            eprint!("\r[{}] {}/{} ({}%) {}", bar, idx, total, pct, name);
            let _ = std::io::Write::flush(&mut std::io::stderr());
        }

        match annex::load_metadata(r) {
            Ok(m) => {
                cache.repos.insert(r.to_string_lossy().to_string(), m);
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

    if let Err(e) = annex::save_cache(&cache) {
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
    let scan_root: PathBuf = PathBuf::from(&cfg.dir).canonicalize().unwrap_or_else(|_| PathBuf::from(&cfg.dir));

    if cfg.scan {
        return run_scan(&cfg, &scan_root);
    }

    if cfg.dump {
        // Non-interactive dump mode
        let repos = annex::find_annex_repos(&scan_root);
        println!("git-annex-browser dump for {}", scan_root.display());
        println!("found {} annex repos\n", repos.len());
        for r in &repos {
            match annex::load_metadata(r) {
                Ok(m) => {
                    let clean = r.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| r.display().to_string());
                    let desc_note = if !m.description.is_empty() && m.description != clean { format!(" ({})", m.description) } else { String::new() };
                    println!("=== {}{} ===", clean, desc_note);
                    println!("  path: {}", r.display());
                    println!("  uuid: {}", m.uuid);
                    println!("  files in tree: {}, keys: {}", m.files.len(), m.total_keys);
                    println!("  remotes/drives:");
                    let mut rems: Vec<_> = m.remotes.values().collect();
                    // sort by last fsck (most recent first) then name
                    rems.sort_by_key(|r| (std::cmp::Reverse(r.last_fsck.unwrap_or(0)), r.name().to_string()));
                    for rem in rems {
                        let marker = if rem.uuid == m.uuid { " [HERE]" } else { "" };
                        let fs = rem.last_fsck.map(|t| format!(" fsck={}", util::fmt_unix(t))).unwrap_or_default();
                        let sp = rem.available_space.map(|b| format!(" {} free", util::human_bytes(b))).unwrap_or_default();
                        println!("    - {} ({}){} trust={} present={} keys{}{}", rem.name(), rem.rtype(), marker, rem.trust.as_str(), rem.present_count, fs, sp);
                    }
                    println!();
                }
                Err(e) => eprintln!("  load {} failed: {}", r.display(), e),
            }
        }
        // Also persist a cache snapshot from this dump (fresh data)
        let mut cache_repos = std::collections::HashMap::new();
        for r in &repos {
            if let Ok(m) = annex::load_metadata(r) {
                cache_repos.insert(r.to_string_lossy().to_string(), m);
            }
        }
        if !cache_repos.is_empty() {
            let c = annex::AnnexCache {
                version: 1,
                updated: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64,
                repos: cache_repos,
            };
            let _ = annex::save_cache(&c);
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

    loop {
        while let Ok(s) = snap_rx.try_recv() {
            pending = pending.saturating_sub(1);
            detail_scroll = 0;
            snapshot = Some(s);
        }

        guard.term.draw(|frame| {
            tui::draw(frame, snapshot.as_ref(), pending > 0, show_help, show_raw, detail_scroll)
        })?;

        if !event::poll(tick)? { continue; }
        let Event::Key(key) = event::read()? else { continue; };

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            break;
        }

        if key.kind != KeyEventKind::Press { continue; }

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
            Command::Quit => break,
            Command::ToggleHelp => show_help = !show_help,
            Command::None => (),
            _ if show_help => { show_help = false; }
            // Handle pure navigation locally for instant UI response.
            // Worker will receive the command for authoritative state.
            Command::Up | Command::Down | Command::PageUp | Command::PageDown |
            Command::Top | Command::Bottom => {
                if let Some(s) = &mut snapshot {
                    let len = s.list.len().saturating_sub(1);
                    let page_size = tui::page_size(&guard.term).max(1);
                    match cmd {
                        Command::Up => s.selected = s.selected.saturating_sub(1),
                        Command::Down => s.selected = (s.selected + 1).min(len),
                        Command::PageUp => s.selected = s.selected.saturating_sub(page_size),
                        Command::PageDown => s.selected = (s.selected + page_size).min(len),
                        Command::Top => s.selected = 0,
                        Command::Bottom => s.selected = len,
                        _ => {}
                    }
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