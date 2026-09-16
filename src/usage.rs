//! ncdu-style disk usage tree built from git-annex file sizes (not the filesystem).

use crate::annex::{AnnexMetadata, AnnexedFile};
use crate::node::Node;
use crate::util::human_bytes;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub struct UsageChild {
    pub name: String,
    pub rel_path: String,
    pub is_dir: bool,
    pub size: u64,
    pub file_count: usize,
    pub file_idx: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct UsageRow {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub file_count: usize,
}

#[derive(Debug, Clone)]
pub struct UsageListing {
    pub path: String,
    pub total_size: u64,
    pub file_count: usize,
    pub entries: Vec<UsageRow>,
}

#[derive(Debug, Clone)]
pub struct UsageTree {
    /// Directory path (`""` for repo root) -> children, largest first.
    children: HashMap<String, Vec<UsageChild>>,
    totals: HashMap<String, (u64, usize)>,
}

impl UsageTree {
    pub fn stats(&self, dir: &str) -> (u64, usize) {
        self.totals.get(dir).copied().unwrap_or((0, 0))
    }

    pub fn children_sorted(&self, dir: &str) -> &[UsageChild] {
        self.children.get(dir).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn listing(&self, dir: &str) -> UsageListing {
        let (total_size, file_count) = self.stats(dir);
        let entries = self
            .children_sorted(dir)
            .iter()
            .map(|c| UsageRow {
                name: if c.is_dir {
                    format!("{}/", c.name)
                } else {
                    c.name.clone()
                },
                size: c.size,
                is_dir: c.is_dir,
                file_count: c.file_count,
            })
            .collect();
        UsageListing {
            path: if dir.is_empty() {
                "/".into()
            } else {
                format!("/{dir}")
            },
            total_size,
            file_count,
            entries,
        }
    }
}

pub fn build_usage_tree(files: &[AnnexedFile]) -> UsageTree {
    let mut nested: HashMap<String, HashMap<String, UsageChild>> = HashMap::new();
    let mut totals: HashMap<String, (u64, usize)> = HashMap::new();
    nested.entry(String::new()).or_default();
    totals.insert(String::new(), (0, 0));

    for (idx, f) in files.iter().enumerate() {
        let size = f.size.unwrap_or(0);
        let path = f.path.trim_matches('/');
        if path.is_empty() {
            continue;
        }
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }

        {
            let t = totals.entry(String::new()).or_default();
            t.0 = t.0.saturating_add(size);
            t.1 += 1;
        }

        let mut prefix = String::new();
        for (i, part) in parts.iter().enumerate() {
            let parent = prefix.clone();
            let child_path = if prefix.is_empty() {
                (*part).to_string()
            } else {
                format!("{prefix}/{part}")
            };
            let is_last = i + 1 == parts.len();
            let slot = nested.entry(parent).or_default();
            let child = slot
                .entry((*part).to_string())
                .or_insert_with(|| UsageChild {
                    name: (*part).to_string(),
                    rel_path: child_path.clone(),
                    is_dir: !is_last,
                    size: 0,
                    file_count: 0,
                    file_idx: None,
                });
            child.size = child.size.saturating_add(size);
            child.file_count += 1;
            if is_last {
                child.is_dir = false;
                child.file_idx = Some(idx);
            } else {
                child.is_dir = true;
                let t = totals.entry(child_path.clone()).or_default();
                t.0 = t.0.saturating_add(size);
                t.1 += 1;
                nested.entry(child_path.clone()).or_default();
            }
            prefix = child_path;
        }
    }

    let mut children = HashMap::with_capacity(nested.len());
    for (dir, map) in nested {
        let mut v: Vec<UsageChild> = map.into_values().collect();
        v.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
        children.insert(dir, v);
    }
    UsageTree { children, totals }
}

/// `..` row so the usage browser matches ncdu (Enter goes up).
pub struct ParentDirNode;

impl Node for ParentDirNode {
    fn label(&self) -> String {
        "..".into()
    }
    fn kind(&self) -> &'static str {
        "parent"
    }
    fn details(&self) -> Vec<String> {
        vec!["Go up one directory (same as ← / h / Back).".into()]
    }
}

pub struct UsageDirNode {
    meta: Rc<AnnexMetadata>,
    tree: Rc<UsageTree>,
    dir_path: String,
    cached_children: RefCell<Option<Vec<Rc<dyn Node>>>>,
}

impl UsageDirNode {
    pub fn root(meta: Rc<AnnexMetadata>) -> Self {
        let tree = Rc::new(build_usage_tree(&meta.files));
        Self {
            meta,
            tree,
            dir_path: String::new(),
            cached_children: RefCell::new(None),
        }
    }

    fn is_root(&self) -> bool {
        self.dir_path.is_empty()
    }
}

