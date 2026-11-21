//! Configuration file (TOML) format and validation.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const APP_DIR: &str = "github-grab";
/// The program's name before it was renamed GitHub-Grab (versions up to 0.7).
pub const LEGACY_APP_DIR: &str = "github-release-archiver";

// `deny_unknown_fields` doesn't work together with `flatten`, so unknown keys
// are caught by `check_unknown_keys` instead.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Root folder of the archive: a mounted path or a UNC path (\\server\share\dir).
    pub archive_root: PathBuf,
    /// Extra folders that get a copy of the archive (e.g. a network share).
    #[serde(default)]
    pub mirrors: Vec<PathBuf>,
    /// GitHub token. If unset: GITHUB_TOKEN / GH_TOKEN, then the token saved by `login`.
    #[serde(default)]
    pub github_token: Option<String>,
    /// API base URL; change it for GitHub Enterprise Server.
    #[serde(default = "default_api_url")]
    pub api_url: String,
    /// Client ID of a GitHub OAuth App with device flow enabled, for `login`.
    #[serde(default)]
    pub oauth_client_id: Option<String>,
    /// Minutes between checks in `watch` mode.
    #[serde(default = "default_interval")]
    pub interval_minutes: u64,
    #[serde(flatten)]
    pub defaults: RepoOptions,
    /// Also archive the repos starred by an account.
    #[serde(default)]
    pub starred: Option<StarredConfig>,
    #[serde(default, rename = "repo")]
    pub repos: Vec<RepoConfig>,
}

/// Options that can be set globally and overridden per repo (or for starred repos).
#[derive(Debug, Default, Deserialize, Clone)]
pub struct RepoOptions {
    /// How many of the newest releases to archive.
    pub releases: Option<usize>,
    pub include_prereleases: Option<bool>,
    /// Also archive GitHub's auto-generated source zip into a "Source" folder.
    pub include_source: Option<bool>,
    /// Delete archived releases older than the newest N (only ones this tool downloaded).
    pub keep_versions: Option<usize>,
    /// Only download assets matching one of these wildcard patterns (empty = all).
    #[serde(default)]
    pub include: Vec<String>,
    /// Skip assets matching any of these wildcard patterns.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Keep a copy of the repo's README in the project folder.
    pub save_readme: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RepoConfig {
    /// "owner/name"
    pub repo: String,
    /// Project folder name; defaults to the repo name.
    pub folder: Option<String>,
    #[serde(flatten)]
    pub options: RepoOptions,
}

#[derive(Debug, Deserialize)]
pub struct StarredConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Whose stars to use. Unset = the logged-in account (needs a token).
    pub user: Option<String>,
    /// Starred repos to skip, as owner/name wildcards (e.g. "someorg/*").
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default = "yes")]
    pub include_forks: bool,
    #[serde(default = "yes")]
    pub include_archived: bool,
    /// Options for starred repos, overriding the global ones.
    #[serde(flatten)]
    pub options: RepoOptions,
}

/// The settings for one repo after merging the option layers.
#[derive(Debug, Clone)]
pub struct Effective {
    pub releases: usize,
    pub include_prereleases: bool,
    pub include_source: bool,
    pub keep_versions: Option<usize>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub save_readme: bool,
}

fn default_api_url() -> String {
    "https://api.github.com".into()
}
fn default_interval() -> u64 {
    360
}
fn yes() -> bool {
    true
}

const OPTION_KEYS: &[&str] =
    &["releases", "include_prereleases", "include_source", "keep_versions", "include", "exclude", "save_readme"];
const TOP_KEYS: &[&str] = &[
    "archive_root", "mirrors", "github_token", "api_url", "oauth_client_id", "interval_minutes", "starred", "repo",
];
const REPO_KEYS: &[&str] = &["repo", "folder"];
/// Settings from older versions that are now ignored (warned about, not rejected).
const RETIRED_KEYS: &[&str] = &["only_os", "os_rules"];
const STARRED_KEYS: &[&str] = &["enabled", "user", "ignore", "include_forks", "include_archived"];

/// Rejects misspelled keys, which serde would otherwise silently ignore.
fn check_unknown_keys(text: &str) -> Result<()> {
    let table: toml::Table = toml::from_str(text)?;
    let known = |k: &str, extra: &[&str]| {
        if RETIRED_KEYS.contains(&k) {
            log::warn!("config setting \"{k}\" is no longer used (files are no longer sorted by OS); ignoring it");
            return true;
        }
        OPTION_KEYS.contains(&k) || extra.contains(&k)
    };
    for k in table.keys() {
        if !known(k, TOP_KEYS) {
            bail!("unknown setting \"{k}\" in config");
        }
    }
    if let Some(toml::Value::Table(s)) = table.get("starred") {
        for k in s.keys() {
            if !known(k, STARRED_KEYS) {
                bail!("unknown setting \"{k}\" in [starred]");
            }
        }
    }
    if let Some(toml::Value::Array(repos)) = table.get("repo") {
        for r in repos.iter().filter_map(|r| r.as_table()) {
            let name = r.get("repo").and_then(|v| v.as_str()).unwrap_or("?");
            for k in r.keys() {
                if !known(k, REPO_KEYS) {
                    bail!("unknown setting \"{k}\" in [[repo]] {name}");
                }
            }
        }
    }
    Ok(())
}

