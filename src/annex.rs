//! git-annex metadata parsing and data structures.
//! Pure access via `git` CLI (no scraping of user-facing commands where possible).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    Trusted,
    SemiTrusted,
    UnTrusted,
    Dead,
}

impl TrustLevel {
    /// Parse a trust token from `trust.log` or a UI shorthand.
    ///
    /// git-annex `trust.log` uses `1` (trusted), `0` (untrusted), `?` (semitrusted),
    /// `X` (dead). Letter / word forms are accepted for robustness.
    pub fn from_token(s: &str) -> Self {
        match s.trim() {
            "1" | "T" | "t" | "trusted" => TrustLevel::Trusted,
            "0" | "U" | "u" | "untrusted" => TrustLevel::UnTrusted,
            "X" | "x" | "D" | "d" | "dead" => TrustLevel::Dead,
            _ => TrustLevel::SemiTrusted,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TrustLevel::Trusted => "trusted",
            TrustLevel::SemiTrusted => "semitrusted",
            TrustLevel::UnTrusted => "untrusted",
            TrustLevel::Dead => "dead",
        }
    }
    pub fn short(&self) -> char {
        match self {
            TrustLevel::Trusted => 'T',
            TrustLevel::SemiTrusted => '?',
            TrustLevel::UnTrusted => 'U',
            TrustLevel::Dead => 'D',
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remote {
    pub uuid: String,
    pub description: String,
    /// From remote.log: type, name, encryption, directory, etc.
    pub config: HashMap<String, String>,
    pub trust: TrustLevel,
    pub last_fsck: Option<i64>, // unix timestamp
    /// Number of keys present according to location logs (computed)
    pub present_count: usize,
    /// Sum of those keys' sizes (from key names when available)
    #[serde(default)]
    pub present_size: u64,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub wanted: Option<String>,
    #[serde(default)]
    pub required: Option<String>,
}

impl Remote {
    pub fn new(
        uuid: String,
        description: String,
        config: HashMap<String, String>,
        trust: TrustLevel,
        last_fsck: Option<i64>,
    ) -> Self {
        Self {
            uuid,
            description,
            config,
            trust,
            last_fsck,
            present_count: 0,
            present_size: 0,
            groups: vec![],
            wanted: None,
            required: None,
        }
    }

    pub fn name(&self) -> &str {
        self.config
            .get("name")
            .map(|s| s.as_str())
            .unwrap_or(&self.description)
    }
    pub fn rtype(&self) -> &str {
        self.config
            .get("type")
            .map(|s| s.as_str())
            .unwrap_or("repo")
    }
    pub fn is_special(&self) -> bool {
        self.config.contains_key("type")
    }

    /// Cloud/external special remotes (rclone, s3, …). Other annex clones and
    /// `directory` remotes are local storage, not listed in "storage per remote".
    pub fn is_transfer_remote(&self) -> bool {
        match self.config.get("type").map(|s| s.as_str()) {
            None | Some("directory") => false,
            Some(_) => true,
        }
    }

    /// Human type for transfer remotes: `rclone` if externaltype is set, else `type`.
    pub fn transfer_kind(&self) -> Option<String> {
        if !self.is_transfer_remote() {
            return None;
        }
        let t = self.config.get("type")?;
        if t == "external"
            && let Some(ext) = self.config.get("externaltype")
            && !ext.is_empty()
        {
            return Some(ext.clone());
        }
        Some(t.clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnexedFile {
    pub path: String,
    pub key: String,
    /// Extracted size from key if E-style (e.g. SHA256E-s12345-...)
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnexMetadata {
    pub root: PathBuf,
    pub uuid: String,
    pub description: String,
    /// Default numcopies for this repo
    #[serde(default)]
    pub numcopies: Option<u32>,
    /// Additional config lines (e.g. from .gitattributes or annex.* config)
    #[serde(default)]
    pub additional_configs: Vec<String>,
    /// All known UUIDs -> Remote (merged from uuid.log + remote.log + trusts)
    pub remotes: HashMap<String, Remote>,
    /// key -> set of UUIDs that currently have the content (latest record wins)
    pub locations: HashMap<String, HashSet<String>>,
    /// Working tree annexed files
    pub files: Vec<AnnexedFile>,
    /// total keys known (from location logs)
    pub total_keys: usize,
    /// Sum of sizes of all unique keys (deduplicated size)
    #[serde(default)]
    pub unique_size: u64,
    /// Total storage consumed across all drives (size × number of copies)
    #[serde(default)]
    pub consumed_size: u64,
    /// Refs this load was made from; used to skip unchanged repos.
    #[serde(default)]
    pub fingerprint: Option<RepoFingerprint>,
}

/// Lightweight summary for fast top-level listing and caching.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoSummary {
    pub root: PathBuf,
    pub uuid: String,
    /// Clean name for display, preferably the directory basename (e.g. "my-repo")
    #[serde(default)]
    pub name: String,
    /// The annex internal description (often "orca" or similar on your machines)
    #[serde(default)]
    pub annex_description: String,
    pub file_count: usize,
    pub remote_count: usize,
    pub here_present_count: usize,
    /// Sum of sizes of all unique keys (1 copy each)
    #[serde(default)]
    pub unique_size: u64,
    /// Total space used across all drives (with duplicates counted per copy)
    #[serde(default)]
    pub consumed_size: u64,
    /// Per-remote occupancy, for the global report (name is the grouping key).
    #[serde(default)]
    pub remote_usage: Vec<RemoteUsage>,
    /// Desired copies (`annex.numcopies`), default 1 when unset.
    #[serde(default)]
    pub numcopies: Option<u32>,
    /// Keys with location records (for copy-health).
    #[serde(default)]
    pub keys_tracked: usize,
    /// Keys with fewer copies than numcopies.
    #[serde(default)]
    pub keys_under: usize,
    /// Keys with exactly numcopies copies.
    #[serde(default)]
    pub keys_ok: usize,
    /// Keys with more copies than numcopies.
    #[serde(default)]
    pub keys_over: usize,
}

/// One remote's stored keys/bytes inside a single annex.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RemoteUsage {
    pub name: String,
    #[serde(default)]
    pub uuid: String,
    #[serde(default)]
    pub present_count: usize,
    #[serde(default)]
    pub present_size: u64,
    /// Special-remote type (`rclone`, `s3`, `external`, …). None for other annex clones.
    #[serde(default)]
    pub special_type: Option<String>,
}

impl RemoteUsage {
    pub fn is_transfer_remote(&self) -> bool {
        match self.special_type.as_deref() {
            None | Some("directory") => false,
            Some(_) => true,
        }
    }
}

impl AnnexMetadata {
    pub fn to_summary(&self) -> RepoSummary {
        let here_present = self
            .remotes
            .get(&self.uuid)
            .map(|r| r.present_count)
            .unwrap_or(0);
        let name = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.description.clone());
        let want = self.numcopies.unwrap_or(1).max(1);
        let (keys_under, keys_ok, keys_over) =
            copy_health_counts(&self.locations, want, &self.remotes);
        RepoSummary {
            root: self.root.clone(),
            uuid: self.uuid.clone(),
            name,
            annex_description: self.description.clone(),
            file_count: self.files.len(),
            remote_count: self.remotes.len(),
            here_present_count: here_present,
            unique_size: self.unique_size,
            consumed_size: self.consumed_size,
            remote_usage: self
                .remotes
                .values()
                .map(|r| RemoteUsage {
                    name: r.name().to_string(),
                    uuid: r.uuid.clone(),
                    present_count: r.present_count,
                    present_size: r.present_size,
                    special_type: r.transfer_kind(),
                })
                .collect(),
            numcopies: self.numcopies,
            keys_tracked: self.locations.len(),
            keys_under,
            keys_ok,
            keys_over,
        }
    }

    /// Drop remotes marked `git annex dead` (except this clone) and recount.
    /// Location logs often still list keys on retired machines; those rows
    /// inflate drive lists and consumed-size totals.
    pub fn omit_dead_remotes(&mut self) {
        if drop_dead_remotes(&self.uuid, &mut self.remotes, &mut self.locations) {
            self.recount_presence();
        }
    }

    fn recount_presence(&mut self) {
        let key_sizes = collect_key_sizes(&self.files, &self.locations);
        apply_present_stats(&mut self.remotes, &self.locations, &key_sizes);
        let (unique_size, consumed_size) = size_totals(&self.locations, &key_sizes);
        self.unique_size = unique_size;
        self.consumed_size = consumed_size;
        self.total_keys = self.locations.len().max(self.files.len());
    }

    /// Ensure size stats are populated (for old caches that didn't have them)
    pub fn ensure_sizes(&mut self) {
        self.omit_dead_remotes();
        let need_totals =
            self.unique_size == 0 && self.consumed_size == 0 && !self.locations.is_empty();
        let need_remote = self
            .remotes
            .values()
            .any(|r| r.present_count > 0 && r.present_size == 0);
        if !need_totals && !need_remote {
            return;
        }
        let key_sizes = collect_key_sizes(&self.files, &self.locations);
        if need_totals {
            let (u, c) = size_totals(&self.locations, &key_sizes);
            self.unique_size = u;
            self.consumed_size = c;
        }
        if need_remote {
            apply_present_stats(&mut self.remotes, &self.locations, &key_sizes);
        }
    }
}

/// Aggregate occupancy by remote name across many repos (same drive in several annexes).
/// Returns (name, bytes, keys, repo_count) sorted by bytes descending.
pub fn aggregate_remote_usage(summaries: &[RepoSummary]) -> Vec<(String, u64, usize, usize)> {
    let mut by_name: HashMap<String, (u64, usize, usize)> = HashMap::new();
    for s in summaries {
        for u in &s.remote_usage {
            if !u.is_transfer_remote() {
                continue;
            }
            if u.present_count == 0 && u.present_size == 0 {
                continue;
            }
            let label = match u.special_type.as_deref() {
                Some(kind) => format!("{} ({kind})", u.name),
                None => u.name.clone(),
            };
            let e = by_name.entry(label).or_insert((0, 0, 0));
            e.0 += u.present_size;
            e.1 += u.present_count;
            e.2 += 1;
        }
    }
    let mut rows: Vec<_> = by_name
        .into_iter()
        .map(|(name, (bytes, keys, repos))| (name, bytes, keys, repos))
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows
}

/// Count keys below / at / above the desired number of copies.
/// Untrusted and dead remotes are omitted — git-annex `numcopies` does the same
/// (Glacier etc. store bytes but do not satisfy the copy requirement).
pub fn copy_health_counts(
    locations: &HashMap<String, HashSet<String>>,
    numcopies: u32,
    remotes: &HashMap<String, Remote>,
) -> (usize, usize, usize) {
    let want = numcopies.max(1);
    let mut under = 0;
    let mut ok = 0;
    let mut over = 0;
    for uuids in locations.values() {
        let n = uuids
            .iter()
            .filter(|u| counts_toward_numcopies(remotes, u))
            .count() as u32;
        if n < want {
            under += 1;
        } else if n == want {
            ok += 1;
        } else {
            over += 1;
        }
    }
    (under, ok, over)
}

fn counts_toward_numcopies(remotes: &HashMap<String, Remote>, uuid: &str) -> bool {
    !matches!(
        remotes.get(uuid).map(|r| r.trust),
        Some(TrustLevel::UnTrusted) | Some(TrustLevel::Dead)
    )
}

/// Unique remote names vs summed per-repo remote entries.
pub fn remote_name_stats(summaries: &[RepoSummary]) -> (usize, usize) {
    let mut names = HashSet::new();
    let mut summed = 0usize;
    for s in summaries {
        if s.remote_usage.is_empty() {
            summed += s.remote_count;
        } else {
            summed += s.remote_usage.len();
            for u in &s.remote_usage {
                names.insert(u.name.as_str());
            }
        }
    }
    (names.len(), summed)
}

impl RepoSummary {
    /// Minimal entry for a discovered repo whose metadata is not loaded yet.
    pub fn placeholder(root: PathBuf) -> Self {
        let mut s = RepoSummary {
            root,
            ..Default::default()
        };
        s.ensure_name();
        s
    }

    /// Ensure we have a usable display name (for old caches or minimal entries)
    pub fn ensure_name(&mut self) {
        if self.name.is_empty() {
            self.name = self
                .root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| self.annex_description.clone());
        }
    }
}

/// Resolve the git directory for a work tree (plain `.git` dir or `gitdir:` file).
fn resolve_git_dir(path: &Path) -> Option<PathBuf> {
    let git = path.join(".git");
    if git.is_dir() {
        return Some(git);
    }
    if git.is_file() {
        let text = std::fs::read_to_string(&git).ok()?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("gitdir:") {
                let p = PathBuf::from(rest.trim());
                return Some(if p.is_absolute() { p } else { path.join(p) });
            }
        }
    }
    None
}

/// Directory holding shared refs and objects. Linked worktrees point to it via `commondir`.
fn common_git_dir(git_dir: &Path) -> PathBuf {
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(s) => {
            let p = PathBuf::from(s.trim());
            if p.is_absolute() { p } else { git_dir.join(p) }
        }
        Err(_) => git_dir.to_path_buf(),
    }
}

/// Result of reading a ref straight from the filesystem.
#[derive(Debug, PartialEq, Eq)]
enum RefLookup {
    Found(String),
    Absent,
    /// Could not tell (unreadable files, reftable, …); ask git.
    Unknown,
}

/// Look up `name` (e.g. `refs/heads/git-annex`) in loose refs, then `packed-refs`.
fn read_ref(common: &Path, name: &str, depth: u8) -> RefLookup {
    if depth > 4 {
        return RefLookup::Unknown;
    }
    match std::fs::read_to_string(common.join(name)) {
        Ok(s) => {
            let s = s.trim();
            return match s.strip_prefix("ref: ") {
                Some(target) => read_ref(common, target.trim(), depth + 1),
                None if !s.is_empty() => RefLookup::Found(s.to_string()),
                None => RefLookup::Unknown,
            };
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return RefLookup::Unknown,
        Err(_) => {}
    }
    if common.join("reftable").exists() {
        return RefLookup::Unknown;
    }
    match std::fs::read_to_string(common.join("packed-refs")) {
        Ok(text) => {
            for line in text.lines() {
                if line.starts_with('#') || line.starts_with('^') {
                    continue;
                }
                if let Some((sha, refname)) = line.split_once(' ')
                    && refname.trim() == name
                {
                    return RefLookup::Found(sha.to_string());
                }
            }
            RefLookup::Absent
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => RefLookup::Absent,
        Err(_) => RefLookup::Unknown,
    }
}

/// `HEAD` of this work tree: symbolic refs are followed; an unborn branch is `Absent`.
fn read_head(git_dir: &Path, common: &Path) -> RefLookup {
    let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD")) else {
        return RefLookup::Unknown;
    };
    let head = head.trim();
    match head.strip_prefix("ref: ") {
        Some(target) => read_ref(common, target.trim(), 0),
        None if !head.is_empty() => RefLookup::Found(head.to_string()),
        None => RefLookup::Unknown,
    }
}

pub fn is_annex_repo(path: &Path) -> bool {
    let Some(git_dir) = resolve_git_dir(path) else {
        return false;
    };
    let common = common_git_dir(&git_dir);
    if git_dir.join("annex").exists() || common.join("annex").exists() {
        return true;
    }
    match read_ref(&common, "refs/heads/git-annex", 0) {
        RefLookup::Found(_) => true,
        RefLookup::Absent => false,
        RefLookup::Unknown => Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["rev-parse", "--verify", "--quiet", "git-annex^{commit}"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success()),
    }
}

/// What a load of this repo depends on. Equal fingerprints mean a cached load is current.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoFingerprint {
    /// Commit of the local `git-annex` branch (location logs, trust, remotes…).
    pub annex_branch: String,
    /// Commit of `HEAD` (the annexed file list). Empty on an unborn branch.
    pub head: String,
    /// Newest mtime of `.git/config` and `.gitattributes` (numcopies, annex.* settings).
    pub config_mtime: i64,
}

fn mtime_secs(p: &Path) -> Option<i64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64)
}

