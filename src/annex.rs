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
    /// Filesystem free bytes for this drive (if we could determine a local path for it)
    /// Note: no longer displayed by default as it's not git-annex metadata.
    #[serde(default)]
    pub available_space: Option<u64>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub wanted: Option<String>,
    #[serde(default)]
    pub required: Option<String>,
}

impl Remote {
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
    pub version: Option<u32>,
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
}

/// Lightweight summary for fast top-level listing and caching.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub here_available_space: Option<u64>,
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
            here_available_space: None, // no longer populated
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

    /// Ensure size stats are populated (for old caches that didn't have them)
    pub fn ensure_sizes(&mut self) {
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
            let mut u = 0u64;
            let mut c = 0u64;
            for (key, uuids) in &self.locations {
                if uuids.is_empty() {
                    continue;
                }
                if let Some(&sz) = key_sizes.get(key) {
                    u += sz;
                    c += sz * (uuids.len() as u64);
                }
            }
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

pub fn is_annex_repo(path: &Path) -> bool {
    let Some(git_dir) = resolve_git_dir(path) else {
        return false;
    };
    if git_dir.join("annex").exists() {
        return true;
    }
    Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--verify")
        .arg("--quiet")
        .arg("git-annex^{commit}")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn find_annex_repos(root: &Path) -> Vec<PathBuf> {
    let mut repos = Vec::new();
    let mut it = walkdir::WalkDir::new(root).follow_links(false).into_iter();
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

/// Parse uuid.log. Last-write-wins per UUID using the line timestamp.
fn parse_uuid_log(text: &str) -> Vec<(String, String, Option<i64>)> {
    let mut latest: HashMap<String, (i64, String)> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (body, ts) = split_log_timestamp(line);
        let mut parts = body.splitn(2, ' ');
        let uuid = parts.next().unwrap_or("").to_string();
        if uuid.is_empty() {
            continue;
        }
        let desc = parts.next().unwrap_or("").trim().to_string();
        let desc = if desc.is_empty() { uuid.clone() } else { desc };
        let ts = ts.unwrap_or(0);
        match latest.get(&uuid) {
            Some((old, _)) if ts < *old => {}
            _ => {
                latest.insert(uuid, (ts, desc));
            }
        }
    }
    latest
        .into_iter()
        .map(|(uuid, (ts, desc))| (uuid, desc, Some(ts)))
        .collect()
}

fn parse_remote_log(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut latest: HashMap<String, (i64, HashMap<String, String>)> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (body, ts) = split_log_timestamp(line);
        let mut it = body.split_whitespace();
        let Some(uuid) = it.next().filter(|u| !u.is_empty()) else {
            continue;
        };
        let mut cfg = HashMap::new();
        for tok in it {
            if let Some((k, v)) = tok.split_once('=') {
                if is_secret_remote_key(k) {
                    cfg.insert(k.to_string(), "[redacted]".to_string());
                } else {
                    cfg.insert(k.to_string(), v.to_string());
                }
            }
        }
        let ts = ts.unwrap_or(0);
        match latest.get(uuid) {
            Some((old, _)) if ts < *old => {}
            _ => {
                latest.insert(uuid.to_string(), (ts, cfg));
            }
        }
    }
    latest.into_iter().map(|(u, (_, cfg))| (u, cfg)).collect()
}

fn parse_trust_log(text: &str) -> HashMap<String, TrustLevel> {
    let mut latest: HashMap<String, (i64, TrustLevel)> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (body, ts) = split_log_timestamp(line);
        let mut parts = body.split_whitespace();
        let Some(uuid) = parts.next().filter(|u| !u.is_empty()) else {
            continue;
        };
        let flag = parts.next().unwrap_or("?");
        let lvl = TrustLevel::from_token(flag);
        let ts = ts.unwrap_or(0);
        match latest.get(uuid) {
            Some((old, _)) if ts < *old => {}
            _ => {
                latest.insert(uuid.to_string(), (ts, lvl));
            }
        }
    }
    latest.into_iter().map(|(u, (_, t))| (u, t)).collect()
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
    let mut latest: HashMap<String, (i64, Vec<String>)> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (body, ts) = split_log_timestamp(line);
        let mut parts = body.split_whitespace();
        let Some(uuid) = parts.next().filter(|u| !u.is_empty()) else {
            continue;
        };
        let mut groups: Vec<String> = parts
            .filter(|g| !g.is_empty() && !g.starts_with("timestamp="))
            .map(|g| g.to_string())
            .collect();
        groups.sort();
        groups.dedup();
        let ts = ts.unwrap_or(0);
        match latest.get(uuid) {
            Some((old, _)) if ts < *old => {}
            _ => {
                latest.insert(uuid.to_string(), (ts, groups));
            }
        }
    }
    latest.into_iter().map(|(u, (_, g))| (u, g)).collect()
}

