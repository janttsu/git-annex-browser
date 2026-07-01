//! git-annex metadata parsing and data structures.
//! Pure access via `git` CLI (no scraping of user-facing commands where possible).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::io::{BufRead, BufReader, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    Trusted,
    SemiTrusted,
    UnTrusted,
    Dead,
}

impl TrustLevel {
    pub fn from_char(c: char) -> Self {
        match c {
            'T' | 't' => TrustLevel::Trusted,
            'U' | 'u' => TrustLevel::UnTrusted,
            'D' | 'd' => TrustLevel::Dead,
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
        self.config.get("name")
            .map(|s| s.as_str())
            .unwrap_or(&self.description)
    }
    pub fn rtype(&self) -> &str {
        self.config.get("type").map(|s| s.as_str()).unwrap_or("repo")
    }
    pub fn is_special(&self) -> bool {
        self.config.contains_key("type")
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
}

/// Lightweight summary for fast top-level listing and caching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoSummary {
    pub root: PathBuf,
    pub uuid: String,
    /// Clean name for display, preferably the directory basename (e.g. "archive-photos-annex")
    #[serde(default)]
    pub name: String,
    /// The annex internal description (often "orca" or similar on your machines)
    #[serde(default)]
    pub annex_description: String,
    pub file_count: usize,
    pub remote_count: usize,
    pub here_present_count: usize,
    pub here_available_space: Option<u64>,
}

impl AnnexMetadata {
    pub fn to_summary(&self) -> RepoSummary {
        let here_present = self.remotes.get(&self.uuid).map(|r| r.present_count).unwrap_or(0);
        let name = self.root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.description.clone());
        RepoSummary {
            root: self.root.clone(),
            uuid: self.uuid.clone(),
            name,
            annex_description: self.description.clone(),
            file_count: self.files.len(),
            remote_count: self.remotes.len(),
            here_present_count: here_present,
            here_available_space: None, // no longer populated
        }
    }
}

impl RepoSummary {
    /// Ensure we have a usable display name (for old caches or minimal entries)
    pub fn ensure_name(&mut self) {
        if self.name.is_empty() {
            self.name = self.root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| self.annex_description.clone());
        }
    }
}

pub fn is_annex_repo(path: &Path) -> bool {
    // Has .git/annex or git-annex branch
    let git_dir = if path.join(".git").is_dir() {
        path.join(".git")
    } else if path.join(".git").is_file() {
        // worktree or submodule etc, skip for simplicity or resolve
        return false;
    } else {
        return false;
    };
    if git_dir.join("annex").exists() {
        return true;
    }
    // Check for git-annex branch without side effect
    Command::new("git")
        .arg("-C").arg(path)
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
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // prune obvious heavy / non-repo dirs; still allow .git to detect
            let name = e.file_name().to_string_lossy().to_string();
            if name == ".git" { return true; }
            if e.depth() <= 2 { return true; }
            !name.starts_with('.') && name != "target" && name != "node_modules"
        })
    {
        if let Ok(e) = entry {
            if e.file_type().is_dir() && e.file_name() == ".git" {
                let repo_root = e.path().parent().unwrap().to_path_buf();
                if is_annex_repo(&repo_root) {
                    repos.push(repo_root);
                }
            }
        }
    }
    repos.sort();
    repos
}

fn run_git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C").arg(repo)
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

fn run_git_lines(repo: &Path, args: &[&str]) -> Result<Vec<String>> {
    let s = run_git(repo, args)?;
    Ok(s.lines().map(|l| l.to_string()).collect())
}

/// Parse a simple space separated log like "uuid desc timestamp=123s"
fn parse_uuid_log(text: &str) -> Vec<(String, String, Option<i64>)> {
    let mut out = vec![];
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let mut parts = line.splitn(2, ' ');
        let uuid = parts.next().unwrap_or("").to_string();
        let rest = parts.next().unwrap_or("");
        let mut desc = rest.to_string();
        let mut ts = None;
        if let Some(pos) = rest.find("timestamp=") {
            desc = rest[..pos].trim().to_string();
            let ts_part = &rest[pos + 10..];
            if let Some(end) = ts_part.find('s') {
                if let Ok(t) = ts_part[..end].parse::<i64>() {
                    ts = Some(t);
                }
            }
        }
        if !uuid.is_empty() {
            let d = if desc.is_empty() { uuid.clone() } else { desc };
            out.push((uuid, d, ts));
        }
    }
    out
}

