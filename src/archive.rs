//! The archiving run: fetch releases and save all their files into <root>/<project>/<tag>/.

use crate::config::Effective;
use crate::forge::{Forge, Release};
use crate::util::{glob_match, sanitize};
use anyhow::{Context, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const STATE_FILE: &str = ".git-grab.json";

fn read_state_text(project_dir: &Path) -> Option<String> {
    fs::read_to_string(project_dir.join(STATE_FILE)).ok()
}

#[derive(Default, Debug)]
pub struct Summary {
    pub downloaded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub pruned: usize,
    pub bytes: u64,
    /// Project folders removed because their repo was unstarred.
    pub unstarred: usize,
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
    /// Archived only because it was starred, so `remove_unstarred` may delete it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    via_stars: bool,
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
    /// The site's folder in the archive, e.g. "GitHub" or "git.magicbean.dev".
    pub source: String,
    /// Project folder name inside it (already sanitized).
    pub folder: String,
    pub opts: Effective,
    /// Archived because it's starred (not listed in [[repo]] or [[account]]).
    pub via_stars: bool,
}

/// Which repo a project folder belongs to, according to its state file.
pub fn folder_owner(project_dir: &Path) -> Option<String> {
    let text = read_state_text(project_dir)?;
    serde_json::from_str::<State>(&text).ok()?.repo
}

#[derive(Default, Serialize, Deserialize)]
struct ReleaseState {
    published_at: String,
    /// Folders relative to the project folder, e.g. "v1.2.0".
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
            let rel = Path::new(&t.source).join(&t.folder);
            if let Err(e) = crate::mirror::sync_project(&root.join(&rel), &m.join(&rel), dry_run, &mut summary.mirror) {
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
    let project_dir = root.join(&t.source).join(project);

    // Never download more than retention would keep, or we'd fetch and delete
    // the same release on every run.
    let wanted = opts.keep_versions.map_or(opts.releases, |k| opts.releases.min(k));
    let (releases, fell_back) = select_releases(client.releases(&t.path, wanted)?, opts, wanted);
    if fell_back {
        info!("{}: has only prereleases, so archiving its newest prerelease(s)", rc.repo);
    }
    if releases.is_empty() {
        log::debug!("{}: no matching releases", rc.repo);
        return Ok(());
    }

    let mut state = State::load(&project_dir);
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
        // Updated every run: listing a starred repo in [[repo]] protects it.
        state.via_stars = t.via_stars;
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

/// Picks the releases to archive, newest first. Drafts are never archived.
/// Prereleases only if they're turned on, or if the project has nothing else
/// (`prereleases_if_no_release`); the second value says whether that happened.
fn select_releases(all: Vec<Release>, opts: &Effective, wanted: usize) -> (Vec<Release>, bool) {
    let published: Vec<Release> = all.into_iter().filter(|r| !r.draft).collect();
    let has_regular = published.iter().any(|r| !r.prerelease);
    let fallback = !opts.include_prereleases && opts.prereleases_if_no_release && !has_regular && !published.is_empty();
    let use_pre = opts.include_prereleases || fallback;
    let picked = published.into_iter().filter(|r| use_pre || !r.prerelease).take(wanted).collect();
    (picked, fallback)
}

/// Deletes the folders of projects that were archived only because they were
/// starred and are no longer starred, in `site_dir` (the GitHub site folder of
/// the archive or a mirror). `keep` holds every repo still starred or still
/// being archived for another reason (lowercase ids). Only files the state
/// file records are deleted; a folder is removed only once it's empty.
pub fn remove_unstarred(site_dir: &Path, keep: &HashSet<String>, dry_run: bool, summary: &mut Summary) {
    let Ok(entries) = fs::read_dir(site_dir) else { return };
    for entry in entries.flatten() {
        let dir = entry.path();
        let Some(text) = read_state_text(&dir) else { continue };
        let Ok(state) = serde_json::from_str::<State>(&text) else { continue };
        let Some(repo) = state.repo.as_deref().filter(|_| state.via_stars) else { continue };
        if keep.contains(&repo.to_lowercase()) {
            continue;
        }
        if dry_run {
            info!("{repo}: no longer starred; would remove {}", dir.display());
            summary.unstarred += 1;
            continue;
        }
        let recorded = state.releases.values().flat_map(|r| r.dirs.iter()).filter(|d| is_safe_relative(d));
        for d in recorded {
            if let Err(e) = fs::remove_dir_all(dir.join(d)).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }) {
                warn!("{repo}: cannot remove {}: {e}", dir.join(d).display());
            }
        }
        if let Some(r) = state.readme.as_deref().filter(|r| is_plain_file_name(r)) {
            let _ = fs::remove_file(dir.join(r));
        }
        let _ = fs::remove_file(dir.join(STATE_FILE));
        match fs::remove_dir(&dir) {
            Ok(()) => info!("{repo}: no longer starred; removed {}", dir.display()),
            Err(_) => info!("{repo}: no longer starred; removed its archived files from {} (kept files you added)", dir.display()),
        }
        summary.unstarred += 1;
    }
}

fn wanted_asset(name: &str, opts: &Effective) -> bool {
    (opts.include.is_empty() || opts.include.iter().any(|p| glob_match(p, name)))
        && !opts.exclude.iter().any(|p| glob_match(p, name))
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
                }
                Err(e) => warn!("{repo}: cannot remove {}: {e}", dir.display()),
            }
        }
        removed += 1;
    }
    removed
}

