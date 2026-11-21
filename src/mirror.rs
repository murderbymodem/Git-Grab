//! Copies project folders from `archive_root` to the mirror folders.
//!
//! Files are downloaded from GitHub once, into the main archive, and then
//! copied here. Each run brings a mirror up to date, so one that was offline
//! catches up later. Folders are deleted from a mirror only when the mirror's
//! own copy of the state file records them and the main archive no longer does
//! (i.e. retention removed that release).

use crate::archive::{recorded, Recorded, LEGACY_STATE_FILE, STATE_FILE};
use anyhow::{Context, Result};
use log::{info, warn};
use std::fs;
use std::path::{Component, Path};

/// Files up to this size are compared by content, so a README or state file
/// that changed but kept its size is still copied. Larger files (release
/// downloads, which never change under the same name) are compared by size.
const COMPARE_CONTENT_BELOW: u64 = 1024 * 1024;

#[derive(Default, Debug)]
pub struct Stats {
    pub copied: usize,
    pub bytes: u64,
    pub removed: usize,
}

/// Brings `dst` (a project folder in a mirror) up to date with `src`.
pub fn sync_project(src: &Path, dst: &Path, dry_run: bool, stats: &mut Stats) -> Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    if let (Some(old), Some(new)) = (recorded(dst), recorded(src)) {
        remove_dropped(&old, &new, dst, dry_run, stats);
    }
    copy_tree(src, dst, dry_run, stats)?;
    // The state file goes last: if the copy is interrupted, the mirror's state
    // never lists folders that weren't copied.
    copy_file_if_changed(&src.join(STATE_FILE), &dst.join(STATE_FILE), dry_run, stats)?;
    if !dry_run && dst.join(STATE_FILE).is_file() {
        let _ = fs::remove_file(dst.join(LEGACY_STATE_FILE)); // name from before the rename
    }
    Ok(())
}

fn remove_dropped(old: &Recorded, new: &Recorded, dst: &Path, dry_run: bool, stats: &mut Stats) {
    let mut gone: Vec<&String> = old.dirs.iter().filter(|d| !new.dirs.contains(*d)).collect();
    if let Some(r) = old.readme.as_ref().filter(|r| new.readme.as_ref() != Some(*r)) {
        gone.push(r);
    }
    for rel in gone {
        if !is_safe(rel) {
            continue;
        }
        let path = dst.join(rel);
        if !path.exists() {
            continue;
        }
        if dry_run {
            info!("would remove {} from the mirror", path.display());
            continue;
        }
        let result = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
        match result {
            Ok(()) => {
                info!("removed {} from the mirror", path.display());
                stats.removed += 1;
                if let Some(parent) = path.parent().filter(|p| *p != dst) {
                    let _ = fs::remove_dir(parent); // a pre-0.5 OS folder, if now empty
                }
            }
            Err(e) => warn!("cannot remove {}: {e}", path.display()),
        }
    }
}

/// Relative paths from a state file: at most two plain components.
fn is_safe(p: &str) -> bool {
    let path = Path::new(p);
    (1..=2).contains(&path.components().count())
        && path.components().all(|c| matches!(c, Component::Normal(_)))
        && p != STATE_FILE
        && p != LEGACY_STATE_FILE
}

fn copy_tree(src: &Path, dst: &Path, dry_run: bool, stats: &mut Stats) -> Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("cannot read {}", src.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        if name_s == STATE_FILE || name_s == LEGACY_STATE_FILE || name_s.ends_with(".part") || name_s.ends_with(".tmp") {
            continue; // state file is copied last; skip unfinished downloads
        }
        let from = entry.path();
        let to = dst.join(&name);
        if entry.file_type()?.is_dir() {
            copy_tree(&from, &to, dry_run, stats)?;
        } else {
            copy_file_if_changed(&from, &to, dry_run, stats)?;
        }
    }
    Ok(())
}

fn copy_file_if_changed(from: &Path, to: &Path, dry_run: bool, stats: &mut Stats) -> Result<()> {
    let Ok(src_meta) = fs::metadata(from) else { return Ok(()) };
    if let Ok(dst_meta) = fs::metadata(to) {
        let same = src_meta.len() == dst_meta.len()
            && (src_meta.len() >= COMPARE_CONTENT_BELOW || fs::read(from).ok() == fs::read(to).ok());
        if same {
            return Ok(());
        }
    }
    if dry_run {
        info!("would copy {} to {}", from.display(), to.display());
        stats.copied += 1;
        return Ok(());
    }
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let part = crate::github::part_path(to);
    let n = fs::copy(from, &part).with_context(|| format!("cannot copy {} to {}", from.display(), part.display()));
    let n = match n {
        Ok(n) => n,
        Err(e) => {
            let _ = fs::remove_file(&part);
            return Err(e);
        }
    };
    if to.exists() {
        fs::remove_file(to)?;
    }
    fs::rename(&part, to).with_context(|| format!("cannot rename to {}", to.display()))?;
    stats.copied += 1;
    stats.bytes += n;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, text: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn state(dirs: &[&str], readme: &str) -> String {
        let releases: Vec<String> =
            dirs.iter().map(|d| format!(r#""{d}": {{"published_at": "2026", "dirs": ["{d}"]}}"#)).collect();
        format!(r#"{{"repo": "a/b", "readme": "{readme}", "releases": {{{}}}}}"#, releases.join(","))
    }

    #[test]
    fn copies_new_and_changed_files_and_removes_pruned_releases() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let (s, d) = (src.path(), dst.path());

        // First sync: everything is copied, the unfinished download isn't.
        write(&s.join("v1/app.zip"), "one");
        write(&s.join("v1/app.zip.part"), "partial");
        write(&s.join("README.md"), "old readme");
        write(&s.join(STATE_FILE), &state(&["v1"], "README.md"));
        let mut st = Stats::default();
        sync_project(s, d, false, &mut st).unwrap();
        assert_eq!(st.copied, 3);
        assert!(d.join("v1/app.zip").is_file() && !d.join("v1/app.zip.part").exists());

        // Nothing changed: nothing copied.
        let mut st = Stats::default();
        sync_project(s, d, false, &mut st).unwrap();
        assert_eq!(st.copied, 0);

        // New release v2, v1 pruned, README changed (same size) and renamed.
        fs::remove_dir_all(s.join("v1")).unwrap();
        fs::remove_file(s.join("README.md")).unwrap();
        write(&s.join("v2/app.zip"), "two");
        write(&s.join("README.rst"), "new readme");
        write(&s.join(STATE_FILE), &state(&["v2"], "README.rst"));
        write(&d.join("keep-me.txt"), "not ours"); // never touched
        let mut st = Stats::default();
        sync_project(s, d, false, &mut st).unwrap();
        assert!(!d.join("v1").exists() && !d.join("README.md").exists());
        assert!(d.join("v2/app.zip").is_file() && d.join("keep-me.txt").is_file());
        assert_eq!(fs::read_to_string(d.join("README.rst")).unwrap(), "new readme");
        assert_eq!(st.removed, 2);
    }

    #[test]
    fn dry_run_changes_nothing() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        write(&src.path().join("v1/a"), "x");
        let mut st = Stats::default();
        sync_project(src.path(), dst.path(), true, &mut st).unwrap();
        assert_eq!(st.copied, 1);
        assert!(!dst.path().join("v1").exists());
    }
}
