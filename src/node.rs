/*!
The browsable tree model for git-annex.

Similar structure to zfs-browser: everything is a Node. Child lists are built
once per node and shared as `Rc<[..]>`, so moving the selection never rebuilds them.
*/

use crate::annex::{
    AnnexMetadata, AnnexedFile, DriveProfile, Remote, RepoSummary, TrustLevel, parse_size_from_key,
};
use crate::usage::{UsageDirNode, UsageListing, UsageTree};
use crate::util::{fmt_unix, human_bytes, short_uuid};
use std::cell::OnceCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// What a row in the browser represents. Drives colours, visuals and navigation rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Root,
    Report,
    Repo,
    Here,
    Drive,
    Drives,
    Info,
    Files,
    Dir,
    File,
    Usage,
    Viz,
    Parent,
}

impl NodeKind {
    pub fn label(self) -> &'static str {
        match self {
            NodeKind::Root => "root",
            NodeKind::Report => "report",
            NodeKind::Repo => "repo",
            NodeKind::Here => "here",
            NodeKind::Drive => "drive",
            NodeKind::Drives => "drives",
            NodeKind::Info => "info",
            NodeKind::Files => "files",
            NodeKind::Dir => "dir",
            NodeKind::File => "file",
            NodeKind::Usage => "usage",
            NodeKind::Viz => "viz",
            NodeKind::Parent => "parent",
        }
    }

    /// Dashboards that zoom instead of descending.
    pub fn is_visual(self) -> bool {
        matches!(self, NodeKind::Report | NodeKind::Viz)
    }
}

impl std::fmt::Display for NodeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Shared, immutable list of child nodes.
pub type Children = Rc<[Rc<dyn Node>]>;

pub fn no_children() -> Children {
    Rc::from(Vec::<Rc<dyn Node>>::new())
}

pub trait Node {
    fn label(&self) -> String;
    fn kind(&self) -> NodeKind;
    fn children(&self) -> Children {
        no_children()
    }
    fn details(&self) -> Vec<String> {
        vec![]
    }
    /// Optional raw text for "x" key (e.g. full log)
    fn raw_text(&self) -> Option<String> {
        None
    }
    /// If this is a summary for a repo that needs full load on descend, return its path.
    fn annex_repo_path(&self) -> Option<&Path> {
        None
    }
    /// If this node represents a loading state, return the path being loaded.
    fn loading_path(&self) -> Option<&Path> {
        None
    }
    /// Whether this item (typically a drive) has setup that differs from other repos' same-named drive.
    fn anomalous(&self) -> bool {
        false
    }
    fn trust(&self) -> Option<TrustLevel> {
        None
    }
    /// Fully loaded repo node for this annex path (not the summary placeholder).
    fn loaded_repo_path(&self) -> Option<&Path> {
        None
    }
    /// Annex size for ncdu-style listings (git-annex key size, not the filesystem).
    fn size(&self) -> Option<u64> {
        None
    }
    /// Whether annexed content is present on this repo (`here`).
    fn present(&self) -> Option<bool> {
        None
    }
    /// Index to select after descending into this node (ncdu skips `..`).
    fn initial_selected(&self) -> usize {
        0
    }
    fn usage_listing(&self) -> Option<UsageListing> {
        None
    }
    /// Some keys have fewer counting copies than numcopies.
    fn under_copies(&self) -> bool {
        false
    }
}

fn cached(cell: &OnceCell<Children>, build: impl FnOnce() -> Vec<Rc<dyn Node>>) -> Children {
    Rc::clone(cell.get_or_init(|| Rc::from(build())))
}

/// Top level: discovered repos under the scan dir.
pub struct RootNode {
    pub scan_root: PathBuf,
    pub summaries: Rc<[RepoSummary]>,
    children: OnceCell<Children>,
}

impl RootNode {
    pub fn new(scan_root: PathBuf, summaries: Rc<[RepoSummary]>) -> Self {
        Self {
            scan_root,
            summaries,
            children: OnceCell::new(),
        }
    }
}

