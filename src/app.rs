/*!
App state and command dispatch, modeled on zfs-browser.

The heavy data lives on the worker thread.
*/

use crate::annex::{self, AnnexMetadata, RepoSummary, aggregate_remote_usage};
use crate::node::{Node, RepoLoadingNode, RepoNode, RootNode};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

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
    ToggleHelp,
    ToggleRaw, // like 'x' for raw log/details
    Select(usize),
    None,
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
    /// Preloaded full metadata for fast navigation (populated from cache + bg scan)
    pub preloaded: HashMap<PathBuf, Rc<AnnexMetadata>>,
    /// Lightweight summaries for instant root listing (from cache or computed)
    pub summaries: Vec<RepoSummary>,
    /// Paths we still want to (re)hydrate from disk in the background
    pub to_hydrate: Vec<PathBuf>,
    /// Profiles of drives by their name (e.g. "remote-foo") across all repos, for anomaly detection.
    pub drive_profiles: Rc<HashMap<String, annex::DriveProfile>>,
    /// Background scan still running (hydrate queue or in-flight loads).
    pub scanning: bool,
}

impl App {
    pub fn new(scan_root: PathBuf) -> Self {
        let root = Rc::new(RootNode::new(scan_root.clone()));
        Self {
            stack: vec![Level {
                node: root,
                selected: 0,
            }],
            root_path: scan_root,
            status: "scanning for git annex repos...".into(),
            preloaded: HashMap::new(),
            summaries: vec![],
            to_hydrate: vec![],
            drive_profiles: Rc::new(HashMap::new()),
            scanning: false,
        }
    }

    fn current_level_mut(&mut self) -> &mut Level {
        self.stack.last_mut().unwrap()
    }

    /// Build a fresh snapshot-friendly view from current selection path.
    pub fn snapshot(&self, _page: usize) -> ViewSnapshot {
        let level = self.stack.last().unwrap();
        let kids = level.node.children();
        let sel = level.selected.min(kids.len().saturating_sub(1));
        let selected_node = kids.get(sel).cloned();

        let list_items: Vec<ListItem> = kids
            .iter()
            .map(|n| {
                let repo_name = n.annex_repo_path().or(n.loaded_repo_path()).and_then(|p| {
                    self.summaries
                        .iter()
                        .find(|s| s.root == p)
                        .map(|s| s.name.clone())
                });
                ListItem {
                    label: n.label(),
                    kind: n.kind().to_string(),
                    anomalous: n.anomalous(),
                    trust: n.trust(),
                    repo_name,
                }
            })
            .collect();

        let details = if let Some(n) = &selected_node {
            n.details()
        } else {
            vec!["no selection".into()]
        };

        let raw = selected_node.as_ref().and_then(|n| n.raw_text());

        let crumb: Vec<String> = self.stack.iter().map(|l| l.node.label()).collect();

        let visual = Some(VisualReport::from_summaries(&self.summaries));
        let repo_visuals: Vec<VisualRepoDetail> = self
            .summaries
            .iter()
            .map(VisualRepoDetail::from_summary)
            .collect();

        ViewSnapshot {
            crumb,
            list: list_items,
            selected: sel,
            details,
            raw,
            visual,
            repo_visuals,
            status: self.status.clone(),
            total_repos: if let Some(r) = self.stack.first() {
                r.node
                    .children()
                    .iter()
                    .filter(|k| k.kind() != "report")
                    .count()
            } else {
                0
            },
            scanning: self.scanning,
        }
    }

