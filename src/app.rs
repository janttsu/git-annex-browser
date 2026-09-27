/*!
App state and command dispatch, modeled on zfs-browser.

`App` lives on the worker thread. Metadata is shared as `Arc` so the cache
writer and loader threads can use it; the node tree itself is `Rc`.
*/

use crate::annex::{AnnexMetadata, DriveProfile, RepoSummary, aggregate_remote_usage};
use crate::node::{Node, NodeKind, RepoLoadingNode, RepoNode, RootNode};
use crate::usage::UsageListing;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Quit,
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
    Descend,
    Back,
    Refresh,
    CycleSort,
    ToggleHelp,
    Select(usize),
    None,
}

/// Order of repos in the root list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    #[default]
    Name,
    Size,
    Health,
    Files,
}

impl SortMode {
    pub fn next(self) -> Self {
        match self {
            SortMode::Name => SortMode::Size,
            SortMode::Size => SortMode::Health,
            SortMode::Health => SortMode::Files,
            SortMode::Files => SortMode::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SortMode::Name => "name",
            SortMode::Size => "size",
            SortMode::Health => "copy health",
            SortMode::Files => "file count",
        }
    }

    fn sort(self, v: &mut [RepoSummary]) {
        match self {
            SortMode::Name => v.sort_by(|a, b| a.name.cmp(&b.name).then(a.root.cmp(&b.root))),
            SortMode::Size => v.sort_by_key(|a| std::cmp::Reverse(a.unique_size)),
            // Share of keys under numcopies, worst first.
            SortMode::Health => v.sort_by(|a, b| {
                let ratio = |s: &RepoSummary| s.keys_under as f64 / s.keys_tracked.max(1) as f64;
                ratio(b)
                    .total_cmp(&ratio(a))
                    .then(b.keys_under.cmp(&a.keys_under))
            }),
            SortMode::Files => v.sort_by_key(|a| std::cmp::Reverse(a.file_count)),
        }
    }
}

pub struct Level {
    pub node: Rc<dyn Node>,
    pub selected: usize,
}

pub struct App {
    /// Navigation stack. Top is current view.
    pub stack: Vec<Level>,
    /// The initial scan root.
    pub root_path: PathBuf,
    /// Last status message for UI.
    pub status: String,
    /// Full metadata for fast navigation (from cache files and background loads).
    pub preloaded: HashMap<PathBuf, Arc<AnnexMetadata>>,
    /// Lightweight summaries for the root listing.
    pub summaries: Vec<RepoSummary>,
    /// Repos in the cache that the last discovery did not find (path -> since).
    pub missing: HashMap<PathBuf, i64>,
    /// Profiles of drives by their name (e.g. "remote-foo") across all repos, for anomaly detection.
    pub drive_profiles: Rc<HashMap<String, DriveProfile>>,
    /// Background scan still running (hydrate queue or in-flight loads).
    pub scanning: bool,
    pub sort: SortMode,
}

impl App {
    pub fn new(scan_root: PathBuf) -> Self {
        let root = Rc::new(RootNode::new(
            scan_root.clone(),
            Rc::from(Vec::new()),
            Rc::new(HashMap::new()),
        ));
        Self {
            stack: vec![Level {
                node: root,
                selected: 0,
            }],
            root_path: scan_root,
            status: "scanning for git annex repos...".into(),
            preloaded: HashMap::new(),
            summaries: vec![],
            missing: HashMap::new(),
            drive_profiles: Rc::new(HashMap::new()),
            scanning: false,
            sort: SortMode::default(),
        }
    }

    fn current_level_mut(&mut self) -> &mut Level {
        self.stack.last_mut().expect("stack has root")
    }

    /// Build a snapshot of the current view for the UI thread.
    pub fn snapshot(&self) -> ViewSnapshot {
        let level = self.stack.last().expect("stack has root");
        let kids = level.node.children();
        let sel = level.selected.min(kids.len().saturating_sub(1));
        let selected_node = kids.get(sel).cloned();

        let names: HashMap<&Path, &str> = self
            .summaries
            .iter()
            .map(|s| (s.root.as_path(), s.name.as_str()))
            .collect();
        let list_items: Vec<ListItem> = kids
            .iter()
            .map(|n| ListItem {
                label: n.label(),
                kind: n.kind(),
                anomalous: n.anomalous(),
                trust: n.trust(),
                repo_name: n
                    .annex_repo_path()
                    .or(n.loaded_repo_path())
                    .and_then(|p| names.get(p))
                    .map(|s| s.to_string()),
                size: n.size(),
                missing: n.present() == Some(false),
                under_copies: n.under_copies(),
            })
            .collect();

        let details = match &selected_node {
            Some(n) => n.details(),
            None => vec!["no selection".into()],
        };
        let raw = selected_node.as_ref().and_then(|n| n.raw_text());
        let usage = selected_node.as_ref().and_then(|n| n.usage_listing());
        let crumb: Vec<String> = self.stack.iter().map(|l| l.node.label()).collect();

        ViewSnapshot {
            crumb,
            list: list_items,
            selected: sel,
            details,
            raw,
            visual: Some(VisualReport::from_summaries(&self.summaries)),
            repo_visuals: self
                .summaries
                .iter()
                .map(VisualRepoDetail::from_summary)
                .collect(),
            usage,
            status: self.status.clone(),
            total_repos: self.summaries.len(),
            scanning: self.scanning,
        }
    }

