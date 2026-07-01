/*!
App state and command dispatch, modeled on zfs-browser.

The heavy data lives on the worker thread.
*/

use crate::annex::{self, AnnexMetadata, RepoSummary};
use crate::node::{Node, RepoLoadingNode, RepoNode, RootNode};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

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
    ToggleRaw,   // like 'x' for raw log/details
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
    pub preloaded: HashMap<PathBuf, AnnexMetadata>,
    /// Lightweight summaries for instant root listing (from cache or computed)
    pub summaries: Vec<RepoSummary>,
    /// Paths we still want to (re)hydrate from disk in the background
    pub to_hydrate: Vec<PathBuf>,
    /// Profiles of drives by their name (e.g. "remote-foo") across all repos, for anomaly detection.
    pub drive_profiles: HashMap<String, annex::DriveProfile>,
}

impl App {
    pub fn new(scan_root: PathBuf) -> Self {
        let root = Rc::new(RootNode::new(scan_root.clone()));
        Self {
            stack: vec![Level { node: root, selected: 0 }],
            root_path: scan_root,
            status: "scanning for git annex repos...".into(),
            preloaded: HashMap::new(),
            summaries: vec![],
            to_hydrate: vec![],
            drive_profiles: HashMap::new(),
        }
    }

    pub fn current(&self) -> &Rc<dyn Node> {
        &self.stack.last().unwrap().node
    }

    pub fn selected_idx(&self) -> usize {
        self.stack.last().map(|l| l.selected).unwrap_or(0)
    }

    fn current_level_mut(&mut self) -> &mut Level {
        self.stack.last_mut().unwrap()
    }

    /// Build a fresh snapshot-friendly view from current selection path.
    pub fn snapshot(&self, page: usize) -> ViewSnapshot {
        let level = self.stack.last().unwrap();
        let kids = level.node.children();
        let sel = level.selected.min(kids.len().saturating_sub(1));
        let selected_node = kids.get(sel).cloned();

        let list_items: Vec<ListItem> = kids.iter().map(|n| ListItem {
            label: n.label(),
            kind: n.kind().to_string(),
            anomalous: n.anomalous(),
        }).collect();

        let details = if let Some(n) = &selected_node {
            n.details()
        } else {
            vec!["no selection".into()]
        };

        let raw = selected_node.as_ref().and_then(|n| n.raw_text());

        let crumb: Vec<String> = self.stack.iter().map(|l| l.node.label()).collect();

        ViewSnapshot {
            crumb,
            list: list_items,
            selected: sel,
            details,
            raw,
            status: self.status.clone(),
            total_repos: if let Some(r) = self.stack.first() {
                r.node.children().len()
            } else { 0 },
        }
    }

