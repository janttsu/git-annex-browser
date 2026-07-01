/*!
The browsable tree model for git-annex.

Similar structure to zfs-browser: everything is a Node.
*/

use crate::annex::{parse_size_from_key, AnnexMetadata, AnnexedFile, Remote, TrustLevel};
use crate::util::{fmt_unix, human_bytes, short_uuid};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;

pub trait Node {
    fn label(&self) -> String;
    fn kind(&self) -> &'static str;
    fn children(&self) -> Vec<Rc<dyn Node>> { vec![] }
    fn details(&self) -> Vec<String> { vec![] }
    /// Optional raw text for "x" key (e.g. full log)
    fn raw_text(&self) -> Option<String> { None }
    /// If this is a summary for a repo that needs full load on descend, return its path.
    fn annex_repo_path(&self) -> Option<&std::path::Path> { None }
    /// If this node represents a loading state, return the path being loaded.
    fn loading_path(&self) -> Option<std::path::PathBuf> { None }
    /// Whether this item (typically a drive) has setup that differs from other repos' same-named drive.
    fn anomalous(&self) -> bool { false }
}

/// Top level: discovered repos under the scan dir.
pub struct RootNode {
    pub scan_root: PathBuf,
    pub summaries: Vec<crate::annex::RepoSummary>,
}

impl RootNode {
    pub fn new(scan_root: PathBuf) -> Self {
        Self { scan_root, summaries: vec![] }
    }
}

impl Node for RootNode {
    fn label(&self) -> String {
        format!("scan: {}", self.scan_root.display())
    }
    fn kind(&self) -> &'static str { "root" }
    fn children(&self) -> Vec<Rc<dyn Node>> {
        self.summaries.iter().map(|s| {
            Rc::new(RepoSummaryNode { summary: s.clone() }) as Rc<dyn Node>
        }).collect()
    }
    fn details(&self) -> Vec<String> {
        vec![
            format!("root: {}", self.scan_root.display()),
            format!("annex repos found: {}", self.summaries.len()),
            "Drives sorted by last fsck time. Use --scan to refresh cache.".to_string(),
        ]
    }
}

/// Placeholder shown while loading a repo's full metadata.
pub struct RepoLoadingNode {
    pub path: PathBuf,
}

impl Node for RepoLoadingNode {
    fn label(&self) -> String { format!("{} (loading...)", self.path.display()) }
    fn kind(&self) -> &'static str { "repo" }
    fn details(&self) -> Vec<String> {
        vec!["Loading git-annex metadata in background...".into()]
    }
    fn loading_path(&self) -> Option<std::path::PathBuf> {
        Some(self.path.clone())
    }
}

/// Summary / entry for a repo before full load. Carries lightweight info for instant nice list.
pub struct RepoSummaryNode {
    pub summary: crate::annex::RepoSummary,
}

impl Node for RepoSummaryNode {
    fn label(&self) -> String {
        let s = &self.summary;
        let desc = if !s.annex_description.is_empty() && s.annex_description != s.name {
            format!(" ({})", s.annex_description)
        } else {
            String::new()
        };
        format!("{}{} — {} files, {} drives", s.name, desc, s.file_count, s.remote_count)
    }
    fn kind(&self) -> &'static str { "repo" }
    fn children(&self) -> Vec<Rc<dyn Node>> {
        vec![]
    }
    fn annex_repo_path(&self) -> Option<&std::path::Path> {
        Some(&self.summary.root)
    }
    fn details(&self) -> Vec<String> {
        let s = &self.summary;
        let mut d = vec![
            format!("path: {}", s.root.display()),
        ];
        if !s.uuid.is_empty() {
            d.push(format!("uuid: {}", s.uuid));
        }
        if !s.annex_description.is_empty() {
            d.push(format!("annex description: {}", s.annex_description));
        }
        d.push(format!("files in working tree: {}", s.file_count));
        d.push(format!("known remotes/drives: {}", s.remote_count));
        d.push(format!("keys present here: {}", s.here_present_count));
        d.push("→ descend to see drives (sorted by last fsck), groups, wanted, numcopies etc.".to_string());
        d
    }
}