    /// Apply a navigation command. Returns the repo path to load when the user
    /// descended into a repo whose metadata is not available yet.
    pub fn execute(&mut self, cmd: Command, page: usize) -> Option<PathBuf> {
        let page = page.max(1);
        match cmd {
            Command::None | Command::Quit | Command::ToggleHelp | Command::Refresh => {}
            Command::Select(i) => {
                let l = self.current_level_mut();
                let max = l.node.children().len().saturating_sub(1);
                l.selected = i.min(max);
            }
            Command::Up => {
                let l = self.current_level_mut();
                l.selected = l.selected.saturating_sub(1);
            }
            Command::Down => {
                let l = self.current_level_mut();
                let max = l.node.children().len().saturating_sub(1);
                l.selected = (l.selected + 1).min(max);
            }
            Command::PageUp => {
                let l = self.current_level_mut();
                l.selected = l.selected.saturating_sub(page);
            }
            Command::PageDown => {
                let l = self.current_level_mut();
                let max = l.node.children().len().saturating_sub(1);
                l.selected = (l.selected + page).min(max);
            }
            Command::Top => self.current_level_mut().selected = 0,
            Command::Bottom => {
                let l = self.current_level_mut();
                l.selected = l.node.children().len().saturating_sub(1);
            }
            Command::Back => {
                if self.stack.len() > 1 {
                    self.stack.pop();
                }
            }
            Command::CycleSort => {
                self.sort = self.sort.next();
                self.status = format!("repos sorted by {}", self.sort.label());
                self.refresh_root_view();
            }
            Command::Descend => return self.descend(),
        }
        None
    }

    fn descend(&mut self) -> Option<PathBuf> {
        let l = self.stack.last().expect("stack has root");
        let kids = l.node.children();
        let child = kids.get(l.selected)?;
        if child.kind().is_visual() {
            return None;
        }
        if child.kind() == NodeKind::Parent {
            if self.stack.len() > 1 {
                self.stack.pop();
            }
            return None;
        }
        let Some(p) = child.annex_repo_path() else {
            let selected = child.initial_selected();
            let node = Rc::clone(child);
            self.stack.push(Level { node, selected });
            return None;
        };
        let p = p.to_path_buf();
        if let Some(meta) = self.preloaded.get(&p).cloned() {
            let node = Rc::new(RepoNode::new(meta, Rc::clone(&self.drive_profiles)));
            self.stack.push(Level { node, selected: 0 });
            return None;
        }
        self.status = format!("loading {} ...", p.display());
        self.stack.push(Level {
            node: Rc::new(RepoLoadingNode { path: p.clone() }),
            selected: 0,
        });
        Some(p)
    }

    /// Back to the root list (used by refresh).
    pub fn pop_to_root(&mut self) {
        self.stack.truncate(1);
        self.stack[0].selected = 0;
    }

    /// The loading placeholder on top of the stack, if any.
    pub fn awaiting_load(&self) -> Option<PathBuf> {
        self.stack
            .last()
            .and_then(|l| l.node.loading_path())
            .map(Path::to_path_buf)
    }

    /// Drop a loading placeholder that cannot be satisfied.
    pub fn cancel_loading(&mut self, root: &Path, why: String) {
        if self.awaiting_load().as_deref() == Some(root) {
            self.stack.pop();
        }
        self.status = why;
    }

    /// Replace the root list with cached index rows.
    pub fn set_cached(&mut self, summaries: Vec<RepoSummary>, missing: HashMap<PathBuf, i64>) {
        self.summaries = summaries;
        for s in &mut self.summaries {
            s.ensure_name();
        }
        self.missing = missing;
        self.refresh_root_view();
    }