    pub fn execute(&mut self, cmd: Command, page: usize) -> Result<()> {
        match cmd {
            Command::None | Command::Quit | Command::ToggleHelp | Command::ToggleRaw => {}
            Command::Select(i) => {
                let l = self.current_level_mut();
                let max = l.node.children().len().saturating_sub(1);
                l.selected = i.min(max);
            }
            Command::Up => {
                let l = self.current_level_mut();
                if l.selected > 0 {
                    l.selected -= 1;
                }
            }
            Command::Down => {
                let l = self.current_level_mut();
                let max = l.node.children().len().saturating_sub(1);
                if l.selected < max {
                    l.selected += 1;
                }
            }
            Command::PageUp => {
                let l = self.current_level_mut();
                let page = page.max(1);
                l.selected = l.selected.saturating_sub(page);
            }
            Command::PageDown => {
                let l = self.current_level_mut();
                let kids_len = l.node.children().len();
                let page = page.max(1);
                l.selected = (l.selected + page).min(kids_len.saturating_sub(1));
            }
            Command::Top => {
                self.current_level_mut().selected = 0;
            }
            Command::Bottom => {
                let l = self.current_level_mut();
                l.selected = l.node.children().len().saturating_sub(1);
            }
            Command::Back => {
                if self.stack.len() > 1 {
                    self.stack.pop();
                }
            }
            Command::Descend => {
                let l = self.stack.last().unwrap();
                let kids = l.node.children();
                if let Some(child) = kids.get(l.selected)
                    && child.kind() != "report"
                    && child.kind() != "viz"
                {
                    if let Some(p) = child.annex_repo_path() {
                        if let Some(meta) = self.preloaded.get(p).cloned() {
                            // Instant because of bg pre-scan or cache
                            let profiles = Rc::clone(&self.drive_profiles);
                            let node = Rc::new(RepoNode::new(meta).with_profiles(profiles));
                            self.stack.push(Level { node, selected: 0 });
                            self.status = "preloaded".into();
                        } else {
                            self.status = format!("loading {} ...", p.display());
                            let loading = Rc::new(RepoLoadingNode {
                                path: p.to_path_buf(),
                            });
                            self.stack.push(Level {
                                node: loading,
                                selected: 0,
                            });
                        }
                    } else {
                        self.stack.push(Level {
                            node: Rc::clone(child),
                            selected: 0,
                        });
                    }
                }
            }
            Command::Refresh => {
                // Rebuild from root, replay selection if possible.
                self.refresh();
            }
        }
        Ok(())
    }

    /// Full refresh: re-discover and reload current path if possible.
    fn refresh(&mut self) {
        self.status = "refreshing...".into();
        // Simplest: pop to root and let user re-descend. Full path replay is more work.
        // For v1 we reset to a fresh root scan (worker will re-discover).
        while self.stack.len() > 1 {
            self.stack.pop();
        }
        if let Some(root_level) = self.stack.first_mut() {
            // The actual new root node is created in worker on Refresh
            root_level.selected = 0;
        }
    }

    /// Called from worker after a successful full repo load.
    /// Replaces the top loading node with the real RepoNode.
    pub fn install_loaded_repo(&mut self, meta: AnnexMetadata) {
        if self
            .stack
            .last()
            .is_some_and(|l| l.node.loading_path().is_some())
        {
            self.stack.pop();
        }
        let root = meta.root.clone();
        self.ingest_meta(meta);
        let meta = Rc::clone(self.preloaded.get(&root).expect("metadata just ingested"));
        let profiles = Rc::clone(&self.drive_profiles);
        let node = Rc::new(RepoNode::new(meta).with_profiles(profiles));
        self.stack.push(Level { node, selected: 0 });
        self.status = "loaded".into();
        self.refresh_root_view();
    }

