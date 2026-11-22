//! The archiving run: fetch releases and save all their files into <root>/<project>/<tag>/.

use crate::config::Effective;
use crate::forge::{Forge, Release};
use crate::util::{glob_match, sanitize};
use anyhow::{Context, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const STATE_FILE: &str = ".github-grab.json";
/// The state file's name before the rename to GitHub-Grab; read, then replaced.
pub const LEGACY_STATE_FILE: &str = ".github-release-archiver.json";

/// Reads a project's state file under its current or pre-rename name.
fn read_state_text(project_dir: &Path) -> Option<String> {
    fs::read_to_string(project_dir.join(STATE_FILE))
        .or_else(|_| fs::read_to_string(project_dir.join(LEGACY_STATE_FILE)))
        .ok()
}

#[derive(Default, Debug)]
pub struct Summary {
    pub downloaded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub pruned: usize,
    pub bytes: u64,
    pub mirror: crate::mirror::Stats,
}

/// What a project's state file records: release folders and the README copy.
pub struct Recorded {
    pub dirs: BTreeSet<String>,
    pub readme: Option<String>,
}

/// Reads the state file of a project folder, if there is one.
pub fn recorded(project_dir: &Path) -> Option<Recorded> {
    let text = read_state_text(project_dir)?;
    let state: State = serde_json::from_str(&text).ok()?;
    let dirs = state.releases.into_values().flat_map(|r| r.dirs).collect();
    Some(Recorded { dirs, readme: state.readme })
}

/// Records which releases this tool archived for a project, so retention only
/// ever deletes folders the tool itself created.
#[derive(Default, Serialize, Deserialize)]
struct State {
    /// The repo ("owner/name") this project folder belongs to.
    #[serde(default)]
    repo: Option<String>,
    /// File name of the README copy in the project folder, if one was saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    readme: Option<String>,
    releases: BTreeMap<String, ReleaseState>,
}

/// One repo to archive, with its resolved folder and options.
#[derive(Clone)]
pub struct Target {
    /// "owner/name" on github.com, otherwise "host/path" (used in logs and state files).
    pub repo: String,
    /// The repo's path on its site.
    pub path: String,
    pub forge: std::rc::Rc<Forge>,
    /// Project folder name (already sanitized).
    pub folder: String,
    pub opts: Effective,
}

/// Which repo a project folder belongs to, according to its state file.
pub fn folder_owner(project_dir: &Path) -> Option<String> {
    let text = read_state_text(project_dir)?;
    serde_json::from_str::<State>(&text).ok()?.repo
}

#[derive(Default, Serialize, Deserialize)]
struct ReleaseState {
    published_at: String,
    /// Folders relative to the project folder, e.g. "v1.2.0". Versions before
    /// 0.5 used "<OS>/<tag>"; `migrate_layout` converts those.
    dirs: BTreeSet<String>,
}

impl State {
    fn load(project_dir: &Path) -> State {
        match read_state_text(project_dir) {
            Some(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                warn!("ignoring unreadable state file in {}: {e}", project_dir.display());
                State::default()
            }),
            None => State::default(),
        }
    }

    fn save(&self, project_dir: &Path) -> Result<()> {
        if self.releases.is_empty() && !project_dir.exists() {
            return Ok(());
        }
        fs::create_dir_all(project_dir)?;
        let path = project_dir.join(STATE_FILE);
        let tmp = project_dir.join(format!("{STATE_FILE}.tmp"));
        fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        fs::rename(&tmp, &path)?;
        let _ = fs::remove_file(project_dir.join(LEGACY_STATE_FILE)); // renamed
        Ok(())
    }
}

pub fn run(root: &Path, mirrors: &[PathBuf], targets: &[Target], dry_run: bool, summary: &mut Summary) {
    if targets.is_empty() {
        warn!("no repos to check: add [[repo]] entries or enable [starred] in the config");
    }
    // An unreachable mirror is skipped for this run, not an error: the next
    // run copies whatever it missed.
    let mirrors: Vec<&PathBuf> = mirrors
        .iter()
        .filter(|m| {
            let ok = m.is_dir();
            if !ok {
                warn!("mirror {} is not reachable; skipping it this time (it will catch up later)", m.display());
            }
            ok
        })
        .collect();
    for t in targets {
        if let Err(e) = archive_repo(root, t, dry_run, summary) {
            warn!("{}: {e:#}", t.repo);
            summary.failed += 1;
        }
        // Mirror even if checking GitHub failed: what's archived is still good.
        for m in &mirrors {
            if let Err(e) = crate::mirror::sync_project(&root.join(&t.folder), &m.join(&t.folder), dry_run, &mut summary.mirror) {
                warn!("{}: copying to mirror {} failed: {e:#}", t.repo, m.display());
            }
        }
    }
}