fn parse_remote_log(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut map: HashMap<String, HashMap<String, String>> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let mut it = line.split_whitespace();
        let uuid = it.next().unwrap_or("").to_string();
        if uuid.is_empty() { continue; }
        let mut cfg = HashMap::new();
        for tok in it {
            if let Some((k, v)) = tok.split_once('=') {
                cfg.insert(k.to_string(), v.to_string());
            }
        }
        map.insert(uuid, cfg);
    }
    map
}

fn parse_trust_log(text: &str) -> HashMap<String, TrustLevel> {
    let mut m = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let mut parts = line.split_whitespace();
        let uuid = parts.next().unwrap_or("").to_string();
        let flag = parts.next().unwrap_or("?");
        let lvl = TrustLevel::from_char(flag.chars().next().unwrap_or('?'));
        if !uuid.is_empty() {
            m.insert(uuid, lvl);
        }
    }
    m
}

fn parse_activity_log(text: &str) -> HashMap<String, i64> {
    // lines like: UUID Fsck timestamp=UNIXs
    let mut m = HashMap::new();
    for line in text.lines() {
        if let Some(uuid) = line.split_whitespace().next() {
            if let Some(pos) = line.find("Fsck timestamp=") {
                let ts_str = &line[pos + 15..];
                if let Some(end) = ts_str.find('s') {
                    if let Ok(ts) = ts_str[..end].parse::<i64>() {
                        m.insert(uuid.to_string(), ts);
                    }
                }
            }
        }
    }
    m
}

fn parse_group_log(text: &str) -> HashMap<String, Vec<String>> {
    // group.log format: UUID groupname [timestamp=UNIX.s]
    // We need the *current* groups, not historical ones.
    // Strategy: for each (uuid, group) keep the highest timestamp.
    // Then for each uuid take the groups that have the overall max timestamp for that uuid.
    let mut by_uuid: HashMap<String, HashMap<String, i64>> = HashMap::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }

        // Split off the timestamp part if present
        let (before_ts, ts_str) = if let Some(pos) = line.find(" timestamp=") {
            (&line[0..pos], &line[pos + 11..])
        } else {
            (line, "")
        };

        let mut parts = before_ts.split_whitespace();
        let uuid = parts.next().unwrap_or("").to_string();
        let group = parts.next().unwrap_or("").to_string();

        if uuid.is_empty() || group.is_empty() || group.starts_with("timestamp=") {
            continue;
        }

        // Parse timestamp (e.g. 1699273888.593667289s)
        let ts = if let Some(end) = ts_str.find('s') {
            ts_str[..end].parse::<i64>().unwrap_or(0)
        } else {
            0
        };

        let groups = by_uuid.entry(uuid).or_default();
        let current = groups.get(&group).copied().unwrap_or(-1);
        if ts > current {
            groups.insert(group, ts);
        }
    }

    // Now reduce to current groups per uuid (those sharing the max timestamp)
    let mut result: HashMap<String, Vec<String>> = HashMap::new();
    for (uuid, group_ts) in by_uuid {
        if group_ts.is_empty() {
            continue;
        }
        let max_ts = *group_ts.values().max().unwrap();
        let current_groups: Vec<String> = group_ts
            .into_iter()
            .filter(|(_, t)| *t == max_ts)
            .map(|(g, _)| g)
            .collect();
        result.insert(uuid, current_groups);
    }
    result
}

fn parse_content_log(text: &str) -> HashMap<String, String> {
    // preferred-content.log / required-content.log format:
    // uuid <expression> [timestamp=...]
    let mut m = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let mut parts = line.splitn(2, ' ');
        let uuid = parts.next().unwrap_or("").to_string();
        let mut expr = parts.next().unwrap_or("").to_string();
        if let Some(pos) = expr.find(" timestamp=") {
            expr = expr[..pos].trim().to_string();
        }
        if !uuid.is_empty() {
            m.insert(uuid, expr);
        }
    }
    m
}

fn extract_key_from_pointer(ptr: &str) -> Option<String> {
    // e.g. .git/annex/objects/xx/yy/KEY/KEY  -> KEY
    let p = ptr.trim();
    if let Some(last) = p.rsplit('/').next() {
        if last.contains("--") || last.starts_with("SHA") || last.starts_with("WORM") || last.starts_with("MD5") {
            return Some(last.to_string());
        }
    }
    None
}

pub fn parse_size_from_key(key: &str) -> Option<u64> {
    // SHA256E-s12345-...
    if let Some(s_pos) = key.find("-s") {
        let rest = &key[s_pos + 2..];
        if let Some(end) = rest.find('-').or_else(|| rest.find("--")) {
            rest[..end].parse::<u64>().ok()
        } else {
            rest.parse::<u64>().ok()
        }
    } else {
        None
    }
}