/// Cheap fingerprint from ref files; falls back to one `git rev-parse` when needed.
pub fn repo_fingerprint(root: &Path) -> Option<RepoFingerprint> {
    let git_dir = resolve_git_dir(root)?;
    let common = common_git_dir(&git_dir);
    let config_mtime = [common.join("config"), root.join(".gitattributes")]
        .iter()
        .filter_map(|p| mtime_secs(p))
        .max()
        .unwrap_or(0);
    let branch = read_ref(&common, "refs/heads/git-annex", 0);
    let head = read_head(&git_dir, &common);
    let (annex_branch, head) = match (branch, head) {
        (RefLookup::Found(b), RefLookup::Found(h)) => (b, h),
        (RefLookup::Found(b), RefLookup::Absent) => (b, String::new()),
        (RefLookup::Absent, _) => return None,
        _ => {
            let out = run_git(root, &["rev-parse", "git-annex", "HEAD"]).ok()?;
            let mut lines = out.lines();
            let b = lines.next()?.trim().to_string();
            let h = lines.next().unwrap_or("").trim().to_string();
            (b, h)
        }
    };
    Some(RepoFingerprint {
        annex_branch,
        head,
        config_mtime,
    })
}

/// Limits for walking the scan root.
#[derive(Debug, Clone, Default)]
pub struct DiscoverOptions {
    /// Maximum directory depth below the root (None = unlimited).
    pub max_depth: Option<usize>,
    /// Do not cross into other mounted filesystems.
    pub one_file_system: bool,
}

