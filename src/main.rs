// git-annex-browser - TUI for exploring git-annex repositories, drives, trust levels, and file locations.

use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

mod annex;
mod app;
mod node;
mod ui;
mod usage;
mod util;
mod worker;

use app::Command;
use std::sync::atomic::Ordering;
use ui::state::{UiAction, UiState};
use ui::tui;
use worker::WorkerMsg;

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
            let name = r
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| r.display().to_string());
            let name = util::truncate_start(&name, 40);
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
        println!("  total working tree files: {}", total_files);
        let summaries: Vec<_> = loaded.iter().map(|(_, m)| m.to_summary()).collect();
        let per_remote = annex::aggregate_remote_usage(&summaries);
        if !per_remote.is_empty() {
            println!("  storage per special remote (rclone etc.):");
            for (name, bytes, keys, repos) in per_remote {
                println!(
                    "    - {} : {} ({} keys, {} repos)",
                    name,
                    util::human_bytes(bytes),
                    keys,
                    repos
                );
            }
        }
        println!();

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
                    "    - {} ({}){} trust={} present={} keys{}{}{}",
                    rem.name(),
                    rem.rtype(),
                    marker,
                    rem.trust.as_str(),
                    rem.present_count,
                    if rem.present_size > 0 {
                        format!(" {}", util::human_bytes(rem.present_size))
                    } else {
                        String::new()
                    },
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
    let mut ui = UiState::default();
    let mut dirty = true;

    loop {
        while let Ok(msg) = snap_rx.try_recv() {
            ui.on_worker(msg);
            dirty = true;
        }

        if dirty {
            guard.term.draw(|frame| tui::draw(frame, &ui))?;
            dirty = false;
        }

        if !event::poll(tick)? {
            continue;
        }
        let key = match event::read()? {
            Event::Key(key) => key,
            Event::Resize(..) => {
                dirty = true;
                continue;
            }
            _ => continue,
        };
        dirty = true;

        let page = tui::page_size(&guard.term);
        match ui.handle_key(key, page) {
            UiAction::None => {}
            UiAction::Quit => {
                request_quit(&cancel, &cmd_tx);
                break;
            }
            UiAction::Send(cmd) => {
                if cmd_tx.send(WorkerMsg::Nav(cmd, page)).is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn request_quit(cancel: &Arc<AtomicBool>, cmd_tx: &std::sync::mpsc::Sender<WorkerMsg>) {
    cancel.store(true, Ordering::Relaxed);
    let _ = cmd_tx.send(WorkerMsg::Nav(Command::Quit, 0));
}