/// Query live locations for a specific key using git annex whereis --json.
/// This can be used as fallback when the batch whereis at load time didn't have the record.
pub fn get_live_locations_for_key(repo: &Path, key: &str) -> Result<HashSet<String>> {
    let output = Command::new("git")
        .arg("-C").arg(repo)
        .arg("annex").arg("whereis").arg("--json").arg(key)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()?;
    let mut present = HashSet::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(whereis_arr) = val.get("whereis").and_then(|w| w.as_array()) {
                for item in whereis_arr {
                    if let Some(u) = item.get("uuid").and_then(|u| u.as_str()) {
                        if !u.is_empty() {
                            present.insert(u.to_string());
                        }
                    }
                }
            }
        }
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
        .and_then(|s| s.trim().parse::<u32>().ok());

    // Read logs from git-annex branch
    let uuid_log = run_git(&root, &["show", "git-annex:uuid.log"]).unwrap_or_default();
    let remote_log = run_git(&root, &["show", "git-annex:remote.log"]).unwrap_or_default();
    let trust_log = run_git(&root, &["show", "git-annex:trust.log"]).unwrap_or_default();
    let activity_log = run_git(&root, &["show", "git-annex:activity.log"]).unwrap_or_default();
    let group_log = run_git(&root, &["show", "git-annex:group.log"]).unwrap_or_default();
    let preferred_log = run_git(&root, &["show", "git-annex:preferred-content.log"]).unwrap_or_default();
    let required_log = run_git(&root, &["show", "git-annex:required-content.log"]).unwrap_or_default();

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
        } else if d.is_empty() { u.clone() } else { d.clone() };
        remotes.insert(u.clone(), Remote {
            uuid: u.clone(),
            description,
            config: cfg,
            trust,
            last_fsck,
            present_count: 0,
            available_space: None,
            groups: vec![],
            wanted: None,
            required: None,
        });
    }

    // Ensure any only-in-remote.log are present
    for (u, cfg) in &remote_cfgs {
        if !remotes.contains_key(u) {
            let trust = trusts.get(u).copied().unwrap_or(TrustLevel::SemiTrusted);
            let last_fsck = fscks.get(u).copied();
            let description = cfg.get("name").cloned().unwrap_or_else(|| u.clone());
            remotes.insert(u.clone(), Remote {
                uuid: u.clone(),
                description,
                config: cfg.clone(),
                trust,
                last_fsck,
                present_count: 0,
                available_space: None,
                groups: vec![],
                wanted: None,
                required: None,
            });
        }
    }

    // Add the local "here" if missing (from config)
    if !uuid.is_empty() && !remotes.contains_key(&uuid) {
        let mut cfg = HashMap::new();
        cfg.insert("name".to_string(), "here".to_string());
        remotes.insert(uuid.clone(), Remote {
            uuid: uuid.clone(),
            description: if desc.is_empty() { "here".to_string() } else { desc.clone() },
            config: cfg,
            trust: trusts.get(&uuid).copied().unwrap_or(TrustLevel::SemiTrusted),
            last_fsck: fscks.get(&uuid).copied(),
            present_count: 0,
            available_space: None,
            groups: vec![],
            wanted: None,
            required: None,
        });
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
    if ga_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&ga_path) {
            for line in content.lines() {
                let l = line.trim();
                if l.contains("annex.numcopies") || l.contains("annex.") {
                    additional_configs.push(format!(".gitattributes: {}", l));
                }
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
            .arg("-C").arg(&root)
            .arg("annex").arg("find").arg("--print0")
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
            .arg("-C").arg(&root)
            .arg("annex").arg("lookupkey").arg("--batch")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        {
            let mut stdin = child.stdin.take().unwrap();
            for p in &annexed_paths {
                writeln!(stdin, "{}", p)?;
            }
        }
        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout);
        for (i, line_res) in reader.lines().enumerate() {
            if let Ok(key) = line_res {
                if i < annexed_paths.len() {
                    let path = annexed_paths[i].clone();
                    let size = parse_size_from_key(&key);
                    files.push(AnnexedFile { path, key, size });
                }
            }
        }
        let _ = child.wait();
    }

    // Build locations using whereis --json --all  (NDJSON)
    let mut locations: HashMap<String, HashSet<String>> = HashMap::new();
    let whereis_out = Command::new("git")
        .arg("-C").arg(&root)
        .arg("annex").arg("whereis").arg("--json").arg("--all")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()?;
    // Always try to parse stdout. git annex whereis --json --all can succeed in outputting
    // useful JSON even if the overall command exits non-zero (warnings, etc.).
    let stdout = String::from_utf8_lossy(&whereis_out.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        // Use proper JSON parsing for speed and correctness on large outputs
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(key) = val.get("key").and_then(|k| k.as_str()) {
                let mut present = HashSet::new();
                if let Some(whereis_arr) = val.get("whereis").and_then(|w| w.as_array()) {
                    for item in whereis_arr {
                        if let Some(u) = item.get("uuid").and_then(|u| u.as_str()) {
                            if !u.is_empty() {
                                present.insert(u.to_string());
                            }
                        }
                    }
                }
                locations.insert(key.to_string(), present);
            }
        }
    }
    if !whereis_out.status.success() {
        // We still parsed what we could. Log status is ignored for robustness.
    }

    // Compute present counts
    let mut present_counts: HashMap<String, usize> = HashMap::new();
    for (_k, us) in &locations {
        for u in us {
            *present_counts.entry(u.clone()).or_default() += 1;
        }
    }
    for (u, r) in remotes.iter_mut() {
        r.present_count = present_counts.get(u).copied().unwrap_or(0);
    }

    let total_keys = locations.len().max(files.len());

    // Fill local desc if empty
    let description = if desc.is_empty() {
        uuid_entries.iter().find(|(u,_d,_)| u == &uuid).map(|(_,d,_)| d.clone()).unwrap_or_else(|| uuid.clone())
    } else { desc };

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
    })
}