fn archive_repo(root: &Path, t: &Target, dry_run: bool, summary: &mut Summary) -> Result<()> {
    let client = t.forge.as_ref();
    let rc = t; // keeps the log lines below short: rc.repo
    let opts = &t.opts;
    let project = &t.folder;
    let project_dir = root.join(project);

    // Never download more than retention would keep, or we'd fetch and delete
    // the same release on every run.
    let wanted = opts.keep_versions.map_or(opts.releases, |k| opts.releases.min(k));
    let releases: Vec<Release> = client
        .releases(&t.path, wanted)?
        .into_iter()
        .filter(|r| !r.draft && (opts.include_prereleases || !r.prerelease))
        .take(wanted)
        .collect();
    if releases.is_empty() {
        log::debug!("{}: no matching releases", rc.repo);
        return Ok(());
    }

    let mut state = State::load(&project_dir);
    if !dry_run {
        migrate_layout(&mut state, &project_dir, &rc.repo);
    }
    for rel in &releases {
        let tag = sanitize(&rel.tag_name);
        let mut dirs = BTreeSet::new();

        for asset in &rel.assets {
            if !wanted_asset(&asset.name, opts) {
                continue;
            }
            let dest = project_dir.join(&tag).join(sanitize(&asset.name));
            dirs.insert(tag.clone());
            fetch(client, summary, dry_run, &rc.repo, &dest, &asset.url, asset.accept, asset.size, asset.digest.as_deref());
        }

        if opts.include_source {
            if let Some((url, accept)) = &rel.source_zip {
                let dest = project_dir.join(&tag).join(sanitize(&format!("{project}-{}-source.zip", rel.tag_name)));
                dirs.insert(tag.clone());
                // Size isn't known ahead of time for generated archives; skip if present.
                fetch(client, summary, dry_run, &rc.repo, &dest, url, accept, None, None);
            }
        }

        if !dry_run && !dirs.is_empty() {
            let entry = state.releases.entry(rel.tag_name.clone()).or_default();
            entry.published_at = rel.published_at.clone().or_else(|| rel.created_at.clone()).unwrap_or_default();
            // Only record folders that actually exist (a failed download leaves none).
            entry.dirs.extend(dirs.into_iter().filter(|d| project_dir.join(d).is_dir()));
            if entry.dirs.is_empty() {
                state.releases.remove(&rel.tag_name);
            }
        }
    }

    if !dry_run {
        if state.repo.is_none() && project_dir.exists() {
            state.repo = Some(rc.repo.clone());
        }
        if let Some(keep) = opts.keep_versions {
            summary.pruned += prune(&mut state, &project_dir, keep, &rc.repo);
        }
        // Only for projects that have archived files, so starred repos without
        // releases don't get folders just for a README.
        if opts.save_readme && project_dir.is_dir() {
            if let Err(e) = save_readme(client, &rc.repo, &t.path, &project_dir, &mut state) {
                warn!("{}: couldn't save the README: {e:#}", rc.repo);
            }
        }
        state.save(&project_dir).context("cannot save state file")?;
    }
    Ok(())
}

fn wanted_asset(name: &str, opts: &Effective) -> bool {
    (opts.include.is_empty() || opts.include.iter().any(|p| glob_match(p, name)))
        && !opts.exclude.iter().any(|p| glob_match(p, name))
}