    /// Record discovery: add placeholders for new repos and mark cached ones not found.
    pub fn set_discovered(&mut self, found: &[PathBuf], now: i64) {
        for p in found {
            self.missing.remove(p);
            if !self.summaries.iter().any(|s| &s.root == p) {
                self.summaries.push(RepoSummary::placeholder(p.clone()));
            }
        }
        for s in &self.summaries {
            if !found.contains(&s.root) {
                self.missing.entry(s.root.clone()).or_insert(now);
            }
        }
        self.refresh_root_view();
    }

    pub fn apply_summary(&mut self, mut s: RepoSummary) {
        s.ensure_name();
        if let Some(existing) = self.summaries.iter_mut().find(|e| e.root == s.root) {
            *existing = s;
        } else {
            self.summaries.push(s);
        }
        self.refresh_root_view();
    }

    /// Merge freshly loaded metadata into the model and refresh open views.
    pub fn ingest_meta(&mut self, mut meta: AnnexMetadata) -> Arc<AnnexMetadata> {
        meta.ensure_sizes();
        let sum = meta.to_summary();
        let root = meta.root.clone();
        let meta = Arc::new(meta);
        self.preloaded.insert(root.clone(), Arc::clone(&meta));
        self.apply_summary(sum);
        self.recompute_drive_profiles();
        self.replace_open_repo(&root);
        if self.awaiting_load().as_deref() == Some(root.as_path()) {
            self.stack.pop();
            let node = Rc::new(RepoNode::new(
                Arc::clone(&meta),
                Rc::clone(&self.drive_profiles),
            ));
            self.stack.push(Level { node, selected: 0 });
            self.status = "loaded".into();
        }
        meta
    }

    /// Recompute drive name -> profile map from all preloaded metas.
    /// Used to highlight drives that differ (trust, groups, wanted, required) from the common setup.
    pub fn recompute_drive_profiles(&mut self) {
        let mut profiles: HashMap<String, DriveProfile> = HashMap::new();
        for meta in self.preloaded.values() {
            for r in meta.remotes.values() {
                let p = profiles.entry(r.name().to_string()).or_default();
                *p.trusts.entry(r.trust).or_default() += 1;
                let mut gs = r.groups.clone();
                gs.sort();
                *p.group_sets.entry(gs).or_default() += 1;
                *p.wanteds.entry(r.wanted.clone()).or_default() += 1;
                *p.requireds.entry(r.required.clone()).or_default() += 1;
            }
        }
        self.drive_profiles = Rc::new(profiles);
    }

    /// Rebuild the root level from the current summaries, keeping the selected repo.
    pub fn refresh_root_view(&mut self) {
        let prev_path = {
            let lvl = &self.stack[0];
            lvl.node
                .children()
                .get(lvl.selected)
                .and_then(|n| n.annex_repo_path().map(Path::to_path_buf))
        };
        let prev_idx = self.stack[0].selected;
        let mut sorted = self.summaries.clone();
        self.sort.sort(&mut sorted);
        let node = Rc::new(RootNode::new(
            self.root_path.clone(),
            Rc::from(sorted),
            Rc::new(self.missing.clone()),
        ));
        let kids = node.children();
        let selected = prev_path
            .and_then(|p| kids.iter().position(|k| k.annex_repo_path() == Some(&p)))
            .unwrap_or(prev_idx)
            .min(kids.len().saturating_sub(1));
        self.stack[0] = Level { node, selected };
        if self.stack.len() >= 2 && self.stack[1].node.kind() == NodeKind::Report {
            let report = kids.iter().find(|n| n.kind() == NodeKind::Report).cloned();
            if let Some(report) = report {
                self.stack[1].node = report;
            }
        }
    }

    /// If the user is inside a repo that was just re-scanned, rebuild that subtree.
    pub fn replace_open_repo(&mut self, root: &Path) {
        let Some(pos) = self.stack.iter().position(|l| {
            l.node.loaded_repo_path() == Some(root) && l.node.kind() == NodeKind::Repo
        }) else {
            return;
        };
        let selections: Vec<usize> = self.stack[pos..].iter().map(|l| l.selected).collect();
        self.stack.truncate(pos);
        let Some(meta) = self.preloaded.get(root).cloned() else {
            return;
        };
        let node = Rc::new(RepoNode::new(meta, Rc::clone(&self.drive_profiles)));
        self.stack.push(Level {
            node,
            selected: selections[0],
        });
        for &child_sel in &selections[1..] {
            let top = self.stack.last_mut().expect("just pushed");
            let kids = top.node.children();
            if kids.is_empty() {
                break;
            }
            top.selected = top.selected.min(kids.len() - 1);
            let child = Rc::clone(&kids[top.selected]);
            let child_sel = child_sel.min(child.children().len().saturating_sub(1));
            self.stack.push(Level {
                node: child,
                selected: child_sel,
            });
        }
    }
}

