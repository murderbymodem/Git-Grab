//! The archiving run: fetch releases, file assets into <root>/<project>/<OS>/<tag>/.

use crate::classify::classify;
use crate::config::Effective;
use crate::github::{Client, Release};
use crate::util::{glob_match, sanitize};
use anyhow::{Context, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const STATE_FILE: &str = ".github-release-archiver.json";
pub const SOURCE_FOLDER: &str = "Source";

#[derive(Default, Debug)]
pub struct Summary {
    pub downloaded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub pruned: usize,
    pub bytes: u64,
}

/// Records which releases this tool archived for a project, so retention only
/// ever deletes folders the tool itself created.
#[derive(Default, Serialize, Deserialize)]
struct State {
    /// The repo ("owner/name") this project folder belongs to.
    #[serde(default)]
    repo: Option<String>,
    releases: BTreeMap<String, ReleaseState>,
}

/// One repo to archive, with its resolved folder and options.
#[derive(Debug, Clone)]
pub struct Target {
    pub repo: String,
    /// Project folder name (already sanitized).
    pub folder: String,
    pub opts: Effective,
}

/// Which repo a project folder belongs to, according to its state file.
pub fn folder_owner(project_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(project_dir.join(STATE_FILE)).ok()?;
    serde_json::from_str::<State>(&text).ok()?.repo
}

#[derive(Default, Serialize, Deserialize)]
struct ReleaseState {
    published_at: String,
    /// Folders relative to the project folder, e.g. "Windows/v1.2.0".
    dirs: BTreeSet<String>,
}

impl State {
    fn load(project_dir: &Path) -> State {
        let path = project_dir.join(STATE_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                warn!("ignoring unreadable state file {}: {e}", path.display());
                State::default()
            }),
            Err(_) => State::default(),
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
        Ok(())
    }
}

pub fn run(root: &Path, targets: &[Target], client: &Client, dry_run: bool, summary: &mut Summary) {
    if targets.is_empty() {
        warn!("no repos to check: add [[repo]] entries or enable [starred] in the config");
    }
    for t in targets {
        if let Err(e) = archive_repo(root, t, client, dry_run, summary) {
            warn!("{}: {e:#}", t.repo);
            summary.failed += 1;
        }
    }
}

fn archive_repo(root: &Path, t: &Target, client: &Client, dry_run: bool, summary: &mut Summary) -> Result<()> {
    let rc = t; // keeps the log lines below short: rc.repo
    let opts = &t.opts;
    let project = &t.folder;
    let project_dir = root.join(project);

    // Never download more than retention would keep, or we'd fetch and delete
    // the same release on every run.
    let wanted = opts.keep_versions.map_or(opts.releases, |k| opts.releases.min(k));
    let releases: Vec<Release> = client
        .releases(&rc.repo, wanted)?
        .into_iter()
        .filter(|r| !r.draft && (opts.include_prereleases || !r.prerelease))
        .take(wanted)
        .collect();
    if releases.is_empty() {
        log::debug!("{}: no matching releases", rc.repo);
        return Ok(());
    }

    let mut state = State::load(&project_dir);
    for rel in &releases {
        let tag = sanitize(&rel.tag_name);
        let mut dirs = BTreeSet::new();

        for asset in &rel.assets {
            let os = sanitize(&classify(&asset.name, &opts.os_rules));
            if !wanted_asset(&asset.name, &os, opts) {
                continue;
            }
            let rel_dir = format!("{os}/{tag}");
            let dest = project_dir.join(&os).join(&tag).join(sanitize(&asset.name));
            dirs.insert(rel_dir);
            fetch(client, summary, dry_run, &rc.repo, &dest, &asset.url, "application/octet-stream", Some(asset.size), asset.digest.as_deref());
        }

        if opts.include_source {
            if let Some(url) = &rel.zipball_url {
                let dest = project_dir.join(SOURCE_FOLDER).join(&tag).join(sanitize(&format!("{project}-{}.zip", rel.tag_name)));
                dirs.insert(format!("{SOURCE_FOLDER}/{tag}"));
                // Size isn't known ahead of time for generated archives; skip if present.
                fetch(client, summary, dry_run, &rc.repo, &dest, url, "application/vnd.github+json", None, None);
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
        state.save(&project_dir).context("cannot save state file")?;
    }
    Ok(())
}

fn wanted_asset(name: &str, os: &str, opts: &Effective) -> bool {
    (opts.include.is_empty() || opts.include.iter().any(|p| glob_match(p, name)))
        && !opts.exclude.iter().any(|p| glob_match(p, name))
        && (opts.only_os.is_empty() || opts.only_os.iter().any(|o| o.eq_ignore_ascii_case(os)))
}

#[allow(clippy::too_many_arguments)]
fn fetch(client: &Client, summary: &mut Summary, dry_run: bool, repo: &str, dest: &Path, url: &str, accept: &str, size: Option<u64>, digest: Option<&str>) {
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
                    // Tidy up an OS folder left empty.
                    if let Some(parent) = dir.parent() {
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
fn is_safe_relative(p: &str) -> bool {
    let path = Path::new(p);
    path.components().count() == 2 && path.components().all(|c| matches!(c, std::path::Component::Normal(_)))
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
        assert!(is_safe_relative("Windows/v1.0"));
        assert!(!is_safe_relative("../x"));
        assert!(!is_safe_relative("Windows/../../x"));
        assert!(!is_safe_relative("/etc/x"));
        assert!(!is_safe_relative("Windows"));
    }

    #[test]
    fn prune_keeps_newest() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::default();
        for (tag, date) in [("v1", "2024-01-01T00:00:00Z"), ("v2", "2024-02-01T00:00:00Z"), ("v3", "2024-03-01T00:00:00Z")] {
            let rel = format!("Linux/{tag}");
            fs::create_dir_all(dir.path().join(&rel)).unwrap();
            state.releases.insert(tag.into(), ReleaseState { published_at: date.into(), dirs: [rel].into() });
        }
        assert_eq!(prune(&mut state, dir.path(), 2, "t/t"), 1);
        assert!(!dir.path().join("Linux/v1").exists());
        assert!(dir.path().join("Linux/v2").exists() && dir.path().join("Linux/v3").exists());
        assert_eq!(state.releases.keys().collect::<Vec<_>>(), ["v2", "v3"]);
    }
}