/// Versions before 0.5 sorted files into <OS>/<tag>/ (plus Source/<tag>/).
/// Moves those recorded folders' files into <tag>/, so nothing is downloaded
/// twice. Only folders listed in the state file are touched.
fn migrate_layout(state: &mut State, project_dir: &Path, repo: &str) {
    for rel in state.releases.values_mut() {
        let old: Vec<String> = rel.dirs.iter().filter(|d| Path::new(d).components().count() == 2).cloned().collect();
        for d in old {
            if !is_safe_relative(&d) {
                continue;
            }
            let src = project_dir.join(&d);
            let Some(tag) = Path::new(&d).file_name().map(|t| t.to_string_lossy().to_string()) else { continue };
            let dest = project_dir.join(&tag);
            if src.is_dir() {
                if let Err(e) = move_contents(&src, &dest) {
                    warn!("{repo}: couldn't move {} to {}: {e}", src.display(), dest.display());
                    continue;
                }
                info!("{repo}: moved {} to {}", src.display(), dest.display());
                let _ = fs::remove_dir(&src); // fails (harmlessly) if a conflicting file was left
                if let Some(os_dir) = src.parent() {
                    let _ = fs::remove_dir(os_dir); // only if now empty
                }
            }
            rel.dirs.remove(&d);
            rel.dirs.insert(tag);
        }
    }
}

fn move_contents(src: &Path, dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        if !target.exists() {
            fs::rename(entry.path(), &target)?;
        } else if fs::metadata(&target)?.len() == entry.metadata()?.len() {
            fs::remove_file(entry.path())?; // same file already in place
        } // else: leave it where it is rather than overwrite anything
    }
    Ok(())
}

/// Writes the repo's README into the project folder when it's new or changed.
fn save_readme(client: &Forge, repo: &str, path: &str, project_dir: &Path, state: &mut State) -> Result<()> {
    let Some((name, bytes)) = client.readme(path)? else { return Ok(()) };
    let name = sanitize(&name);
    let dest = project_dir.join(&name);
    if fs::read(&dest).ok().as_deref() != Some(bytes.as_slice()) {
        let tmp = project_dir.join(format!("{name}.part"));
        fs::write(&tmp, &bytes)?;
        if dest.exists() {
            fs::remove_file(&dest)?;
        }
        fs::rename(&tmp, &dest)?;
        info!("{repo}: saved {}", dest.display());
    }
    // If the README was renamed upstream (README.rst -> README.md), remove the
    // old copy, but only the one this program recorded.
    if let Some(old) = state.readme.replace(name.clone()) {
        if old != name && is_plain_file_name(&old) {
            let _ = fs::remove_file(project_dir.join(&old));
        }
    }
    Ok(())
}

fn is_plain_file_name(name: &str) -> bool {
    let mut c = Path::new(name).components();
    matches!((c.next(), c.next()), (Some(std::path::Component::Normal(_)), None))
        && name != STATE_FILE
        && name != LEGACY_STATE_FILE
}

#[allow(clippy::too_many_arguments)]
fn fetch(client: &Forge, summary: &mut Summary, dry_run: bool, repo: &str, dest: &Path, url: &str, accept: &str, size: Option<u64>, digest: Option<&str>) {
    if let Ok(meta) = fs::metadata(dest) {
        if size.is_none_or(|s| s == meta.len()) {
            summary.skipped += 1;
            return;
        }
        warn!("{repo}: {} exists with a different size; downloading again", dest.display());
    }
    if dry_run {
        info!("{repo}: would download {}", dest.display());
        summary.downloaded += 1;
        return;
    }
    let result = dest
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .with_context(|| format!("cannot create folder for {}", dest.display()))
        .and_then(|_| client.download(url, accept, dest, size, digest));
    match result {
        Ok(n) => {
            info!("{repo}: saved {} ({})", dest.display(), human(n));
            summary.downloaded += 1;
            summary.bytes += n;
        }
        Err(e) => {
            warn!("{repo}: failed to download {}: {e:#}", dest.display());
            summary.failed += 1;
            // Don't leave an empty tag folder behind.
            if let Some(dir) = dest.parent() {
                let _ = fs::remove_dir(dir);
            }
        }
    }
}