impl Node for RootNode {
    fn label(&self) -> String {
        format!("scan: {}", self.scan_root.display())
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Root
    }
    fn children(&self) -> Children {
        cached(&self.children, || {
            let mut kids: Vec<Rc<dyn Node>> = vec![Rc::new(GlobalReportNode {
                summaries: Rc::clone(&self.summaries),
            })];
            kids.extend((0..self.summaries.len()).map(|idx| {
                Rc::new(RepoSummaryNode {
                    summaries: Rc::clone(&self.summaries),
                    idx,
                }) as Rc<dyn Node>
            }));
            kids
        })
    }
    fn details(&self) -> Vec<String> {
        vec![
            format!("root: {}", self.scan_root.display()),
            format!("annex repos found: {}", self.summaries.len()),
            "The first item is 'Global report' — size bars and copy-health vs numcopies, live as scan proceeds.".into(),
            "Use --scan to refresh cache.".into(),
        ]
    }
}

/// Global info/summary node shown first at root level.
/// Shows aggregates across ALL scanned repos.
pub struct GlobalReportNode {
    pub summaries: Rc<[RepoSummary]>,
}

impl Node for GlobalReportNode {
    fn label(&self) -> String {
        "Global report (all repos)".into()
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Report
    }
    fn details(&self) -> Vec<String> {
        let num_repos = self.summaries.len();
        let total_files: usize = self.summaries.iter().map(|s| s.file_count).sum();
        let total_here: usize = self.summaries.iter().map(|s| s.here_present_count).sum();
        let total_unique: u64 = self.summaries.iter().map(|s| s.unique_size).sum();
        let total_consumed: u64 = self.summaries.iter().map(|s| s.consumed_size).sum();
        let per_remote = crate::annex::aggregate_remote_usage(&self.summaries);
        let (unique_remotes, summed_remotes) = crate::annex::remote_name_stats(&self.summaries);
        let remotes_line = if unique_remotes > 0 {
            format!("{unique_remotes} unique remotes ({summed_remotes} across {num_repos} repos)")
        } else {
            format!("Remotes/drives: {summed_remotes}")
        };

        let mut rows = vec![
            "GLOBAL REPORT — totals across ALL repos".into(),
            format!("Repos: {}", num_repos),
            format!("Working tree files: {}", total_files),
            remotes_line,
            format!("Keys present here (across repos): {}", total_here),
            format!(
                "Unique data size (1 copy each): {}",
                human_bytes(total_unique)
            ),
            format!(
                "Total storage used across drives (with copies): {}",
                human_bytes(total_consumed)
            ),
        ];
        let has_usage = self.summaries.iter().any(|s| !s.remote_usage.is_empty());
        if !has_usage {
            rows.push("Storage per remote: (re-scan or wait for cache hydrate)".into());
        } else if per_remote.is_empty() {
            rows.push("No special remotes (rclone etc.) with stored content.".into());
        } else {
            rows.push("".into());
            rows.push("Storage per special remote (rclone etc.):".into());
            for (name, bytes, keys, repos) in per_remote {
                let repo_note = if repos == 1 {
                    "1 repo".to_string()
                } else {
                    format!("{repos} repos")
                };
                rows.push(format!(
                    "  {} : {}  ({} keys, {})",
                    name,
                    human_bytes(bytes),
                    keys,
                    repo_note
                ));
            }
        }
        rows.push("Select a repo below to browse its drives/files.".into());
        rows
    }
}

/// Placeholder shown while loading a repo's full metadata.
pub struct RepoLoadingNode {
    pub path: PathBuf,
}

impl Node for RepoLoadingNode {
    fn label(&self) -> String {
        format!("{} (loading...)", self.path.display())
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Repo
    }
    fn details(&self) -> Vec<String> {
        vec!["Loading git-annex metadata in background...".into()]
    }
    fn loading_path(&self) -> Option<&Path> {
        Some(&self.path)
    }
}

/// Summary / entry for a repo before full load. Carries lightweight info for instant nice list.
pub struct RepoSummaryNode {
    summaries: Rc<[RepoSummary]>,
    idx: usize,
}

impl RepoSummaryNode {
    fn summary(&self) -> &RepoSummary {
        &self.summaries[self.idx]
    }
}