impl Node for UsageDirNode {
    fn label(&self) -> String {
        if self.is_root() {
            let (sz, n) = self.tree.stats("");
            format!(
                "disk usage — {}, {} files, largest first",
                human_bytes(sz),
                n
            )
        } else {
            format!(
                "{}/",
                self.dir_path.rsplit('/').next().unwrap_or(&self.dir_path)
            )
        }
    }
    fn kind(&self) -> &'static str {
        if self.is_root() { "usage" } else { "dir" }
    }
    fn size(&self) -> Option<u64> {
        // Root is a repo-menu entry; only in-tree rows get ncdu size columns.
        if self.is_root() {
            None
        } else {
            Some(self.tree.stats(&self.dir_path).0)
        }
    }
    fn initial_selected(&self) -> usize {
        // Skip `..` so the largest child is selected, like ncdu.
        let n = self.children().len();
        if n > 1 { 1 } else { 0 }
    }
    fn usage_listing(&self) -> Option<UsageListing> {
        Some(self.tree.listing(&self.dir_path))
    }
    fn children(&self) -> Vec<Rc<dyn Node>> {
        let mut cache = self.cached_children.borrow_mut();
        if let Some(c) = cache.as_ref() {
            return c.clone();
        }
        let mut kids: Vec<Rc<dyn Node>> = vec![Rc::new(ParentDirNode)];
        for child in self.tree.children_sorted(&self.dir_path) {
            if child.is_dir {
                kids.push(Rc::new(UsageDirNode {
                    meta: Rc::clone(&self.meta),
                    tree: Rc::clone(&self.tree),
                    dir_path: child.rel_path.clone(),
                    cached_children: RefCell::new(None),
                }));
            } else if let Some(idx) = child.file_idx
                && let Some(f) = self.meta.files.get(idx)
            {
                kids.push(Rc::new(UsageFileNode {
                    meta: Rc::clone(&self.meta),
                    file: f.clone(),
                    name: child.name.clone(),
                    size: child.size,
                }));
            }
        }
        *cache = Some(kids.clone());
        kids
    }
    fn details(&self) -> Vec<String> {
        let (sz, n) = self.tree.stats(&self.dir_path);
        let path = if self.dir_path.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", self.dir_path)
        };
        vec![
            format!("directory: {path}"),
            format!("annex size: {} ({} files)", human_bytes(sz), n),
            "Sizes come from git-annex keys, not the filesystem.".into(),
            "Dropped or missing content is still counted and listed.".into(),
            "Entries are sorted largest first (ncdu-style). Enter a dir to browse it.".into(),
        ]
    }
    fn raw_text(&self) -> Option<String> {
        let listing = self.tree.listing(&self.dir_path);
        let mut s = format!(
            "{}\n{}  {} files\n",
            listing.path,
            human_bytes(listing.total_size),
            listing.file_count
        );
        for e in listing.entries {
            s.push_str(&format!(
                "{}\t{}\t{}\n",
                e.size,
                if e.is_dir { "dir" } else { "file" },
                e.name
            ));
        }
        Some(s)
    }
}

pub struct UsageFileNode {
    meta: Rc<AnnexMetadata>,
    file: AnnexedFile,
    name: String,
    size: u64,
}

impl UsageFileNode {
    fn present_here(&self) -> bool {
        self.meta
            .locations
            .get(&self.file.key)
            .map(|s| s.contains(&self.meta.uuid))
            .unwrap_or(false)
    }
}

impl Node for UsageFileNode {
    fn label(&self) -> String {
        self.name.clone()
    }
    fn kind(&self) -> &'static str {
        "file"
    }
    fn size(&self) -> Option<u64> {
        Some(self.size)
    }
    fn present(&self) -> Option<bool> {
        Some(self.present_here())
    }
    fn details(&self) -> Vec<String> {
        let mut d = crate::node::AnnexFileNode {
            meta: Rc::clone(&self.meta),
            file: self.file.clone(),
            highlight_drive: None,
        }
        .details();
        if !self.present_here() {
            d.insert(
                1,
                "content not present here — size is from the git-annex key".into(),
            );
        }
        d
    }
    fn raw_text(&self) -> Option<String> {
        crate::node::AnnexFileNode {
            meta: Rc::clone(&self.meta),
            file: self.file.clone(),
            highlight_drive: None,
        }
        .raw_text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, size: u64) -> AnnexedFile {
        AnnexedFile {
            path: path.into(),
            key: format!("SHA256E-s{size}--{path}"),
            size: Some(size),
        }
    }

    #[test]
    fn aggregates_and_sorts_largest_first() {
        let tree = build_usage_tree(&[
            f("videos/big.mkv", 50_000),
            f("videos/clips/mid.bin", 8_000),
            f("photos/small.jpg", 1_000),
            f("readme.txt", 50),
        ]);
        let (root_sz, root_n) = tree.stats("");
        assert_eq!(root_sz, 59_050);
        assert_eq!(root_n, 4);

        let root = tree.children_sorted("");
        let names: Vec<_> = root.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["videos", "photos", "readme.txt"]);
        assert!(root[0].is_dir);
        assert_eq!(root[0].size, 58_000);
        assert_eq!(root[0].file_count, 2);
        assert_eq!(root[1].size, 1_000);
        assert!(!root[2].is_dir);

        let videos = tree.children_sorted("videos");
        assert_eq!(videos[0].name, "big.mkv");
        assert_eq!(videos[0].size, 50_000);
        assert_eq!(videos[1].name, "clips");
        assert_eq!(videos[1].size, 8_000);
        assert!(videos[1].is_dir);

        let listing = tree.listing("videos");
        assert_eq!(listing.total_size, 58_000);
        assert_eq!(listing.entries[0].name, "big.mkv");
        assert_eq!(listing.entries[1].name, "clips/");
    }

    #[test]
    fn counts_each_path_even_with_shared_key_size() {
        let tree = build_usage_tree(&[f("a/one.bin", 100), f("a/two.bin", 100)]);
        assert_eq!(tree.stats("a").0, 200);
        assert_eq!(tree.stats("").1, 2);
    }
}
