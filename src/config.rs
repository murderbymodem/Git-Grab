//! Configuration file (TOML) format and validation.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const APP_DIR: &str = "github-release-archiver";

// `deny_unknown_fields` doesn't work together with `flatten`, so unknown keys
// are caught by `check_unknown_keys` instead.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Root folder of the archive: a mounted path or a UNC path (\\server\share\dir).
    pub archive_root: PathBuf,
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
    /// Rules applied to every repo, before the built-in OS detection.
    #[serde(default)]
    pub os_rules: Vec<OsRule>,
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
    /// Only download assets classified into these OS folders (empty = all).
    #[serde(default)]
    pub only_os: Vec<String>,
    /// Keep a copy of the repo's README in the project folder.
    pub save_readme: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct OsRule {
    pub pattern: String,
    pub os: String,
}

#[derive(Debug, Deserialize)]
pub struct RepoConfig {
    /// "owner/name"
    pub repo: String,
    /// Project folder name; defaults to the repo name.
    pub folder: Option<String>,
    #[serde(flatten)]
    pub options: RepoOptions,
    #[serde(default)]
    pub os_rules: Vec<OsRule>,
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
    pub only_os: Vec<String>,
    pub save_readme: bool,
    pub os_rules: Vec<OsRule>,
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
    &["releases", "include_prereleases", "include_source", "keep_versions", "include", "exclude", "only_os", "save_readme"];
const TOP_KEYS: &[&str] = &[
    "archive_root", "github_token", "api_url", "oauth_client_id", "interval_minutes", "os_rules", "starred", "repo",
];
const REPO_KEYS: &[&str] = &["repo", "folder", "os_rules"];
const STARRED_KEYS: &[&str] = &["enabled", "user", "ignore", "include_forks", "include_archived"];

/// Rejects misspelled keys, which serde would otherwise silently ignore.
fn check_unknown_keys(text: &str) -> Result<()> {
    let table: toml::Table = toml::from_str(text)?;
    let known = |k: &str, extra: &[&str]| OPTION_KEYS.contains(&k) || extra.contains(&k);
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

/// Default config location: ./config.toml if present, else the per-user config folder
/// (%APPDATA%\github-release-archiver\config.toml or ~/.config/github-release-archiver/config.toml).
pub fn default_path() -> PathBuf {
    let local = PathBuf::from("config.toml");
    if local.is_file() {
        return local;
    }
    dirs::config_dir().map(|d| d.join(APP_DIR).join("config.toml")).unwrap_or(local)
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
    pub fn effective(&self, layer: &RepoOptions, repo_rules: &[OsRule]) -> Effective {
        let d = &self.defaults;
        let o = layer;
        let pick = |a: &Vec<String>, b: &Vec<String>| if a.is_empty() { b.clone() } else { a.clone() };
        let mut exclude = d.exclude.clone();
        exclude.extend(o.exclude.iter().cloned());
        let mut os_rules = repo_rules.to_vec();
        os_rules.extend(self.os_rules.iter().cloned());
        Effective {
            releases: o.releases.or(d.releases).unwrap_or(1),
            include_prereleases: o.include_prereleases.or(d.include_prereleases).unwrap_or(false),
            include_source: o.include_source.or(d.include_source).unwrap_or(false),
            keep_versions: o.keep_versions.or(d.keep_versions),
            include: pick(&o.include, &d.include),
            exclude,
            only_os: pick(&o.only_os, &d.only_os),
            save_readme: o.save_readme.or(d.save_readme).unwrap_or(true),
            os_rules,
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
        let a = cfg.effective(&cfg.repos[0].options, &cfg.repos[0].os_rules);
        assert_eq!(a.releases, 5);
        assert_eq!(a.exclude, vec!["*.sig", "*.txt"]);
        assert_eq!(cfg.effective(&cfg.repos[1].options, &[]).releases, 3);
        let s = cfg.starred_enabled().unwrap();
        assert_eq!(cfg.effective(&s.options, &[]).releases, 1);
        assert_eq!(s.ignore, ["big/*"]);
        assert_eq!(cfg.project_folder(&cfg.repos[0]), "b");
        assert_eq!(cfg.project_folder(&cfg.repos[1]), "Dee");
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