impl Node for RepoSummaryNode {
    fn label(&self) -> String {
        let s = self.summary();
        let desc = if !s.annex_description.is_empty() && s.annex_description != s.name {
            format!(" ({})", s.annex_description)
        } else {
            String::new()
        };
        format!(
            "{}{} — {} files, {} drives",
            s.name, desc, s.file_count, s.remote_count
        )
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Repo
    }
    fn annex_repo_path(&self) -> Option<&Path> {
        Some(&self.summary().root)
    }
    fn under_copies(&self) -> bool {
        self.summary().keys_under > 0
    }
    fn details(&self) -> Vec<String> {
        let s = self.summary();
        let mut d = vec![format!("path: {}", s.root.display())];
        if !s.uuid.is_empty() {
            d.push(format!("uuid: {}", s.uuid));
        }
        if !s.annex_description.is_empty() {
            d.push(format!("annex description: {}", s.annex_description));
        }
        d.push(format!("files in working tree: {}", s.file_count));
        d.push(format!("known remotes/drives: {}", s.remote_count));
        d.push(format!("keys present here: {}", s.here_present_count));
        d.push(format!(
            "unique data size (1 copy): {}",
            human_bytes(s.unique_size)
        ));
        d.push(format!(
            "consumed across drives: {}",
            human_bytes(s.consumed_size)
        ));
        if s.keys_under > 0 {
            d.push(format!(
                "keys under numcopies: {} of {}",
                s.keys_under, s.keys_tracked
            ));
        }
        d.push(
            "→ descend to see drives (sorted by last fsck), groups, wanted, numcopies etc."
                .to_string(),
        );
        d
    }
}

/// Fully loaded repo. This is the interesting level.
pub struct RepoNode {
    pub meta: Rc<AnnexMetadata>,
    pub drive_profiles: Rc<HashMap<String, DriveProfile>>,
    children: OnceCell<Children>,
}

impl RepoNode {
    pub fn new(meta: Rc<AnnexMetadata>, drive_profiles: Rc<HashMap<String, DriveProfile>>) -> Self {
        Self {
            meta,
            drive_profiles,
            children: OnceCell::new(),
        }
    }
}

/// Directory name (`a/b/`) of the last path component.
fn dir_label(dir_path: &str) -> String {
    format!("{}/", dir_path.rsplit('/').next().unwrap_or(dir_path))
}

fn repo_display_name(meta: &AnnexMetadata) -> String {
    meta.root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| meta.description.clone())
}

impl Node for RepoNode {
    fn label(&self) -> String {
        let short = short_uuid(&self.meta.uuid);
        let clean_name = repo_display_name(&self.meta);
        if clean_name != self.meta.description && !self.meta.description.is_empty() {
            format!("{} ({}) [{}]", clean_name, self.meta.description, short)
        } else {
            format!("{} [{}]", clean_name, short)
        }
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Repo
    }
    fn loaded_repo_path(&self) -> Option<&Path> {
        Some(&self.meta.root)
    }
    fn children(&self) -> Children {
        cached(&self.children, || {
            let meta = &self.meta;
            let usage = Rc::new(UsageTree::build(&meta.files));
            let mut kids: Vec<Rc<dyn Node>> = vec![
                Rc::new(RepoVisualNode {
                    root: meta.root.clone(),
                }),
                Rc::new(UsageDirNode::root(Rc::clone(meta), usage)),
                Rc::new(DrivesNode {
                    meta: Rc::clone(meta),
                    drive_profiles: Rc::clone(&self.drive_profiles),
                }),
                Rc::new(RepoInfoNode {
                    meta: Rc::clone(meta),
                }),
            ];
            if !meta.files.is_empty() {
                kids.push(Rc::new(FileTreeNode::all_files(Rc::clone(meta))));
            }
            // Quick link to files present locally
            if meta.remotes.contains_key(&meta.uuid) {
                kids.push(Rc::new(FileTreeNode::on_drive(
                    Rc::clone(meta),
                    meta.uuid.clone(),
                    "here".to_string(),
                )));
            }
            kids
        })
    }
    fn details(&self) -> Vec<String> {
        let m = &self.meta;
        let mut d = vec![
            format!("path: {}", m.root.display()),
            format!("uuid: {}", m.uuid),
            format!("description: {}", m.description),
            format!("annexed files (working tree): {}", m.files.len()),
            format!("known keys (locations): {}", m.total_keys),
            format!("unique data size (1 copy): {}", human_bytes(m.unique_size)),
            format!(
                "consumed across all drives (with copies): {}",
                human_bytes(m.consumed_size)
            ),
            format!("known remotes/drives: {}", m.remotes.len()),
            "Open 'disk usage' for an ncdu-style tree (largest first; sizes from git-annex keys)."
                .into(),
        ];
        if let Some(h) = m.remotes.get(&m.uuid) {
            d.push(format!("here present: {} keys", h.present_count));
        }
        d
    }
}