fn parse_content_log(text: &str) -> HashMap<String, String> {
    // preferred-content.log / required-content.log:
    // uuid <expression> timestamp=...
    let mut latest: HashMap<String, (i64, String)> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (body, ts) = split_log_timestamp(line);
        let mut parts = body.splitn(2, ' ');
        let Some(uuid) = parts.next().filter(|u| !u.is_empty()) else {
            continue;
        };
        let expr = parts.next().unwrap_or("").trim().to_string();
        let ts = ts.unwrap_or(0);
        match latest.get(uuid) {
            Some((old, _)) if ts < *old => {}
            _ => {
                latest.insert(uuid.to_string(), (ts, expr));
            }
        }
    }
    latest.into_iter().map(|(u, (_, e))| (u, e)).collect()
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

/// Presence map from git-annex branch location logs (includes untrusted remotes).
fn load_locations_from_branch(root: &Path) -> HashMap<String, HashSet<String>> {
    let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-tree", "-r", "-z", "git-annex"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    else {
        return HashMap::new();
    };
    if !out.status.success() {
        return HashMap::new();
    }

    let mut shas = Vec::new();
    let mut keys = Vec::new();
    for entry in out.stdout.split(|&b| b == 0) {
        if entry.is_empty() {
            continue;
        }
        let Ok(s) = std::str::from_utf8(entry) else {
            continue;
        };
        let Some((meta, path)) = s.split_once('\t') else {
            continue;
        };
        if !is_location_log_path(path) {
            continue;
        }
        let Some(key) = location_log_key(path) else {
            continue;
        };
        let Some(sha) = meta.split_whitespace().nth(2) else {
            continue;
        };
        shas.push(sha.to_string());
        keys.push(key.to_string());
    }
    if shas.is_empty() {
        return HashMap::new();
    }

    cat_file_location_logs(root, shas, keys)
}

/// `git cat-file --batch` the location-log blobs. A writer thread avoids
/// deadlock when the pipe buffer fills before we start reading.
fn cat_file_location_logs(
    root: &Path,
    shas: Vec<String>,
    keys: Vec<String>,
) -> HashMap<String, HashSet<String>> {
    let mut child = match Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    let Some(mut stdin) = child.stdin.take() else {
        return HashMap::new();
    };
    let Some(stdout) = child.stdout.take() else {
        return HashMap::new();
    };

    let writer = std::thread::spawn(move || {
        for sha in shas {
            if stdin.write_all(sha.as_bytes()).is_err() || stdin.write_all(b"\n").is_err() {
                break;
            }
        }
    });

    let mut reader = BufReader::new(stdout);
    let mut locations = HashMap::with_capacity(keys.len());
    let mut header = String::new();
    let mut nl = [0u8; 1];
    for key in keys {
        header.clear();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            break;
        }
        let header_trim = header.trim_end();
        if header_trim.ends_with("missing") {
            continue;
        }
        let size: usize = header_trim
            .split_whitespace()
            .nth(2)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let mut buf = vec![0u8; size];
        if size > 0 && reader.read_exact(&mut buf).is_err() {
            break;
        }
        if reader.read_exact(&mut nl).is_err() {
            break;
        }
        let uuids = parse_location_log(&String::from_utf8_lossy(&buf));
        if !uuids.is_empty() {
            locations.insert(key, uuids);
        }
    }
    drop(reader);
    let _ = child.wait();
    let _ = writer.join();
    locations
}