    pub fn execute(&mut self, cmd: Command, page: usize) -> Result<()> {
        match cmd {
            Command::None | Command::Quit | Command::ToggleHelp | Command::ToggleRaw => {}
            Command::Up => {
                let l = self.current_level_mut();
                if l.selected > 0 { l.selected -= 1; }
            }
            Command::Down => {
                let l = self.current_level_mut();
                let max = l.node.children().len().saturating_sub(1);
                if l.selected < max { l.selected += 1; }
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
            Command::Top => { self.current_level_mut().selected = 0; }
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
                if let Some(child) = kids.get(l.selected) {
                    if let Some(p) = child.annex_repo_path() {
                        if let Some(meta) = self.preloaded.get(p).cloned() {
                            // Instant because of bg pre-scan or cache
                            let profiles = self.drive_profiles.clone();
                            let node = Rc::new(RepoNode::new(meta).with_profiles(profiles));
                            self.stack.push(Level { node, selected: 0 });
                            self.status = "preloaded".into();
                        } else {
                            self.status = format!("loading {} ...", p.display());
                            let loading = Rc::new(RepoLoadingNode { path: p.to_path_buf() });
                            self.stack.push(Level { node: loading, selected: 0 });
                        }
                    } else {
                        self.stack.push(Level { node: Rc::clone(child), selected: 0 });
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
        if self.stack.last().map_or(false, |l| l.node.loading_path().is_some()) {
            self.stack.pop();
        }
        self.ingest_meta(meta.clone());
        let profiles = self.drive_profiles.clone();
        let node = Rc::new(RepoNode::new(meta).with_profiles(profiles));
        self.stack.push(Level { node, selected: 0 });
        self.status = "loaded".into();
        self.refresh_root_view();
    }

    /// Update the root list and status. Also rebuilds the root node from current summaries.
    pub fn set_discovered(&mut self, repos: Vec<PathBuf>) {
        // If we have cached summaries, use them; otherwise create minimal ones from paths
        if self.summaries.is_empty() {
            self.summaries = repos.iter().map(|p| {
                let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                let mut s = RepoSummary {
                    root: p.clone(),
                    uuid: String::new(),
                    name,
                    annex_description: String::new(),
                    file_count: 0,
                    remote_count: 0,
                    here_present_count: 0,
                    here_available_space: None,
                };
                s.ensure_name();
                s
            }).collect();
        }
        if let Some(lvl) = self.stack.first_mut() {
            let mut new_root = crate::node::RootNode::new(self.root_path.clone());
            new_root.summaries = self.summaries.clone();
            lvl.node = Rc::new(new_root);
            lvl.selected = 0;
        }
        self.status = format!("found {} annex repos ({} cached)", repos.len(), self.preloaded.len());
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
    pub fn ingest_meta(&mut self, meta: AnnexMetadata) {
        let sum = meta.to_summary();
        self.preloaded.insert(meta.root.clone(), meta);
        self.apply_summary(sum);
        self.recompute_drive_profiles();
    }

    /// Recompute drive name -> profile map from all preloaded metas.
    /// Used to highlight drives that differ (trust, groups, wanted, required) from the common setup.
    pub fn recompute_drive_profiles(&mut self) {
        use crate::annex::{DriveProfile, TrustLevel};
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
        self.drive_profiles = profiles;
    }

    /// Returns true if this drive's setup (trust/groups/wanted/required) differs from the common one for that name.
    pub fn drive_is_anomalous(&self, name: &str, remote: &crate::annex::Remote) -> bool {
        let p = match self.drive_profiles.get(name) {
            Some(p) => p,
            None => return false,
        };
        if p.has_variation() {
            // Check if this one matches the most common for each dimension
            if let Some(common_t) = p.most_common_trust() {
                if remote.trust != common_t {
                    return true;
                }
            }
            if let Some(common_g) = p.most_common_groups() {
                let mut my_g = remote.groups.clone();
                my_g.sort();
                if my_g != common_g {
                    return true;
                }
            }
            if let Some(common_w) = p.most_common_wanted() {
                if remote.wanted.as_ref() != Some(&common_w) && remote.wanted.is_some() {
                    // if this has a wanted but common doesn't, or different
                    if remote.wanted != p.most_common_wanted().map(Some).unwrap_or(None) {
                        return true;
                    }
                }
            }
            // similar for required
            if let Some(common_r) = p.most_common_required() {
                if remote.required.as_ref() != Some(&common_r) {
                    if remote.required != p.most_common_required().map(Some).unwrap_or(None) {
                        return true;
                    }
                }
            }
        }
        // Also consider if this name appears in many repos but not configured here? 
        // For now, if the drive is listed here but the profile shows variation in other attrs.
        false
    }

    /// Rebuild/replace the root level node using the current summaries (no downcast).
    pub fn refresh_root_view(&mut self) {
        if self.stack.len() == 1 {
            let mut new_root = RootNode::new(self.root_path.clone());
            new_root.summaries = self.summaries.clone();
            if let Some(lvl) = self.stack.first_mut() {
                lvl.node = Rc::new(new_root);
            }
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
    pub status: String,
    pub total_repos: usize,
}

#[derive(Debug, Clone)]
pub struct ListItem {
    pub label: String,
    pub kind: String,
    /// True if this drive/repo setup differs from the common setup for drives/repos with the same name/folder.
    pub anomalous: bool,
}

// Small helper for downcasting Rc<dyn Node> (simple since Rust 1.0 no built-in, use a tiny trick or Any).
// We use a manual approach with type ids or just match in app. For simplicity here we added a helper in node? 
// Since we control all types, in practice the descend logic above uses concrete check before push.

impl RootNode {
    // helper
}
