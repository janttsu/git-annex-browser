/*!
Worker thread pattern copied from zfs-browser: UI talks via mpsc, worker owns the App + heavy data.
Slow loads happen here.
*/

use crate::app::{App, Command, ViewSnapshot};
use crate::annex::{self, RepoSummary};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub enum WorkerMsg {
    Nav(Command, usize /*page size*/),
}

pub struct Worker {
    pub app: App,
}

pub fn spawn(
    scan_root: PathBuf,
    cancel: Arc<AtomicBool>,
) -> (Sender<WorkerMsg>, Receiver<ViewSnapshot>) {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (snap_tx, snap_rx) = mpsc::channel();
    let (meta_tx, meta_rx) = mpsc::channel::<(PathBuf, Result<annex::AnnexMetadata>)>();

    thread::spawn(move || {
        // meta_rx moved into this worker thread for draining results
        let mut meta_rx = meta_rx;
        let mut worker = Worker { app: App::new(scan_root.clone()) };

        // 1. Try cache first for instant UI
        if let Some(cache) = annex::load_cache() {
            for (pstr, meta) in cache.repos {
                let p = PathBuf::from(pstr);
                worker.app.preloaded.insert(p.clone(), meta.clone());
                let mut sum = meta.to_summary();
                sum.ensure_name();
                worker.app.summaries.push(sum);
            }
            worker.app.status = format!("loaded {} from cache — scanning…", worker.app.preloaded.len());
            worker.app.recompute_drive_profiles();
        }

        // 2. On-disk discovery (always)
        let discovered = annex::find_annex_repos(&scan_root);

        // Seed any missing summaries from discovery (for repos not in cache)
        for p in &discovered {
            if !worker.app.summaries.iter().any(|s| &s.root == p) {
                let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                let mut sum = RepoSummary {
                    root: p.clone(),
                    uuid: String::new(),
                    name,
                    annex_description: String::new(),
                    file_count: 0,
                    remote_count: 0,
                    here_present_count: 0,
                    here_available_space: None,
                };
                sum.ensure_name();
                worker.app.summaries.push(sum);
            }
            // Always (re)hydrate everything in background on open
            if !worker.app.preloaded.contains_key(p) {
                worker.app.to_hydrate.push(p.clone());
            }
        }

        // Only re-hydrate things not already in cache for faster startup.
        // Explicit refresh will re-scan.
        for p in &discovered {
            if !worker.app.preloaded.contains_key(p) && !worker.app.to_hydrate.iter().any(|x| x == p) {
                worker.app.to_hydrate.push(p.clone());
            }
        }

        worker.app.set_discovered(discovered.clone());
        let _ = snap_tx.send(worker.app.snapshot(20));

        // 3. Background hydration loop + cache updates
        // We interleave with the normal command loop using the timeout path.
        let mut dirty = false;
        let mut last_save = std::time::Instant::now();

        loop {
            if cancel.load(Ordering::Relaxed) {
                break;
            }

            // Drain any completed metadata loads (from background threads). This keeps the worker responsive.
            while let Ok((p, res)) = meta_rx.try_recv() {
                match res {
                    Ok(meta) => {
                        let mut sum = meta.to_summary();
                        sum.ensure_name();
                        worker.app.preloaded.insert(p.clone(), meta.clone());
                        if let Some(existing) = worker.app.summaries.iter_mut().find(|s| s.root == p) {
                            *existing = sum;
                        } else {
                            worker.app.summaries.push(sum);
                        }
                        worker.app.refresh_root_view();
                        dirty = true;
                        let _ = snap_tx.send(worker.app.snapshot(20));
                        if last_save.elapsed() > Duration::from_secs(4) {
                            let cache = annex::AnnexCache {
                                version: 1,
                                updated: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64,
                                repos: worker.app.preloaded.iter()
                                    .map(|(k, v)| (k.to_string_lossy().to_string(), v.clone()))
                                    .collect(),
                            };
                            let _ = annex::save_cache(&cache);
                            last_save = std::time::Instant::now();
                            dirty = false;
                        }
                    }
                    Err(_e) => {}
                }
            }

            let msg = match cmd_rx.recv_timeout(Duration::from_millis(180)) {
                Ok(m) => m,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // --- Background work on idle ticks ---
                    // Spawn heavy loads off-thread so UI commands stay responsive.
                    if let Some(p) = worker.app.to_hydrate.pop() {
                        let meta_tx = meta_tx.clone();
                        std::thread::spawn(move || {
                            let res = annex::load_metadata(&p);
                            let _ = meta_tx.send((p, res));
                        });
                    } else if dirty && last_save.elapsed() > Duration::from_secs(2) {
                        let cache = annex::AnnexCache {
                            version: 1,
                            updated: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64,
                            repos: worker.app.preloaded.iter()
                                .map(|(k, v)| (k.to_string_lossy().to_string(), v.clone()))
                                .collect(),
                        };
                        let _ = annex::save_cache(&cache);
                        last_save = std::time::Instant::now();
                        dirty = false;
                        worker.app.status = format!("{} repos • cache updated", worker.app.preloaded.len());
                        let _ = snap_tx.send(worker.app.snapshot(20));
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
                        worker.app.to_hydrate = repos.clone();
                        worker.app.set_discovered(repos);
                        worker.app.recompute_drive_profiles();
                        while worker.app.stack.len() > 1 {
                            worker.app.stack.pop();
                        }
                        dirty = true;
                    }

                    // On-demand load for a loading placeholder (rare now thanks to bg)
                    if let Some(loading) = worker.app.stack.last() {
                        if let Some(p) = loading.node.loading_path() {
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
                    }

                    let snap = worker.app.snapshot(page);
                    let _ = snap_tx.send(snap);
                }
            }
        }

        // best effort final save on exit
        if !worker.app.preloaded.is_empty() {
            let cache = annex::AnnexCache {
                version: 1,
                updated: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64,
                repos: worker.app.preloaded.iter()
                    .map(|(k, v)| (k.to_string_lossy().to_string(), v.clone()))
                    .collect(),
            };
            let _ = annex::save_cache(&cache);
        }
    });

    (cmd_tx, snap_rx)
}


