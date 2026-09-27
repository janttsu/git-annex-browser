/*!
Worker thread: owns the `App` and answers UI navigation. Everything slow runs on
helper threads and reports back as `Event`s, so navigation never waits on disk:

- discovery walks the scan root and fingerprints each repo,
- a cache reader loads cached metadata files,
- up to `jobs` loaders run `load_metadata` for new or changed repos,
- a writer thread saves loaded repos to the cache.
*/

use crate::annex::{AnnexMetadata, DiscoverOptions, now_unix};
use crate::app::{App, Command, ViewSnapshot};
use crate::cache::{Cache, Index};
use crate::scan::{self, Found};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
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

enum Event {
    Ui(WorkerMsg),
    Discovered(Vec<Found>),
    /// Metadata read from the cache (None: file missing or unreadable).
    Cached(PathBuf, Option<AnnexMetadata>),
    /// Metadata loaded from the repo on disk.
    Loaded(PathBuf, Result<AnnexMetadata>),
}

enum Persist {
    Store(Arc<AnnexMetadata>),
    MarkScan(Vec<PathBuf>),
}

pub struct WorkerConfig {
    pub scan_root: PathBuf,
    pub discover: DiscoverOptions,
    pub cache: Cache,
    pub jobs: usize,
    /// Show the cache only; never touch the repos.
    pub offline: bool,
    /// Reload every repo even when its fingerprint is unchanged.
    pub force_rescan: bool,
}

/// Sends UI messages to the worker.
#[derive(Clone)]
pub struct WorkerHandle {
    tx: Sender<Event>,
}

impl WorkerHandle {
    pub fn send(&self, msg: WorkerMsg) -> bool {
        self.tx.send(Event::Ui(msg)).is_ok()
    }
}

struct Worker {
    app: App,
    cfg: WorkerConfig,
    events: Sender<Event>,
    out: Sender<WorkerOut>,
    persist: Sender<Persist>,
    index: Index,
    /// Repos waiting for a disk load; popped from the end.
    queue: Vec<PathBuf>,
    in_flight: usize,
    scan_total: usize,
    /// Repos loaded from disk this session; later cache reads must not override them.
    fresh: HashSet<PathBuf>,
    /// Repos whose cache file has been read (or requested).
    cache_requested: HashSet<PathBuf>,
    discovering: bool,
    cached_loading: usize,
}

pub fn spawn(
    cfg: WorkerConfig,
    cancel: Arc<AtomicBool>,
) -> (WorkerHandle, Receiver<WorkerOut>, thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let (out_tx, out_rx) = mpsc::channel();
    let handle = WorkerHandle { tx: tx.clone() };
    let join = thread::spawn(move || {
        let (persist_tx, persist_rx) = mpsc::channel();
        let writer = spawn_writer(cfg.cache.clone(), cfg.scan_root.clone(), persist_rx);
        let mut w = Worker {
            app: App::new(cfg.scan_root.clone()),
            index: Index::default(),
            cfg,
            events: tx,
            out: out_tx,
            persist: persist_tx,
            queue: vec![],
            in_flight: 0,
            scan_total: 0,
            fresh: HashSet::new(),
            cache_requested: HashSet::new(),
            discovering: false,
            cached_loading: 0,
        };
        w.run(rx, &cancel);
        // Closing the persist channel lets the writer finish pending saves and exit.
        drop(w);
        let _ = writer.join();
    });
    (handle, out_rx, join)
}

fn spawn_writer(cache: Cache, scan_root: PathBuf, rx: Receiver<Persist>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for job in rx {
            let res = match job {
                Persist::Store(meta) => cache.store_repo(&meta),
                Persist::MarkScan(found) => cache.mark_scan(&scan_root, &found, false),
            };
            if let Err(e) = res {
                eprintln!("git-annex-browser: cache write failed: {e:#}");
            }
        }
    })
}

impl Worker {
    fn run(&mut self, rx: Receiver<Event>, cancel: &AtomicBool) {
        self.start_from_cache();
        if !self.cfg.offline {
            self.start_discovery();
        }
        self.update_progress();
        self.send_background();

        loop {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let ev = match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(ev) => ev,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            match ev {
                Event::Ui(WorkerMsg::Nav(Command::Quit, _)) => break,
                Event::Ui(WorkerMsg::Nav(cmd, page)) => self.on_nav(cmd, page),
                Event::Discovered(found) => self.on_discovered(found),
                Event::Cached(p, meta) => self.on_cached(p, meta),
                Event::Loaded(p, res) => self.on_loaded(p, res),
            }
            self.pump_loads();
        }
    }