pub fn find_annex_repos(root: &Path, opts: &DiscoverOptions) -> Vec<PathBuf> {
    let mut repos = Vec::new();
    let mut walk = walkdir::WalkDir::new(root)
        .follow_links(false)
        .same_file_system(opts.one_file_system);
    if let Some(d) = opts.max_depth {
        walk = walk.max_depth(d);
    }
    let mut it = walk.into_iter();
    while let Some(entry) = it.next() {
        let Ok(e) = entry else {
            continue;
        };
        if !e.file_type().is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy();
        if name == ".git" {
            it.skip_current_dir();
            continue;
        }
        let skip_noise = name == "target" || name == "node_modules";
        let skip_hidden = e.depth() > 2 && name.starts_with('.');
        if skip_noise || skip_hidden {
            it.skip_current_dir();
            continue;
        }
        if is_annex_repo(e.path()) {
            repos.push(e.path().to_path_buf());
            it.skip_current_dir();
        }
    }
    repos.sort();
    repos
}

/// Parse a git-annex log timestamp (`1317929189.157237s` or `1317929189s`) to unix seconds.
pub fn parse_annex_timestamp(raw: &str) -> Option<i64> {
    let s = raw.trim().trim_end_matches('s').trim();
    if s.is_empty() {
        return None;
    }
    let secs = s.split_once('.').map(|(a, _)| a).unwrap_or(s);
    secs.parse().ok()
}

/// Split `... timestamp=UNIXs` off a log line. Returns (prefix, timestamp).
fn split_log_timestamp(line: &str) -> (&str, Option<i64>) {
    if let Some(pos) = line.find(" timestamp=") {
        let ts = parse_annex_timestamp(&line[pos + 11..]);
        (line[..pos].trim_end(), ts)
    } else if let Some(pos) = line.find("timestamp=") {
        let ts = parse_annex_timestamp(&line[pos + 10..]);
        (line[..pos].trim_end(), ts)
    } else {
        (line, None)
    }
}

fn keep_latest<T>(slot: &mut Option<(i64, T)>, ts: Option<i64>, value: T) {
    let ts = ts.unwrap_or(0);
    match slot {
        Some((old, _)) if ts < *old => {}
        _ => *slot = Some((ts, value)),
    }
}

fn run_git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("git {:?} in {:?}", args, repo))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git {:?} failed: {}", args, err);
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Last-write-wins per UUID for branch logs shaped `UUID <body> timestamp=…s`.
/// `parse` receives the body after the UUID (without the timestamp).
fn latest_per_uuid<T>(text: &str, parse: impl Fn(&str) -> Option<T>) -> HashMap<String, T> {
    let mut latest: HashMap<String, (i64, T)> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (body, ts) = split_log_timestamp(line);
        let (uuid, rest) = body.split_once(' ').unwrap_or((body, ""));
        if uuid.is_empty() {
            continue;
        }
        let Some(value) = parse(rest.trim()) else {
            continue;
        };
        let ts = ts.unwrap_or(0);
        match latest.get(uuid) {
            Some((old, _)) if ts < *old => {}
            _ => {
                latest.insert(uuid.to_string(), (ts, value));
            }
        }
    }
    latest.into_iter().map(|(u, (_, v))| (u, v)).collect()
}

/// Parse uuid.log. Last-write-wins per UUID using the line timestamp.
fn parse_uuid_log(text: &str) -> HashMap<String, String> {
    latest_per_uuid(text, |desc| Some(desc.to_string()))
}

fn parse_remote_log(text: &str) -> HashMap<String, HashMap<String, String>> {
    latest_per_uuid(text, |body| {
        let mut cfg = HashMap::new();
        for tok in body.split_whitespace() {
            if let Some((k, v)) = tok.split_once('=') {
                let v = if is_secret_remote_key(k) {
                    "[redacted]".to_string()
                } else {
                    v.to_string()
                };
                cfg.insert(k.to_string(), v);
            }
        }
        Some(cfg)
    })
}

fn parse_trust_log(text: &str) -> HashMap<String, TrustLevel> {
    latest_per_uuid(text, |body| {
        Some(TrustLevel::from_token(
            body.split_whitespace().next().unwrap_or("?"),
        ))
    })
}

fn parse_activity_log(text: &str) -> HashMap<String, i64> {
    // lines like: UUID Fsck timestamp=UNIXs
    let mut m: HashMap<String, i64> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(uuid) = line.split_whitespace().next() else {
            continue;
        };
        if !line.contains("Fsck") {
            continue;
        }
        let (_, ts) = split_log_timestamp(line);
        let Some(ts) = ts else {
            continue;
        };
        let e = m.entry(uuid.to_string()).or_insert(ts);
        if ts > *e {
            *e = ts;
        }
    }
    m
}

fn parse_group_log(text: &str) -> HashMap<String, Vec<String>> {
    // Official format: UUID group1 group2 ... timestamp=UNIX.s
    // The line with the highest timestamp is the complete current set.
    latest_per_uuid(text, |body| {
        let mut groups: Vec<String> = body.split_whitespace().map(str::to_string).collect();
        groups.sort();
        groups.dedup();
        Some(groups)
    })
}

fn parse_content_log(text: &str) -> HashMap<String, String> {
    // preferred-content.log / required-content.log: uuid <expression> timestamp=...
    latest_per_uuid(text, |expr| Some(expr.to_string()))
}

/// Path of a per-key location log on the git-annex branch (`xx/yy/KEY.log`).
fn is_location_log_path(path: &str) -> bool {
    let mut parts = path.split('/');
    let Some(a) = parts.next() else {
        return false;
    };
    let Some(b) = parts.next() else {
        return false;
    };
    let Some(name) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    let hashdir = (a.len() == 2 || a.len() == 3) && (b.len() == 2 || b.len() == 3);
    hashdir && name.ends_with(".log") && !name.contains(".log.")
}

fn location_log_key(path: &str) -> Option<&str> {
    path.rsplit('/').next()?.strip_suffix(".log")
}

/// Parse one location-log file. Last-write-wins per UUID.
///
/// git-annex 10+ (timestamp first): `1317929189.157s 1 UUID`
/// Older: `UUID 1 timestamp=1317929189s`
pub fn parse_location_log(text: &str) -> HashSet<String> {
    let mut latest: HashMap<String, (i64, bool)> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((uuid, ts, present)) = parse_location_log_line(line) else {
            continue;
        };
        match latest.get(&uuid) {
            Some((old, _)) if ts < *old => {}
            _ => {
                latest.insert(uuid, (ts, present));
            }
        }
    }
    latest
        .into_iter()
        .filter_map(|(u, (_, present))| present.then_some(u))
        .collect()
}

fn parse_location_log_line(line: &str) -> Option<(String, i64, bool)> {
    if line.contains("timestamp=") {
        let (body, ts) = split_log_timestamp(line);
        let mut parts = body.split_whitespace();
        let uuid = parts.next().filter(|u| !u.is_empty())?.to_string();
        let status = parts.next().unwrap_or("1");
        Some((uuid, ts.unwrap_or(0), status == "1"))
    } else {
        let mut parts = line.split_whitespace();
        let ts_raw = parts.next()?;
        let status = parts.next()?;
        let uuid = parts.next().filter(|u| !u.is_empty())?.to_string();
        Some((
            uuid,
            parse_annex_timestamp(ts_raw).unwrap_or(0),
            status == "1",
        ))
    }
}

/// Blobs of the git-annex branch that `load_metadata` reads.
#[derive(Default)]
struct BranchLogs {
    /// Top-level logs by file name (`uuid.log`, `trust.log`, …).
    named: HashMap<String, String>,
    /// Per-key location logs: key -> UUIDs with content.
    locations: HashMap<String, HashSet<String>>,
}

const NAMED_LOGS: &[&str] = &[
    "uuid.log",
    "remote.log",
    "trust.log",
    "activity.log",
    "group.log",
    "preferred-content.log",
    "required-content.log",
    "numcopies.log",
];

/// Read every log `load_metadata` needs with one `ls-tree` and one `cat-file --batch`.
/// Location logs include untrusted remotes (e.g. Glacier); `whereis --all` would be far slower.
fn read_branch_logs(root: &Path) -> Result<BranchLogs> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-tree", "-r", "-z", "git-annex"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("{}: running git ls-tree", root.display()))?;
    if !out.status.success() {
        anyhow::bail!(
            "{}: git ls-tree git-annex failed: {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    enum Target {
        Named(String),
        Location(String),
    }
    let mut shas = Vec::new();
    let mut targets = Vec::new();
    for entry in out.stdout.split(|&b| b == 0) {
        let Ok(s) = std::str::from_utf8(entry) else {
            continue;
        };
        let Some((meta, path)) = s.split_once('\t') else {
            continue;
        };
        let Some(sha) = meta.split_whitespace().nth(2) else {
            continue;
        };
        let target = if NAMED_LOGS.contains(&path) {
            Target::Named(path.to_string())
        } else if is_location_log_path(path)
            && let Some(key) = location_log_key(path)
        {
            Target::Location(key.to_string())
        } else {
            continue;
        };
        shas.push(sha.to_string());
        targets.push(target);
    }

    let mut logs = BranchLogs::default();
    if shas.is_empty() {
        return Ok(logs);
    }
    let blobs = cat_file_batch(root, shas)?;
    logs.locations.reserve(targets.len());
    for (target, blob) in targets.into_iter().zip(blobs) {
        let Some(blob) = blob else {
            continue;
        };
        let text = String::from_utf8_lossy(&blob);
        match target {
            Target::Named(name) => {
                logs.named.insert(name, text.into_owned());
            }
            Target::Location(key) => {
                let uuids = parse_location_log(&text);
                if !uuids.is_empty() {
                    logs.locations.insert(key, uuids);
                }
            }
        }
    }
    Ok(logs)
}