pub struct RepoInfoNode {
    pub meta: Rc<AnnexMetadata>,
}

impl Node for RepoInfoNode {
    fn label(&self) -> String {
        "info / summary".into()
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Info
    }
    fn details(&self) -> Vec<String> {
        let m = &self.meta;
        let clean = repo_display_name(m);
        let mut rows = vec![
            format!("repo path: {}", m.root.display()),
            format!(
                "name: {}{}",
                clean,
                if clean != m.description && !m.description.is_empty() {
                    format!(" ({})", m.description)
                } else {
                    String::new()
                }
            ),
            format!("local uuid: {}", m.uuid),
            format!("working tree annexed files: {}", m.files.len()),
            format!("unique keys tracked: {}", m.total_keys),
        ];
        let count = |t: TrustLevel| m.remotes.values().filter(|r| r.trust == t).count();
        rows.push(format!(
            "trust summary: {} trusted, {} semitrusted, {} untrusted",
            count(TrustLevel::Trusted),
            count(TrustLevel::SemiTrusted),
            count(TrustLevel::UnTrusted)
        ));
        if let Some(n) = m.numcopies {
            rows.push(format!("numcopies: {}", n));
        }
        // local fsck + groups/wanted
        if let Some(h) = m.remotes.get(&m.uuid) {
            if let Some(ts) = h.last_fsck {
                rows.push(format!("last fsck (here): {}", fmt_unix(ts)));
            }
            push_remote_prefs(&mut rows, h);
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

fn push_remote_prefs(rows: &mut Vec<String>, r: &Remote) {
    if !r.groups.is_empty() {
        rows.push(format!("groups: {}", r.groups.join(", ")));
    }
    if let Some(w) = &r.wanted {
        rows.push(format!("wanted: {}", w));
    }
    if let Some(req) = &r.required {
        rows.push(format!("required: {}", req));
    }
}

/// Per-repo visual dashboard (opened from inside a repo).
pub struct RepoVisualNode {
    pub root: PathBuf,
}

impl Node for RepoVisualNode {
    fn label(&self) -> String {
        "visual overview".into()
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Viz
    }
    fn loaded_repo_path(&self) -> Option<&Path> {
        Some(&self.root)
    }
    fn details(&self) -> Vec<String> {
        vec![
            "Live size bars and copy-health for this repository.".into(),
            "Press z for a full-screen view.".into(),
        ]
    }
}

/// List of all drives/remotes for the repo.
pub struct DrivesNode {
    pub meta: Rc<AnnexMetadata>,
    pub drive_profiles: Rc<HashMap<String, DriveProfile>>,
}

impl Node for DrivesNode {
    fn label(&self) -> String {
        format!("drives / remotes ({})", self.meta.remotes.len())
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Drives
    }
    fn children(&self) -> Children {
        let mut list: Vec<&Remote> = self.meta.remotes.values().collect();
        // Sort by last fsck time (most recent first). Falls back to name.
        list.sort_by_key(|r| (std::cmp::Reverse(r.last_fsck.unwrap_or(0)), r.name()));
        list.into_iter()
            .map(|r| {
                let anomalous = self
                    .drive_profiles
                    .get(r.name())
                    .is_some_and(|p| p.has_variation() && !p.matches_common(r));
                Rc::new(DriveNode {
                    meta: Rc::clone(&self.meta),
                    remote: r.clone(),
                    anomalous,
                }) as Rc<dyn Node>
            })
            .collect()
    }
    fn details(&self) -> Vec<String> {
        let mut d = vec![
            "All known drives / remotes for this repo.".into(),
            "Sorted by last fsck time (most recent first). Descend (Enter) to browse files present on each.".into(),
        ];
        // Compact view of disks that have content for this repo
        let mut with_content: Vec<_> = self
            .meta
            .remotes
            .values()
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
            let my_names: HashSet<&str> = self.meta.remotes.values().map(|r| r.name()).collect();
            let mut missing_common: Vec<&str> = self
                .drive_profiles
                .iter()
                .filter(|(name, prof)| {
                    !my_names.contains(name.as_str())
                        && prof.has_variation()
                        && prof.trusts.values().sum::<usize>() >= 3
                })
                .map(|(name, _)| name.as_str())
                .collect();
            missing_common.sort();
            if !missing_common.is_empty() {
                d.push("".into());
                d.push(format!(
                    "missing in this repo (but common elsewhere): {}",
                    missing_common.join(", ")
                ));
            }
        }
        d
    }
}

pub struct DriveNode {
    pub meta: Rc<AnnexMetadata>,
    pub remote: Remote,
    pub anomalous: bool,
}

impl Node for DriveNode {
    fn label(&self) -> String {
        let r = &self.remote;
        let marker = if r.uuid == self.meta.uuid {
            " [here]"
        } else {
            ""
        };
        let special = if r.is_special() {
            format!(" ({})", r.rtype())
        } else {
            "".into()
        };
        let sz = if r.present_size > 0 {
            format!(" {}", human_bytes(r.present_size))
        } else {
            String::new()
        };
        format!(
            "{}{}{} {} {} keys{}",
            r.name(),
            special,
            marker,
            r.trust.short(),
            r.present_count,
            sz
        )
    }
    fn kind(&self) -> NodeKind {
        if self.remote.uuid == self.meta.uuid {
            NodeKind::Here
        } else if self.remote.is_special() {
            NodeKind::Drive
        } else {
            NodeKind::Repo
        }
    }
    fn children(&self) -> Children {
        let mut kids: Vec<Rc<dyn Node>> = vec![Rc::new(DriveInfoNode {
            remote: self.remote.clone(),
        })];
        if self.remote.present_count > 0 {
            kids.push(Rc::new(FileTreeNode::on_drive(
                Rc::clone(&self.meta),
                self.remote.uuid.clone(),
                self.remote.name().to_string(),
            )));
        }
        Rc::from(kids)
    }
    fn details(&self) -> Vec<String> {
        let r = &self.remote;
        let mut d = vec![
            format!("name: {}", r.name()),
            format!("uuid: {}", r.uuid),
            format!("type: {}", r.rtype()),
            format!("trust: {} ({})", r.trust.as_str(), r.trust.short()),
            format!("present keys: {}", r.present_count),
            format!("present size: {}", human_bytes(r.present_size)),
        ];
        if let Some(ts) = r.last_fsck {
            d.push(format!("last fsck: {}", fmt_unix(ts)));
        }
        push_remote_prefs(&mut d, r);
        let mut cfg: Vec<_> = r
            .config
            .iter()
            .filter(|(k, _)| {
                k.as_str() != "name"
                    && k.as_str() != "type"
                    && !crate::annex::is_secret_remote_key(k)
            })
            .collect();
        cfg.sort();
        for (k, v) in cfg {
            d.push(format!("{}: {}", k, v));
        }
        d
    }

    fn anomalous(&self) -> bool {
        self.anomalous
    }
    fn trust(&self) -> Option<TrustLevel> {
        Some(self.remote.trust)
    }
}

pub struct DriveInfoNode {
    pub remote: Remote,
}

impl Node for DriveInfoNode {
    fn label(&self) -> String {
        "drive info".into()
    }
    fn kind(&self) -> NodeKind {
        NodeKind::Info
    }
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
        rows.push(format!(
            "Storage used on this: {}",
            human_bytes(r.present_size)
        ));
        push_remote_prefs(&mut rows, r);
        if let Some(path) = r.config.get("directory") {
            rows.push(format!("directory: {}", path));
        }
        rows
    }
}

/// Which directory tree a `FileTreeNode` shows.
#[derive(Clone)]
enum TreeScope {
    /// All annexed files in the working tree.
    All,
    /// Only files whose content is recorded on this drive (uuid, display name).
    Drive(String, String),
}

impl TreeScope {
    fn drive_uuid(&self) -> Option<&str> {
        match self {
            TreeScope::All => None,
            TreeScope::Drive(u, _) => Some(u),
        }
    }
}

/// One level of the annexed-files tree: the "all files" and "files on drive"
/// entries (at `dir_path == ""`) and every directory below them.
pub struct FileTreeNode {
    meta: Rc<AnnexMetadata>,
    scope: TreeScope,
    /// `""` for the top of the tree, else `a/b`.
    dir_path: String,
    children: OnceCell<Children>,
}

impl FileTreeNode {
    pub fn all_files(meta: Rc<AnnexMetadata>) -> Self {
        Self {
            meta,
            scope: TreeScope::All,
            dir_path: String::new(),
            children: OnceCell::new(),
        }
    }

