# git-annex-browser

[![License: MIT/Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)

A [Crossterm](https://github.com/crossterm-rs/crossterm) +
[Ratatui](https://ratatui.rs) terminal UI for exploring git-annex repositories.

**Binary name:** `git-annex-browser`

Inspired by zfs-browser: takes a directory, discovers all git-annex repos underneath, and lets you browse drives (special remotes), trust levels, last fsck timestamps, file lists, and — most importantly — **which files live on which drives**.

Pure data access via the git repo and git-annex plumbing / branch logs where possible.

## Features
- Recursive discovery of all annex repos under the given root dir.
- Per-repo view of:
  - Summary (uuid, counts, trust breakdown, last fsck)
  - **Drives / remotes** list with type, trust (color hints via symbols T?UD), present key counts, last fsck
  - Files present on a specific drive (including here)
  - All annexed files in the working tree, each annotated with short presence badges
- For each file: full list of locations (which UUIDs / names currently have the content) + key + size
- Location data comes from authoritative `git annex whereis --json --all` + git-annex branch logs
- Keyboard-driven tree navigation like zfs-browser

## Usage
```
git-annex-browser [DIR]
```

`DIR` defaults to the current directory (`.`).

- `DIR` defaults to `.` (current directory)
- Descend into a repo → drives → see files on that drive
- `r` to re-scan from disk

Keys (similar to zfs-browser):
```
↑ / k          up
↓ / j          down
PgUp / PgDn    page list
Shift+PgUp/Dn  scroll details pane
g / G          top / bottom
→ / Enter / l  descend
← / h / Back   back
r / F5         refresh / re-scan
x              toggle raw view (locations / logs for selection)
? / F1         help
q              quit
```

## Requirements
- git + git-annex installed
- A Rust toolchain (for building from source)

## Installation

### From source

```sh
git clone https://github.com/janttsu/git-annex-browser.git
cd git-annex-browser
cargo build --release
./target/release/git-annex-browser /path/with/annexes
```

You can install it with:

```sh
cargo install --path .
```

Then run:

```sh
git-annex-browser /path/with/annexes
```

## Notes

> **Note on rename**: The project was previously known as `annex-browser`. The binary and cache directory are now `git-annex-browser`. If you have an old cache at `~/.cache/annex-browser/`, you can safely delete it.

## Future Ideas
- Works great for personal collections of annexes on multiple drives (the original use case).
- For very large annexes (>50k files) the file lists are still loaded fully when you enter "all files" or a drive's file list; drive-specific filtered views are the recommended way.
- No write support yet (view only). Adding `git annex trust` / copy hints etc is possible behind an opt-in flag.
- Additional creative polish possible: coverage %, risk files (present only on untrusted), global dedup view across multiple repos, etc.

Licensed under MIT OR Apache-2.0.