/// Removes releases beyond the newest `keep` (by publish date), deleting only
/// the folders recorded in the state file.
fn prune(state: &mut State, project_dir: &Path, keep: usize, repo: &str) -> usize {
    let mut tags: Vec<(String, String)> =
        state.releases.iter().map(|(t, r)| (r.published_at.clone(), t.clone())).collect();
    tags.sort_by(|a, b| b.cmp(a)); // newest first; ISO-8601 sorts lexically
    let mut removed = 0;
    for (_, tag) in tags.into_iter().skip(keep) {
        let Some(rel) = state.releases.remove(&tag) else { continue };
        for d in &rel.dirs {
            let dir: PathBuf = project_dir.join(d);
            if !is_safe_relative(d) || !dir.is_dir() {
                continue;
            }
            match fs::remove_dir_all(&dir) {
                Ok(()) => {
                    info!("{repo}: removed old release folder {}", dir.display());
                    // Tidy up a pre-0.5 OS folder left empty.
                    if let Some(parent) = dir.parent().filter(|p| *p != project_dir) {
                        let _ = fs::remove_dir(parent);
                    }
                }
                Err(e) => warn!("{repo}: cannot remove {}: {e}", dir.display()),
            }
        }
        removed += 1;
    }
    removed
}

/// Guards against a hand-edited state file pointing outside the project folder.
/// "<tag>", or "<OS>/<tag>" from versions before 0.5.
fn is_safe_relative(p: &str) -> bool {
    let path = Path::new(p);
    (1..=2).contains(&path.components().count())
        && path.components().all(|c| matches!(c, std::path::Component::Normal(_)))
        && p != STATE_FILE
        && p != LEGACY_STATE_FILE
}

pub fn human(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", units[i]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_relative_paths() {
        assert!(is_safe_relative("v1.0"));
        assert!(is_safe_relative("Windows/v1.0"));
        assert!(!is_safe_relative("../x"));
        assert!(!is_safe_relative("Windows/../../x"));
        assert!(!is_safe_relative("/etc/x"));
        assert!(!is_safe_relative("a/b/c"));
        assert!(!is_safe_relative(STATE_FILE));
    }

    #[test]
    fn migrates_old_os_folders() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        for (d, f) in [("Windows/v1", "a.exe"), ("Linux/v1", "a.tar.gz"), ("Source/v1", "src.zip")] {
            fs::create_dir_all(p.join(d)).unwrap();
            fs::write(p.join(d).join(f), f).unwrap();
        }
        let mut state = State::default();
        state.releases.insert(
            "v1".into(),
            ReleaseState { published_at: "2024".into(), dirs: ["Windows/v1", "Linux/v1", "Source/v1"].map(String::from).into() },
        );
        migrate_layout(&mut state, p, "t/t");
        for f in ["a.exe", "a.tar.gz", "src.zip"] {
            assert!(p.join("v1").join(f).is_file(), "{f}");
        }
        assert!(!p.join("Windows").exists() && !p.join("Linux").exists() && !p.join("Source").exists());
        assert_eq!(state.releases["v1"].dirs.iter().collect::<Vec<_>>(), ["v1"]);
    }

    #[test]
    fn renames_pre_rename_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        fs::write(p.join(LEGACY_STATE_FILE), r#"{"repo": "a/b", "releases": {}}"#).unwrap();
        assert_eq!(folder_owner(p).as_deref(), Some("a/b"));
        let state = State::load(p);
        state.save(p).unwrap();
        assert!(p.join(STATE_FILE).is_file() && !p.join(LEGACY_STATE_FILE).exists());
        assert_eq!(folder_owner(p).as_deref(), Some("a/b"));
    }

    #[test]
    fn plain_file_names() {
        assert!(is_plain_file_name("README.md"));
        assert!(!is_plain_file_name("../README.md"));
        assert!(!is_plain_file_name("Windows/README.md"));
        assert!(!is_plain_file_name(STATE_FILE));
    }

    #[test]
    fn prune_keeps_newest() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::default();
        for (tag, date) in [("v1", "2024-01-01T00:00:00Z"), ("v2", "2024-02-01T00:00:00Z"), ("v3", "2024-03-01T00:00:00Z")] {
            let rel = tag.to_string();
            fs::create_dir_all(dir.path().join(&rel)).unwrap();
            state.releases.insert(tag.into(), ReleaseState { published_at: date.into(), dirs: [rel].into() });
        }
        assert_eq!(prune(&mut state, dir.path(), 2, "t/t"), 1);
        assert!(!dir.path().join("v1").exists());
        assert!(dir.path().join("v2").exists() && dir.path().join("v3").exists());
        assert_eq!(state.releases.keys().collect::<Vec<_>>(), ["v2", "v3"]);
    }
}