/// `git cat-file --batch` for `shas`, in order. Missing objects are `None`.
/// A writer thread avoids deadlock when the pipe buffer fills before we start reading.
fn cat_file_batch(root: &Path, shas: Vec<String>) -> Result<Vec<Option<Vec<u8>>>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("{}: starting git cat-file", root.display()))?;
    let mut stdin = child.stdin.take().context("cat-file stdin")?;
    let stdout = child.stdout.take().context("cat-file stdout")?;
    let n = shas.len();

    let writer = std::thread::spawn(move || {
        let mut w = std::io::BufWriter::new(&mut stdin);
        for sha in shas {
            if w.write_all(sha.as_bytes()).is_err() || w.write_all(b"\n").is_err() {
                break;
            }
        }
        let _ = w.flush();
    });

    let mut reader = BufReader::new(stdout);
    let mut blobs = Vec::with_capacity(n);
    let mut header = String::new();
    let mut nl = [0u8; 1];
    for _ in 0..n {
        header.clear();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header_trim = header.trim_end();
        if header_trim.ends_with("missing") {
            blobs.push(None);
            continue;
        }
        let size: usize = header_trim
            .split_whitespace()
            .nth(2)
            .and_then(|s| s.parse().ok())
            .context("bad cat-file header")?;
        let mut buf = vec![0u8; size];
        reader.read_exact(&mut buf)?;
        reader.read_exact(&mut nl)?;
        blobs.push(Some(buf));
    }
    drop(reader);
    let _ = writer.join();
    let status = child.wait()?;
    if blobs.len() != n {
        anyhow::bail!(
            "{}: git cat-file returned {} of {} objects ({status})",
            root.display(),
            blobs.len(),
            n
        );
    }
    Ok(blobs)
}

/// mtime of the git-annex branch ref (or packed-refs). Used to hydrate recently
/// updated repos first so Glacier copies show up without waiting for the whole scan.
pub fn annex_branch_mtime(root: &Path) -> Option<i64> {
    let common = common_git_dir(&resolve_git_dir(root)?);
    [
        common.join("refs/heads/git-annex"),
        common.join("packed-refs"),
    ]
    .iter()
    .filter_map(|p| mtime_secs(p))
    .max()
}

/// numcopies.log / mincopies.log: `timestamp number` (timestamp-first).
fn parse_count_log(text: &str) -> Option<u32> {
    let mut best: Option<(i64, u32)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(ts) = parts.next().and_then(parse_annex_timestamp) else {
            continue;
        };
        let Some(n) = parts.next().and_then(|s| s.parse().ok()) else {
            continue;
        };
        keep_latest(&mut best, Some(ts), n);
    }
    best.map(|(_, n)| n)
}

pub fn parse_size_from_key(key: &str) -> Option<u64> {
    // Standard key: BACKEND[-sSIZE][-mMTIME][-S..]--HASH
    let prefix = key.split_once("--").map(|(p, _)| p).unwrap_or(key);
    for field in prefix.split('-').skip(1) {
        if let Some(rest) = field.strip_prefix('s')
            && let Ok(n) = rest.parse::<u64>()
        {
            return Some(n);
        }
    }
    None
}

fn collect_key_sizes(
    files: &[AnnexedFile],
    locations: &HashMap<String, HashSet<String>>,
) -> HashMap<String, u64> {
    let mut key_sizes: HashMap<String, u64> = HashMap::new();
    for f in files {
        if let Some(sz) = f.size {
            key_sizes.insert(f.key.clone(), sz);
        }
    }
    for key in locations.keys() {
        if !key_sizes.contains_key(key)
            && let Some(sz) = parse_size_from_key(key)
        {
            key_sizes.insert(key.clone(), sz);
        }
    }
    key_sizes
}

fn size_totals(
    locations: &HashMap<String, HashSet<String>>,
    key_sizes: &HashMap<String, u64>,
) -> (u64, u64) {
    let mut unique_size = 0u64;
    let mut consumed_size = 0u64;
    for (key, uuids) in locations {
        if uuids.is_empty() {
            continue;
        }
        if let Some(&sz) = key_sizes.get(key) {
            unique_size += sz;
            consumed_size += sz * (uuids.len() as u64);
        }
    }
    (unique_size, consumed_size)
}

/// Remove remotes marked dead (except `here`). Returns true if anything was dropped.
fn drop_dead_remotes(
    here: &str,
    remotes: &mut HashMap<String, Remote>,
    locations: &mut HashMap<String, HashSet<String>>,
) -> bool {
    let dead: HashSet<String> = remotes
        .iter()
        .filter(|(uuid, r)| r.trust == TrustLevel::Dead && *uuid != here)
        .map(|(uuid, _)| uuid.clone())
        .collect();
    if dead.is_empty() {
        return false;
    }
    remotes.retain(|uuid, _| !dead.contains(uuid));
    for locs in locations.values_mut() {
        locs.retain(|u| !dead.contains(u));
    }
    locations.retain(|_, locs| !locs.is_empty());
    true
}

fn apply_present_stats(
    remotes: &mut HashMap<String, Remote>,
    locations: &HashMap<String, HashSet<String>>,
    key_sizes: &HashMap<String, u64>,
) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut sizes: HashMap<String, u64> = HashMap::new();
    for (key, uuids) in locations {
        let sz = key_sizes.get(key).copied().unwrap_or(0);
        for u in uuids {
            *counts.entry(u.clone()).or_default() += 1;
            *sizes.entry(u.clone()).or_default() += sz;
        }
    }
    for (u, r) in remotes.iter_mut() {
        r.present_count = counts.get(u).copied().unwrap_or(0);
        r.present_size = sizes.get(u).copied().unwrap_or(0);
    }
}

pub fn is_secret_remote_key(k: &str) -> bool {
    matches!(
        k,
        "cipher" | "embedcreds" | "encryptionkey" | "secret" | "password" | "keyid"
    )
}

/// Parse `git annex find --format='${file}\000${key}\000${bytesize}\000'` output.
pub fn parse_find_nul_triplets(buf: &[u8]) -> Vec<AnnexedFile> {
    let mut out = Vec::new();
    let mut fields = buf.split(|&b| b == 0);
    while let Some(path_b) = fields.next() {
        if path_b.is_empty() {
            break;
        }
        let Some(key_b) = fields.next() else {
            break;
        };
        let Some(size_b) = fields.next() else {
            break;
        };
        let path = String::from_utf8_lossy(path_b).into_owned();
        let key = String::from_utf8_lossy(key_b).into_owned();
        if path.is_empty() || key.is_empty() {
            continue;
        }
        let parsed = std::str::from_utf8(size_b)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&n| n > 0);
        let size = parsed.or_else(|| parse_size_from_key(&key));
        out.push(AnnexedFile { path, key, size });
    }
    out
}

fn find_files_with_format(root: &Path, use_branch: bool) -> Option<Vec<AnnexedFile>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).arg("annex").arg("find");
    if use_branch {
        cmd.arg("--branch").arg("HEAD");
    }
    let out = cmd
        .arg("--anything")
        .arg("--format=${file}\\000${key}\\000${bytesize}\\000")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    if out.stdout.is_empty() {
        return Some(Vec::new());
    }
    if !out.stdout.contains(&0) {
        return None;
    }
    Some(parse_find_nul_triplets(&out.stdout))
}