    /// Root list from the index right away; full metadata files load in the background.
    fn start_from_cache(&mut self) {
        self.index = self.cfg.cache.load_index();
        let root = self.cfg.scan_root.clone();
        let mut summaries = vec![];
        let mut missing = HashMap::new();
        let mut paths = vec![];
        for (p, e) in self.index.under(&root) {
            summaries.push(e.summary.clone());
            if let Some(ts) = e.missing_since {
                missing.insert(p.clone(), ts);
            }
            paths.push(p);
        }
        self.app.status = if paths.is_empty() {
            "discovering annexes…".into()
        } else if self.cfg.offline {
            format!("{} repos from cache (offline)", paths.len())
        } else {
            format!("{} repos from cache — discovering…", paths.len())
        };
        self.app.set_cached(summaries, missing);
        self.read_cached(paths);
    }

    fn read_cached(&mut self, paths: Vec<PathBuf>) {
        let entries: Vec<_> = paths
            .into_iter()
            .filter(|p| self.cache_requested.insert(p.clone()))
            .filter_map(|p| self.index.get(&p).cloned().map(|e| (p, e)))
            .collect();
        if entries.is_empty() {
            return;
        }
        self.cached_loading += entries.len();
        let cache = self.cfg.cache.clone();
        let tx = self.events.clone();
        thread::spawn(move || {
            for (p, e) in entries {
                if tx.send(Event::Cached(p, cache.load_repo(&e))).is_err() {
                    break;
                }
            }
        });
    }

    fn start_discovery(&mut self) {
        self.discovering = true;
        let root = self.cfg.scan_root.clone();
        let opts = self.cfg.discover.clone();
        let tx = self.events.clone();
        thread::spawn(move || {
            let _ = tx.send(Event::Discovered(scan::discover(&root, &opts)));
        });
    }

    fn on_nav(&mut self, cmd: Command, page: usize) {
        if cmd == Command::Refresh {
            self.app.pop_to_root();
            if self.cfg.offline {
                self.app.status = "offline: showing cache only".into();
            } else {
                self.app.status = "refreshing…".into();
                self.cfg.force_rescan = true;
                self.start_discovery();
            }
        }
        if let Some(p) = self.app.execute(cmd, page) {
            self.request_load(p);
        }
        self.update_progress();
        let _ = self.out.send(WorkerOut::Nav(self.app.snapshot()));
    }

    /// The user opened a repo that is not loaded yet: fetch it before anything else.
    fn request_load(&mut self, p: PathBuf) {
        if !self.cache_requested.contains(&p)
            && let Some(e) = self.index.get(&p).cloned()
        {
            self.cache_requested.insert(p.clone());
            self.cached_loading += 1;
            let cache = self.cfg.cache.clone();
            let tx = self.events.clone();
            thread::spawn(move || {
                let meta = cache.load_repo(&e);
                let _ = tx.send(Event::Cached(p, meta));
            });
            return;
        }
        if self.cache_requested.contains(&p) && self.cached_loading > 0 {
            return; // the pending cache read will deliver it
        }
        self.request_uncached(p);
    }

    /// Load from disk with top priority, or give up when the repo cannot be reached.
    fn request_uncached(&mut self, p: PathBuf) {
        if self.cfg.offline || self.app.missing.contains_key(&p) {
            self.app.cancel_loading(
                &p,
                format!("{} is not cached and not reachable", p.display()),
            );
            return;
        }
        self.queue.retain(|q| q != &p);
        self.queue.push(p);
        self.scan_total = self.scan_total.max(self.queue.len() + self.in_flight);
    }