    /// Update the root list and status. Also rebuilds the root node from current summaries.
    pub fn set_discovered(&mut self, repos: Vec<PathBuf>) {
        // If we have cached summaries, use them; otherwise create minimal ones from paths
        if self.summaries.is_empty() {
            self.summaries = repos
                .iter()
                .map(|p| {
                    let name = p
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let mut s = RepoSummary {
                        root: p.clone(),
                        uuid: String::new(),
                        name,
                        annex_description: String::new(),
                        file_count: 0,
                        remote_count: 0,
                        here_present_count: 0,
                        here_available_space: None,
                        unique_size: 0,
                        consumed_size: 0,
                        remote_usage: vec![],
                        numcopies: None,
                        keys_tracked: 0,
                        keys_under: 0,
                        keys_ok: 0,
                        keys_over: 0,
                    };
                    s.ensure_name();
                    s
                })
                .collect();
        }
        self.refresh_root_view();
        self.status = format!(
            "found {} annex repos ({} cached)",
            repos.len(),
            self.preloaded.len()
        );
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

    /// Merge a freshly loaded full meta into preloaded + summaries.
    pub fn ingest_meta(&mut self, mut meta: AnnexMetadata) {
        meta.ensure_sizes();
        let sum = meta.to_summary();
        let root = meta.root.clone();
        self.preloaded.insert(root.clone(), Rc::new(meta));
        self.apply_summary(sum);
        self.recompute_drive_profiles();
        self.replace_open_repo(&root);
    }

    /// Recompute drive name -> profile map from all preloaded metas.
    /// Used to highlight drives that differ (trust, groups, wanted, required) from the common setup.
    pub fn recompute_drive_profiles(&mut self) {
        use crate::annex::DriveProfile;
        let mut profiles: HashMap<String, DriveProfile> = HashMap::new();
        for meta in self.preloaded.values() {
            for r in meta.remotes.values() {
                let name = r.name().to_string();
                let p = profiles.entry(name).or_default();
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

    /// Rebuild/replace the root level node using the current summaries (no downcast).
    pub fn refresh_root_view(&mut self) {
        let prev = self.stack.first().map(|l| l.selected).unwrap_or(0);
        let mut new_root = RootNode::new(self.root_path.clone());
        new_root.summaries = self.summaries.clone();
        if let Some(lvl) = self.stack.first_mut() {
            lvl.node = Rc::new(new_root);
            let max = lvl.node.children().len().saturating_sub(1);
            lvl.selected = prev.min(max);
        }
        if self.stack.len() >= 2 && self.stack[1].node.kind() == "report" {
            let kids = self.stack[0].node.children();
            if let Some(report) = kids.iter().find(|n| n.kind() == "report") {
                let sel = self.stack[1].selected;
                self.stack[1].node = Rc::clone(report);
                self.stack[1].selected = sel;
            }
        }
    }

    /// If the user is inside a repo that was just re-scanned, rebuild that subtree.
    pub fn replace_open_repo(&mut self, root: &std::path::Path) {
        let Some(pos) = self
            .stack
            .iter()
            .position(|l| l.node.loaded_repo_path() == Some(root))
        else {
            return;
        };
        let selections: Vec<usize> = self.stack[pos..].iter().map(|l| l.selected).collect();
        self.stack.truncate(pos);
        let Some(meta) = self.preloaded.get(root).cloned() else {
            return;
        };
        let node = Rc::new(RepoNode::new(meta).with_profiles(Rc::clone(&self.drive_profiles)));
        let first_sel = selections.first().copied().unwrap_or(0);
        self.stack.push(Level {
            node,
            selected: first_sel,
        });
        for i in 0..selections.len().saturating_sub(1) {
            let kids = self.stack.last().unwrap().node.children();
            if kids.is_empty() {
                break;
            }
            let sel = self.stack.last().unwrap().selected.min(kids.len() - 1);
            self.stack.last_mut().unwrap().selected = sel;
            let child = Rc::clone(&kids[sel]);
            let child_sel = selections.get(i + 1).copied().unwrap_or(0);
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
    pub status: String,
    pub total_repos: usize,
    pub scanning: bool,
}

#[derive(Debug, Clone)]
pub struct ListItem {
    pub label: String,
    pub kind: String,
    /// True if this drive/repo setup differs from the common setup for drives/repos with the same name/folder.
    pub anomalous: bool,
    pub trust: Option<crate::annex::TrustLevel>,
    /// Short annex name, when this row is a repo or per-repo visual.
    pub repo_name: Option<String>,
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

// Small helper for downcasting Rc<dyn Node> (simple since Rust 1.0 no built-in, use a tiny trick or Any).
// We use a manual approach with type ids or just match in app. For simplicity here we added a helper in node?
// Since we control all types, in practice the descend logic above uses concrete check before push.