/// Fully loaded repo. This is the interesting level.
pub struct RepoNode {
    pub meta: AnnexMetadata,
    /// Profiles for detecting inconsistent drive setups across repos.
    pub drive_profiles: std::collections::HashMap<String, crate::annex::DriveProfile>,
}

impl RepoNode {
    pub fn new(meta: AnnexMetadata) -> Self { 
        Self { meta, drive_profiles: std::collections::HashMap::new() } 
    }
    pub fn with_profiles(mut self, profiles: std::collections::HashMap<String, crate::annex::DriveProfile>) -> Self {
        self.drive_profiles = profiles;
        self
    }
}

impl Node for RepoNode {
    fn label(&self) -> String {
        let short = short_uuid(&self.meta.uuid);
        let clean_name = self.meta.root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.meta.description.clone());
        if clean_name != self.meta.description && !self.meta.description.is_empty() {
            format!("{} ({}) [{}]", clean_name, self.meta.description, short)
        } else {
            format!("{} [{}]", clean_name, short)
        }
    }
    fn kind(&self) -> &'static str { "repo" }
    fn children(&self) -> Vec<Rc<dyn Node>> {
        // Lead with drives so you immediately see presence on all disks
        let mut kids: Vec<Rc<dyn Node>> = vec![
            Rc::new(DrivesNode { meta: self.meta.clone(), drive_profiles: self.drive_profiles.clone() }),
            Rc::new(RepoInfoNode { meta: self.meta.clone() }),
        ];
        if !self.meta.files.is_empty() {
            kids.push(Rc::new(AllFilesNode { meta: self.meta.clone(), cached_children: std::cell::RefCell::new(None) }));
        }
        // Quick link to files present locally
        if self.meta.remotes.contains_key(&self.meta.uuid) {
            kids.push(Rc::new(FilesOnDriveNode {
                meta: self.meta.clone(),
                drive_uuid: self.meta.uuid.clone(),
                drive_name: "here".to_string(),
                cached_children: std::cell::RefCell::new(None),
            }));
        }
        kids
    }
    fn details(&self) -> Vec<String> {
        let m = &self.meta;
        let mut d = vec![
            format!("path: {}", m.root.display()),
            format!("uuid: {}", m.uuid),
            format!("description: {}", m.description),
            format!("annexed files (working tree): {}", m.files.len()),
            format!("known keys (locations): {}", m.total_keys),
            format!("known remotes/drives: {}", m.remotes.len()),
        ];
        if let Some(h) = m.remotes.get(&m.uuid) {
            d.push(format!("here present: {} keys", h.present_count));
        }
        d
    }
}

pub struct RepoInfoNode {
    pub meta: AnnexMetadata,
}

impl Node for RepoInfoNode {
    fn label(&self) -> String { "info / summary".into() }
    fn kind(&self) -> &'static str { "info" }
    fn details(&self) -> Vec<String> {
        let m = &self.meta;
        let clean = m.root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| m.description.clone());
        let mut rows = vec![
            format!("repo path: {}", m.root.display()),
            format!("name: {}{}", clean, if clean != m.description && !m.description.is_empty() { format!(" ({})", m.description) } else { String::new() }),
            format!("local uuid: {}", m.uuid),
            format!("working tree annexed files: {}", m.files.len()),
            format!("unique keys tracked: {}", m.total_keys),
        ];
        // Quick stats
        let trusted = m.remotes.values().filter(|r| r.trust == TrustLevel::Trusted).count();
        let semi = m.remotes.values().filter(|r| r.trust == TrustLevel::SemiTrusted).count();
        let untr = m.remotes.values().filter(|r| r.trust == TrustLevel::UnTrusted).count();
        rows.push(format!("trust summary: {} trusted, {} semitrusted, {} untrusted", trusted, semi, untr));
        if let Some(n) = m.numcopies {
            rows.push(format!("numcopies: {}", n));
        }
        // local fsck + groups/wanted
        if let Some(h) = m.remotes.get(&m.uuid) {
            if let Some(ts) = h.last_fsck {
                rows.push(format!("last fsck (here): {}", fmt_unix(ts)));
            }
            if !h.groups.is_empty() {
                rows.push(format!("groups: {}", h.groups.join(", ")));
            }
            if let Some(w) = &h.wanted {
                rows.push(format!("wanted: {}", w));
            }
            if let Some(req) = &h.required {
                rows.push(format!("required: {}", req));
            }
        }
        if !m.additional_configs.is_empty() {
            rows.push("additional configs:".into());
            for c in &m.additional_configs {
                rows.push(format!("  {}", c));
            }
        }
        rows
    }
}