/// Return a short human name for a UUID (prefers name in config or desc)
pub fn short_name(meta: &AnnexMetadata, uuid: &str) -> String {
    if uuid == meta.uuid {
        return "here".to_string();
    }
    meta.remotes.get(uuid)
        .map(|r| {
            if r.name() != r.uuid { r.name().to_string() } else { r.description.clone() }
        })
        .unwrap_or_else(|| uuid[..8.min(uuid.len())].to_string())
}

/// Best effort local filesystem path for a remote (for space queries).
fn remote_drive_path(rem: &Remote, repo_root: &Path, here_uuid: &str) -> Option<PathBuf> {
    if let Some(dir) = rem.config.get("directory") {
        let p = PathBuf::from(dir);
        // Only return if it currently exists (drive may be unmounted)
        if p.exists() { return Some(p); }
    }
    if rem.uuid == here_uuid {
        return Some(repo_root.to_path_buf());
    }
    None
}

/// Fill available_space for drives that map to local directories.
fn fill_drive_spaces(repo_root: &Path, here_uuid: &str, remotes: &mut HashMap<String, Remote>) {
    for r in remotes.values_mut() {
        if let Some(p) = remote_drive_path(r, repo_root, here_uuid) {
            if let Some((avail, _total)) = get_fs_space(&p) {
                r.available_space = Some(avail);
            }
        }
    }
}

/// Query filesystem available and total bytes for a path using statvfs (Linux).
pub fn get_fs_space(path: &Path) -> Option<(u64, u64)> {
    use std::ffi::CString;
    use libc::statvfs;
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
        self.group_sets.iter().max_by_key(|(_, c)| *c).map(|(g, _)| g.clone())
    }
    pub fn most_common_wanted(&self) -> Option<String> {
        self.wanteds.iter()
            .filter(|(w, _)| w.is_some())
            .max_by_key(|(_, c)| *c)
            .and_then(|(w, _)| w.clone())
    }
    pub fn most_common_required(&self) -> Option<String> {
        self.requireds.iter()
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

pub fn cache_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".cache")
        .join("git-annex-browser")
        .join("cache.json")
}

pub fn load_cache() -> Option<AnnexCache> {
    let p = cache_path();
    let data = std::fs::read_to_string(&p).ok()?;
    serde_json::from_str(&data).ok()
}

pub fn save_cache(cache: &AnnexCache) -> Result<()> {
    let p = cache_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = p.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(cache)?;
    std::fs::write(&tmp, json.as_bytes())?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_find_and_load_demo() {
        // recreate minimal if needed? but assume /tmp/annex-demo2 exists from previous
        let p = std::path::Path::new("/tmp/annex-demo2");
        if !is_annex_repo(p) {
            // skip or recreate? for CI just assert true when present
            eprintln!("demo not present, skipping load test");
            return;
        }
        let meta = load_metadata(p).expect("load demo");
        assert!(!meta.uuid.is_empty());
        assert!(meta.remotes.len() >= 1);
        println!("loaded demo: files={} remotes={} keys={}", meta.files.len(), meta.remotes.len(), meta.total_keys);
    }
}
