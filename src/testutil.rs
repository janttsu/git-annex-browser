//! Helpers shared by unit tests.

use crate::annex::{AnnexMetadata, AnnexedFile, Remote, TrustLevel};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Empty metadata for a repo at `root` whose own uuid is `u`.
pub fn meta(root: &str) -> AnnexMetadata {
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

pub fn remote(uuid: &str, name: &str, trust: TrustLevel) -> Remote {
    Remote::new(uuid.into(), name.into(), HashMap::new(), trust, None)
}

pub fn file(path: &str, size: u64) -> AnnexedFile {
    AnnexedFile {
        path: path.into(),
        key: format!("SHA256E-s{size}--{}", path.replace('/', "_")),
        size: Some(size),
    }
}

/// Metadata with `here` (trusted), `usb` (semitrusted) and `glacier` (untrusted),
/// numcopies 2 and a few files spread over them.
pub fn sample_meta() -> AnnexMetadata {
    let mut m = meta("/data/photos");
    m.uuid = "here".into();
    m.numcopies = Some(2);
    for r in [
        remote("here", "here", TrustLevel::Trusted),
        remote("usb", "usb", TrustLevel::SemiTrusted),
        remote("glacier", "glacier", TrustLevel::UnTrusted),
    ] {
        m.remotes.insert(r.uuid.clone(), r);
    }
    let placements: &[(&str, u64, &[&str])] = &[
        ("2024/a.jpg", 100, &["here", "usb"]),
        ("2024/b.jpg", 200, &["here"]),
        ("2024/raw/c.cr2", 5000, &["usb", "glacier"]),
        ("2023/d.jpg", 50, &["glacier"]),
        ("notes.txt", 10, &["usb"]),
    ];
    for (path, size, locs) in placements {
        let f = file(path, *size);
        m.locations.insert(
            f.key.clone(),
            locs.iter().map(|u| (*u).into()).collect::<HashSet<_>>(),
        );
        m.files.push(f);
    }
    m.ensure_sizes();
    m
}

pub fn annex_available() -> bool {
    Command::new("git")
        .args(["annex", "version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Run git in `dir`, panicking on failure.
pub fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A real git-annex repo named `name` in `parent` with a few annexed files.
/// Returns None (test should skip) when git-annex is not installed, unless running in CI.
pub fn real_annex(parent: &Path, name: &str) -> Option<PathBuf> {
    if !annex_available() {
        assert!(
            std::env::var_os("CI").is_none(),
            "git-annex must be installed in CI"
        );
        return None;
    }
    let root = parent.join(name);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "t@t"]);
    git(&root, &["config", "user.name", "t"]);
    git(&root, &["annex", "init", "-q", name]);
    std::fs::create_dir_all(root.join("videos/clips")).unwrap();
    std::fs::create_dir_all(root.join("photos")).unwrap();
    std::fs::write(root.join("photos/small.jpg"), vec![b'x'; 1000]).unwrap();
    std::fs::write(root.join("videos/big.mkv"), vec![b'y'; 50000]).unwrap();
    std::fs::write(root.join("videos/clips/mid.bin"), vec![b'z'; 8000]).unwrap();
    git(&root, &["annex", "add", "-q", "photos", "videos"]);
    git(&root, &["commit", "-q", "-m", "add"]);
    Some(root)
}

/// git-annex makes object dirs read-only; allow tempdir cleanup.
pub fn make_writable(path: &Path) {
    let _ = Command::new("chmod").args(["-R", "u+w"]).arg(path).status();
}