fn find_annexed_paths(root: &Path) -> Vec<String> {
    let attempts: &[&[&str]] = &[
        &[
            "annex",
            "find",
            "--branch",
            "HEAD",
            "--anything",
            "--print0",
        ],
        &["annex", "find", "--anything", "--print0"],
        &["annex", "find", "--print0"],
    ];
    for args in attempts {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(*args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        let Ok(out) = out else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        let mut v = Vec::new();
        for p in out.stdout.split(|&b| b == 0) {
            if !p.is_empty() {
                v.push(String::from_utf8_lossy(p).into_owned());
            }
        }
        return v;
    }
    Vec::new()
}

fn lookup_keys_for_paths(root: &Path, paths: &[String]) -> Vec<AnnexedFile> {
    if paths.is_empty() {
        return Vec::new();
    }
    let mut child = match Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("annex")
        .arg("lookupkey")
        .arg("--batch")
        .arg("-z")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    {
        let Some(mut stdin) = child.stdin.take() else {
            return Vec::new();
        };
        for p in paths {
            if stdin.write_all(p.as_bytes()).is_err() || stdin.write_all(&[0]).is_err() {
                break;
            }
        }
    }
    let Some(stdout) = child.stdout.take() else {
        let _ = child.wait();
        return Vec::new();
    };
    let mut files = Vec::new();
    let reader = BufReader::new(stdout);
    for (i, line_res) in reader.lines().enumerate() {
        if let Ok(key) = line_res
            && i < paths.len()
            && !key.is_empty()
        {
            let path = paths[i].clone();
            let size = parse_size_from_key(&key);
            files.push(AnnexedFile { path, key, size });
        }
    }
    let _ = child.wait();
    files
}

/// All annexed files recorded in git, with sizes from git-annex (not the filesystem).
///
/// Prefers `git annex find --branch HEAD --anything` so files still appear when
/// their content is dropped or the working-tree symlink is missing.
pub fn load_annexed_files(root: &Path) -> Vec<AnnexedFile> {
    if let Some(files) = find_files_with_format(root, true) {
        return files;
    }
    if let Some(files) = find_files_with_format(root, false) {
        return files;
    }
    lookup_keys_for_paths(root, &find_annexed_paths(root))
}

/// `annex.*` git config in one call. Keys are lowercase (`annex.uuid`).
fn read_annex_config(root: &Path) -> Result<Vec<(String, String)>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--get-regexp", "^annex\\."])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("{}: running git config", root.display()))?;
    // Exit 1 means "no matching keys", which the uuid check below reports.
    if !out.status.success() && out.status.code() != Some(1) {
        anyhow::bail!(
            "{}: git config failed: {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| {
            let (k, v) = l.split_once(' ').unwrap_or((l, ""));
            (k.to_string(), v.trim().to_string())
        })
        .collect())
}

/// Load full metadata for one annex repo. May be slow on huge annex; called off the UI thread.
///
/// Runs about four git processes: `config`, `ls-tree`, `cat-file --batch` and `annex find`.
pub fn load_metadata(repo: &Path) -> Result<AnnexMetadata> {
    let root = repo.to_path_buf();
    let fingerprint = repo_fingerprint(&root);

    let config = read_annex_config(&root)?;
    let cfg = |k: &str| {
        config
            .iter()
            .find(|(ck, _)| ck == k)
            .map(|(_, v)| v.clone())
    };
    // A missing uuid means the annex is not initialised; returning empty metadata
    // would overwrite a good cache entry with zeros.
    let uuid = cfg("annex.uuid").unwrap_or_default();
    if uuid.is_empty() {
        anyhow::bail!(
            "{}: annex.uuid is empty — not an initialised annex?",
            root.display()
        );
    }
    let desc = cfg("annex.describe").unwrap_or_default();

    let logs = read_branch_logs(&root)?;
    let log = |name: &str| logs.named.get(name).map(String::as_str).unwrap_or("");

    // `git annex numcopies` (branch) overrides the legacy annex.numcopies git config.
    let numcopies = parse_count_log(log("numcopies.log"))
        .or_else(|| cfg("annex.numcopies").and_then(|s| s.parse().ok()));

    let uuid_entries = parse_uuid_log(log("uuid.log"));
    let remote_cfgs = parse_remote_log(log("remote.log"));
    let trusts = parse_trust_log(log("trust.log"));
    let fscks = parse_activity_log(log("activity.log"));
    let groups_map = parse_group_log(log("group.log"));
    let wanted_map = parse_content_log(log("preferred-content.log"));
    let required_map = parse_content_log(log("required-content.log"));

    let trust_of = |u: &str| trusts.get(u).copied().unwrap_or(TrustLevel::SemiTrusted);

    // Build remotes map. Start from uuid.log entries + remotes
    let mut remotes: HashMap<String, Remote> = HashMap::new();
    for (u, d) in &uuid_entries {
        let cfg = remote_cfgs.get(u).cloned().unwrap_or_default();
        let description = match cfg.get("name") {
            Some(name) if d.is_empty() || d == u => name.clone(),
            _ if d.is_empty() => u.clone(),
            _ => d.clone(),
        };
        remotes.insert(
            u.clone(),
            Remote::new(
                u.clone(),
                description,
                cfg,
                trust_of(u),
                fscks.get(u).copied(),
            ),
        );
    }
    // Ensure any only-in-remote.log are present
    for (u, cfg) in &remote_cfgs {
        remotes.entry(u.clone()).or_insert_with(|| {
            let description = cfg.get("name").cloned().unwrap_or_else(|| u.clone());
            Remote::new(
                u.clone(),
                description,
                cfg.clone(),
                trust_of(u),
                fscks.get(u).copied(),
            )
        });
    }
    // Add the local "here" if missing (from config)
    remotes.entry(uuid.clone()).or_insert_with(|| {
        let cfg = HashMap::from([("name".to_string(), "here".to_string())]);
        let description = if desc.is_empty() {
            "here".to_string()
        } else {
            desc.clone()
        };
        Remote::new(
            uuid.clone(),
            description,
            cfg,
            trust_of(&uuid),
            fscks.get(&uuid).copied(),
        )
    });

    // Assign groups / wanted / required to remotes (including here)
    for (u, r) in remotes.iter_mut() {
        if let Some(gs) = groups_map.get(u) {
            r.groups = gs.clone();
        }
        r.wanted = wanted_map.get(u).cloned();
        r.required = required_map.get(u).cloned();
    }

    // Collect additional configurations (numcopies, gitattributes etc.)
    let mut additional_configs = vec![];
    if let Some(n) = numcopies {
        additional_configs.push(format!("annex.numcopies={}", n));
    }
    // Top-level .gitattributes annex.* settings (numcopies per path etc.)
    if let Ok(content) = std::fs::read_to_string(root.join(".gitattributes")) {
        for line in content.lines() {
            let l = line.trim();
            if l.contains("annex.") {
                additional_configs.push(format!(".gitattributes: {}", l));
            }
        }
    }
    for (k, v) in &config {
        if !matches!(
            k.as_str(),
            "annex.numcopies" | "annex.uuid" | "annex.describe"
        ) {
            additional_configs.push(format!("{k} {v}"));
        }
    }

    // Working-tree / HEAD annexed files. Sizes come from git-annex (key / bytesize),
    // so dropped or missing content is still listed.
    let files = load_annexed_files(&root);

    let mut locations = logs.locations;
    drop_dead_remotes(&uuid, &mut remotes, &mut locations);

    let total_keys = locations.len().max(files.len());
    let key_sizes = collect_key_sizes(&files, &locations);
    apply_present_stats(&mut remotes, &locations, &key_sizes);
    let (unique_size, consumed_size) = size_totals(&locations, &key_sizes);

    // Fill local desc if empty
    let description = if desc.is_empty() {
        uuid_entries
            .get(&uuid)
            .filter(|d| !d.is_empty())
            .cloned()
            .unwrap_or_else(|| uuid.clone())
    } else {
        desc
    };

    Ok(AnnexMetadata {
        root,
        uuid,
        description,
        numcopies,
        additional_configs,
        remotes,
        locations,
        files,
        total_keys,
        unique_size,
        consumed_size,
        fingerprint,
    })
}

/// Return a short human name for a UUID (prefers name in config or desc)
pub fn short_name(meta: &AnnexMetadata, uuid: &str) -> String {
    if uuid == meta.uuid {
        return "here".to_string();
    }
    meta.remotes
        .get(uuid)
        .map(|r| {
            if r.name() != r.uuid {
                r.name().to_string()
            } else {
                r.description.clone()
            }
        })
        .unwrap_or_else(|| uuid.chars().take(8).collect())
}

// ---------------------- Cache (local DB file) ----------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AnnexCache {
    pub version: u32,
    pub updated: i64,
    /// canonical path string -> full metadata snapshot
    pub repos: HashMap<String, AnnexMetadata>,
}

/// Profile of a drive/remote name across all known repos, used to detect differences.
#[derive(Debug, Clone, Default)]
pub struct DriveProfile {
    pub trusts: HashMap<TrustLevel, usize>,
    pub group_sets: HashMap<Vec<String>, usize>,
    pub wanteds: HashMap<Option<String>, usize>,
    pub requireds: HashMap<Option<String>, usize>,
}