/// Plain data snapshot sent to UI thread.
#[derive(Debug, Clone)]
pub struct ViewSnapshot {
    pub crumb: Vec<String>,
    pub list: Vec<ListItem>,
    pub selected: usize,
    pub details: Vec<String>,
    pub raw: Option<String>,
    pub visual: Option<VisualReport>,
    pub repo_visuals: Vec<VisualRepoDetail>,
    pub usage: Option<UsageListing>,
    pub status: String,
    pub total_repos: usize,
    pub scanning: bool,
}

#[derive(Debug, Clone)]
pub struct ListItem {
    pub label: String,
    pub kind: NodeKind,
    /// True if this drive/repo setup differs from the common setup for drives/repos with the same name/folder.
    pub anomalous: bool,
    pub trust: Option<crate::annex::TrustLevel>,
    /// Short annex name, when this row is a repo or per-repo visual.
    pub repo_name: Option<String>,
    /// git-annex size for ncdu-style rows (not filesystem size).
    pub size: Option<u64>,
    /// True when annexed content is not present on this repo, or the repo is not mounted.
    pub missing: bool,
    /// Some keys have fewer counting copies than numcopies.
    pub under_copies: bool,
}

/// Case-insensitive match of a list row against the `/` filter.
/// `filter_lower` must already be lowercase.
pub fn matches_filter(item: &ListItem, filter_lower: &str) -> bool {
    filter_lower.is_empty()
        || item.label.to_lowercase().contains(filter_lower)
        || item.kind.label().contains(filter_lower)
}

/// Indices of list rows visible under `filter`.
pub fn visible_indices(list: &[ListItem], filter: &str) -> Vec<usize> {
    let f = filter.to_lowercase();
    list.iter()
        .enumerate()
        .filter(|(_, it)| matches_filter(it, &f))
        .map(|(i, _)| i)
        .collect()
}

/// Live dashboard for the global report (bars + copy-health).
#[derive(Debug, Clone)]
pub struct VisualReport {
    pub unique_size: u64,
    pub consumed_size: u64,
    pub repos: Vec<VisualRepo>,
    pub remotes: Vec<(String, u64)>,
}

#[derive(Debug, Clone)]
pub struct VisualRepo {
    pub name: String,
    pub unique_size: u64,
    pub numcopies: u32,
    pub keys_tracked: usize,
    pub keys_under: usize,
    pub keys_ok: usize,
    pub keys_over: usize,
}

pub fn copy_health_kind(tracked: usize, under: usize) -> &'static str {
    if tracked == 0 {
        "unknown"
    } else if under == 0 {
        "ok"
    } else if under * 2 >= tracked {
        "poor"
    } else {
        "mixed"
    }
}

impl VisualRepo {
    pub fn health_kind(&self) -> &'static str {
        copy_health_kind(self.keys_tracked, self.keys_under)
    }
}