/// List of all drives/remotes for the repo.
pub struct DrivesNode {
    pub meta: AnnexMetadata,
    /// Drive profiles from across all repos (for anomaly detection). Cloned in for simplicity.
    pub drive_profiles: std::collections::HashMap<String, crate::annex::DriveProfile>,
}

impl Node for DrivesNode {
    fn label(&self) -> String {
        format!("drives / remotes ({})", self.meta.remotes.len())
    }
    fn kind(&self) -> &'static str { "drives" }
    fn children(&self) -> Vec<Rc<dyn Node>> {
        let mut list: Vec<_> = self.meta.remotes.values().cloned().collect();
        // Sort by last fsck time (most recent first). Falls back to name.
        // This gives visibility into which drives were fsck'ed recently.
        list.sort_by_key(|r| {
            let fsck_ts = r.last_fsck.unwrap_or(0);
            (std::cmp::Reverse(fsck_ts), r.name().to_string())
        });
        list.into_iter().map(|r| {
            let name = r.name().to_string();
            let anomalous = if let Some(p) = self.drive_profiles.get(&name) {
                p.has_variation() && !self.matches_common(p, &r)
            } else {
                false
            };
            Rc::new(DriveNode { meta: self.meta.clone(), remote: r, anomalous }) as Rc<dyn Node>
        }).collect()
    }
    fn details(&self) -> Vec<String> {
        let mut d = vec![
            "All known drives / remotes for this repo.".into(),
            "Sorted by last fsck time (most recent first). Descend (Enter) to browse files present on each.".into(),
        ];
        // Compact view of disks that have content for this repo
        let mut with_content: Vec<_> = self.meta.remotes.values()
            .filter(|r| r.present_count > 0)
            .collect();
        with_content.sort_by_key(|r| std::cmp::Reverse(r.present_count));
        if !with_content.is_empty() {
            d.push("".into());
            d.push("disks with content from this repo:".into());
            for r in with_content.iter().take(8) {
                d.push(format!("  {} : {} keys", r.name(), r.present_count));
            }
            if with_content.len() > 8 {
                d.push(format!("  ... and {} more", with_content.len() - 8));
            }
        }
        // Detect drives that are commonly present elsewhere but missing here ( "not configured" )
        if self.drive_profiles.len() > 3 {
            let my_names: std::collections::HashSet<_> = self.meta.remotes.values().map(|r| r.name().to_string()).collect();
            let mut missing_common: Vec<String> = vec![];
            for (name, prof) in &self.drive_profiles {
                if !my_names.contains(name) && prof.has_variation() {
                    // appears in multiple configs but not in this repo
                    if prof.trusts.values().sum::<usize>() >= 3 {
                        missing_common.push(name.clone());
                    }
                }
            }
            if !missing_common.is_empty() {
                d.push("".into());
                d.push(format!("missing in this repo (but common elsewhere): {}", missing_common.join(", ")));
            }
        }
        d
    }
}

pub struct DriveNode {
    pub meta: AnnexMetadata,
    pub remote: Remote,
    pub anomalous: bool,
}

