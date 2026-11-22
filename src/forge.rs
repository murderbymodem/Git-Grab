//! Sites ("forges") that host releases: GitHub, GitLab and Gitea/Forgejo.
//!
//! A repo or account is written as `owner/name` (GitHub), `host/path`, or a
//! full URL such as `https://git.example.dev/Org/project`. The kind of site is
//! known for github.com, gitlab.com and codeberg.org, can be set with a
//! `[[site]]` entry, and is otherwise detected from the site's API.

use crate::github::{self, AuthStyle, Client};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const GITHUB_HOST: &str = "github.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    GitHub,
    GitLab,
    /// Gitea and Forgejo share the same API.
    Gitea,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        match s.to_ascii_lowercase().as_str() {
            "github" => Some(Kind::GitHub),
            "gitlab" => Some(Kind::GitLab),
            "gitea" | "forgejo" => Some(Kind::Gitea),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::GitHub => "GitHub",
            Kind::GitLab => "GitLab",
            Kind::Gitea => "Gitea/Forgejo",
        }
    }

    /// Sites whose kind is known without asking them.
    pub fn known(host: &str) -> Option<Kind> {
        match host {
            "github.com" => Some(Kind::GitHub),
            "gitlab.com" => Some(Kind::GitLab),
            "codeberg.org" => Some(Kind::Gitea),
            _ => None,
        }
    }
}

/// Where a repo or account lives: the site's base URL and the path on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// "https://host[:port]", no trailing slash.
    pub base: String,
    /// "host[:port]", lowercase.
    pub host: String,
    /// "owner/name", "group/subgroup/name", or for an account just "owner".
    pub path: String,
}

impl Location {
    /// Parses `owner/name`, `host/owner/name` or a URL. `min_segments` is 2
    /// for a repo and 1 for an account.
    pub fn parse(spec: &str, min_segments: usize) -> Result<Location> {
        let s = spec.trim();
        let (scheme, rest) = match s.split_once("://") {
            Some((sch, rest)) if sch.eq_ignore_ascii_case("http") || sch.eq_ignore_ascii_case("https") => {
                (sch.to_ascii_lowercase(), rest)
            }
            Some(_) => bail!("\"{spec}\": only http:// and https:// addresses are supported"),
            None => ("https".to_string(), s),
        };
        // Drop query/fragment, GitLab's "/-/..." page suffixes and ".git".
        let rest = rest.split(['?', '#']).next().unwrap_or("");
        let rest = rest.split("/-/").next().unwrap_or("");
        let mut segs: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
        let has_host = s.contains("://") || segs.first().is_some_and(|f| f.contains('.') || f.contains(':'));
        let host = if has_host {
            if segs.is_empty() {
                bail!("\"{spec}\" has no host");
            }
            segs.remove(0).to_ascii_lowercase()
        } else {
            GITHUB_HOST.to_string()
        };
        if let Some(last) = segs.last_mut() {
            *last = last.strip_suffix(".git").unwrap_or(last);
        }
        if host == GITHUB_HOST || host == "www.github.com" {
            segs.truncate(min_segments.max(1)); // github.com/o/r/releases -> o/r
        }
        if segs.len() < min_segments {
            let want = if min_segments == 1 { "owner" } else { "owner/name" };
            bail!("\"{spec}\" should look like {want}, host/{want}, or a link to it");
        }
        let host = if host == "www.github.com" { GITHUB_HOST.to_string() } else { host };
        Ok(Location { base: format!("{scheme}://{host}"), host, path: segs.join("/") })
    }

    /// Name used in logs and state files: "owner/name" on github.com (as in
    /// earlier versions), otherwise "host/path".
    pub fn id(&self) -> String {
        if self.host == GITHUB_HOST {
            self.path.clone()
        } else {
            format!("{}/{}", self.host, self.path)
        }
    }

    /// The last path segment: the repo (or account) name.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    pub fn owner(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(o, _)| o)
    }
}