impl VisualReport {
    pub fn from_summaries(summaries: &[RepoSummary]) -> Self {
        let mut repos: Vec<VisualRepo> = summaries
            .iter()
            .map(|s| VisualRepo {
                name: s.name.clone(),
                unique_size: s.unique_size,
                numcopies: s.numcopies.unwrap_or(1).max(1),
                keys_tracked: s.keys_tracked,
                keys_under: s.keys_under,
                keys_ok: s.keys_ok,
                keys_over: s.keys_over,
            })
            .collect();
        repos.sort_by(|a, b| {
            b.unique_size
                .cmp(&a.unique_size)
                .then_with(|| a.name.cmp(&b.name))
        });
        let remotes = aggregate_remote_usage(summaries)
            .into_iter()
            .map(|(n, b, _, _)| (n, b))
            .collect();
        Self {
            unique_size: summaries.iter().map(|s| s.unique_size).sum(),
            consumed_size: summaries.iter().map(|s| s.consumed_size).sum(),
            repos,
            remotes,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualRemoteKind {
    Here,
    Special,
    Other,
}

#[derive(Debug, Clone)]
pub struct VisualRemote {
    pub name: String,
    pub size: u64,
    pub count: usize,
    pub kind: VisualRemoteKind,
}

/// Per-repo dashboard (size by drive + copy health).
#[derive(Debug, Clone)]
pub struct VisualRepoDetail {
    pub name: String,
    pub unique_size: u64,
    pub consumed_size: u64,
    pub numcopies: u32,
    pub keys_tracked: usize,
    pub keys_under: usize,
    pub keys_ok: usize,
    pub keys_over: usize,
    pub remotes: Vec<VisualRemote>,
}

impl VisualRepoDetail {
    pub fn health_kind(&self) -> &'static str {
        copy_health_kind(self.keys_tracked, self.keys_under)
    }

    pub fn from_summary(s: &RepoSummary) -> Self {
        let mut remotes: Vec<VisualRemote> = s
            .remote_usage
            .iter()
            .filter(|u| u.present_count > 0 || u.present_size > 0)
            .map(|u| VisualRemote {
                name: u.name.clone(),
                size: u.present_size,
                count: u.present_count,
                kind: if u.uuid == s.uuid {
                    VisualRemoteKind::Here
                } else if u.is_transfer_remote() {
                    VisualRemoteKind::Special
                } else {
                    VisualRemoteKind::Other
                },
            })
            .collect();
        remotes.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
        Self {
            name: s.name.clone(),
            unique_size: s.unique_size,
            consumed_size: s.consumed_size,
            numcopies: s.numcopies.unwrap_or(1).max(1),
            keys_tracked: s.keys_tracked,
            keys_under: s.keys_under,
            keys_ok: s.keys_ok,
            keys_over: s.keys_over,
            remotes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::sample_meta;

    fn app_with_sample() -> App {
        let mut app = App::new(PathBuf::from("/data"));
        app.set_discovered(&[PathBuf::from("/data/photos")], 1);
        app
    }

    fn select_label(app: &mut App, needle: &str) {
        let kids = app.stack.last().unwrap().node.children();
        let idx = kids
            .iter()
            .position(|k| k.label().contains(needle))
            .unwrap_or_else(|| panic!("no child containing {needle:?}"));
        app.execute(Command::Select(idx), 10);
    }

    #[test]
    fn descending_before_load_shows_placeholder_then_repo() {
        let mut app = app_with_sample();
        select_label(&mut app, "photos");
        let want = app.execute(Command::Descend, 10);
        assert_eq!(want, Some(PathBuf::from("/data/photos")));
        assert_eq!(app.awaiting_load(), Some(PathBuf::from("/data/photos")));
        app.ingest_meta(sample_meta());
        assert_eq!(app.awaiting_load(), None);
        assert_eq!(app.stack.len(), 2);
        assert_eq!(app.stack[1].node.kind(), NodeKind::Repo);
    }

    #[test]
    fn rescan_keeps_the_user_inside_the_repo_tree() {
        let mut app = app_with_sample();
        app.ingest_meta(sample_meta());
        select_label(&mut app, "photos");
        assert_eq!(app.execute(Command::Descend, 10), None);
        select_label(&mut app, "all annexed files");
        app.execute(Command::Descend, 10);
        select_label(&mut app, "2024/");
        app.execute(Command::Descend, 10);
        let crumb_before: Vec<String> = app.stack.iter().map(|l| l.node.label()).collect();
        app.ingest_meta(sample_meta());
        let crumb_after: Vec<String> = app.stack.iter().map(|l| l.node.label()).collect();
        assert_eq!(crumb_before, crumb_after);
    }

    #[test]
    fn undiscovered_cached_repo_is_marked_missing() {
        let mut app = App::new(PathBuf::from("/data"));
        let mut s = RepoSummary::placeholder(PathBuf::from("/data/offline"));
        s.file_count = 3;
        app.set_cached(vec![s], HashMap::new());
        app.set_discovered(&[], 42);
        assert_eq!(app.missing.get(Path::new("/data/offline")), Some(&42));
        let snap = app.snapshot();
        let row = snap
            .list
            .iter()
            .find(|r| r.label.contains("offline"))
            .unwrap();
        assert!(row.missing);
        assert!(row.label.contains("not found"), "{}", row.label);
    }

    #[test]
    fn cycle_sort_keeps_selected_repo() {
        let mut app = App::new(PathBuf::from("/data"));
        let mut a = RepoSummary::placeholder(PathBuf::from("/data/a"));
        a.unique_size = 1;
        let mut b = RepoSummary::placeholder(PathBuf::from("/data/b"));
        b.unique_size = 100;
        app.set_cached(vec![a, b], HashMap::new());
        select_label(&mut app, "a —");
        app.execute(Command::CycleSort, 10);
        assert_eq!(app.sort, SortMode::Size);
        let kids = app.stack[0].node.children();
        assert!(kids[1].label().starts_with("b"));
        assert!(kids[app.stack[0].selected].label().starts_with("a"));
    }
}