/// Guards against a hand-edited state file pointing outside the project
/// folder: a release folder is a single plain name.
pub fn is_safe_relative(p: &str) -> bool {
    let mut c = Path::new(p).components();
    matches!((c.next(), c.next()), (Some(std::path::Component::Normal(_)), None)) && p != STATE_FILE
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
        assert!(!is_safe_relative("Windows/v1.0"));
        assert!(!is_safe_relative("../x"));
        assert!(!is_safe_relative("/etc/x"));
        assert!(!is_safe_relative(STATE_FILE));
    }

    fn rel(tag: &str, prerelease: bool, draft: bool) -> Release {
        Release {
            tag_name: tag.into(),
            draft,
            prerelease,
            published_at: None,
            created_at: None,
            assets: vec![],
            source_zip: None,
        }
    }

    fn tags(v: &[Release]) -> Vec<&str> {
        v.iter().map(|r| r.tag_name.as_str()).collect()
    }

    #[test]
    fn release_selection() {
        let cfg = crate::config::Config::parse("archive_root = '/x'").unwrap();
        let mut opts = cfg.effective(&Default::default());
        // Regular releases win over newer nightlies (like LongshotLauncher).
        let mixed = || vec![rel("nightly-2", true, false), rel("v1.2", false, false), rel("nightly-1", true, false)];
        assert_eq!(tags(&select_releases(mixed(), &opts, 1).0), ["v1.2"]);
        // Only nightlies (like Beanstalk): take the newest instead of nothing.
        let only_pre = || vec![rel("draft", false, true), rel("nightly-9", true, false), rel("nightly-8", true, false)];
        let (picked, fell_back) = select_releases(only_pre(), &opts, 1);
        assert_eq!((tags(&picked), fell_back), (vec!["nightly-9"], true));
        // ... unless that's turned off.
        opts.prereleases_if_no_release = false;
        assert!(select_releases(only_pre(), &opts, 1).0.is_empty());
        // include_prereleases takes everything but drafts, newest first.
        opts.include_prereleases = true;
        let (picked, fell_back) = select_releases(mixed(), &opts, 2);
        assert_eq!((tags(&picked), fell_back), (vec!["nightly-2", "v1.2"], false));
    }

    #[test]
    fn removes_only_unstarred_starred_projects() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path();
        let project = |folder: &str, repo: &str, via_stars: bool| {
            let p = site.join(folder);
            fs::create_dir_all(p.join("v1")).unwrap();
            fs::write(p.join("v1/app.zip"), "x").unwrap();
            fs::write(p.join("README.md"), "readme").unwrap();
            let state = format!(
                r#"{{"repo": "{repo}", "readme": "README.md", "via_stars": {via_stars}, "releases": {{"v1": {{"published_at": "2026", "dirs": ["v1"]}}}}}}"#
            );
            fs::write(p.join(STATE_FILE), state).unwrap();
        };
        project("gone", "a/gone", true); // unstarred: deleted
        project("mine", "a/mine", true); // unstarred, but has a file you added
        fs::write(site.join("mine/notes.txt"), "keep me").unwrap();
        project("still", "a/still", true); // still starred
        project("listed", "a/listed", false); // from [[repo]]: never touched
        let keep: HashSet<String> = ["a/still".to_string()].into();

        let mut s = Summary::default();
        remove_unstarred(site, &keep, true, &mut s); // dry run changes nothing
        assert_eq!(s.unstarred, 2);
        assert!(site.join("gone/v1/app.zip").is_file());

        let mut s = Summary::default();
        remove_unstarred(site, &keep, false, &mut s);
        assert_eq!(s.unstarred, 2);
        assert!(!site.join("gone").exists());
        assert!(site.join("mine/notes.txt").is_file() && !site.join("mine/v1").exists() && !site.join("mine/README.md").exists());
        assert!(site.join("still/v1/app.zip").is_file());
        assert!(site.join("listed/v1/app.zip").is_file());
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