/// mtime of the git-annex branch ref (or packed-refs). Used to hydrate recently
/// updated repos first so Glacier copies show up without waiting for the whole scan.
pub fn annex_branch_mtime(root: &Path) -> Option<i64> {
    let git_dir = resolve_git_dir(root)?;
    let candidates = [
        git_dir.join("refs/heads/git-annex"),
        git_dir.join("packed-refs"),
    ];
    let mut best: Option<i64> = None;
    for p in candidates {
        if let Ok(meta) = std::fs::metadata(p)
            && let Ok(modified) = meta.modified()
            && let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            let secs = d.as_secs() as i64;
            best = Some(best.map_or(secs, |b| b.max(secs)));
        }
    }
    best
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

fn is_secret_remote_key(k: &str) -> bool {
    matches!(
        k,
        "cipher" | "embedcreds" | "encryptionkey" | "secret" | "password" | "keyid"
    )
}

#[derive(Debug, Deserialize)]
struct WhereisRemoteJson {
    #[serde(default)]
    uuid: String,
}

#[derive(Debug, Deserialize)]
struct WhereisJson {
    key: Option<String>,
    #[serde(default)]
    whereis: Vec<WhereisRemoteJson>,
    #[serde(default)]
    untrusted: Vec<WhereisRemoteJson>,
}

impl WhereisJson {
    fn uuids(&self) -> HashSet<String> {
        self.whereis
            .iter()
            .chain(self.untrusted.iter())
            .map(|r| r.uuid.as_str())
            .filter(|u| !u.is_empty())
            .map(|u| u.to_string())
            .collect()
    }
}

fn parse_whereis_json_lines(stdout: &str) -> HashMap<String, HashSet<String>> {
    let mut locations = HashMap::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<WhereisJson>(line)
            && let Some(key) = val.key.clone()
        {
            locations.insert(key, val.uuids());
        }
    }
    locations
}

