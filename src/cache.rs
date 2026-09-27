//! On-disk cache: one metadata file per repo plus a small index.
//!
//! Layout under the cache directory:
//!
//! ```text
//! v2/index.json          summaries, fingerprints, missing-since markers
//! v2/repos/<hash>.json   full AnnexMetadata for one repo
//! v2/lock                advisory lock for index updates
//! ```
//!
//! The root list is drawn from `index.json` alone, so start-up does not parse
//! every repo's location data. Saving one repo rewrites only that repo's file
//! and the index. The v1 single-file `cache.json` is imported once and left in place.

use crate::annex::{
    AnnexMetadata, RepoFingerprint, RepoSummary, is_secret_remote_key, now_unix, path_is_under,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const CACHE_VERSION: u32 = 2;

/// Index row for one repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    pub summary: RepoSummary,
    /// File name under `repos/`.
    pub file: String,
    #[serde(default)]
    pub fingerprint: Option<RepoFingerprint>,
    /// Unix time of the load this entry came from.
    pub scanned_at: i64,
    /// Set when a scan of a parent directory did not find the repo (e.g. drive unplugged).
    #[serde(default)]
    pub missing_since: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    pub version: u32,
    pub updated: i64,
    /// Repo path -> entry.
    pub repos: BTreeMap<String, IndexEntry>,
}

impl Index {
    pub fn get(&self, path: &Path) -> Option<&IndexEntry> {
        self.repos.get(&path_key(path))
    }

    /// Entries whose repo path is under `root`.
    pub fn under<'a>(&'a self, root: &'a Path) -> impl Iterator<Item = (PathBuf, &'a IndexEntry)> {
        self.repos
            .iter()
            .map(|(k, e)| (PathBuf::from(k), e))
            .filter(move |(p, _)| path_is_under(p, root))
    }
}

/// The v1 single-file cache, read only for migration.
#[derive(Deserialize)]
struct LegacyCache {
    #[serde(default)]
    repos: HashMap<String, AnnexMetadata>,
}

#[derive(Debug, Clone)]
pub struct Cache {
    /// Base directory (contains `v2/`, maybe a legacy `cache.json`).
    base: PathBuf,
    /// Legacy single-file cache to import when no index exists yet.
    legacy: PathBuf,
}

pub fn path_key(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// FNV-1a: stable across Rust versions, unlike `DefaultHasher`.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn repo_file_name(path: &Path) -> String {
    format!("{:016x}.json", fnv1a64(path_key(path).as_bytes()))
}