/// A release, the same shape for every site.
#[derive(Debug)]
pub struct Release {
    pub tag_name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub created_at: Option<String>,
    pub assets: Vec<Asset>,
    /// URL and Accept header of the source zip.
    pub source_zip: Option<(String, &'static str)>,
}

#[derive(Debug)]
pub struct Asset {
    pub name: String,
    /// Known on GitHub and Gitea; GitLab release links don't say.
    pub size: Option<u64>,
    pub url: String,
    pub accept: &'static str,
    pub digest: Option<String>,
}

/// A repo found in an account or stars, as its path on the site.
#[derive(Debug)]
pub struct Listed {
    pub path: String,
    pub fork: bool,
    pub archived: bool,
}

pub struct Forge {
    pub kind: Kind,
    pub host: String,
    client: Client,
}

impl Forge {
    /// `github_api` overrides the API URL for github.com (the `api_url` setting).
    pub fn new(base: &str, host: &str, kind: Kind, token: Option<String>, cache: Option<PathBuf>, github_api: &str) -> Result<Forge> {
        let client = match kind {
            Kind::GitHub if host == GITHUB_HOST => Client::new(github_api, token, cache)?,
            Kind::GitHub => Client::new(&format!("{base}/api/v3"), token, cache)?, // GitHub Enterprise
            Kind::GitLab => Client::for_site(&format!("{base}/api/v4"), token, cache, AuthStyle::PrivateToken, "application/json")?,
            Kind::Gitea => Client::for_site(&format!("{base}/api/v1"), token, cache, AuthStyle::Token, "application/json")?,
        };
        Ok(Forge { kind, host: host.to_string(), client })
    }

    pub fn github(&self) -> Option<&Client> {
        (self.kind == Kind::GitHub).then_some(&self.client)
    }

    /// Gitea repos are always owner/name; web links may carry more (…/releases).
    pub fn normalize_repo_path(&self, path: &str) -> String {
        match self.kind {
            Kind::GitLab => path.to_string(),
            _ => path.split('/').take(2).collect::<Vec<_>>().join("/"),
        }
    }

    fn api(&self) -> &str {
        self.client.api_url()
    }

    /// Newest first.
    pub fn releases(&self, path: &str, wanted: usize) -> Result<Vec<Release>> {
        let per_page = (wanted * 2 + 10).min(100);
        match self.kind {
            Kind::GitHub => Ok(self
                .client
                .releases(path, wanted)?
                .into_iter()
                .map(|r| Release {
                    tag_name: r.tag_name,
                    draft: r.draft,
                    prerelease: r.prerelease,
                    published_at: r.published_at,
                    created_at: r.created_at,
                    source_zip: r.zipball_url.map(|u| (u, "application/vnd.github+json")),
                    assets: r
                        .assets
                        .into_iter()
                        .map(|a| Asset { name: a.name, size: Some(a.size), url: a.url, accept: "application/octet-stream", digest: a.digest })
                        .collect(),
                })
                .collect()),
            Kind::Gitea => {
                let url = format!("{}/repos/{path}/releases?limit={per_page}", self.api());
                let (list, _): (Vec<GiteaRelease>, _) = self.client.get_json(&url)?;
                Ok(list
                    .into_iter()
                    .map(|r| Release {
                        tag_name: r.tag_name,
                        draft: r.draft,
                        prerelease: r.prerelease,
                        published_at: r.published_at,
                        created_at: r.created_at,
                        source_zip: r.zipball_url.map(|u| (u, "*/*")),
                        assets: r
                            .assets
                            .into_iter()
                            .map(|a| Asset { name: a.name, size: Some(a.size), url: a.browser_download_url, accept: "*/*", digest: None })
                            .collect(),
                    })
                    .collect())
            }
            Kind::GitLab => {
                let url = format!("{}/projects/{}/releases?per_page={per_page}", self.api(), encode_path(path));
                let (list, _): (Vec<GitLabRelease>, _) = self.client.get_json(&url)?;
                Ok(list
                    .into_iter()
                    .map(|r| Release {
                        tag_name: r.tag_name,
                        draft: false,
                        prerelease: r.upcoming_release,
                        published_at: r.released_at,
                        created_at: r.created_at,
                        source_zip: r.assets.sources.into_iter().find(|s| s.format == "zip").map(|s| (s.url, "*/*")),
                        assets: r
                            .assets
                            .links
                            .into_iter()
                            .map(|l| Asset { name: l.name, size: None, url: l.direct_asset_url.unwrap_or(l.url), accept: "*/*", digest: None })
                            .collect(),
                    })
                    .collect())
            }
        }
    }

