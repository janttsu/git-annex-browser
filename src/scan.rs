//! Shared discovery and loading for `--scan`, `--dump` and the TUI worker.

use crate::annex::{self, AnnexMetadata, DiscoverOptions, RepoFingerprint};
use crate::cache::Index;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Default number of repos loaded at once: git-annex is mostly I/O bound.
pub fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, 4)
}

/// A discovered repo and its current fingerprint (None if it could not be read).
#[derive(Debug, Clone)]
pub struct Found {
    pub path: PathBuf,
    pub fingerprint: Option<RepoFingerprint>,
}

pub fn discover(root: &Path, opts: &DiscoverOptions) -> Vec<Found> {
    annex::find_annex_repos(root, opts)
        .into_iter()
        .map(|path| Found {
            fingerprint: annex::repo_fingerprint(&path),
            path,
        })
        .collect()
}

/// Whether the cached load of `found` is still current.
pub fn is_up_to_date(found: &Found, index: &Index) -> bool {
    match (&found.fingerprint, index.get(&found.path)) {
        (Some(fp), Some(entry)) => entry.fingerprint.as_ref() == Some(fp),
        _ => false,
    }
}

/// Repos that need loading from disk, newest git-annex branch first so recent
/// `copy --to` results show up early. With `force` every repo is reloaded.
pub fn needs_load(found: &[Found], index: &Index, force: bool) -> Vec<PathBuf> {
    let mut out: Vec<(bool, i64, PathBuf)> = found
        .iter()
        .filter(|f| force || !is_up_to_date(f, index))
        .map(|f| {
            let cached = index.get(&f.path).is_some();
            let mtime = annex::annex_branch_mtime(&f.path).unwrap_or(0);
            (cached, mtime, f.path.clone())
        })
        .collect();
    // Uncached repos first, then most recently changed.
    out.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    out.into_iter().map(|(_, _, p)| p).collect()
}

/// Load `paths` on up to `jobs` threads. `on_done` runs on the loading thread
/// (serialised by a mutex) as each repo finishes, with its 1-based completion index.
pub fn load_parallel<F>(paths: &[PathBuf], jobs: usize, on_done: F)
where
    F: FnMut(usize, &Path, Result<AnnexMetadata>) + Send,
{
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let on_done = Mutex::new(on_done);
    std::thread::scope(|s| {
        for _ in 0..jobs.clamp(1, paths.len().max(1)) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(p) = paths.get(i) else {
                        break;
                    };
                    let res = annex::load_metadata(p);
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    let mut cb = on_done.lock().unwrap_or_else(|e| e.into_inner());
                    cb(n, p, res);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Cache;

    #[test]
    fn unchanged_repo_is_skipped_until_it_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let Some(root) = crate::testutil::real_annex(tmp.path(), "skipme") else {
            return;
        };
        let cache = Cache::at(tmp.path().join("cache"));
        let found = discover(tmp.path(), &DiscoverOptions::default());
        assert_eq!(found.len(), 1);
        assert_eq!(
            needs_load(&found, &cache.load_index(), false),
            vec![root.clone()]
        );

        let mut loaded = vec![];
        load_parallel(std::slice::from_ref(&root), 2, |_, _, res| {
            loaded.push(res.unwrap())
        });
        cache.store_repo(&loaded[0]).unwrap();
        let found = discover(tmp.path(), &DiscoverOptions::default());
        assert!(needs_load(&found, &cache.load_index(), false).is_empty());
        assert_eq!(needs_load(&found, &cache.load_index(), true).len(), 1);

        // Any git-annex branch change (here: a new description) invalidates the cache.
        crate::testutil::git(&root, &["annex", "describe", "here", "renamed"]);
        let found = discover(tmp.path(), &DiscoverOptions::default());
        assert_eq!(
            needs_load(&found, &cache.load_index(), false),
            vec![root.clone()]
        );
        crate::testutil::make_writable(tmp.path());
    }

    #[test]
    fn load_parallel_reports_every_repo_once() {
        let paths: Vec<PathBuf> = (0..5)
            .map(|i| PathBuf::from(format!("/nonexistent/gab-{i}")))
            .collect();
        let mut seen = vec![];
        load_parallel(&paths, 3, |n, p, res| {
            assert!(res.is_err());
            seen.push((n, p.to_path_buf()));
        });
        seen.sort();
        assert_eq!(seen.len(), 5);
        assert_eq!(seen.last().unwrap().0, 5);
    }
}