/// Query live locations for a specific key using git annex whereis --json.
/// This can be used as fallback when the batch whereis at load time didn't have the record.
pub fn get_live_locations_for_key(repo: &Path, key: &str) -> Result<HashSet<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("annex")
        .arg("whereis")
        .arg("--json")
        .arg(format!("--key={key}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut present = HashSet::new();
    for locs in parse_whereis_json_lines(&stdout).into_values() {
        present.extend(locs);
    }
    Ok(present)
}

/// Load full metadata for one annex repo. May be slow on huge annex; called on worker.
pub fn load_metadata(repo: &Path) -> Result<AnnexMetadata> {
    let root = repo.to_path_buf();

    // Basic config
    let uuid = run_git(&root, &["config", "--get", "annex.uuid"])
        .unwrap_or_default()
        .trim()
        .to_string();
    let desc = run_git(&root, &["config", "--get", "annex.describe"])
        .unwrap_or_default()
        .trim()
        .to_string();
    let numcopies = run_git(&root, &["config", "--get", "annex.numcopies"])
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .or_else(|| {
            run_git(&root, &["show", "git-annex:numcopies.log"])
                .ok()
                .and_then(|t| parse_count_log(&t))
        });

    // Read logs from git-annex branch
    let uuid_log = run_git(&root, &["show", "git-annex:uuid.log"]).unwrap_or_default();
    let remote_log = run_git(&root, &["show", "git-annex:remote.log"]).unwrap_or_default();
    let trust_log = run_git(&root, &["show", "git-annex:trust.log"]).unwrap_or_default();
    let activity_log = run_git(&root, &["show", "git-annex:activity.log"]).unwrap_or_default();
    let group_log = run_git(&root, &["show", "git-annex:group.log"]).unwrap_or_default();
    let preferred_log =
        run_git(&root, &["show", "git-annex:preferred-content.log"]).unwrap_or_default();
    let required_log =
        run_git(&root, &["show", "git-annex:required-content.log"]).unwrap_or_default();

    let uuid_entries = parse_uuid_log(&uuid_log);
    let remote_cfgs = parse_remote_log(&remote_log);
    let trusts = parse_trust_log(&trust_log);
    let fscks = parse_activity_log(&activity_log);
    let groups_map = parse_group_log(&group_log);
    let wanted_map = parse_content_log(&preferred_log);
    let required_map = parse_content_log(&required_log);

    // Build remotes map. Start from uuid.log entries + remotes
    let mut remotes: HashMap<String, Remote> = HashMap::new();

    for (u, d, _ts) in &uuid_entries {
        let cfg = remote_cfgs.get(u).cloned().unwrap_or_default();
        let trust = trusts.get(u).copied().unwrap_or(TrustLevel::SemiTrusted);
        let last_fsck = fscks.get(u).copied();
        let description = if d == u && cfg.contains_key("name") {
            cfg.get("name").unwrap().clone()
        } else if d.is_empty() {
            u.clone()
        } else {
            d.clone()
        };
        remotes.insert(
            u.clone(),
            Remote {
                uuid: u.clone(),
                description,
                config: cfg,
                trust,
                last_fsck,
                present_count: 0,
                present_size: 0,
                available_space: None,
                groups: vec![],
                wanted: None,
                required: None,
            },
        );
    }

    // Ensure any only-in-remote.log are present
    for (u, cfg) in &remote_cfgs {
        if !remotes.contains_key(u) {
            let trust = trusts.get(u).copied().unwrap_or(TrustLevel::SemiTrusted);
            let last_fsck = fscks.get(u).copied();
            let description = cfg.get("name").cloned().unwrap_or_else(|| u.clone());
            remotes.insert(
                u.clone(),
                Remote {
                    uuid: u.clone(),
                    description,
                    config: cfg.clone(),
                    trust,
                    last_fsck,
                    present_count: 0,
                    present_size: 0,
                    available_space: None,
                    groups: vec![],
                    wanted: None,
                    required: None,
                },
            );
        }
    }

    // Add the local "here" if missing (from config)
    if !uuid.is_empty() && !remotes.contains_key(&uuid) {
        let mut cfg = HashMap::new();
        cfg.insert("name".to_string(), "here".to_string());
        remotes.insert(
            uuid.clone(),
            Remote {
                uuid: uuid.clone(),
                description: if desc.is_empty() {
                    "here".to_string()
                } else {
                    desc.clone()
                },
                config: cfg,
                trust: trusts
                    .get(&uuid)
                    .copied()
                    .unwrap_or(TrustLevel::SemiTrusted),
                last_fsck: fscks.get(&uuid).copied(),
                present_count: 0,
                present_size: 0,
                available_space: None,
                groups: vec![],
                wanted: None,
                required: None,
            },
        );
    }

    // Assign groups / wanted / required to remotes (including here)
    for (u, r) in remotes.iter_mut() {
        if let Some(gs) = groups_map.get(u) {
            let mut gs = gs.clone();
            gs.sort();
            r.groups = gs;
        }
        r.wanted = wanted_map.get(u).cloned();
        r.required = required_map.get(u).cloned();
    }

    // Collect additional configurations (numcopies, gitattributes etc.)
    let mut additional_configs = vec![];
    if let Some(n) = numcopies {
        additional_configs.push(format!("annex.numcopies={}", n));
    }
    // Parse top-level .gitattributes for annex.* settings (numcopies per path etc.)
    let ga_path = root.join(".gitattributes");
    if ga_path.exists()
        && let Ok(content) = std::fs::read_to_string(&ga_path)
    {
        for line in content.lines() {
            let l = line.trim();
            if l.contains("annex.numcopies") || l.contains("annex.") {
                additional_configs.push(format!(".gitattributes: {}", l));
            }
        }
    }
    // Also pull other annex.* config for visibility
    if let Ok(cfg_list) = run_git(&root, &["config", "--get-regexp", "^annex\\."]) {
        for line in cfg_list.lines() {
            if !line.contains("numcopies") && !line.contains("uuid") && !line.contains("describe") {
                additional_configs.push(line.to_string());
            }
        }
    }

    // Fill drive space information for any remotes that have a resolvable local path
    // (kept for internal use / old caches, but not shown in UI by default)
    fill_drive_spaces(&root, &uuid, &mut remotes);

    // Load files + keys + locations
    let annexed_paths = {
        let out = Command::new("git")
            .arg("-C")
            .arg(&root)
            .arg("annex")
            .arg("find")
            .arg("--print0")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()?;
        if out.status.success() {
            let mut v = vec![];
            for p in out.stdout.split(|&b| b == 0) {
                if !p.is_empty() {
                    v.push(String::from_utf8_lossy(p).to_string());
                }
            }
            v
        } else {
            vec![]
        }
    };

    // Batch lookup keys for paths
    let mut files: Vec<AnnexedFile> = vec![];
    if !annexed_paths.is_empty() {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&root)
            .arg("annex")
            .arg("lookupkey")
            .arg("--batch")
            .arg("-z")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        {
            let mut stdin = child.stdin.take().unwrap();
            for p in &annexed_paths {
                stdin.write_all(p.as_bytes())?;
                stdin.write_all(&[0])?;
            }
        }
        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout);
        for (i, line_res) in reader.lines().enumerate() {
            if let Ok(key) = line_res
                && i < annexed_paths.len()
            {
                let path = annexed_paths[i].clone();
                let size = parse_size_from_key(&key);
                files.push(AnnexedFile { path, key, size });
            }
        }
        let _ = child.wait();
    }

    // Presence from git-annex branch location logs (includes untrusted remotes
    // such as Glacier). `whereis --json --all` is too slow on large annexes and
    // left used-storage figures stale after `copy --to` Glacier.
    let locations = load_locations_from_branch(&root);

    let total_keys = locations.len().max(files.len());
    let key_sizes = collect_key_sizes(&files, &locations);
    apply_present_stats(&mut remotes, &locations, &key_sizes);

    let mut unique_size = 0u64;
    let mut consumed_size = 0u64;
    for (key, uuids) in &locations {
        if uuids.is_empty() {
            continue;
        }
        if let Some(&sz) = key_sizes.get(key) {
            unique_size += sz;
            consumed_size += sz * (uuids.len() as u64);
        }
    }

    // Fill local desc if empty
    let description = if desc.is_empty() {
        uuid_entries
            .iter()
            .find(|(u, _d, _)| u == &uuid)
            .map(|(_, d, _)| d.clone())
            .unwrap_or_else(|| uuid.clone())
    } else {
        desc
    };

    Ok(AnnexMetadata {
        root,
        uuid,
        description,
        version: None,
        numcopies,
        additional_configs,
        remotes,
        locations,
        files,
        total_keys,
        unique_size,
        consumed_size,
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
        .unwrap_or_else(|| uuid[..8.min(uuid.len())].to_string())
}

/// Best effort local filesystem path for a remote (for space queries).
fn remote_drive_path(rem: &Remote, repo_root: &Path, here_uuid: &str) -> Option<PathBuf> {
    if let Some(dir) = rem.config.get("directory") {
        let p = PathBuf::from(dir);
        // Only return if it currently exists (drive may be unmounted)
        if p.exists() {
            return Some(p);
        }
    }
    if rem.uuid == here_uuid {
        return Some(repo_root.to_path_buf());
    }
    None
}

/// Fill available_space for drives that map to local directories.
fn fill_drive_spaces(repo_root: &Path, here_uuid: &str, remotes: &mut HashMap<String, Remote>) {
    for r in remotes.values_mut() {
        if let Some(p) = remote_drive_path(r, repo_root, here_uuid)
            && let Some((avail, _total)) = get_fs_space(&p)
        {
            r.available_space = Some(avail);
        }
    }
}

/// Query filesystem available and total bytes for a path using statvfs (Linux).
#[cfg(unix)]
pub fn get_fs_space(path: &Path) -> Option<(u64, u64)> {
    use libc::statvfs;
    use std::ffi::CString;
    let cpath = CString::new(path.to_str()?).ok()?;
    let mut st: statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: cpath is valid nul-terminated, st is properly sized.
    if unsafe { statvfs(cpath.as_ptr(), &mut st) } == 0 {
        let bsize = st.f_frsize as u64; // or f_bsize on some systems
        let avail = (st.f_bavail as u64).saturating_mul(bsize);
        let total = (st.f_blocks as u64).saturating_mul(bsize);
        Some((avail, total))
    } else {
        None
    }
}

#[cfg(not(unix))]
pub fn get_fs_space(_path: &Path) -> Option<(u64, u64)> {
    None
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
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            anyhow::bail!("could not lock cache");
        }
    }
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