impl Node for DriveNode {
    fn label(&self) -> String {
        let r = &self.remote;
        let marker = if r.uuid == self.meta.uuid { " [here]" } else { "" };
        let special = if r.is_special() { format!(" ({})", r.rtype()) } else { "".into() };
        format!("{}{}{} {} keys", r.name(), special, marker, r.present_count)
    }
    fn kind(&self) -> &'static str {
        if self.remote.uuid == self.meta.uuid { "here" } else if self.remote.is_special() { "drive" } else { "repo" }
    }
    fn children(&self) -> Vec<Rc<dyn Node>> {
        let mut kids: Vec<Rc<dyn Node>> = vec![
            Rc::new(DriveInfoNode { remote: self.remote.clone() }),
        ];
        if self.remote.present_count > 0 {
            kids.push(Rc::new(FilesOnDriveNode {
                meta: self.meta.clone(),
                drive_uuid: self.remote.uuid.clone(),
                drive_name: self.remote.name().to_string(),
                cached_children: std::cell::RefCell::new(None),
            }));
        }
        kids
    }
    fn details(&self) -> Vec<String> {
        let r = &self.remote;
        let mut d = vec![
            format!("name: {}", r.name()),
            format!("uuid: {}", r.uuid),
            format!("type: {}", r.rtype()),
            format!("trust: {} ({})", r.trust.as_str(), r.trust.short()),
            format!("present keys: {}", r.present_count),
        ];
        if let Some(ts) = r.last_fsck {
            d.push(format!("last fsck: {}", fmt_unix(ts)));
        }
        if !r.groups.is_empty() {
            d.push(format!("groups: {}", r.groups.join(", ")));
        }
        if let Some(w) = &r.wanted {
            d.push(format!("wanted: {}", w));
        }
        if let Some(req) = &r.required {
            d.push(format!("required: {}", req));
        }
        for (k, v) in &r.config {
            if k != "name" && k != "type" {
                d.push(format!("{}: {}", k, v));
            }
        }
        d
    }

    fn anomalous(&self) -> bool {
        self.anomalous
    }
}

impl DrivesNode {
    fn matches_common(&self, p: &crate::annex::DriveProfile, r: &crate::annex::Remote) -> bool {
        if let Some(ct) = p.most_common_trust() {
            if r.trust != ct {
                return false;
            }
        }
        if let Some(cg) = p.most_common_groups() {
            let mut myg = r.groups.clone();
            myg.sort();
            if myg != cg {
                return false;
            }
        }
        if let Some(cw) = p.most_common_wanted() {
            if r.wanted.as_deref() != Some(cw.as_str()) {
                return false;
            }
        }
        if let Some(cr) = p.most_common_required() {
            if r.required.as_deref() != Some(cr.as_str()) {
                return false;
            }
        }
        true
    }
}

pub struct DriveInfoNode {
    pub remote: Remote,
}

impl Node for DriveInfoNode {
    fn label(&self) -> String { "drive info".into() }
    fn kind(&self) -> &'static str { "info" }
    fn details(&self) -> Vec<String> {
        let r = &self.remote;
        let mut rows = vec![
            format!("UUID: {}", r.uuid),
            format!("Description / name: {}", r.description),
            format!("Type: {}", r.rtype()),
            format!("Trust level: {}", r.trust.as_str()),
        ];
        if let Some(ts) = r.last_fsck {
            rows.push(format!("Last fsck recorded: {}", fmt_unix(ts)));
        }
        rows.push(format!("Keys present on this: {}", r.present_count));
        if !r.groups.is_empty() {
            rows.push(format!("groups: {}", r.groups.join(", ")));
        }
        if let Some(w) = &r.wanted {
            rows.push(format!("wanted: {}", w));
        }
        if let Some(req) = &r.required {
            rows.push(format!("required: {}", req));
        }
        if let Some(path) = r.config.get("directory") {
            rows.push(format!("directory: {}", path));
        }
        rows
    }
}

/// Files present on a specific drive/remote (or here).
pub struct FilesOnDriveNode {
    pub meta: AnnexMetadata,
    pub drive_uuid: String,
    pub drive_name: String,
    cached_children: std::cell::RefCell<Option<Vec<Rc<dyn Node>>>>,
}

