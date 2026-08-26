/*!
Worker thread pattern copied from zfs-browser: UI talks via mpsc, worker owns the App + heavy data.
Slow loads happen here.
*/

use crate::annex::{self, RepoSummary};
use crate::app::{App, Command, ViewSnapshot};
use anyhow::Result;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

pub enum WorkerMsg {
    Nav(Command, usize /*page size*/),
}

pub enum WorkerOut {
    Nav(ViewSnapshot),
    Background(ViewSnapshot),
}

pub struct Worker {
    pub app: App,
}

pub fn spawn(
    scan_root: PathBuf,
    cancel: Arc<AtomicBool>,
) -> (Sender<WorkerMsg>, Receiver<WorkerOut>) {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (snap_tx, snap_rx) = mpsc::channel();
    let (meta_tx, meta_rx) = mpsc::channel::<(PathBuf, Result<annex::AnnexMetadata>)>();

    thread::spawn(move || {
        // meta_rx moved into this worker thread for draining results
        let meta_rx = meta_rx;
        let mut worker = Worker {
            app: App::new(scan_root.clone()),
        };

        // 1. Try cache first for instant UI (only repos under this scan root).
        if let Some(cache) = annex::load_cache() {
            for (pstr, mut meta) in cache.repos {
                let p = PathBuf::from(&pstr);
                if !annex::path_is_under(&p, &scan_root) {
                    continue;
                }
                meta.ensure_sizes();
                let mut sum = meta.to_summary();
                worker.app.preloaded.insert(p.clone(), Rc::new(meta));
                sum.ensure_name();
                worker.app.summaries.push(sum);
            }
            worker.app.status = format!(
                "loaded {} from cache — discovering…",
                worker.app.preloaded.len()
            );
            worker.app.recompute_drive_profiles();
            worker.app.refresh_root_view();
            worker.app.scanning = true;
            let _ = snap_tx.send(WorkerOut::Background(worker.app.snapshot(20)));
        }

        // 2. On-disk discovery (always). Cache snapshot already went to the UI.
        let discovered = annex::find_annex_repos(&scan_root);

        worker
            .app
            .summaries
            .retain(|s| discovered.iter().any(|p| p == &s.root));
        worker
            .app
            .preloaded
            .retain(|p, _| discovered.iter().any(|d| d == p));

        for p in &discovered {
            if !worker.app.summaries.iter().any(|s| &s.root == p) {
                let name = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let mut sum = RepoSummary {
                    root: p.clone(),
                    uuid: String::new(),
                    name,
                    annex_description: String::new(),
                    file_count: 0,
                    remote_count: 0,
                    here_present_count: 0,
                    here_available_space: None,
                    unique_size: 0,
                    consumed_size: 0,
                    remote_usage: vec![],
                    numcopies: None,
                    keys_tracked: 0,
                    keys_under: 0,
                    keys_ok: 0,
                    keys_over: 0,
                };
                sum.ensure_name();
                worker.app.summaries.push(sum);
            }
        }

        queue_hydrate(&mut worker.app, &discovered);
        let mut hydrate_total = worker.app.to_hydrate.len();
        worker.app.set_discovered(discovered.clone());
        mark_scan_progress(&mut worker.app, 0, hydrate_total);
        let _ = snap_tx.send(WorkerOut::Background(worker.app.snapshot(20)));

        // 3. Background hydration loop + cache updates
        // We interleave with the normal command loop using the timeout path.
        let mut dirty = false;
        let mut last_save = std::time::Instant::now();
        let mut in_flight = 0usize;
        const MAX_HYDRATE: usize = 2;

        loop {
            if cancel.load(Ordering::Relaxed) {
                break;
            }

            while let Ok((p, res)) = meta_rx.try_recv() {
                in_flight = in_flight.saturating_sub(1);
                match res {
                    Ok(meta) => {
                        worker.app.ingest_meta(meta);
                        dirty = true;
                        mark_scan_progress(&mut worker.app, in_flight, hydrate_total);
                        if !worker.app.scanning {
                            worker.app.status =
                                format!("{} repos • cache updated", worker.app.preloaded.len());
                            persist_scan(&worker.app, &scan_root);
                            last_save = std::time::Instant::now();
                            dirty = false;
                        } else if last_save.elapsed() > Duration::from_secs(4) {
                            persist_preloaded(&worker.app);
                            last_save = std::time::Instant::now();
                            dirty = false;
                        }
                        let _ = snap_tx.send(WorkerOut::Background(worker.app.snapshot(20)));
                    }
                    Err(e) => {
                        mark_scan_progress(&mut worker.app, in_flight, hydrate_total);
                        worker.app.status = format!("failed {}: {}", p.display(), e);
                        let _ = snap_tx.send(WorkerOut::Background(worker.app.snapshot(20)));
                    }
                }
            }

            let msg = match cmd_rx.recv_timeout(Duration::from_millis(180)) {
                Ok(m) => m,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // --- Background work on idle ticks ---
                    // Spawn heavy loads off-thread so UI commands stay responsive.
                    while in_flight < MAX_HYDRATE {
                        let Some(p) = worker.app.to_hydrate.pop() else {
                            break;
                        };
                        in_flight += 1;
                        let meta_tx = meta_tx.clone();
                        std::thread::spawn(move || {
                            let res = annex::load_metadata(&p);
                            let _ = meta_tx.send((p, res));
                        });
                    }
                    mark_scan_progress(&mut worker.app, in_flight, hydrate_total);
                    if in_flight == 0
                        && !worker.app.scanning
                        && dirty
                        && last_save.elapsed() > Duration::from_secs(2)
                    {
                        persist_scan(&worker.app, &scan_root);
                        last_save = std::time::Instant::now();
                        dirty = false;
                        worker.app.status =
                            format!("{} repos • cache updated", worker.app.preloaded.len());
                        let _ = snap_tx.send(WorkerOut::Background(worker.app.snapshot(20)));
                    }
                    continue;
                }
                Err(_) => break,
            };

            match msg {
                WorkerMsg::Nav(cmd, page) => {
                    if cmd == Command::Quit {
                        break;
                    }
                    if let Err(e) = worker.app.execute(cmd, page) {
                        worker.app.status = format!("err: {}", e);
                    }
                    if cmd == Command::Refresh {
                        let repos = annex::find_annex_repos(&scan_root);
                        worker
                            .app
                            .summaries
                            .retain(|s| repos.iter().any(|p| p == &s.root));
                        worker
                            .app
                            .preloaded
                            .retain(|p, _| repos.iter().any(|d| d == p));
                        queue_hydrate(&mut worker.app, &repos);
                        hydrate_total = worker.app.to_hydrate.len() + in_flight;
                        worker.app.set_discovered(repos);
                        worker.app.recompute_drive_profiles();
                        mark_scan_progress(&mut worker.app, in_flight, hydrate_total);
                        while worker.app.stack.len() > 1 {
                            worker.app.stack.pop();
                        }
                        dirty = true;
                    }

                    // On-demand load for a loading placeholder (rare now thanks to bg)
                    if let Some(loading) = worker.app.stack.last()
                        && let Some(p) = loading.node.loading_path()
                    {
                        match annex::load_metadata(&p) {
                            Ok(meta) => {
                                worker.app.install_loaded_repo(meta);
                                dirty = true;
                            }
                            Err(e) => {
                                worker.app.status = format!("load failed: {}", e);
                                worker.app.stack.pop();
                            }
                        }
                    }

                    let snap = worker.app.snapshot(page);
                    let _ = snap_tx.send(WorkerOut::Nav(snap));
                }
            }
        }

        if !worker.app.preloaded.is_empty() {
            persist_preloaded(&worker.app);
        }
    });

    (cmd_tx, snap_rx)
}