impl DriveProfile {
    pub fn most_common_trust(&self) -> Option<TrustLevel> {
        self.trusts.iter().max_by_key(|(_, c)| *c).map(|(t, _)| *t)
    }
    pub fn most_common_groups(&self) -> Option<Vec<String>> {
        self.group_sets
            .iter()
            .max_by_key(|(_, c)| *c)
            .map(|(g, _)| g.clone())
    }
    pub fn most_common_wanted(&self) -> Option<String> {
        self.wanteds
            .iter()
            .filter(|(w, _)| w.is_some())
            .max_by_key(|(_, c)| *c)
            .and_then(|(w, _)| w.clone())
    }
    pub fn most_common_required(&self) -> Option<String> {
        self.requireds
            .iter()
            .filter(|(r, _)| r.is_some())
            .max_by_key(|(_, c)| *c)
            .and_then(|(r, _)| r.clone())
    }
    /// Whether `r` uses the most common trust, groups, wanted and required setup.
    pub fn matches_common(&self, r: &Remote) -> bool {
        if let Some(ct) = self.most_common_trust()
            && r.trust != ct
        {
            return false;
        }
        if let Some(cg) = self.most_common_groups() {
            let mut myg = r.groups.clone();
            myg.sort();
            if myg != cg {
                return false;
            }
        }
        if let Some(cw) = self.most_common_wanted()
            && r.wanted.as_deref() != Some(cw.as_str())
        {
            return false;
        }
        if let Some(cr) = self.most_common_required()
            && r.required.as_deref() != Some(cr.as_str())
        {
            return false;
        }
        true
    }
    pub fn has_variation(&self) -> bool {
        self.trusts.len() > 1
            || self.group_sets.len() > 1
            || self.wanteds.len() > 1
            || self.requireds.len() > 1
    }
}

pub const CACHE_VERSION: u32 = 1;

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn path_is_under(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

pub fn cache_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("git-annex-browser");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".cache").join("git-annex-browser")
}

pub fn cache_path() -> PathBuf {
    if let Ok(p) = std::env::var("GIT_ANNEX_BROWSER_CACHE")
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    cache_dir().join("cache.json")
}

struct CacheLock {
    _file: std::fs::File,
}

fn lock_cache() -> Result<CacheLock> {
    let dir = cache_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(cache_dir);
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("cache.lock"))?;
    file.lock().context("locking cache")?;
    Ok(CacheLock { _file: file })
}

fn redact_meta(mut meta: AnnexMetadata) -> AnnexMetadata {
    for r in meta.remotes.values_mut() {
        for (k, v) in r.config.iter_mut() {
            if is_secret_remote_key(k) {
                *v = "[redacted]".to_string();
            }
        }
    }
    meta
}

/// Merge a scan of `scan_root` into an existing repo map.
/// Repos under `scan_root` that were not found this time are dropped; others stay.
pub fn merge_scan_repos(
    mut existing: HashMap<String, AnnexMetadata>,
    scan_root: &Path,
    found: HashMap<String, AnnexMetadata>,
) -> HashMap<String, AnnexMetadata> {
    existing.retain(|p, _| {
        let pb = Path::new(p);
        !path_is_under(pb, scan_root) || found.contains_key(p)
    });
    for (k, v) in found {
        existing.insert(k, redact_meta(v));
    }
    existing
}

pub fn load_cache() -> Option<AnnexCache> {
    let data = std::fs::read(cache_path()).ok()?;
    let cache: AnnexCache = serde_json::from_slice(&data).ok()?;
    if cache.version != 0 && cache.version != CACHE_VERSION {
        return None;
    }
    Some(cache)
}

fn write_cache_unlocked(cache: &AnnexCache) -> Result<()> {
    let p = cache_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = p.with_extension("json.tmp");
    let json = serde_json::to_vec(cache)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &p)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Replace cached entries under `scan_root` with `found`; keep everything else.
pub fn merge_scan_into_cache(
    scan_root: &Path,
    found: HashMap<String, AnnexMetadata>,
) -> Result<()> {
    let _lock = lock_cache()?;
    let existing = load_cache().unwrap_or_default().repos;
    let repos = merge_scan_repos(existing, scan_root, found);
    let cache = AnnexCache {
        version: CACHE_VERSION,
        updated: now_unix(),
        repos,
    };
    write_cache_unlocked(&cache)
}