impl Node for FilesOnDriveNode {
    fn label(&self) -> String {
        let cnt = self.meta.locations.values()
            .filter(|s| s.contains(&self.drive_uuid))
            .count();
        format!("files on {} ({})", self.drive_name, cnt)
    }
    fn kind(&self) -> &'static str { "files" }
    fn children(&self) -> Vec<Rc<dyn Node>> {
        let mut cache = self.cached_children.borrow_mut();
        if let Some(cached) = cache.as_ref() {
            return cached.clone();
        }

        // Use git annex list --in=<drive> to get the current list of files
        // present on this drive. This gives accurate "currently exists" data.
        let remote_name = if self.drive_uuid == self.meta.uuid {
            "here".to_string()
        } else {
            self.drive_name.clone()
        };

        let annexed_paths: Vec<String> = {
            let output = Command::new("git")
                .arg("-C").arg(&self.meta.root)
                .arg("annex").arg("list")
                .arg(format!("--in={}", remote_name))
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output();
            if let Ok(out) = output {
                if out.status.success() {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    // git annex list outputs a header + grid + "  path"
                    // We take everything after the last "  " on each line.
                    stdout.lines()
                        .filter_map(|line| {
                            if let Some(pos) = line.rfind("  ") {
                                let p = line[pos + 2..].trim();
                                if !p.is_empty() { Some(p.to_string()) } else { None }
                            } else {
                                None
                            }
                        })
                        .collect()
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
        };

        let mut drive_files: Vec<AnnexedFile> = vec![];
        // Batch lookup keys for the paths present on this drive
        if !annexed_paths.is_empty() {
            let child = Command::new("git")
                .arg("-C").arg(&self.meta.root)
                .arg("annex").arg("lookupkey").arg("--batch")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            if let Ok(mut child) = child {
                {
                    if let Some(mut stdin) = child.stdin.take() {
                        for p in &annexed_paths {
                            let _ = writeln!(stdin, "{}", p);
                        }
                    }
                }
                let stdout = child.stdout.take().unwrap();
                let reader = BufReader::new(stdout);
                for (i, line_res) in reader.lines().enumerate() {
                    if let Ok(key) = line_res {
                        if i < annexed_paths.len() {
                            let path = annexed_paths[i].clone();
                            let size = parse_size_from_key(&key);
                            drive_files.push(AnnexedFile { path, key, size });
                        }
                    }
                }
                let _ = child.wait();
            }
        }

        let mut out = vec![];
        // Build hierarchical: group by top level dir using the current list from list --in
        let mut subdirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut direct_files: Vec<AnnexedFile> = vec![];
        for f in &drive_files {
            if let Some(slash) = f.path.find('/') {
                subdirs.insert(f.path[..slash].to_string());
            } else {
                direct_files.push(f.clone());
            }
        }
        for sd in subdirs {
            out.push(Rc::new(DirectoryNode {
                meta: self.meta.clone(),
                dir_path: sd,
                drive_uuid: Some(self.drive_uuid.clone()),
                cached_children: std::cell::RefCell::new(None),
            }) as Rc<dyn Node>);
        }
        for f in direct_files {
            out.push(Rc::new(AnnexFileNode {
                meta: self.meta.clone(),
                file: f.clone(),
                highlight_drive: Some(self.drive_uuid.clone()),
            }) as Rc<dyn Node>);
        }

        // Supplement with unused keys ... (add them as top level or under their dir, for simplicity as top)
        let current_keys: HashSet<_> = drive_files.iter().map(|f| f.key.clone()).collect();
        for (key, locs) in &self.meta.locations {
            if locs.contains(&self.drive_uuid) && !current_keys.contains(key) {
                let size = parse_size_from_key(key);
                let fake = AnnexedFile { path: format!("<unused key> {}", key), key: key.clone(), size };
                out.push(Rc::new(AnnexFileNode {
                    meta: self.meta.clone(),
                    file: fake,
                    highlight_drive: Some(self.drive_uuid.clone()),
                }) as Rc<dyn Node>);
            }
        }

        // Sort: dirs first, then files
        out.sort_by_key(|n| {
            let is_dir = n.kind() == "dir";
            (if is_dir { 0 } else { 1 }, n.label())
        });
        *cache = Some(out.clone());
        out
    }
    fn details(&self) -> Vec<String> {
        vec![
            format!("Drive: {} ({})", self.drive_name, self.drive_uuid),
            "Files whose content is recorded as present on this drive.".into(),
            "For working tree files the path is shown; unused keys shown with <unused key> prefix.".into(),
        ]
    }
}

/// A single annexed file. Details list all locations.
pub struct AnnexFileNode {
    pub meta: AnnexMetadata,
    pub file: AnnexedFile,
    pub highlight_drive: Option<String>,
}

impl Node for AnnexFileNode {
    fn label(&self) -> String {
        let sz = self.file.size.map(human_bytes).unwrap_or_default();
        let _locs = self.meta.locations.get(&self.file.key)
            .map(|s| s.len()).unwrap_or(0);
        let badge = if let Some(locs) = self.meta.locations.get(&self.file.key) {
            let mut names: Vec<_> = locs.iter().map(|u| crate::annex::short_name(&self.meta, u)).collect();
            names.sort();
            if names.len() > 3 {
                format!(" [{}+{}]", names[0], names.len()-1)
            } else {
                format!(" [{}]", names.join(","))
            }
        } else { "".into() };
        let base = if sz.is_empty() { self.file.path.clone() } else { format!("{} ({})", self.file.path, sz) };
        format!("{}{}", base, badge)
    }
    fn kind(&self) -> &'static str { "file" }
    fn details(&self) -> Vec<String> {
        let mut d = vec![
            format!("path: {}", self.file.path),
            format!("key: {}", self.file.key),
        ];
        if let Some(s) = self.file.size {
            d.push(format!("size: {}", human_bytes(s)));
        }
        // locations
        let mut locs: std::collections::HashSet<String> = self.meta.locations.get(&self.file.key)
            .cloned()
            .unwrap_or_default();
        if let Some(h) = &self.highlight_drive {
            locs.insert(h.clone());
        }
        if locs.is_empty() {
            // Fallback to live query using git annex whereis (the user can see locations
            // with "git annex whereis", so we should too when the batch pre-load missed it).
            if let Ok(live) = crate::annex::get_live_locations_for_key(&self.meta.root, &self.file.key) {
                locs = live;
            }
        }
        if !locs.is_empty() {
            d.push(format!("present on {} locations:", locs.len()));
            let mut sorted: Vec<_> = locs.into_iter().collect();
            sorted.sort();
            for u in sorted {
                let name = crate::annex::short_name(&self.meta, &u);
                let trust = self.meta.remotes.get(&u).map(|r| r.trust).unwrap_or(TrustLevel::SemiTrusted);
                let star = if Some(&u) == self.highlight_drive.as_ref() { " ★" } else { "" };
                d.push(format!("  {} {}{}", name, trust.short(), star));
            }
        } else {
            d.push("no location records (perhaps never copied)".into());
        }
        d
    }
    fn raw_text(&self) -> Option<String> {
        // Show the raw location log if we could fetch it, but for now the locations we have
        if let Some(locs) = self.meta.locations.get(&self.file.key) {
            let mut s = format!("key: {}\n", self.file.key);
            for u in locs {
                s.push_str(&format!("  {}\n", u));
            }
            Some(s)
        } else { None }
    }
}

/// Represents a directory in the annexed files tree.
pub struct DirectoryNode {
    meta: AnnexMetadata,
    dir_path: String, // e.g. "subdir/example" or "" for top
    // If set, only include files present on this drive (for per-drive tree views)
    drive_uuid: Option<String>,
    cached_children: std::cell::RefCell<Option<Vec<Rc<dyn Node>>>>,
}

impl Node for DirectoryNode {
    fn label(&self) -> String {
        if self.dir_path.is_empty() {
            "<root>".to_string()
        } else {
            format!("{}/", self.dir_path.rsplit('/').next().unwrap_or(&self.dir_path))
        }
    }
    fn kind(&self) -> &'static str { "dir" }
    fn children(&self) -> Vec<Rc<dyn Node>> {
        let mut cache = self.cached_children.borrow_mut();
        if let Some(c) = cache.as_ref() {
            return c.clone();
        }
        let prefix = if self.dir_path.is_empty() { String::new() } else { format!("{}/", self.dir_path) };
        let mut subdirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut direct_files: Vec<AnnexedFile> = vec![];
        for f in &self.meta.files {
            if !f.path.starts_with(&prefix) { continue; }
            // If we are in a per-drive tree, only include files present on that drive
            if let Some(drive) = &self.drive_uuid {
                if let Some(locs) = self.meta.locations.get(&f.key) {
                    if !locs.contains(drive) { continue; }
                } else {
                    continue;
                }
            }
            let rest = &f.path[prefix.len()..];
            if rest.is_empty() { continue; }
            if let Some(slash_pos) = rest.find('/') {
                subdirs.insert(rest[..slash_pos].to_string());
            } else {
                direct_files.push(f.clone());
            }
        }
        let mut kids: Vec<Rc<dyn Node>> = vec![];
        for sd in subdirs {
            let sub_path = if self.dir_path.is_empty() { sd } else { format!("{}/{}", self.dir_path, sd) };
            kids.push(Rc::new(DirectoryNode {
                meta: self.meta.clone(),
                dir_path: sub_path,
                drive_uuid: self.drive_uuid.clone(),
                cached_children: std::cell::RefCell::new(None),
            }) as Rc<dyn Node>);
        }
        for f in direct_files {
            kids.push(Rc::new(AnnexFileNode {
                meta: self.meta.clone(),
                file: f,
                highlight_drive: None,
            }) as Rc<dyn Node>);
        }
        kids.sort_by_key(|k| {
            let is_dir = k.kind() == "dir";
            (if is_dir { 0 } else { 1 }, k.label())
        });
        *cache = Some(kids.clone());
        kids
    }
    fn details(&self) -> Vec<String> {
        vec![format!("directory: {}", if self.dir_path.is_empty() { "<root>" } else { &self.dir_path })]
    }
}

/// Flat list of ALL annexed files in working tree (with locations summary).
pub struct AllFilesNode {
    pub meta: AnnexMetadata,
    cached_children: std::cell::RefCell<Option<Vec<Rc<dyn Node>>>>,
}

impl Node for AllFilesNode {
    fn label(&self) -> String {
        format!("all annexed files ({})", self.meta.files.len())
    }
    fn kind(&self) -> &'static str { "files" }
    fn children(&self) -> Vec<Rc<dyn Node>> {
        let mut cache = self.cached_children.borrow_mut();
        if let Some(cached) = cache.as_ref() {
            return cached.clone();
        }
        // Build top-level directory tree instead of flat list
        let mut subdirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut direct_files: Vec<AnnexedFile> = vec![];
        for f in &self.meta.files {
            if let Some(slash) = f.path.find('/') {
                subdirs.insert(f.path[..slash].to_string());
            } else {
                direct_files.push(f.clone());
            }
        }
        let mut v: Vec<Rc<dyn Node>> = vec![];
        for sd in subdirs {
            v.push(Rc::new(DirectoryNode {
                meta: self.meta.clone(),
                dir_path: sd,
                drive_uuid: None,
                cached_children: std::cell::RefCell::new(None),
            }) as Rc<dyn Node>);
        }
        for f in direct_files {
            v.push(Rc::new(AnnexFileNode {
                meta: self.meta.clone(),
                file: f.clone(),
                highlight_drive: None,
            }) as Rc<dyn Node>);
        }
        v.sort_by_key(|n| {
            let is_dir = n.kind() == "dir";
            (if is_dir { 0 } else { 1 }, n.label())
        });
        *cache = Some(v.clone());
        v
    }
    fn details(&self) -> Vec<String> {
        vec![
            "All files currently annexed in the working tree.".into(),
            "Each entry shows locations when descended.".into(),
            "For very large repos prefer descending into specific drives to filter.".into(),
        ]
    }
}