fn default_base() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("git-annex-browser");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".cache").join("git-annex-browser")
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` atomically with mode 0600: temp file, fsync, rename.
fn write_private(
    path: &Path,
    write: impl FnOnce(&mut BufWriter<&File>) -> Result<()>,
) -> Result<()> {
    let dir = path.parent().context("cache path has no parent")?;
    create_private_dir(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("cache"),
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    let result = (|| {
        let mut w = BufWriter::new(&file);
        write(&mut w)?;
        w.flush()?;
        drop(w);
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn create_private_dir(dir: &Path) -> Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

/// Secrets are redacted when remote.log is parsed; this guards metadata from any other source.
fn needs_redaction(meta: &AnnexMetadata) -> bool {
    meta.remotes.values().any(|r| {
        r.config
            .iter()
            .any(|(k, v)| is_secret_remote_key(k) && v != "[redacted]")
    })
}

fn redacted(meta: &AnnexMetadata) -> AnnexMetadata {
    let mut m = meta.clone();
    for r in m.remotes.values_mut() {
        for (k, v) in r.config.iter_mut() {
            if is_secret_remote_key(k) {
                *v = "[redacted]".to_string();
            }
        }
    }
    m
}

struct LockGuard(#[allow(dead_code)] File);

impl Cache {
    /// Cache at `location`. A path ending in `.json` is read as the old
    /// single-file setting: its directory becomes the base and the file is imported.
    pub fn at(location: PathBuf) -> Self {
        if location.extension().is_some_and(|e| e == "json") {
            let base = location
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            return Self {
                base,
                legacy: location,
            };
        }
        Self {
            legacy: location.join("cache.json"),
            base: location,
        }
    }

    /// `--cache`, else `$GIT_ANNEX_BROWSER_CACHE`, else the XDG cache directory.
    pub fn from_env(explicit: Option<PathBuf>) -> Self {
        if let Some(p) = explicit {
            return Self::at(p);
        }
        if let Ok(p) = std::env::var("GIT_ANNEX_BROWSER_CACHE")
            && !p.is_empty()
        {
            return Self::at(PathBuf::from(p));
        }
        Self::at(default_base())
    }

    pub fn dir(&self) -> PathBuf {
        self.base.join("v2")
    }

    fn index_path(&self) -> PathBuf {
        self.dir().join("index.json")
    }

    fn repo_path(&self, file: &str) -> PathBuf {
        self.dir().join("repos").join(file)
    }

    fn lock(&self) -> Result<LockGuard> {
        create_private_dir(&self.dir())?;
        let mut opts = OpenOptions::new();
        opts.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(self.dir().join("lock"))?;
        file.lock().context("locking cache")?;
        Ok(LockGuard(file))
    }

    fn read_index(&self) -> Option<Index> {
        let f = File::open(self.index_path()).ok()?;
        let idx: Index = serde_json::from_reader(BufReader::new(f)).ok()?;
        (idx.version == CACHE_VERSION).then_some(idx)
    }

    fn write_index(&self, idx: &mut Index) -> Result<()> {
        idx.version = CACHE_VERSION;
        idx.updated = now_unix();
        write_private(&self.index_path(), |w| Ok(serde_json::to_writer(w, idx)?))
    }

    /// The index, importing a v1 `cache.json` the first time.
    pub fn load_index(&self) -> Index {
        if let Some(idx) = self.read_index() {
            return idx;
        }
        if self.legacy.exists()
            && let Err(e) = self.import_legacy()
        {
            eprintln!(
                "git-annex-browser: could not import {}: {e:#}",
                self.legacy.display()
            );
        }
        self.read_index().unwrap_or_default()
    }

    fn import_legacy(&self) -> Result<()> {
        let f = File::open(&self.legacy)?;
        let legacy: LegacyCache = serde_json::from_reader(BufReader::new(f))
            .with_context(|| format!("parsing {}", self.legacy.display()))?;
        let _lock = self.lock()?;
        if self.read_index().is_some() {
            return Ok(()); // another process imported it meanwhile
        }
        let mut idx = Index::default();
        for (_, mut meta) in legacy.repos {
            meta.ensure_sizes();
            let entry = self.write_repo_file(&meta)?;
            idx.repos.insert(path_key(&meta.root), entry);
        }
        self.write_index(&mut idx)
    }

    fn write_repo_file(&self, meta: &AnnexMetadata) -> Result<IndexEntry> {
        let file = repo_file_name(&meta.root);
        let path = self.repo_path(&file);
        if needs_redaction(meta) {
            let clean = redacted(meta);
            write_private(&path, |w| Ok(serde_json::to_writer(w, &clean)?))?;
        } else {
            write_private(&path, |w| Ok(serde_json::to_writer(w, meta)?))?;
        }
        let mut summary = meta.to_summary();
        summary.ensure_name();
        Ok(IndexEntry {
            summary,
            file,
            fingerprint: meta.fingerprint.clone(),
            scanned_at: now_unix(),
            missing_since: None,
        })
    }

    /// Full metadata for an index entry.
    pub fn load_repo(&self, entry: &IndexEntry) -> Option<AnnexMetadata> {
        let f = File::open(self.repo_path(&entry.file)).ok()?;
        let mut meta: AnnexMetadata = serde_json::from_reader(BufReader::new(f)).ok()?;
        meta.ensure_sizes();
        Some(meta)
    }

    /// Save one freshly loaded repo and its index row. Other repos are untouched.
    pub fn store_repo(&self, meta: &AnnexMetadata) -> Result<()> {
        let entry = self.write_repo_file(meta)?;
        let _lock = self.lock()?;
        let mut idx = self.read_index().unwrap_or_default();
        idx.repos.insert(path_key(&meta.root), entry);
        self.write_index(&mut idx)
    }

    /// Record the result of discovering repos under `scan_root`.
    ///
    /// Cached repos under the root that were not found are marked missing (kept, so
    /// unplugged drives stay browsable). With `prune` they are deleted instead.
    pub fn mark_scan(&self, scan_root: &Path, found: &[PathBuf], prune: bool) -> Result<()> {
        let _lock = self.lock()?;
        let mut idx = self.read_index().unwrap_or_default();
        let found: std::collections::HashSet<String> = found.iter().map(|p| path_key(p)).collect();
        let now = now_unix();
        let mut removed = Vec::new();
        idx.repos.retain(|k, e| {
            if !path_is_under(Path::new(k), scan_root) {
                return true;
            }
            if found.contains(k) {
                e.missing_since = None;
                return true;
            }
            if prune {
                removed.push(e.file.clone());
                return false;
            }
            e.missing_since.get_or_insert(now);
            true
        });
        self.write_index(&mut idx)?;
        for file in removed {
            let _ = fs::remove_file(self.repo_path(&file));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annex::Remote;

    fn meta(root: &str) -> AnnexMetadata {
        let mut m = crate::testutil::meta(root);
        m.fingerprint = Some(RepoFingerprint {
            annex_branch: "b".into(),
            head: "h".into(),
            config_mtime: 1,
        });
        m
    }

    #[test]
    fn store_and_load_round_trip_with_private_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().to_path_buf());
        cache.store_repo(&meta("/data/a")).unwrap();
        let idx = cache.load_index();
        let entry = idx.get(Path::new("/data/a")).unwrap();
        assert_eq!(entry.summary.name, "a");
        assert_eq!(entry.fingerprint.as_ref().unwrap().head, "h");
        let back = cache.load_repo(entry).unwrap();
        assert_eq!(back.root, PathBuf::from("/data/a"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(cache.repo_path(&entry.file))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            let mode = fs::metadata(cache.index_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn mark_scan_keeps_missing_repos_and_prune_removes_them() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().to_path_buf());
        for p in ["/data/media/a", "/data/media/gone", "/data/backup/b"] {
            cache.store_repo(&meta(p)).unwrap();
        }
        cache
            .mark_scan(
                Path::new("/data/media"),
                &[PathBuf::from("/data/media/a")],
                false,
            )
            .unwrap();
        let idx = cache.load_index();
        assert!(
            idx.get(Path::new("/data/media/a"))
                .unwrap()
                .missing_since
                .is_none()
        );
        assert!(
            idx.get(Path::new("/data/media/gone"))
                .unwrap()
                .missing_since
                .is_some()
        );
        assert!(
            idx.get(Path::new("/data/backup/b"))
                .unwrap()
                .missing_since
                .is_none()
        );

        let gone_file = idx.get(Path::new("/data/media/gone")).unwrap().file.clone();
        cache
            .mark_scan(
                Path::new("/data/media"),
                &[PathBuf::from("/data/media/a")],
                true,
            )
            .unwrap();
        let idx = cache.load_index();
        assert!(idx.get(Path::new("/data/media/gone")).is_none());
        assert!(idx.get(Path::new("/data/backup/b")).is_some());
        assert!(!cache.repo_path(&gone_file).exists());
    }

    #[test]
    fn imports_v1_cache_json_and_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("cache.json");
        let mut m = meta("/data/old");
        m.fingerprint = None;
        let v1 = serde_json::json!({
            "version": 1,
            "updated": 5,
            "repos": { "/data/old": m },
        });
        fs::write(&legacy, serde_json::to_vec(&v1).unwrap()).unwrap();
        let cache = Cache::at(legacy.clone());
        let idx = cache.load_index();
        assert!(idx.get(Path::new("/data/old")).is_some());
        assert!(legacy.exists(), "legacy cache must not be deleted");
        assert!(cache.index_path().exists());
    }

    #[test]
    fn secrets_are_redacted_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path().to_path_buf());
        let mut m = meta("/data/s");
        let mut r = Remote::new(
            "u2".into(),
            "s3".into(),
            HashMap::new(),
            crate::annex::TrustLevel::SemiTrusted,
            None,
        );
        r.config.insert("cipher".into(), "TOPSECRET".into());
        m.remotes.insert("u2".into(), r);
        cache.store_repo(&m).unwrap();
        let entry = cache
            .load_index()
            .get(Path::new("/data/s"))
            .unwrap()
            .clone();
        let text = fs::read_to_string(cache.repo_path(&entry.file)).unwrap();
        assert!(!text.contains("TOPSECRET"));
    }

    #[test]
    fn repo_file_names_are_stable() {
        assert_eq!(
            repo_file_name(Path::new("/mnt/hdd/root/movies-annex")),
            repo_file_name(Path::new("/mnt/hdd/root/movies-annex"))
        );
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
