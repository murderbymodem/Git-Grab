//! Logging in and storing tokens.
//!
//! Tokens are kept in the program's folder (next to the config), so the whole
//! setup stays in one place: `token` for GitHub and `token-<host>` for other
//! sites. On Windows the file is encrypted with the Windows data protection
//! API (DPAPI), so only the same Windows user on the same PC can read it; on
//! Linux it's readable only by its owner.

use anyhow::{bail, Context, Result};
use log::warn;
use serde::Deserialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
    match load_for(crate::forge::GITHUB_HOST) {
        Ok(Some(t)) => Some((t, Source::Saved)),
        Ok(None) => None,
        Err(e) => {
            warn!("cannot read saved login: {e:#}");
            None
        }
    }
}

/// The saved token for a site: github.com uses `resolve`; other sites use
/// their [[site]] token from the config, then a login saved with `login --site`.
pub fn resolve_site(host: &str, config_token: Option<&str>, github_config_token: Option<&str>) -> Option<String> {
    if host == crate::forge::GITHUB_HOST {
        return resolve(github_config_token).map(|(t, _)| t);
    }
    if let Some(t) = config_token.filter(|t| !t.is_empty()) {
        return Some(t.to_string());
    }
    load_for(host).unwrap_or_else(|e| {
        warn!("cannot read saved login for {host}: {e:#}");
        None
    })
}

fn token_path(host: &str) -> PathBuf {
    let name = if host == crate::forge::GITHUB_HOST { "token".to_string() } else { format!("token-{}", crate::util::sanitize(host)) };
    crate::paths::data_dir().join(name)
}

/// Saves a token; returns where.
pub fn save_for(host: &str, token: &str) -> Result<String> {
    let p = token_path(host);
    let sealed = seal(token.as_bytes())?;
    write_private(&p, &sealed).with_context(|| format!("cannot save the login to {}", p.display()))?;
    Ok(p.display().to_string())
}

pub fn load_for(host: &str) -> Result<Option<String>> {
    let p = token_path(host);
    let bytes = match fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let plain = unseal(&bytes).with_context(|| {
        format!("{} was saved by another user or on another PC; run `login` again", p.display())
    })?;
    let t = String::from_utf8(plain).context("saved login is damaged; run `login` again")?;
    Ok(Some(t.trim().to_string()).filter(|t| !t.is_empty()))
}

pub fn delete_for(host: &str) -> Result<bool> {
    match fs::remove_file(token_path(host)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Writes a file only its owner can read (on Linux; on Windows the contents
/// are encrypted for the current user instead).
fn write_private(p: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir)?;
    }
    let _ = fs::remove_file(p); // so a new file is created with the permissions below
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(p)?.write_all(bytes)?;
    Ok(())
}

#[cfg(windows)]
fn seal(data: &[u8]) -> Result<Vec<u8>> {
    dpapi::protect(data)
}
#[cfg(windows)]
fn unseal(data: &[u8]) -> Result<Vec<u8>> {
    dpapi::unprotect(data)
}
#[cfg(not(windows))]
fn seal(data: &[u8]) -> Result<Vec<u8>> {
    Ok(data.to_vec())
}
#[cfg(not(windows))]
fn unseal(data: &[u8]) -> Result<Vec<u8>> {
    Ok(data.to_vec())
}

/// Windows data protection: encrypts data so only the current Windows user
/// (on this PC) can decrypt it.
#[cfg(windows)]
mod dpapi {
    use anyhow::{bail, Result};
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn run(data: &[u8], protect: bool) -> Result<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() };
        // SAFETY: input points at `data` for the duration of the call; output
        // is allocated by Windows and freed with LocalFree below.
        let ok = unsafe {
            if protect {
                CryptProtectData(&input, null(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output)
            } else {
                CryptUnprotectData(&input, null_mut(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output)
            }
        };
        if ok == 0 {
            bail!("{}", std::io::Error::last_os_error());
        }
        // SAFETY: on success Windows filled output with a buffer of cbData bytes.
        let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
        unsafe { LocalFree(output.pbData as _) };
        Ok(bytes)
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
        run(data, true)
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>> {
        run(data, false)
    }
}

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
        .user_agent(concat!("Git-Grab/", env!("CARGO_PKG_VERSION")))
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
    fn sealed_tokens_round_trip() {
        let sealed = seal(b"github_pat_example").unwrap();
        #[cfg(windows)]
        assert_ne!(sealed, b"github_pat_example"); // encrypted with DPAPI
        assert_eq!(unseal(&sealed).unwrap(), b"github_pat_example");
    }

    #[test]
    fn token_files_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("token");
        write_private(&p, b"x").unwrap();
        write_private(&p, b"y").unwrap(); // replacing works
        assert_eq!(fs::read(&p).unwrap(), b"y");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn web_urls() {
        assert_eq!(web_url("https://api.github.com"), "https://github.com");
        assert_eq!(web_url("https://api.github.com/"), "https://github.com");
        assert_eq!(web_url("https://ghe.example.com/api/v3"), "https://ghe.example.com");
    }
}