#[allow(dead_code)]
pub fn save_cache(cache: &AnnexCache) -> Result<()> {
    let _lock = lock_cache()?;
    write_cache_unlocked(cache)
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
        let laptop = v
            .iter()
            .find(|(u, _, _)| u.starts_with("e605dca6"))
            .unwrap();
        assert_eq!(laptop.1, "new laptop name");
        let usb = v
            .iter()
            .find(|(u, _, _)| u.starts_with("26339d22"))
            .unwrap();
        assert_eq!(usb.1, "usb disk");
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

    #[test]
    fn whereis_json_includes_untrusted() {
        let line = r#"{"key":"SHA256E-s1--aa","whereis":[{"uuid":"here-uuid"}],"untrusted":[{"uuid":"usb-uuid"}]}"#;
        let locs = parse_whereis_json_lines(line);
        let set = locs.get("SHA256E-s1--aa").unwrap();
        assert!(set.contains("here-uuid"));
        assert!(set.contains("usb-uuid"));
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

        let found = find_annex_repos(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found.len(), 2, "found {found:?}");
        assert!(
            found
                .iter()
                .all(|p| p.ends_with("photos") || p.ends_with("docs"))
        );
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
        let found = find_annex_repos(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert!(ok);
        assert!(found.iter().any(|p| p.ends_with("tree")), "found {found:?}");
    }

    fn dummy_meta(root: &str) -> AnnexMetadata {
        AnnexMetadata {
            root: PathBuf::from(root),
            uuid: "u".into(),
            description: String::new(),
            version: None,
            numcopies: None,
            additional_configs: vec![],
            remotes: HashMap::new(),
            locations: HashMap::new(),
            files: vec![],
            total_keys: 0,
            unique_size: 0,
            consumed_size: 0,
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
                root: PathBuf::from("/r"),
                uuid: String::new(),
                name: "r".into(),
                annex_description: String::new(),
                file_count: 0,
                remote_count: usage.len(),
                here_present_count: 0,
                here_available_space: None,
                unique_size: 0,
                consumed_size: 0,
                remote_usage: usage,
                numcopies: None,
                keys_tracked: 0,
                keys_under: 0,
                keys_ok: 0,
                keys_over: 0,
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
                root: PathBuf::from("/r"),
                uuid: String::new(),
                name: "r".into(),
                annex_description: String::new(),
                file_count: 0,
                remote_count: usage.len(),
                here_present_count: 0,
                here_available_space: None,
                unique_size: 0,
                consumed_size: 0,
                remote_usage: usage,
                numcopies: None,
                keys_tracked: 0,
                keys_under: 0,
                keys_ok: 0,
                keys_over: 0,
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
                Remote {
                    uuid: uuid.into(),
                    description: uuid.into(),
                    config: HashMap::new(),
                    trust,
                    last_fsck: None,
                    present_count: 0,
                    present_size: 0,
                    available_space: None,
                    groups: vec![],
                    wanted: None,
                    required: None,
                },
            );
        }
        // numcopies=1: only-glacier is under (untrusted doesn't count), others ok/over
        assert_eq!(copy_health_counts(&loc, 1, &remotes), (1, 1, 1));
        assert_eq!(copy_health_counts(&loc, 2, &remotes), (2, 1, 0));
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
}