    /// The README's file name and contents, or `None` if there isn't one.
    pub fn readme(&self, path: &str) -> Result<Option<(String, Vec<u8>)>> {
        match self.kind {
            Kind::GitHub => self.client.readme(path),
            Kind::Gitea => {
                let url = format!("{}/repos/{path}/contents", self.api());
                let Some((entries, _)) = self.client.get_json_opt::<Vec<GiteaEntry>>(&url)? else { return Ok(None) };
                let Some(entry) = pick_readme(entries.into_iter().filter(|e| e.kind == "file").map(|e| (e.name, e.download_url)))
                else {
                    return Ok(None);
                };
                let (name, Some(raw)) = entry else { return Ok(None) };
                Ok(self.client.get_bytes(&raw)?.map(|b| (name, b)))
            }
            Kind::GitLab => {
                let url = format!("{}/projects/{}", self.api(), encode_path(path));
                let Some((p, _)) = self.client.get_json_opt::<GitLabProject>(&url)? else { return Ok(None) };
                let Some(web) = p.readme_url else { return Ok(None) };
                let raw = web.replacen("/-/blob/", "/-/raw/", 1);
                let name = web.rsplit('/').next().unwrap_or("README").to_string();
                Ok(self.client.get_bytes(&raw)?.map(|b| (name, b)))
            }
        }
    }

    /// All repos of a user or organization (GitLab: group, including subgroups).
    pub fn account_repos(&self, owner: &str) -> Result<Vec<Listed>> {
        match self.kind {
            Kind::GitHub => Ok(self
                .client
                .account_repos(owner)?
                .into_iter()
                .map(|r| Listed { path: r.full_name, fork: r.fork, archived: r.archived })
                .collect()),
            Kind::Gitea => {
                let org = format!("{}/orgs/{owner}/repos", self.api());
                let user = format!("{}/users/{owner}/repos", self.api());
                let list: Vec<GiteaRepo> = match self.pages(&org, "limit", 50)? {
                    Some(l) => l,
                    None => self.pages(&user, "limit", 50)?.ok_or_else(|| anyhow!("no user or organization \"{owner}\" on {}", self.host))?,
                };
                Ok(list.into_iter().map(|r| Listed { path: r.full_name, fork: r.fork, archived: r.archived }).collect())
            }
            Kind::GitLab => {
                let group = format!("{}/groups/{}/projects?include_subgroups=true", self.api(), encode_path(owner));
                let list: Vec<GitLabProject> = match self.pages(&group, "per_page", 100)? {
                    Some(l) => l,
                    None => {
                        let url = format!("{}/users?username={owner}", self.api());
                        let (users, _): (Vec<GitLabUser>, _) = self.client.get_json(&url)?;
                        let id = users.first().ok_or_else(|| anyhow!("no user or group \"{owner}\" on {}", self.host))?.id;
                        self.pages(&format!("{}/users/{id}/projects", self.api()), "per_page", 100)?.unwrap_or_default()
                    }
                };
                Ok(list
                    .into_iter()
                    .map(|p| Listed { path: p.path_with_namespace, fork: p.forked_from_project.is_some(), archived: p.archived })
                    .collect())
            }
        }
    }

