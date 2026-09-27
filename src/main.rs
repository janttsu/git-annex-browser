// git-annex-browser - TUI for exploring git-annex repositories, drives, trust levels, and file locations.

use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

mod annex;
mod app;
mod cache;
mod node;
mod scan;
#[cfg(test)]
mod testutil;
mod ui;
mod usage;
mod util;
mod worker;

use annex::AnnexMetadata;
use app::Command;
use cache::Cache;
use ui::state::{UiAction, UiState};
use ui::tui;
use worker::{WorkerConfig, WorkerMsg};

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

    /// Print a text summary (no TUI) — useful for scripting or quick inspection.
    #[arg(long)]
    dump: bool,

    /// With --dump, print JSON instead of text.
    #[arg(long, requires = "dump")]
    json: bool,

    /// Scan repos and update the local cache (no TUI). Unchanged repos are skipped.
    #[arg(long)]
    scan: bool,

    /// Suppress progress and summary output (useful with --scan for cron jobs).
    #[arg(long)]
    quiet: bool,

    /// Maximum directory depth below DIR when looking for annexes.
    #[arg(long, value_name = "N")]
    max_depth: Option<usize>,

    /// Do not descend into other mounted filesystems while looking for annexes.
    #[arg(long)]
    one_file_system: bool,

    /// Cache directory (default: $GIT_ANNEX_BROWSER_CACHE or ~/.cache/git-annex-browser).
    #[arg(long, value_name = "DIR")]
    cache: Option<PathBuf>,

    /// Show only cached data; do not read the repos (slow network or unplugged drives).
    #[arg(long, conflicts_with_all = ["scan", "dump"])]
    offline: bool,

    /// Reload every repo even if its git-annex branch and HEAD are unchanged.
    #[arg(long)]
    force_rescan: bool,

    /// With --scan or --dump, delete cached repos under DIR that were not found.
    /// Without it they are kept and shown as "not found since …".
    #[arg(long)]
    prune: bool,

    /// Repos to load in parallel.
    #[arg(long, value_name = "N", default_value_t = scan::default_jobs())]
    jobs: usize,
}

impl Config {
    fn discover_options(&self) -> annex::DiscoverOptions {
        annex::DiscoverOptions {
            max_depth: self.max_depth,
            one_file_system: self.one_file_system,
        }
    }
}

fn progress_line(done: usize, total: usize, path: &Path) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let name = util::truncate_start(&name, 40);
    let pct = (done * 100).checked_div(total).unwrap_or(100);
    let bar_width = 30;
    let filled = (pct * bar_width) / 100;
    let bar = "=".repeat(filled) + &" ".repeat(bar_width - filled);
    eprint!("\r\x1b[K[{bar}] {done}/{total} ({pct}%) {name}");
    let _ = std::io::Write::flush(&mut std::io::stderr());
}

/// Discover repos under `scan_root`, reload changed ones into the cache and return
/// metadata for every repo found (from the cache when unchanged).
fn refresh_cache(
    cfg: &Config,
    cache: &Cache,
    scan_root: &Path,
    verbose: bool,
) -> Result<Vec<AnnexMetadata>> {
    if verbose {
        eprintln!(
            "Scanning for git-annex repos under {} ...",
            scan_root.display()
        );
    }
    let index = cache.load_index();
    let found = scan::discover(scan_root, &cfg.discover_options());
    let todo = scan::needs_load(&found, &index, cfg.force_rescan);
    if verbose {
        eprintln!("Found {} repos, {} new or changed", found.len(), todo.len());
    }

    let mut loaded: Vec<AnnexMetadata> = Vec::new();
    let mut failures = 0usize;
    scan::load_parallel(&todo, cfg.jobs, |n, p, res| {
        if verbose {
            progress_line(n, todo.len(), p);
        }
        match res {
            Ok(meta) => {
                if let Err(e) = cache.store_repo(&meta) {
                    failures += 1;
                    eprintln!("\n  Warning: could not cache {}: {e:#}", p.display());
                }
                loaded.push(meta);
            }
            Err(e) => {
                failures += 1;
                eprintln!("\n  Warning: failed to load {}: {e:#}", p.display());
            }
        }
    });
    if verbose && !todo.is_empty() {
        eprintln!();
    }

    let paths: Vec<PathBuf> = found.iter().map(|f| f.path.clone()).collect();
    cache.mark_scan(scan_root, &paths, cfg.prune)?;

    // Unchanged repos come from the cache.
    let index = cache.load_index();
    for f in &found {
        if loaded.iter().any(|m| m.root == f.path) {
            continue;
        }
        if let Some(meta) = index.get(&f.path).and_then(|e| cache.load_repo(e)) {
            loaded.push(meta);
        }
    }
    loaded.sort_by(|a, b| a.root.cmp(&b.root));
    if failures > 0 {
        anyhow::bail!("{failures} repo(s) could not be loaded or cached");
    }
    Ok(loaded)
}