pub fn example() -> &'static str {
    include_str!("../config.example.toml")
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!("cannot read config file {} (create one with the `init` command)", path.display())
        })?;
        Self::parse(&text).with_context(|| format!("invalid config file {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Config> {
        let cfg: Config = toml::from_str(text)?;
        check_unknown_keys(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if self.archive_root.as_os_str().is_empty() {
            bail!("archive_root must not be empty");
        }
        let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['/', '\\']).to_lowercase();
        let root = norm(&self.archive_root);
        let mut seen = std::collections::HashSet::new();
        for m in &self.mirrors {
            let mn = norm(m);
            if mn.is_empty() {
                bail!("mirrors must not contain an empty path");
            }
            if mn == root {
                bail!("mirror {} is the same folder as archive_root", m.display());
            }
            if !seen.insert(mn) {
                bail!("mirror {} is listed twice", m.display());
            }
        }
        if self.interval_minutes == 0 {
            bail!("interval_minutes must be at least 1");
        }
        let check_counts = |what: &str, o: &RepoOptions| -> Result<()> {
            for (key, v) in [
                ("releases", o.releases.or(self.defaults.releases)),
                ("keep_versions", o.keep_versions.or(self.defaults.keep_versions)),
            ] {
                if v == Some(0) {
                    bail!("{key} for {what} must be at least 1");
                }
            }
            Ok(())
        };
        check_counts("the defaults", &self.defaults)?;
        if let Some(s) = &self.starred {
            check_counts("[starred]", &s.options)?;
        }
        let mut folders = std::collections::HashSet::new();
        for r in &self.repos {
            let parts: Vec<&str> = r.repo.split('/').collect();
            if parts.len() != 2 || parts.iter().any(|p| p.is_empty()) {
                bail!("repo \"{}\" must be in the form owner/name", r.repo);
            }
            check_counts(&r.repo, &r.options)?;
            let folder = self.project_folder(r).to_lowercase();
            if !folders.insert(folder.clone()) {
                bail!("two repos use the project folder \"{folder}\"; set `folder` on one of them");
            }
        }
        Ok(())
    }

    pub fn starred_enabled(&self) -> Option<&StarredConfig> {
        self.starred.as_ref().filter(|s| s.enabled)
    }

    pub fn project_folder(&self, r: &RepoConfig) -> String {
        let name = r.folder.clone().unwrap_or_else(|| r.repo.split('/').nth(1).unwrap_or(&r.repo).to_string());
        crate::util::sanitize(&name)
    }

    /// Merges `layer` (per-repo or [starred] options) over the global defaults.
    pub fn effective(&self, layer: &RepoOptions) -> Effective {
        let d = &self.defaults;
        let o = layer;
        let pick = |a: &Vec<String>, b: &Vec<String>| if a.is_empty() { b.clone() } else { a.clone() };
        let mut exclude = d.exclude.clone();
        exclude.extend(o.exclude.iter().cloned());
        Effective {
            releases: o.releases.or(d.releases).unwrap_or(1),
            include_prereleases: o.include_prereleases.or(d.include_prereleases).unwrap_or(false),
            include_source: o.include_source.or(d.include_source).unwrap_or(false),
            keep_versions: o.keep_versions.or(d.keep_versions),
            include: pick(&o.include, &d.include),
            exclude,
            save_readme: o.save_readme.or(d.save_readme).unwrap_or(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_config() {
        let cfg = Config::parse(example()).unwrap();
        assert!(!cfg.repos.is_empty());
        assert!(cfg.starred.is_some());
    }

    #[test]
    fn per_repo_overrides_global() {
        let cfg = Config::parse(
            r#"
            archive_root = "/tmp/x"
            releases = 3
            exclude = ["*.sig"]
            [starred]
            releases = 1
            ignore = ["big/*"]
            [[repo]]
            repo = "a/b"
            releases = 5
            exclude = ["*.txt"]
            [[repo]]
            repo = "c/d"
            folder = "Dee"
            "#,
        )
        .unwrap();
        let a = cfg.effective(&cfg.repos[0].options);
        assert_eq!(a.releases, 5);
        assert_eq!(a.exclude, vec!["*.sig", "*.txt"]);
        assert_eq!(cfg.effective(&cfg.repos[1].options).releases, 3);
        let s = cfg.starred_enabled().unwrap();
        assert_eq!(cfg.effective(&s.options).releases, 1);
        assert_eq!(s.ignore, ["big/*"]);
        assert_eq!(cfg.project_folder(&cfg.repos[0]), "b");
        assert_eq!(cfg.project_folder(&cfg.repos[1]), "Dee");
    }

    #[test]
    fn mirrors() {
        let cfg = Config::parse("archive_root = 'D:\\Archive'\nmirrors = ['\\\\nas\\software']").unwrap();
        assert_eq!(cfg.mirrors, [PathBuf::from(r"\\nas\software")]);
        assert!(Config::parse("archive_root = '/a'\nmirrors = ['/a/']").is_err());
        assert!(Config::parse("archive_root = '/a'\nmirrors = ['/b', '/B']").is_err());
    }

    #[test]
    fn accepts_retired_settings() {
        Config::parse("archive_root = \"/x\"\nonly_os = [\"Windows\"]\n[[os_rules]]\npattern = \"*\"\nos = \"X\"").unwrap();
    }

    #[test]
    fn rejects_typos_and_bad_values() {
        assert!(Config::parse("archive_root = \"/x\"\nrelease = 2").is_err());
        assert!(Config::parse("archive_root = \"/x\"\n[starred]\nusr = \"me\"").is_err());
        assert!(Config::parse("archive_root = \"/x\"\n[[repo]]\nrepo = \"a/b\"\nfolders = \"x\"").is_err());
        assert!(Config::parse("archive_root = \"/x\"\n[[repo]]\nrepo = \"nope\"").is_err());
        assert!(Config::parse("archive_root = \"/x\"\n[starred]\nkeep_versions = 0").is_err());
    }
}