    /// Follows page numbers until a short page. `None` if the first page is 404.
    fn pages<T: serde::de::DeserializeOwned>(&self, url: &str, size_param: &str, size: usize) -> Result<Option<Vec<T>>> {
        let sep = if url.contains('?') { '&' } else { '?' };
        let mut all = Vec::new();
        for page in 1..=200 {
            let u = format!("{url}{sep}{size_param}={size}&page={page}");
            let Some((mut items, _)) = self.client.get_json_opt::<Vec<T>>(&u)? else {
                return if page == 1 { Ok(None) } else { Ok(Some(all)) };
            };
            let n = items.len();
            all.append(&mut items);
            if n < size {
                break;
            }
        }
        Ok(Some(all))
    }

    /// The login name of the token's owner.
    pub fn current_user(&self) -> Result<String> {
        self.client.current_user_field(match self.kind {
            Kind::GitLab => "username",
            _ => "login",
        })
    }

    pub fn download(&self, url: &str, accept: &str, dest: &Path, size: Option<u64>, digest: Option<&str>) -> Result<u64> {
        self.client.download(url, accept, dest, size, digest)
    }

    /// How to create a token on this site, for `login`.
    pub fn token_help(&self) -> String {
        let base = format!("https://{}", self.host);
        match self.kind {
            Kind::GitHub => "Create a fine-grained token at https://github.com/settings/personal-access-tokens/new".into(),
            Kind::GitLab => format!("Create a token at {base}/-/user_settings/personal_access_tokens with the read_api scope"),
            Kind::Gitea => format!("Create a token at {base}/user/settings/applications with read access to repositories"),
        }
    }
}

/// Asks a site which software it runs.
pub fn detect(base: &str) -> Result<Kind> {
    let http = reqwest::blocking::Client::builder()
        .user_agent(concat!("Git-Grab/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()?;
    let get = |path: &str| http.get(format!("{base}{path}")).header("Accept", "application/json").send().ok();
    let json = |r: reqwest::blocking::Response| r.json::<serde_json::Value>().ok();

    // Gitea and Forgejo answer /api/v1/version with {"version": "..."}.
    if let Some(v) = get("/api/v1/version").filter(|r| r.status().is_success()).and_then(json) {
        if v.get("version").is_some() {
            return Ok(Kind::Gitea);
        }
    }
    // GitLab: /api/v4/version needs a login, but says so in JSON.
    if let Some(r) = get("/api/v4/version") {
        if (r.status().is_success() || r.status().as_u16() == 401)
            && json(r).is_some_and(|v| v.get("version").is_some() || v.get("message").is_some())
        {
            return Ok(Kind::GitLab);
        }
    }
    // GitHub Enterprise Server.
    if get("/api/v3/meta").is_some_and(|r| r.status().is_success()) {
        return Ok(Kind::GitHub);
    }
    let host = github::host_of(base).unwrap_or_else(|| base.to_string());
    bail!("can't tell what kind of site {host} is; add a [[site]] entry with type = \"gitlab\", \"gitea\" or \"forgejo\"")
}

/// Prefers README.md, then any README.*, case-insensitive.
fn pick_readme<U>(files: impl Iterator<Item = (String, U)>) -> Option<(String, U)> {
    let mut best: Option<(u8, (String, U))> = None;
    for (name, url) in files {
        let lower = name.to_ascii_lowercase();
        let rank = if lower == "readme.md" {
            0
        } else if lower.starts_with("readme") {
            1
        } else {
            continue;
        };
        if best.as_ref().is_none_or(|(r, _)| rank < *r) {
            best = Some((rank, (name, url)));
        }
    }
    best.map(|(_, e)| e)
}

/// URL-encodes a GitLab project or group path ("group/sub/name" -> "group%2Fsub%2Fname").
fn encode_path(path: &str) -> String {
    path.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---- API shapes ----

#[derive(Deserialize)]
struct GiteaRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    published_at: Option<String>,
    created_at: Option<String>,
    #[serde(default)]
    assets: Vec<GiteaAsset>,
    zipball_url: Option<String>,
}

#[derive(Deserialize)]
struct GiteaAsset {
    name: String,
    #[serde(default)]
    size: u64,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct GiteaEntry {
    name: String,
    #[serde(rename = "type", default)]
    kind: String,
    download_url: Option<String>,
}

#[derive(Deserialize)]
struct GiteaRepo {
    full_name: String,
    #[serde(default)]
    fork: bool,
    #[serde(default)]
    archived: bool,
}

#[derive(Deserialize)]
struct GitLabRelease {
    tag_name: String,
    released_at: Option<String>,
    created_at: Option<String>,
    #[serde(default)]
    upcoming_release: bool,
    #[serde(default)]
    assets: GitLabAssets,
}

#[derive(Deserialize, Default)]
struct GitLabAssets {
    #[serde(default)]
    links: Vec<GitLabLink>,
    #[serde(default)]
    sources: Vec<GitLabSource>,
}

#[derive(Deserialize)]
struct GitLabLink {
    name: String,
    url: String,
    direct_asset_url: Option<String>,
}

#[derive(Deserialize)]
struct GitLabSource {
    format: String,
    url: String,
}

#[derive(Deserialize)]
struct GitLabProject {
    #[serde(default)]
    path_with_namespace: String,
    #[serde(default)]
    archived: bool,
    forked_from_project: Option<serde_json::Value>,
    readme_url: Option<String>,
}

#[derive(Deserialize)]
struct GitLabUser {
    id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(s: &str, n: usize) -> Location {
        Location::parse(s, n).unwrap()
    }

    #[test]
    fn parses_repo_specs() {
        let l = loc("BurntSushi/ripgrep", 2);
        assert_eq!((l.base.as_str(), l.path.as_str(), l.id()), ("https://github.com", "BurntSushi/ripgrep", "BurntSushi/ripgrep".into()));
        assert_eq!(loc("https://github.com/BurntSushi/ripgrep/releases/tag/14.1.0", 2).path, "BurntSushi/ripgrep");
        assert_eq!(loc("github.com/a/b.git", 2).id(), "a/b");
        let g = loc("https://gitlab.com/inkscape/inkscape/-/releases", 2);
        assert_eq!((g.host.as_str(), g.path.as_str()), ("gitlab.com", "inkscape/inkscape"));
        assert_eq!(g.id(), "gitlab.com/inkscape/inkscape");
        assert_eq!(loc("gitlab.com/group/sub/proj", 2).path, "group/sub/proj");
        let m = loc("https://git.magicbean.dev/MagicBeanDev/tool", 2);
        assert_eq!((m.base.as_str(), m.name(), m.owner()), ("https://git.magicbean.dev", "tool", "MagicBeanDev"));
        assert_eq!(loc("http://127.0.0.1:8080/o/r", 2).base, "http://127.0.0.1:8080");
        assert!(Location::parse("nope", 2).is_err());
        assert!(Location::parse("ftp://x.org/a/b", 2).is_err());
    }

    #[test]
    fn parses_account_specs() {
        let a = loc("https://git.magicbean.dev/MagicBeanDev", 1);
        assert_eq!((a.host.as_str(), a.path.as_str()), ("git.magicbean.dev", "MagicBeanDev"));
        assert_eq!(loc("someorg", 1).base, "https://github.com");
        assert_eq!(loc("https://github.com/someorg?tab=repositories", 1).path, "someorg");
        assert_eq!(loc("gitlab.com/group/subgroup", 1).path, "group/subgroup");
    }

    #[test]
    fn readme_choice_and_encoding() {
        let files = vec![("README.rst".to_string(), 1), ("readme.md".to_string(), 2), ("main.rs".to_string(), 3)];
        assert_eq!(pick_readme(files.into_iter()), Some(("readme.md".to_string(), 2)));
        assert_eq!(pick_readme(vec![("x".to_string(), 1)].into_iter()), None);
        assert_eq!(encode_path("group/sub proj"), "group%2Fsub%20proj");
    }
}