/// Insert or update the given repos without dropping others.
pub fn upsert_cache_repos(repos: impl IntoIterator<Item = (String, AnnexMetadata)>) -> Result<()> {
    let _lock = lock_cache()?;
    let mut cache = load_cache().unwrap_or_default();
    cache.version = CACHE_VERSION;
    cache.updated = now_unix();
    for (k, v) in repos {
        cache.repos.insert(k, redact_meta(v));
    }
    write_cache_unlocked(&cache)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_fractional_and_integer() {
        assert_eq!(
            parse_annex_timestamp("1317929189.157237s"),
            Some(1317929189)
        );
        assert_eq!(
            parse_annex_timestamp("1699273888.593667289s"),
            Some(1699273888)
        );
        assert_eq!(parse_annex_timestamp("1422387398s"), Some(1422387398));
        assert_eq!(parse_annex_timestamp("  42.0s  "), Some(42));
        assert_eq!(parse_annex_timestamp(""), None);
        assert_eq!(parse_annex_timestamp("not-a-time"), None);
    }

    #[test]
    fn trust_tokens_match_git_annex_log() {
        assert_eq!(TrustLevel::from_token("1"), TrustLevel::Trusted);
        assert_eq!(TrustLevel::from_token("0"), TrustLevel::UnTrusted);
        assert_eq!(TrustLevel::from_token("?"), TrustLevel::SemiTrusted);
        assert_eq!(TrustLevel::from_token("X"), TrustLevel::Dead);
        assert_eq!(TrustLevel::from_token("trusted"), TrustLevel::Trusted);
        assert_eq!(TrustLevel::from_token("T"), TrustLevel::Trusted);
    }

    #[test]
    fn trust_log_last_write_wins_and_fractional_ts() {
        let text = "\
aaa-aaa 1 timestamp=100.1s
aaa-aaa 0 timestamp=200.9s
bbb-bbb ? timestamp=50.0s
ccc-ccc X timestamp=10s
aaa-aaa 1 timestamp=150.0s
";
        let m = parse_trust_log(text);
        assert_eq!(m.get("aaa-aaa"), Some(&TrustLevel::UnTrusted));
        assert_eq!(m.get("bbb-bbb"), Some(&TrustLevel::SemiTrusted));
        assert_eq!(m.get("ccc-ccc"), Some(&TrustLevel::Dead));
    }

    #[test]
    fn uuid_log_keeps_latest_description_with_spaces() {
        let text = "\
e605dca6-446a-11e0-8b2a-002170d25c55 laptop timestamp=1317929189.157237s
26339d22-446b-11e0-9101-002170d25c55 usb disk timestamp=1317929330.769997s
e605dca6-446a-11e0-8b2a-002170d25c55 new laptop name timestamp=2000000000.0s
e605dca6-446a-11e0-8b2a-002170d25c55 stale timestamp=1000s
";
        let v = parse_uuid_log(text);
        assert_eq!(v["e605dca6-446a-11e0-8b2a-002170d25c55"], "new laptop name");
        assert_eq!(v["26339d22-446b-11e0-9101-002170d25c55"], "usb disk");
    }

    #[test]
    fn group_log_reads_all_groups_on_latest_line() {
        let text = "\
u1 archive timestamp=100.1s
u1 archive backup client timestamp=200.5s
u1 only-old timestamp=50s
u2 transfer timestamp=10s
";
        let m = parse_group_log(text);
        let mut g1 = m.get("u1").cloned().unwrap();
        g1.sort();
        assert_eq!(g1, vec!["archive", "backup", "client"]);
        assert_eq!(m.get("u2"), Some(&vec!["transfer".to_string()]));
    }

    #[test]
    fn content_log_last_write_wins() {
        let text = "\
u1 include=*.jpg timestamp=10.1s
u1 exclude=* timestamp=20.2s
u1 include=*.png timestamp=15s
";
        let m = parse_content_log(text);
        assert_eq!(m.get("u1").map(String::as_str), Some("exclude=*"));
    }

    #[test]
    fn activity_log_fractional_fsck() {
        let text = "\
u1 Fsck timestamp=1422387398.30395s
u1 Fsck timestamp=1000.0s
u2 something else timestamp=9s
";
        let m = parse_activity_log(text);
        assert_eq!(m.get("u1"), Some(&1422387398));
        assert!(!m.contains_key("u2"));
    }

    #[test]
    fn numcopies_log_timestamp_first() {
        let text = "\
100.1s 1
200.9s 3
150s 2
";
        assert_eq!(parse_count_log(text), Some(3));
    }

    #[test]
    fn size_from_standard_keys() {
        assert_eq!(parse_size_from_key("SHA256E-s12345--abcd"), Some(12345));
        assert_eq!(
            parse_size_from_key(
                "SHA256E-s86558--e79a0891bb94fc9212ce2f28178fe84591c5fb24c07b5239d367099118e12ede.jpg"
            ),
            Some(86558)
        );
        assert_eq!(parse_size_from_key("WORM-s99-m100--name"), Some(99));
        assert_eq!(parse_size_from_key("URL--http://example"), None);
    }

    #[test]
    fn remote_log_redacts_cipher() {
        let text = "u1 type=S3 name=bucket cipher=SUPERSECRET timestamp=10s\n";
        let m = parse_remote_log(text);
        assert_eq!(m["u1"].get("type").map(String::as_str), Some("S3"));
        assert_eq!(
            m["u1"].get("cipher").map(String::as_str),
            Some("[redacted]")
        );
    }

    fn mkdir(p: &Path) {
        std::fs::create_dir_all(p).unwrap();
    }

    fn touch_annex_git(repo: &Path) {
        mkdir(&repo.join(".git/annex/objects/aa/bb/FAKEKEY"));
        std::fs::write(repo.join(".git/annex/objects/aa/bb/FAKEKEY/FAKEKEY"), b"x").unwrap();
        // decoy: if we walked objects we would pick this up as another repo
        mkdir(&repo.join(".git/annex/objects/aa/bb/FAKEKEY/.git/annex"));
    }

    #[test]
    fn find_annex_repos_skips_object_store_and_finds_siblings() {
        let root = std::env::temp_dir().join(format!(
            "gab-find-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        mkdir(&root);
        let photos = root.join("photos");
        let docs = root.join("docs");
        touch_annex_git(&photos);
        touch_annex_git(&docs);
        mkdir(&root.join("plain"));
        std::fs::write(root.join("plain/file.txt"), b"hi").unwrap();

        let found = find_annex_repos(&root, &DiscoverOptions::default());
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found.len(), 2, "found {found:?}");
        assert!(
            found
                .iter()
                .all(|p| p.ends_with("photos") || p.ends_with("docs"))
        );
    }

    #[test]
    fn read_ref_handles_loose_packed_symbolic_and_absent() {
        let dir = tempfile::tempdir().unwrap();
        let g = dir.path();
        mkdir(&g.join("refs/heads"));
        std::fs::write(g.join("refs/heads/main"), "aaaa\n").unwrap();
        std::fs::write(g.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            g.join("packed-refs"),
            "# pack-refs with: peeled fully-peeled sorted\nbbbb refs/heads/git-annex\n^cccc\n",
        )
        .unwrap();
        assert_eq!(
            read_ref(g, "refs/heads/main", 0),
            RefLookup::Found("aaaa".into())
        );
        assert_eq!(
            read_ref(g, "refs/heads/git-annex", 0),
            RefLookup::Found("bbbb".into())
        );
        assert_eq!(read_ref(g, "refs/heads/nope", 0), RefLookup::Absent);
        assert_eq!(read_head(g, g), RefLookup::Found("aaaa".into()));
        std::fs::write(g.join("HEAD"), "dddd\n").unwrap();
        assert_eq!(read_head(g, g), RefLookup::Found("dddd".into()));
    }

    #[test]
    fn plain_git_repo_is_not_an_annex_without_spawning_git() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("plain");
        mkdir(&repo.join(".git/refs/heads"));
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(!is_annex_repo(&repo));
        std::fs::write(repo.join(".git/packed-refs"), "eeee refs/heads/git-annex\n").unwrap();
        assert!(is_annex_repo(&repo));
    }

    #[test]
    fn is_annex_repo_follows_worktree_gitdir_file() {
        let root = std::env::temp_dir().join(format!(
            "gab-wt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        mkdir(&root);
        let git = root.join("real.git");
        mkdir(&git.join("annex"));
        let wt = root.join("tree");
        mkdir(&wt);
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", git.display())).unwrap();
        let ok = is_annex_repo(&wt);
        let found = find_annex_repos(&root, &DiscoverOptions::default());
        let _ = std::fs::remove_dir_all(&root);
        assert!(ok);
        assert!(found.iter().any(|p| p.ends_with("tree")), "found {found:?}");
    }

    fn dummy_meta(root: &str) -> AnnexMetadata {
        AnnexMetadata {
            root: PathBuf::from(root),
            uuid: "u".into(),
            description: String::new(),
            numcopies: None,
            additional_configs: vec![],
            remotes: HashMap::new(),
            locations: HashMap::new(),
            files: vec![],
            total_keys: 0,
            unique_size: 0,
            consumed_size: 0,
            fingerprint: None,
        }
    }

    #[test]
    fn merge_scan_keeps_repos_outside_root() {
        let mut existing = HashMap::new();
        existing.insert("/data/media/a".into(), dummy_meta("/data/media/a"));
        existing.insert("/data/backup/b".into(), dummy_meta("/data/backup/b"));
        existing.insert("/data/media/gone".into(), dummy_meta("/data/media/gone"));
        let mut found = HashMap::new();
        found.insert("/data/media/a".into(), dummy_meta("/data/media/a"));
        found.insert("/data/media/c".into(), dummy_meta("/data/media/c"));
        let merged = merge_scan_repos(existing, Path::new("/data/media"), found);
        assert!(merged.contains_key("/data/media/a"));
        assert!(merged.contains_key("/data/media/c"));
        assert!(merged.contains_key("/data/backup/b"));
        assert!(!merged.contains_key("/data/media/gone"));
    }

    #[test]
    fn aggregate_remote_usage_sums_same_name_across_repos() {
        fn usage(name: &str, size: u64, count: usize, kind: Option<&str>) -> RemoteUsage {
            RemoteUsage {
                name: name.into(),
                uuid: format!("{name}-uuid"),
                present_count: count,
                present_size: size,
                special_type: kind.map(|s| s.to_string()),
            }
        }
        fn summary(usage: Vec<RemoteUsage>) -> RepoSummary {
            RepoSummary {
                name: "r".into(),
                remote_count: usage.len(),
                remote_usage: usage,
                ..RepoSummary::placeholder(PathBuf::from("/r"))
            }
        }
        let rows = aggregate_remote_usage(&[
            summary(vec![
                usage("orca", 1000, 2, None),
                usage("hetzner", 500, 1, Some("rclone")),
            ]),
            summary(vec![
                usage("orca", 250, 3, None),
                usage("hetzner", 50, 1, Some("rclone")),
                usage("web", 10, 1, Some("web")),
            ]),
            summary(vec![usage("empty", 0, 0, Some("rclone"))]),
        ]);
        assert_eq!(rows[0], ("hetzner (rclone)".into(), 550, 2, 2));
        assert_eq!(rows[1], ("web (web)".into(), 10, 1, 1));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn remote_name_stats_counts_unique_names() {
        fn usage(name: &str) -> RemoteUsage {
            RemoteUsage {
                name: name.into(),
                uuid: format!("{name}-uuid"),
                present_count: 0,
                present_size: 0,
                special_type: None,
            }
        }
        fn summary(usage: Vec<RemoteUsage>) -> RepoSummary {
            RepoSummary {
                name: "r".into(),
                remote_count: usage.len(),
                remote_usage: usage,
                ..RepoSummary::placeholder(PathBuf::from("/r"))
            }
        }
        let (unique, summed) = remote_name_stats(&[
            summary(vec![usage("hdd-sata-02"), usage("orca")]),
            summary(vec![usage("hdd-sata-02"), usage("usb")]),
        ]);
        assert_eq!(unique, 3);
        assert_eq!(summed, 4);
    }

    #[test]
    fn copy_health_counts_vs_numcopies() {
        let mut loc = HashMap::new();
        loc.insert("a".into(), HashSet::from(["u1".into()]));
        loc.insert("b".into(), HashSet::from(["u1".into(), "u2".into()]));
        loc.insert(
            "c".into(),
            HashSet::from(["u1".into(), "u2".into(), "u3".into()]),
        );
        let none = HashMap::new();
        assert_eq!(copy_health_counts(&loc, 2, &none), (1, 1, 1));
        assert_eq!(copy_health_counts(&loc, 1, &none), (0, 1, 2));
    }

    #[test]
    fn copy_health_ignores_untrusted_and_dead() {
        let mut loc = HashMap::new();
        loc.insert("only-glacier".into(), HashSet::from(["glacier".into()]));
        loc.insert(
            "here-and-glacier".into(),
            HashSet::from(["here".into(), "glacier".into()]),
        );
        loc.insert(
            "here-disk-glacier".into(),
            HashSet::from([
                "here".into(),
                "disk".into(),
                "glacier".into(),
                "dead".into(),
            ]),
        );
        let mut remotes = HashMap::new();
        for (uuid, trust) in [
            ("here", TrustLevel::Trusted),
            ("disk", TrustLevel::SemiTrusted),
            ("glacier", TrustLevel::UnTrusted),
            ("dead", TrustLevel::Dead),
        ] {
            remotes.insert(
                uuid.to_string(),
                Remote::new(uuid.into(), uuid.into(), HashMap::new(), trust, None),
            );
        }
        // numcopies=1: only-glacier is under (untrusted doesn't count), others ok/over
        assert_eq!(copy_health_counts(&loc, 1, &remotes), (1, 1, 1));
        assert_eq!(copy_health_counts(&loc, 2, &remotes), (2, 1, 0));
    }

    fn remote(uuid: &str, trust: TrustLevel) -> Remote {
        Remote::new(uuid.into(), uuid.into(), HashMap::new(), trust, None)
    }

    #[test]
    fn omit_dead_remotes_strips_them_from_data_and_totals() {
        let mut remotes = HashMap::new();
        remotes.insert("here".into(), remote("here", TrustLevel::Trusted));
        remotes.insert("disk".into(), remote("disk", TrustLevel::SemiTrusted));
        remotes.insert("old-laptop".into(), remote("old-laptop", TrustLevel::Dead));
        let mut locations = HashMap::new();
        locations.insert(
            "SHA256E-s100--aa".into(),
            HashSet::from(["here".into(), "old-laptop".into()]),
        );
        locations.insert(
            "SHA256E-s50--bb".into(),
            HashSet::from(["old-laptop".into()]),
        );
        locations.insert("SHA256E-s10--cc".into(), HashSet::from(["disk".into()]));
        let mut meta = dummy_meta("/tmp/a");
        meta.uuid = "here".into();
        meta.remotes = remotes;
        meta.locations = locations;
        meta.omit_dead_remotes();

        assert!(!meta.remotes.contains_key("old-laptop"));
        assert!(meta.remotes.contains_key("here"));
        assert!(meta.remotes.contains_key("disk"));
        assert_eq!(
            meta.locations.get("SHA256E-s100--aa").unwrap(),
            &HashSet::from(["here".to_string()])
        );
        assert!(!meta.locations.contains_key("SHA256E-s50--bb"));
        assert_eq!(meta.unique_size, 110);
        assert_eq!(meta.consumed_size, 110);
        assert_eq!(meta.remotes["here"].present_count, 1);
        assert_eq!(meta.remotes["here"].present_size, 100);
        assert_eq!(meta.remotes["disk"].present_size, 10);
    }

    #[test]
    fn omit_dead_keeps_here_even_if_marked_dead() {
        let mut remotes = HashMap::new();
        remotes.insert("here".into(), remote("here", TrustLevel::Dead));
        remotes.insert("gone".into(), remote("gone", TrustLevel::Dead));
        let mut locations = HashMap::new();
        locations.insert(
            "SHA256E-s8--x".into(),
            HashSet::from(["here".into(), "gone".into()]),
        );
        let mut meta = dummy_meta("/tmp/a");
        meta.uuid = "here".into();
        meta.remotes = remotes;
        meta.locations = locations;
        meta.omit_dead_remotes();
        assert!(meta.remotes.contains_key("here"));
        assert!(!meta.remotes.contains_key("gone"));
        assert_eq!(meta.unique_size, 8);
        assert_eq!(meta.consumed_size, 8);
    }

    #[test]
    fn ensure_sizes_omits_dead_from_cached_metadata() {
        let mut remotes = HashMap::new();
        remotes.insert("here".into(), {
            let mut r = remote("here", TrustLevel::Trusted);
            r.present_count = 1;
            r.present_size = 20;
            r
        });
        remotes.insert("dead-box".into(), {
            let mut r = remote("dead-box", TrustLevel::Dead);
            r.present_count = 1;
            r.present_size = 20;
            r
        });
        let mut locations = HashMap::new();
        locations.insert(
            "SHA256E-s20--k".into(),
            HashSet::from(["here".into(), "dead-box".into()]),
        );
        let mut meta = dummy_meta("/tmp/a");
        meta.uuid = "here".into();
        meta.remotes = remotes;
        meta.locations = locations;
        meta.unique_size = 20;
        meta.consumed_size = 40;
        meta.ensure_sizes();
        assert!(!meta.remotes.contains_key("dead-box"));
        assert_eq!(meta.consumed_size, 20);
    }

    #[test]
    fn location_log_new_format_last_write_wins_includes_untrusted() {
        let text = "\
100.0s 1 here-uuid
200.5s 1 glacier-uuid
150.0s 1 here-uuid
250s 0 here-uuid
300.1s 1 glacier-uuid
";
        let set = parse_location_log(text);
        assert!(!set.contains("here-uuid"), "dropped locally: {set:?}");
        assert!(
            set.contains("glacier-uuid"),
            "glacier still present: {set:?}"
        );
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn location_log_old_format_and_fractional_ts() {
        let text = "\
here-uuid 1 timestamp=100.1s
glacier-uuid 1 timestamp=200.9s
here-uuid 0 timestamp=150.0s
here-uuid 1 timestamp=125s
";
        let set = parse_location_log(text);
        assert!(!set.contains("here-uuid"));
        assert!(set.contains("glacier-uuid"));
    }

    #[test]
    fn location_log_path_is_hashed_key_log() {
        assert!(is_location_log_path("1ba/48f/SHA256E-s4171072--aa.mp4.log"));
        assert!(is_location_log_path("ab/cd/WORM-s1-m2--x.log"));
        assert!(!is_location_log_path("uuid.log"));
        assert!(!is_location_log_path("1ba/48f/SHA256E-s1--aa.log.cnk"));
        assert!(!is_location_log_path("1ba/48f/SHA256E-s1--aa.log.met"));
        assert_eq!(
            location_log_key("1ba/48f/SHA256E-s1--aa.mp4.log"),
            Some("SHA256E-s1--aa.mp4")
        );
    }

    #[test]
    fn test_find_and_load_demo() {
        let p = Path::new("/tmp/annex-demo");
        if !is_annex_repo(p) {
            return;
        }
        let meta = load_metadata(p).expect("load demo");
        assert!(!meta.uuid.is_empty());
        assert!(!meta.remotes.is_empty());
    }

    #[test]
    fn parse_find_nul_triplets_uses_bytesize_and_key_fallback() {
        let mut buf = Vec::new();
        buf.extend(b"videos/big.mkv\0SHA256E-s50000--abcd.mkv\0");
        buf.extend(b"50000\0");
        buf.extend(b"photos/x.jpg\0SHA256E-s12--ef.jpg\0\0"); // empty bytesize -> key
        buf.extend(b"url.dat\0URL--http://x\0");
        buf.extend(b"0\0"); // 0 bytesize, no size in key
        let files = parse_find_nul_triplets(&buf);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "videos/big.mkv");
        assert_eq!(files[0].size, Some(50000));
        assert_eq!(files[1].path, "photos/x.jpg");
        assert_eq!(files[1].size, Some(12));
        assert_eq!(files[2].path, "url.dat");
        assert_eq!(files[2].size, None);
    }

    fn annex_available() -> bool {
        Command::new("git")
            .arg("annex")
            .arg("version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn chmod_u_write(path: &Path) {
        let _ = Command::new("chmod").args(["-R", "u+w"]).arg(path).status();
    }

    #[test]
    fn load_annexed_files_includes_dropped_and_missing_with_annex_size() {
        if !annex_available() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "gab-ncdu-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        mkdir(&root);
        let init = || {
            let git = |args: &[&str]| {
                Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(args)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
            };
            assert!(git(&["init", "-q"]).success());
            assert!(git(&["config", "user.email", "t@t"]).success());
            assert!(git(&["config", "user.name", "t"]).success());
            assert!(git(&["annex", "init", "-q", "testdemo"]).success());
            mkdir(&root.join("videos/clips"));
            mkdir(&root.join("photos"));
            std::fs::write(root.join("photos/small.jpg"), vec![b'x'; 1000]).unwrap();
            std::fs::write(root.join("videos/big.mkv"), vec![b'y'; 50000]).unwrap();
            std::fs::write(root.join("videos/clips/mid.bin"), vec![b'z'; 8000]).unwrap();
            assert!(git(&["annex", "add", "-q", "photos", "videos"]).success());
            assert!(git(&["commit", "-q", "-m", "add"]).success());
            assert!(git(&["annex", "drop", "--force", "-q", "videos/big.mkv"]).success());
            let _ = std::fs::remove_file(root.join("photos/small.jpg"));
        };
        init();
        let files = load_annexed_files(&root);
        let meta = load_metadata(&root);
        chmod_u_write(&root);
        let _ = std::fs::remove_dir_all(&root);
        let by_path: HashMap<_, _> = files.iter().map(|f| (f.path.as_str(), f)).collect();
        assert_eq!(
            by_path.len(),
            3,
            "files: {:?}",
            files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        assert_eq!(by_path["videos/big.mkv"].size, Some(50000));
        assert_eq!(by_path["photos/small.jpg"].size, Some(1000));
        assert_eq!(by_path["videos/clips/mid.bin"].size, Some(8000));
        let meta = meta.expect("load_metadata");
        assert_eq!(meta.files.len(), 3);
        let fp = meta.fingerprint.expect("fingerprint");
        assert_eq!(fp.annex_branch.len(), 40, "{fp:?}");
        assert_eq!(fp.head.len(), 40, "{fp:?}");
        // Two keys still here (small.jpg's content stays even though the symlink is gone).
        assert_eq!(meta.remotes[&meta.uuid].present_count, 2);
        assert_eq!(meta.description, "testdemo");
    }
}
