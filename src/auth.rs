//! Logging in to GitHub and storing the token.
//!
//! The token is kept in Windows Credential Manager on Windows (safer than a
//! file, and nothing to carry around), and on Linux in a `token` file readable
//! only by its owner, next to the config.

use anyhow::{bail, Context, Result};
use log::warn;
use serde::Deserialize;
use std::time::{Duration, Instant};

const SERVICE: &str = "github-release-archiver";

/// Where a token came from, for `status`.
pub enum Source {
    Config,
    Env(&'static str),
    Saved,
}

/// Token lookup order: config file, GITHUB_TOKEN, GH_TOKEN, saved login.
pub fn resolve(config_token: Option<&str>) -> Option<(String, Source)> {
    if let Some(t) = config_token.filter(|t| !t.is_empty()) {
        return Some((t.to_string(), Source::Config));
    }
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            if !t.is_empty() {
                return Some((t, Source::Env(var)));
            }
        }
    }
    match load() {
        Ok(Some(t)) => Some((t, Source::Saved)),
        Ok(None) => None,
        Err(e) => {
            warn!("cannot read saved login: {e:#}");
            None
        }
    }
}

#[cfg(windows)]
mod store {
    /// The Windows token lives in Credential Manager, which doesn't move.
    pub fn migrate_legacy() {}

    use super::SERVICE;
    use anyhow::Result;

    fn entry() -> Result<keyring::Entry> {
        Ok(keyring::Entry::new(SERVICE, "github-token")?)
    }
    pub fn save(token: &str) -> Result<String> {
        entry()?.set_password(token)?;
        Ok("Windows Credential Manager".into())
    }
    pub fn load() -> Result<Option<String>> {
        match entry()?.get_password() {
            Ok(t) => Ok(Some(t)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub fn delete() -> Result<bool> {
        match entry()?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(not(windows))]
mod store {
    use super::SERVICE;
    use anyhow::Result;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::PathBuf;

    /// Next to the program (portable), like the config.
    fn path() -> Result<PathBuf> {
        Ok(crate::paths::data_dir().join("token"))
    }
    /// Where versions before 0.7 saved it.
    fn legacy_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join(SERVICE).join("token"))
    }
    pub fn save(token: &str) -> Result<String> {
        let p = path()?;
        fs::create_dir_all(p.parent().unwrap())?;
        let _ = fs::remove_file(&p); // so the new file is created with mode 600
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&p)?;
        f.write_all(token.as_bytes())?;
        Ok(p.display().to_string())
    }
    pub fn load() -> Result<Option<String>> {
        let p = path()?;
        let p = match legacy_path() {
            Some(old) if !p.exists() && old.exists() => old,
            _ => p,
        };
        match fs::read_to_string(p) {
            Ok(t) => Ok(Some(t.trim().to_string()).filter(|t| !t.is_empty())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    /// Moves a token saved by an earlier version next to the program.
    pub fn migrate_legacy() {
        let (Ok(new), Some(old)) = (path(), legacy_path()) else { return };
        if old == new || !old.exists() {
            return;
        }
        if !new.exists() && fs::copy(&old, &new).is_err() {
            return; // fs::copy keeps the 600 permissions
        }
        let _ = fs::remove_file(&old);
    }
    pub fn delete() -> Result<bool> {
        match fs::remove_file(path()?) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

pub use store::{delete, load, migrate_legacy, save};

/// The web host for an API URL: api.github.com -> github.com; GHES strips /api/v3.
pub fn web_url(api_url: &str) -> String {
    let api = api_url.trim_end_matches('/');
    if api == "https://api.github.com" {
        "https://github.com".into()
    } else {
        api.trim_end_matches("/api/v3").to_string()
    }
}

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<u64>,
}

/// GitHub's device flow: show a code, the user approves it in a browser, and
/// we receive a token. Needs the client ID of an OAuth App with device flow
/// enabled (no client secret is involved).
pub fn device_login(api_url: &str, client_id: &str) -> Result<String> {
    let web = web_url(api_url);
    let http = reqwest::blocking::Client::builder()
        .user_agent(concat!("github-release-archiver/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()?;
    // No scopes: read access to public data, which covers public stars and releases.
    let code: DeviceCode = http
        .post(format!("{web}/login/device/code"))
        .header("Accept", "application/json")
        .form(&[("client_id", client_id), ("scope", "")])
        .send()
        .context("cannot reach GitHub")?
        .error_for_status()
        .context("GitHub refused the login request (is the client ID right, and device flow enabled on the OAuth App?)")?
        .json()?;

    println!("\nOpen {} and enter the code:\n\n    {}\n", code.verification_uri, code.user_code);
    open_browser(&code.verification_uri);
    println!("Waiting for you to approve the login...");

    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let mut interval = code.interval.max(5);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(interval));
        let reply: TokenReply = http
            .post(format!("{web}/login/oauth/access_token"))
            .header("Accept", "application/json")
            .form(&[
                ("client_id", client_id),
                ("device_code", code.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()?
            .json()?;
        if let Some(t) = reply.access_token {
            return Ok(t);
        }
        match reply.error.as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval = reply.interval.unwrap_or(interval + 5),
            Some("expired_token") => bail!("the code expired; run `login` again"),
            Some("access_denied") => bail!("login was cancelled in the browser"),
            Some(e) => bail!("login failed: {e} {}", reply.error_description.unwrap_or_default()),
            None => bail!("unexpected reply from GitHub"),
        }
    }
    bail!("the code expired; run `login` again")
}

/// Reads a pasted token without echoing it.
pub fn prompt_token() -> Result<String> {
    println!("Create a token at https://github.com/settings/personal-access-tokens/new");
    println!("  (fine-grained; under Account permissions give \"Starring\" read access, nothing else is needed)");
    let t = rpassword::prompt_password("Paste the token (input is hidden): ")?;
    let t = t.trim().to_string();
    if t.is_empty() {
        bail!("no token entered");
    }
    Ok(t)
}

fn open_browser(url: &str) {
    #[cfg(windows)]
    let r = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    #[cfg(not(windows))]
    let r = if std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some() {
        std::process::Command::new("xdg-open").arg(url).spawn()
    } else {
        return;
    };
    let _ = r;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_urls() {
        assert_eq!(web_url("https://api.github.com"), "https://github.com");
        assert_eq!(web_url("https://api.github.com/"), "https://github.com");
        assert_eq!(web_url("https://ghe.example.com/api/v3"), "https://ghe.example.com");
    }
}