fn persist_preloaded(app: &App) {
    let repos = app
        .preloaded
        .iter()
        .map(|(k, v)| (k.to_string_lossy().to_string(), v.as_ref().clone()))
        .collect::<Vec<_>>();
    let _ = annex::upsert_cache_repos(repos);
}

fn persist_scan(app: &App, scan_root: &std::path::Path) {
    let repos = app
        .preloaded
        .iter()
        .filter(|(k, _)| annex::path_is_under(k, scan_root))
        .map(|(k, v)| (k.to_string_lossy().to_string(), v.as_ref().clone()))
        .collect();
    let _ = annex::merge_scan_into_cache(scan_root, repos);
}

/// Uncached repos first (pop from the end), then refresh already-cached ones.
fn queue_hydrate(app: &mut App, discovered: &[PathBuf]) {
    let mut cached = Vec::new();
    let mut fresh = Vec::new();
    for p in discovered {
        if app.preloaded.contains_key(p) {
            cached.push(p.clone());
        } else {
            fresh.push(p.clone());
        }
    }
    app.to_hydrate.clear();
    app.to_hydrate.extend(cached);
    app.to_hydrate.extend(fresh);
}

fn mark_scan_progress(app: &mut App, in_flight: usize, total: usize) {
    let remaining = app.to_hydrate.len() + in_flight;
    app.scanning = remaining > 0;
    if total == 0 {
        app.scanning = false;
        return;
    }
    if remaining > 0 {
        let done = total.saturating_sub(remaining).min(total);
        app.status = format!("scanning {done}/{total}…");
    }
}
