//! Minimal GitHub REST client: releases, starred repos, the current user, and
//! asset downloads. JSON responses are cached with ETags, so unchanged data
//! comes back as "304 Not Modified", which doesn't count against the API rate
//! limit. That keeps hourly checks of many repos cheap.

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use log::debug;
use reqwest::blocking::{Client as Http, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION, ETAG, IF_NONE_MATCH, LINK};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub created_at: Option<String>,
    #[serde(default)]
    pub assets: Vec<Asset>,
    pub zipball_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    /// API URL; with `Accept: application/octet-stream` it serves the file
    /// (works for private repos too, given a token).
    pub url: String,
    /// "sha256:<hex>" on assets uploaded since mid-2025.
    pub digest: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RepoInfo {
    pub full_name: String,
    #[serde(default)]
    pub fork: bool,
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Deserialize)]
struct Readme {
    name: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    encoding: String,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub login: String,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    etag: String,
    next: Option<String>,
    body: String,
}

pub struct Client {
    http: Http,
    api_url: String,
    token: Option<String>,
    cache_dir: Option<PathBuf>,
}

impl Client {
    pub fn new(api_url: &str, token: Option<String>, cache_dir: Option<PathBuf>) -> Result<Client> {
        let http = Http::builder()
            .user_agent(concat!("GitHub-Grab/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .timeout(None) // no overall limit: assets can be many GB
            .tcp_keepalive(Duration::from_secs(60))
            .build()?;
        if let Some(d) = &cache_dir {
            let _ = fs::create_dir_all(d);
        }
        Ok(Client { http, api_url: api_url.trim_end_matches('/').to_string(), token, cache_dir })
    }

    fn request(&self, url: &str, accept: &str, etag: Option<&str>) -> Result<Response> {
        let mut req = self.http.get(url).header(ACCEPT, accept).header("X-GitHub-Api-Version", "2022-11-28");
        // reqwest drops the Authorization header when a redirect leaves the
        // host, so the token is never sent to the CDN serving the file.
        if let Some(t) = &self.token {
            req = req.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        if let Some(e) = etag {
            req = req.header(IF_NONE_MATCH, e);
        }
        req.send().with_context(|| format!("request to {url} failed"))
    }

    /// GET a JSON API URL, using the ETag cache. Returns the body and the
    /// "next page" link, if any, or `None` if GitHub answers 404.
    fn get_json_raw(&self, url: &str) -> Result<Option<(String, Option<String>)>> {
        let cache_file = self.cache_dir.as_ref().map(|d| d.join(format!("{}.json", &hex::encode(Sha256::digest(url))[..24])));
        let cached: Option<CacheEntry> =
            cache_file.as_ref().and_then(|f| fs::read_to_string(f).ok()).and_then(|t| serde_json::from_str(&t).ok());

        let resp = self.request(url, "application/vnd.github+json", cached.as_ref().map(|c| c.etag.as_str()))?;
        if resp.status() == StatusCode::NOT_MODIFIED {
            if let Some(c) = cached {
                debug!("not modified: {url}");
                return Ok(Some((c.body, c.next)));
            }
        }
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = check_status(resp, url)?;
        let etag = resp.headers().get(ETAG).and_then(|v| v.to_str().ok()).map(str::to_string);
        let next = resp.headers().get(LINK).and_then(|v| v.to_str().ok()).and_then(next_link);
        let body = resp.text().with_context(|| format!("cannot read response from {url}"))?;
        if let (Some(f), Some(etag)) = (&cache_file, etag) {
            let entry = CacheEntry { etag, next: next.clone(), body: body.clone() };
            let _ = fs::write(f, serde_json::to_string(&entry).unwrap_or_default());
        }
        Ok(Some((body, next)))
    }

    /// Like `get_json`, but a 404 gives `Ok(None)`.
    fn get_json_opt<T: DeserializeOwned>(&self, url: &str) -> Result<Option<(T, Option<String>)>> {
        match self.get_json_raw(url)? {
            None => Ok(None),
            Some((body, next)) => {
                let v = serde_json::from_str(&body).with_context(|| format!("unexpected response from {url}"))?;
                Ok(Some((v, next)))
            }
        }
    }

    fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<(T, Option<String>)> {
        self.get_json_opt(url)?
            .ok_or_else(|| anyhow!("not found: {url} (check the name; private repos need a token)"))
    }

    /// The repo's README (name and file contents), or `None` if it has none.
    pub fn readme(&self, repo: &str) -> Result<Option<(String, Vec<u8>)>> {
        let url = format!("{}/repos/{repo}/readme", self.api_url);
        let Some((r, _)) = self.get_json_opt::<Readme>(&url)? else { return Ok(None) };
        if r.encoding != "base64" {
            bail!("README of {repo} has unexpected encoding \"{}\"", r.encoding);
        }
        // GitHub wraps the base64 text in lines.
        let packed: String = r.content.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = BASE64.decode(packed).with_context(|| format!("README of {repo} is not valid base64"))?;
        Ok(Some((r.name, bytes)))
    }

    /// Newest releases first (as GitHub returns them), drafts included.
    pub fn releases(&self, repo: &str, wanted: usize) -> Result<Vec<Release>> {
        // Fetch extra so filtering out drafts and prereleases still leaves enough.
        let per_page = (wanted * 2 + 10).min(100);
        let url = format!("{}/repos/{repo}/releases?per_page={per_page}", self.api_url);
        Ok(self.get_json(&url)?.0)
    }

    /// Repos starred by `user`, or by the logged-in account if `None`.
    pub fn starred(&self, user: Option<&str>) -> Result<Vec<RepoInfo>> {
        let mut url = Some(match user {
            Some(u) => format!("{}/users/{u}/starred?per_page=100", self.api_url),
            None => {
                if self.token.is_none() {
                    bail!("listing your own starred repos needs a login: run `login`, set a token, or set `user` under [starred]");
                }
                format!("{}/user/starred?per_page=100", self.api_url)
            }
        });
        let mut all = Vec::new();
        let mut pages = 0;
        while let Some(u) = url {
            let (mut page, next): (Vec<RepoInfo>, _) = self.get_json(&u)?;
            all.append(&mut page);
            url = next;
            pages += 1;
            if pages >= 100 {
                break; // 10,000 stars is plenty
            }
        }
        Ok(all)
    }

    pub fn current_user(&self) -> Result<User> {
        let url = format!("{}/user", self.api_url);
        let resp = check_status(self.request(&url, "application/vnd.github+json", None)?, &url)?;
        Ok(resp.json()?)
    }

    /// Downloads `url` to `dest` via a `.part` file, checking size and SHA-256
    /// when known. Returns the number of bytes written.
    pub fn download(&self, url: &str, accept: &str, dest: &Path, size: Option<u64>, digest: Option<&str>) -> Result<u64> {
        let part = part_path(dest);
        match self.download_inner(url, accept, &part, size, digest) {
            Ok(n) => {
                if dest.exists() {
                    fs::remove_file(dest)?; // stale/incomplete copy being replaced
                }
                fs::rename(&part, dest).with_context(|| format!("cannot rename to {}", dest.display()))?;
                Ok(n)
            }
            Err(e) => {
                let _ = fs::remove_file(&part);
                Err(e)
            }
        }
    }

    fn download_inner(&self, url: &str, accept: &str, part: &Path, size: Option<u64>, digest: Option<&str>) -> Result<u64> {
        let mut resp = check_status(self.request(url, accept, None)?, url)?;
        let mut file = File::create(part).with_context(|| format!("cannot create {}", part.display()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut total = 0u64;
        loop {
            let n = resp.read(&mut buf).context("download interrupted")?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).with_context(|| format!("cannot write {}", part.display()))?;
            hasher.update(&buf[..n]);
            total += n as u64;
        }
        file.sync_all()?;
        drop(file);
        if let Some(expected) = size {
            if total != expected {
                bail!("size mismatch: expected {expected} bytes, got {total}");
            }
        }
        if let Some(expected) = digest.and_then(|d| d.strip_prefix("sha256:")) {
            let actual = hex::encode(hasher.finalize());
            if !actual.eq_ignore_ascii_case(expected) {
                bail!("SHA-256 mismatch: expected {expected}, got {actual}");
            }
        }
        Ok(total)
    }
}

pub fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Extracts the rel="next" URL from a Link header.
fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        params.contains("rel=\"next\"").then(|| url.trim().trim_start_matches('<').trim_end_matches('>').to_string())
    })
}

fn check_status(resp: Response, url: &str) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let headers = resp.headers().clone();
    let header = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
    let body: String = resp.text().unwrap_or_default().chars().take(300).collect();
    if (status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS)
        && header("x-ratelimit-remaining").as_deref() == Some("0")
    {
        let mins = header("x-ratelimit-reset")
            .and_then(|r| r.parse::<u64>().ok())
            .map(|reset| {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                reset.saturating_sub(now).div_ceil(60)
            })
            .unwrap_or(0);
        return Err(anyhow!(
            "GitHub API rate limit reached (resets in ~{mins} min). Run `login` or set a token to get 5000 requests/hour"
        ));
    }
    if status == StatusCode::UNAUTHORIZED {
        return Err(anyhow!("GitHub rejected the token (expired or revoked?). Run `login` again"));
    }
    if status == StatusCode::NOT_FOUND {
        return Err(anyhow!("not found: {url} (check the name; private repos need a token)"));
    }
    Err(anyhow!("HTTP {status} from {url}: {body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_link_header() {
        let h = r#"<https://api.github.com/user/starred?page=2>; rel="next", <https://api.github.com/user/starred?page=5>; rel="last""#;
        assert_eq!(next_link(h).as_deref(), Some("https://api.github.com/user/starred?page=2"));
        assert_eq!(next_link(r#"<https://x/?page=1>; rel="prev""#), None);
    }
}