fn run_scan(cfg: &Config, cache: &Cache, scan_root: &Path) -> Result<()> {
    let verbose = !cfg.quiet;
    refresh_cache(cfg, cache, scan_root, verbose)?;
    if verbose {
        eprintln!("Cache updated: {}", cache.dir().display());
    }
    Ok(())
}

fn run_dump(cfg: &Config, cache: &Cache, scan_root: &Path) -> Result<()> {
    let loaded = refresh_cache(cfg, cache, scan_root, !cfg.quiet && !cfg.json)?;
    let mut w = std::io::BufWriter::new(std::io::stdout().lock());
    let written = if cfg.json {
        serde_json::to_writer_pretty(&mut w, &dump_json(scan_root, &loaded))
            .map_err(io::Error::from)
            .and_then(|()| writeln!(w))
    } else {
        print_dump(&mut w, scan_root, &loaded)
    };
    match written.and_then(|()| w.flush()) {
        // `--dump | head` closes the pipe early; that is not an error.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
}

fn dump_json(scan_root: &Path, loaded: &[AnnexMetadata]) -> serde_json::Value {
    use serde_json::json;
    let summaries: Vec<_> = loaded.iter().map(|m| m.to_summary()).collect();
    let repos: Vec<_> = loaded
        .iter()
        .zip(&summaries)
        .map(|(m, s)| {
            let mut remotes: Vec<_> = m.remotes.values().collect();
            remotes.sort_by(|a, b| a.name().cmp(b.name()));
            json!({
                "path": m.root,
                "name": s.name,
                "uuid": m.uuid,
                "description": m.description,
                "files": m.files.len(),
                "keys": m.total_keys,
                "unique_size": m.unique_size,
                "consumed_size": m.consumed_size,
                "numcopies": m.numcopies,
                "keys_under_numcopies": s.keys_under,
                "keys_at_numcopies": s.keys_ok,
                "keys_over_numcopies": s.keys_over,
                "remotes": remotes.iter().map(|r| json!({
                    "name": r.name(),
                    "uuid": r.uuid,
                    "type": r.rtype(),
                    "here": r.uuid == m.uuid,
                    "trust": r.trust.as_str(),
                    "present_keys": r.present_count,
                    "present_size": r.present_size,
                    "last_fsck": r.last_fsck,
                    "groups": r.groups,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let special: Vec<_> = annex::aggregate_remote_usage(&summaries)
        .into_iter()
        .map(|(name, bytes, keys, repos)| json!({"name": name, "bytes": bytes, "keys": keys, "repos": repos}))
        .collect();
    json!({
        "root": scan_root,
        "totals": {
            "repos": loaded.len(),
            "unique_size": loaded.iter().map(|m| m.unique_size).sum::<u64>(),
            "consumed_size": loaded.iter().map(|m| m.consumed_size).sum::<u64>(),
            "files": loaded.iter().map(|m| m.files.len()).sum::<usize>(),
        },
        "special_remotes": special,
        "repos": repos,
    })
}

fn print_dump(w: &mut impl Write, scan_root: &Path, loaded: &[AnnexMetadata]) -> io::Result<()> {
    writeln!(w, "git-annex-browser dump for {}", scan_root.display())?;
    writeln!(w, "found {} annex repos\n", loaded.len())?;

    let total_unique: u64 = loaded.iter().map(|m| m.unique_size).sum();
    let total_consumed: u64 = loaded.iter().map(|m| m.consumed_size).sum();
    let total_files: usize = loaded.iter().map(|m| m.files.len()).sum();
    writeln!(w, "REPORT:")?;
    writeln!(
        w,
        "  total unique data (1 copy per file): {}",
        util::human_bytes(total_unique)
    )?;
    writeln!(
        w,
        "  total storage across all drives (with copies): {}",
        util::human_bytes(total_consumed)
    )?;
    writeln!(w, "  total working tree files: {}", total_files)?;
    let summaries: Vec<_> = loaded.iter().map(|m| m.to_summary()).collect();
    let per_remote = annex::aggregate_remote_usage(&summaries);
    if !per_remote.is_empty() {
        writeln!(w, "  storage per special remote (rclone etc.):")?;
        for (name, bytes, keys, repos) in per_remote {
            writeln!(
                w,
                "    - {} : {} ({} keys, {} repos)",
                name,
                util::human_bytes(bytes),
                keys,
                repos
            )?;
        }
    }
    writeln!(w)?;

    for m in loaded {
        let r = &m.root;
        let clean = r
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| r.display().to_string());
        let desc_note = if !m.description.is_empty() && m.description != clean {
            format!(" ({})", m.description)
        } else {
            String::new()
        };
        writeln!(w, "=== {}{} ===", clean, desc_note)?;
        writeln!(w, "  path: {}", r.display())?;
        writeln!(w, "  uuid: {}", m.uuid)?;
        writeln!(
            w,
            "  files in tree: {}, keys: {}",
            m.files.len(),
            m.total_keys
        )?;
        writeln!(
            w,
            "  unique size (1 copy): {}",
            util::human_bytes(m.unique_size)
        )?;
        writeln!(
            w,
            "  consumed across drives: {}",
            util::human_bytes(m.consumed_size)
        )?;
        writeln!(w, "  remotes/drives:")?;
        let mut rems: Vec<_> = m.remotes.values().collect();
        rems.sort_by_key(|r| (std::cmp::Reverse(r.last_fsck.unwrap_or(0)), r.name()));
        for rem in rems {
            let marker = if rem.uuid == m.uuid { " [HERE]" } else { "" };
            let fs = rem
                .last_fsck
                .map(|t| format!(" fsck={}", util::fmt_unix(t)))
                .unwrap_or_default();
            let size = if rem.present_size > 0 {
                format!(" {}", util::human_bytes(rem.present_size))
            } else {
                String::new()
            };
            writeln!(
                w,
                "    - {} ({}){} trust={} present={} keys{}{}",
                rem.name(),
                rem.rtype(),
                marker,
                rem.trust.as_str(),
                rem.present_count,
                size,
                fs,
            )?;
        }
        writeln!(w)?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let cfg = Config::parse();
    let scan_root: PathBuf = PathBuf::from(&cfg.dir)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&cfg.dir));
    let cache = Cache::from_env(cfg.cache.clone());

    if cfg.scan {
        return run_scan(&cfg, &cache, &scan_root);
    }
    if cfg.dump {
        return run_dump(&cfg, &cache, &scan_root);
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let (worker, snap_rx, worker_thread) = worker::spawn(
        WorkerConfig {
            scan_root,
            discover: cfg.discover_options(),
            cache,
            jobs: cfg.jobs,
            offline: cfg.offline,
            force_rescan: cfg.force_rescan,
        },
        Arc::clone(&cancel),
    );

    let result = run_tui(&cfg, &worker, &snap_rx);
    // Stop the worker and let it finish pending cache writes.
    cancel.store(true, Ordering::Relaxed);
    worker.send(WorkerMsg::Nav(Command::Quit, 0));
    let _ = worker_thread.join();
    result
}

fn run_tui(
    cfg: &Config,
    worker: &worker::WorkerHandle,
    snap_rx: &std::sync::mpsc::Receiver<worker::WorkerOut>,
) -> Result<()> {
    let mut guard = tui::TerminalGuard::new()?;
    let tick = Duration::from_millis(cfg.tick_ms.clamp(10, 1000));
    let mut ui = UiState::default();
    let mut info = tui::DrawInfo::default();
    let mut dirty = true;

    loop {
        while let Ok(msg) = snap_rx.try_recv() {
            ui.on_worker(msg);
            dirty = true;
        }

        if dirty {
            guard.term.draw(|frame| info = tui::draw(frame, &ui))?;
            dirty = false;
        }

        if !event::poll(tick)? {
            continue;
        }
        let page = tui::page_size(&guard.term);
        let action = match event::read()? {
            Event::Key(key) => ui.handle_key(key, page),
            Event::Mouse(m) => ui.handle_mouse(m, &info, page),
            Event::Resize(..) => UiAction::None,
            _ => continue,
        };
        dirty = true;

        match action {
            UiAction::None => {}
            UiAction::Quit => return Ok(()),
            UiAction::Send(cmd) => {
                if !worker.send(WorkerMsg::Nav(cmd, page)) {
                    return Ok(());
                }
            }
        }
    }
}
