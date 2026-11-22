//! Builds the list of repos to check: [[repo]] entries, [[account]] entries
//! (every repo of a user or organization, on any site) and starred repos.

use crate::archive::{folder_owner, Target};
use crate::config::{Config, Effective};
use crate::forge::{self, Forge, Kind, Listed, Location};
use crate::util::{glob_match, sanitize};
use log::{info, warn};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

/// One connection per site, created on first use. A site that can't be
/// reached or identified is reported once and its repos are skipped.
pub struct Sites<'a> {
    cfg: &'a Config,
    cache: Option<PathBuf>,
    forges: HashMap<String, Option<Rc<Forge>>>,
    pub failures: usize,
}

impl<'a> Sites<'a> {
    pub fn new(cfg: &'a Config, cache: Option<PathBuf>) -> Self {
        Sites { cfg, cache, forges: HashMap::new(), failures: 0 }
    }

    pub fn get(&mut self, loc: &Location) -> Option<Rc<Forge>> {
        if let Some(f) = self.forges.get(&loc.base) {
            return f.clone();
        }
        let made = self.make(loc);
        let forge = match made {
            Ok(f) => Some(Rc::new(f)),
            Err(e) => {
                warn!("{}: {e:#}", loc.host);
                self.failures += 1;
                None
            }
        };
        self.forges.insert(loc.base.clone(), forge.clone());
        forge
    }

    fn make(&self, loc: &Location) -> anyhow::Result<Forge> {
        let site = self.cfg.site(&loc.host);
        let kind = match site.and_then(|s| s.kind.as_deref()).and_then(Kind::parse).or_else(|| Kind::known(&loc.host)) {
            Some(k) => k,
            None => forge::detect(&loc.base)?,
        };
        let token = crate::auth::resolve_site(&loc.host, site.and_then(|s| s.token.as_deref()), self.cfg.github_token.as_deref());
        Forge::new(&loc.base, &loc.host, kind, token, self.cache.clone(), &self.cfg.api_url)
    }
}

/// Returns the repos to check and how many sources (sites, accounts, stars) failed.
pub fn build(cfg: &Config, cache: Option<PathBuf>) -> (Vec<Target>, usize) {
    let mut sites = Sites::new(cfg, cache);
    let mut list = List::default();
    let mut failures = 0;

    for r in &cfg.repos {
        let Ok(loc) = Location::parse(&r.repo, 2) else { continue }; // validated when loading
        let Some(forge) = sites.get(&loc) else { continue };
        let path = forge.normalize_repo_path(&loc.path);
        let source = cfg.source_folder(&loc.host);
        let id = Location { path: path.clone(), ..loc }.id();
        list.add(id, path, source, cfg.project_folder(r), cfg.effective(&r.options), forge);
    }
    let from_config = list.targets.len();

    for a in &cfg.accounts {
        let Ok(loc) = Location::parse(&a.url, 1) else { continue };
        let Some(forge) = sites.get(&loc) else { continue };
        match forge.account_repos(&loc.path) {
            Ok(repos) => {
                let opts = cfg.effective(&a.options);
                let n = list.add_listed(cfg, &loc, &forge, repos, &a.ignore, a.include_forks, a.include_archived, &opts);
                info!("{}: {n} repo(s)", loc.id());
            }
            Err(e) => {
                warn!("cannot list the repos of {}: {e:#}", loc.id());
                failures += 1;
            }
        }
    }

    if let Some(s) = cfg.starred_enabled() {
        let gh = Location { base: "https://github.com".into(), host: forge::GITHUB_HOST.into(), path: String::new() };
        if let Some(forge) = sites.get(&gh) {
            let starred = forge.github().map(|c| c.starred(s.user.as_deref()));
            match starred {
                Some(Ok(repos)) => {
                    let repos = repos.into_iter().map(|r| Listed { path: r.full_name, fork: r.fork, archived: r.archived }).collect();
                    let opts = cfg.effective(&s.options);
                    let n = list.add_listed(cfg, &gh, &forge, repos, &s.ignore, s.include_forks, s.include_archived, &opts);
                    info!("{n} repo(s) from stars");
                }
                Some(Err(e)) => {
                    warn!("cannot list starred repos: {e:#}");
                    failures += 1;
                }
                None => {}
            }
        }
    }
    info!("{} repo(s) to check ({from_config} listed in the config)", list.targets.len());
    (list.targets, failures + sites.failures)
}

#[derive(Default)]
struct List {
    targets: Vec<Target>,
    /// Project folders in use, per site folder (lowercase).
    folders: HashMap<String, HashSet<String>>,
    ids: HashSet<String>,
}

impl List {
    fn add(&mut self, id: String, path: String, source: String, folder: String, opts: Effective, forge: Rc<Forge>) -> bool {
        if !self.ids.insert(id.to_lowercase()) {
            return false; // listed explicitly, or found twice
        }
        self.folders.entry(source.to_lowercase()).or_default().insert(folder.to_lowercase());
        self.targets.push(Target { repo: id, path, source, folder, opts, forge });
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn add_listed(
        &mut self,
        cfg: &Config,
        site: &Location,
        forge: &Rc<Forge>,
        mut repos: Vec<Listed>,
        ignore: &[String],
        include_forks: bool,
        include_archived: bool,
        opts: &Effective,
    ) -> usize {
        // Sorted, so folder-name conflicts resolve the same way every run.
        repos.sort_by_key(|r| r.path.to_lowercase());
        let mut added = 0;
        for r in repos {
            let loc = Location { base: site.base.clone(), host: site.host.clone(), path: r.path };
            let id = loc.id();
            if self.ids.contains(&id.to_lowercase())
                || ignore.iter().any(|p| glob_match(p, &loc.path) || glob_match(p, &id))
                || (r.fork && !include_forks)
                || (r.archived && !include_archived)
            {
                continue;
            }
            let source = cfg.source_folder(&site.host);
            let used = self.folders.get(&source.to_lowercase()).cloned().unwrap_or_default();
            let site_dir = cfg.archive_root.join(&source);
            let folder = pick_folder(loc.owner(), loc.name(), &id, &used, |f| folder_owner(&site_dir.join(f)));
            if self.add(id, loc.path.clone(), source, folder, opts.clone(), forge.clone()) {
                added += 1;
            }
        }
        added
    }
}

/// Uses the repo name as the folder unless another repo already has it, either
/// in this run or on disk; then "name (owner)".
fn pick_folder(
    owner: &str,
    name: &str,
    full: &str,
    used: &HashSet<String>,
    owner_on_disk: impl Fn(&str) -> Option<String>,
) -> String {
    let plain = sanitize(name);
    let taken = used.contains(&plain.to_lowercase())
        || owner_on_disk(&plain).is_some_and(|o| !o.eq_ignore_ascii_case(full));
    if taken {
        sanitize(&format!("{name} ({owner})"))
    } else {
        plain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_conflicts() {
        let used: HashSet<String> = ["tool".to_string()].into();
        let none = |_: &str| None;
        assert_eq!(pick_folder("a", "app", "a/app", &used, none), "app");
        assert_eq!(pick_folder("b", "Tool", "b/Tool", &used, none), "Tool (b)");
        let disk = |_: &str| Some("other/app".to_string());
        assert_eq!(pick_folder("a", "app", "a/app", &HashSet::new(), disk), "app (a)");
        let mine = |_: &str| Some("A/App".to_string());
        assert_eq!(pick_folder("a", "app", "a/app", &HashSet::new(), mine), "app");
    }
}