    fn on_discovered(&mut self, found: Vec<Found>) {
        self.discovering = false;
        let paths: Vec<PathBuf> = found.iter().map(|f| f.path.clone()).collect();
        self.app.set_discovered(&paths, now_unix());
        let _ = self.persist.send(Persist::MarkScan(paths.clone()));

        let mut todo = scan::needs_load(&found, &self.index, self.cfg.force_rescan);
        self.cfg.force_rescan = false;
        todo.retain(|p| !self.queue.contains(p));
        todo.reverse(); // the queue pops from the end
        self.queue.splice(0..0, todo);
        // A repo the user is waiting for goes first.
        if let Some(w) = self.app.awaiting_load()
            && let Some(pos) = self.queue.iter().position(|q| q == &w)
        {
            let p = self.queue.remove(pos);
            self.queue.push(p);
        }
        self.scan_total = self.queue.len() + self.in_flight;
        self.app.status = format!(
            "found {} annex repos ({} unchanged since last scan)",
            found.len(),
            found.len().saturating_sub(self.scan_total)
        );
        // Changed repos still show their cached data until the reload finishes.
        let cached: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| !self.fresh.contains(p))
            .collect();
        self.read_cached(cached);
        self.update_progress();
        self.send_background();
    }

    fn on_cached(&mut self, p: PathBuf, meta: Option<AnnexMetadata>) {
        self.cached_loading = self.cached_loading.saturating_sub(1);
        match meta {
            Some(meta) if !self.fresh.contains(&p) => {
                self.app.ingest_meta(meta);
            }
            Some(_) => {}
            None if self.app.awaiting_load().as_deref() == Some(p.as_path()) => {
                self.request_uncached(p);
            }
            None => {}
        }
        self.update_progress();
        self.send_background();
    }

    fn on_loaded(&mut self, p: PathBuf, res: Result<AnnexMetadata>) {
        self.in_flight = self.in_flight.saturating_sub(1);
        match res {
            Ok(meta) => {
                self.fresh.insert(p);
                let meta = self.app.ingest_meta(meta);
                let _ = self.persist.send(Persist::Store(meta));
            }
            Err(e) => {
                let msg = format!("failed {}: {e:#}", p.display());
                if self.app.awaiting_load().as_deref() == Some(p.as_path()) {
                    self.app.cancel_loading(&p, msg);
                } else {
                    self.app.status = msg;
                }
            }
        }
        self.update_progress();
        if self.queue.is_empty() && self.in_flight == 0 && self.scan_total > 0 {
            self.app.status = format!("{} repos • cache updated", self.app.summaries.len());
            self.scan_total = 0;
        }
        self.send_background();
    }

    fn pump_loads(&mut self) {
        let mut started = false;
        while self.in_flight < self.cfg.jobs.max(1) {
            let Some(p) = self.queue.pop() else {
                break;
            };
            self.in_flight += 1;
            started = true;
            let tx = self.events.clone();
            thread::spawn(move || {
                let res = crate::annex::load_metadata(&p);
                let _ = tx.send(Event::Loaded(p, res));
            });
        }
        if started {
            self.update_progress();
        }
    }

    fn update_progress(&mut self) {
        let remaining = self.queue.len() + self.in_flight;
        self.app.scanning = self.discovering || remaining > 0 || self.cached_loading > 0;
        if remaining > 0 && self.scan_total > 0 {
            let done = self.scan_total.saturating_sub(remaining);
            self.app.status = format!("scanning {done}/{}…", self.scan_total);
        }
    }

    fn send_background(&self) {
        let _ = self.out.send(WorkerOut::Background(self.app.snapshot()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn config(root: &std::path::Path, cache: &std::path::Path, offline: bool) -> WorkerConfig {
        WorkerConfig {
            scan_root: root.to_path_buf(),
            discover: DiscoverOptions::default(),
            cache: Cache::at(cache.to_path_buf()),
            jobs: 2,
            offline,
            force_rescan: false,
        }
    }

    /// Wait for a background snapshot matching `done`, failing after 60 s.
    fn wait_for(rx: &Receiver<WorkerOut>, done: impl Fn(&ViewSnapshot) -> bool) -> ViewSnapshot {
        let start = Instant::now();
        loop {
            let left = Duration::from_secs(60).saturating_sub(start.elapsed());
            match rx.recv_timeout(left) {
                Ok(WorkerOut::Background(s) | WorkerOut::Nav(s)) if done(&s) => return s,
                Ok(_) => {}
                Err(e) => panic!("worker did not finish: {e}"),
            }
        }
    }

    #[test]
    fn scans_real_annex_caches_it_and_reopens_offline() {
        let tmp = tempfile::tempdir().unwrap();
        let repos = tmp.path().join("repos");
        let Some(root) = crate::testutil::real_annex(&repos, "photos") else {
            return;
        };
        let cache_dir = tmp.path().join("cache");

        let cancel = Arc::new(AtomicBool::new(false));
        let (handle, rx, join) = spawn(config(&repos, &cache_dir, false), Arc::clone(&cancel));
        let snap = wait_for(&rx, |s| !s.scanning && s.status.contains("cache updated"));
        assert_eq!(snap.total_repos, 1);
        let row = snap
            .list
            .iter()
            .find(|r| r.label.starts_with("photos"))
            .unwrap();
        assert!(row.label.contains("3 files"), "{}", row.label);

        // Descend into the repo: the reply is the loaded repo menu.
        let idx = snap
            .list
            .iter()
            .position(|r| r.label.starts_with("photos"))
            .unwrap();
        assert!(handle.send(WorkerMsg::Nav(Command::Select(idx), 10)));
        assert!(handle.send(WorkerMsg::Nav(Command::Descend, 10)));
        let menu = wait_for(&rx, |s| s.crumb.len() == 2);
        assert!(menu.list.iter().any(|r| r.label.starts_with("disk usage")));

        handle.send(WorkerMsg::Nav(Command::Quit, 0));
        join.join().unwrap();
        let idx = Cache::at(cache_dir.clone()).load_index();
        assert!(idx.get(&root).is_some(), "repo was written to the cache");

        // Offline start: repo comes from the cache alone.
        let (handle, rx, join) = spawn(config(&repos, &cache_dir, true), cancel);
        let snap = wait_for(&rx, |s| !s.scanning);
        assert_eq!(snap.total_repos, 1);
        assert!(snap.status.contains("offline"), "{}", snap.status);
        handle.send(WorkerMsg::Nav(Command::Quit, 0));
        join.join().unwrap();
        crate::testutil::make_writable(tmp.path());
    }
}