    pub fn on_drive(meta: Rc<AnnexMetadata>, drive_uuid: String, drive_name: String) -> Self {
        Self {
            meta,
            scope: TreeScope::Drive(drive_uuid, drive_name),
            dir_path: String::new(),
            children: OnceCell::new(),
        }
    }

    fn is_top(&self) -> bool {
        self.dir_path.is_empty()
    }

    fn on_scope(&self, key: &str) -> bool {
        match self.scope.drive_uuid() {
            None => true,
            Some(d) => self.meta.locations.get(key).is_some_and(|l| l.contains(d)),
        }
    }

    fn build_children(&self) -> Vec<Rc<dyn Node>> {
        let prefix = if self.is_top() {
            String::new()
        } else {
            format!("{}/", self.dir_path)
        };
        let mut subdirs: BTreeSet<&str> = BTreeSet::new();
        let mut direct: Vec<&AnnexedFile> = vec![];
        let mut seen_keys: HashSet<&str> = HashSet::new();
        for f in &self.meta.files {
            if !self.on_scope(&f.key) {
                continue;
            }
            seen_keys.insert(f.key.as_str());
            let Some(rest) = f.path.strip_prefix(&prefix) else {
                continue;
            };
            match rest.find('/') {
                Some(slash) => {
                    subdirs.insert(&rest[..slash]);
                }
                None if !rest.is_empty() => direct.push(f),
                None => {}
            }
        }
        let highlight = self.scope.drive_uuid().map(str::to_string);
        let mut kids: Vec<Rc<dyn Node>> = subdirs
            .into_iter()
            .map(|sd| {
                Rc::new(FileTreeNode {
                    meta: Rc::clone(&self.meta),
                    scope: self.scope.clone(),
                    dir_path: format!("{prefix}{sd}"),
                    children: OnceCell::new(),
                }) as Rc<dyn Node>
            })
            .collect();
        let mut files: Vec<Rc<dyn Node>> = direct
            .into_iter()
            .map(|f| {
                Rc::new(AnnexFileNode {
                    meta: Rc::clone(&self.meta),
                    file: f.clone(),
                    highlight_drive: highlight.clone(),
                }) as Rc<dyn Node>
            })
            .collect();
        // Keys on the drive with no working-tree path (old versions, unused content).
        if self.is_top()
            && let Some(drive) = self.scope.drive_uuid()
        {
            for (key, locs) in &self.meta.locations {
                if locs.contains(drive) && !seen_keys.contains(key.as_str()) {
                    files.push(Rc::new(AnnexFileNode {
                        meta: Rc::clone(&self.meta),
                        file: AnnexedFile {
                            path: format!("<unused key> {key}"),
                            key: key.clone(),
                            size: parse_size_from_key(key),
                        },
                        highlight_drive: highlight.clone(),
                    }));
                }
            }
        }
        files.sort_by_cached_key(|n| n.label());
        kids.extend(files);
        kids
    }
}

impl Node for FileTreeNode {
    fn label(&self) -> String {
        if !self.is_top() {
            return dir_label(&self.dir_path);
        }
        match &self.scope {
            TreeScope::All => format!("all annexed files ({})", self.meta.files.len()),
            TreeScope::Drive(uuid, name) => {
                let cnt = self
                    .meta
                    .locations
                    .values()
                    .filter(|s| s.contains(uuid))
                    .count();
                format!("files on {name} ({cnt})")
            }
        }
    }
    fn kind(&self) -> NodeKind {
        if self.is_top() {
            NodeKind::Files
        } else {
            NodeKind::Dir
        }
    }
    fn children(&self) -> Children {
        cached(&self.children, || self.build_children())
    }
    fn details(&self) -> Vec<String> {
        if !self.is_top() {
            return vec![format!("directory: {}", self.dir_path)];
        }
        match &self.scope {
            TreeScope::All => vec![
                "All files currently annexed in the working tree.".into(),
                "Each entry shows locations when descended.".into(),
                "For very large repos prefer descending into specific drives to filter.".into(),
            ],
            TreeScope::Drive(uuid, name) => vec![
                format!("Drive: {name} ({uuid})"),
                "Files whose content is recorded as present on this drive.".into(),
                "For working tree files the path is shown; unused keys shown with <unused key> prefix."
                    .into(),
            ],
        }
    }
}

/// Details rows for one annexed file: path, key, size and every recorded location.
pub fn file_details(
    meta: &AnnexMetadata,
    file: &AnnexedFile,
    highlight: Option<&str>,
) -> Vec<String> {
    let mut d = vec![format!("path: {}", file.path), format!("key: {}", file.key)];
    if let Some(s) = file.size {
        d.push(format!("size: {}", human_bytes(s)));
    }
    let mut locs: Vec<&String> = meta
        .locations
        .get(&file.key)
        .map(|l| l.iter().filter(|u| meta.remotes.contains_key(*u)).collect())
        .unwrap_or_default();
    if locs.is_empty() {
        d.push("no location records (perhaps never copied)".into());
        return d;
    }
    locs.sort();
    d.push(format!("present on {} locations:", locs.len()));
    for u in locs {
        let name = crate::annex::short_name(meta, u);
        let trust = meta
            .remotes
            .get(u)
            .map(|r| r.trust)
            .unwrap_or(TrustLevel::SemiTrusted);
        let star = if Some(u.as_str()) == highlight {
            " ★"
        } else {
            ""
        };
        d.push(format!("  {} {}{}", name, trust.short(), star));
    }
    d
}

/// Raw location record for the `x` view.
pub fn file_raw(meta: &AnnexMetadata, file: &AnnexedFile) -> Option<String> {
    let locs = meta.locations.get(&file.key)?;
    let mut uuids: Vec<&String> = locs.iter().collect();
    uuids.sort();
    let mut s = format!("key: {}\n", file.key);
    for u in uuids {
        s.push_str(&format!("  {}\n", u));
    }
    Some(s)
}

/// A single annexed file. Details list all locations.
pub struct AnnexFileNode {
    pub meta: Rc<AnnexMetadata>,
    pub file: AnnexedFile,
    pub highlight_drive: Option<String>,
}

impl Node for AnnexFileNode {
    fn label(&self) -> String {
        let sz = self.file.size.map(human_bytes).unwrap_or_default();
        let badge = if let Some(locs) = self.meta.locations.get(&self.file.key) {
            let mut names: Vec<_> = locs
                .iter()
                .map(|u| crate::annex::short_name(&self.meta, u))
                .collect();
            names.sort();
            if names.len() > 3 {
                format!(" [{}+{}]", names[0], names.len() - 1)
            } else {
                format!(" [{}]", names.join(","))
            }
        } else {
            "".into()
        };
        let base = if sz.is_empty() {
            self.file.path.clone()
        } else {
            format!("{} ({})", self.file.path, sz)
        };
        format!("{}{}", base, badge)
    }
    fn kind(&self) -> NodeKind {
        NodeKind::File
    }
    fn details(&self) -> Vec<String> {
        file_details(&self.meta, &self.file, self.highlight_drive.as_deref())
    }
    fn raw_text(&self) -> Option<String> {
        file_raw(&self.meta, &self.file)
    }
}
