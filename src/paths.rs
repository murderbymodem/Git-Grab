//! Where the program keeps its files.
//!
//! The program is portable: its config, log and download cache live in the
//! folder that holds the executable. If that folder can't be written to (for
//! example C:\Program Files or /usr/local/bin), the per-user folder
//! (%APPDATA%\github-grab or ~/.config/github-grab) is used instead.

use crate::config::{APP_DIR, LEGACY_APP_DIR};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const CONFIG_FILE: &str = "config.toml";
pub const LOG_FILE: &str = "github-grab.log";
/// Log file name before the rename to GitHub-Grab.
const LEGACY_LOG_FILE: &str = "github-release-archiver.log";
/// Program file name before the rename to GitHub-Grab.
pub const LEGACY_EXE: &str = "github-release-archiver.exe";

/// The folder that holds the running executable.
pub fn exe_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let clean = PathBuf::from(crate::wintask::clean_path(&exe));
    clean.parent().map(Path::to_path_buf)
}

/// The folder for config, log and cache: next to the executable when writable.
pub fn data_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| match exe_dir() {
        Some(d) if is_writable(&d) => d,
        _ => dirs::config_dir().map(|d| d.join(APP_DIR)).unwrap_or_else(|| PathBuf::from(".")),
    })
    .clone()
}

pub fn is_portable() -> bool {
    exe_dir().is_some_and(|d| d == data_dir())
}

pub fn config_path() -> PathBuf {
    data_dir().join(CONFIG_FILE)
}

pub fn cache_dir() -> PathBuf {
    data_dir().join("cache")
}

/// Per-user config folder used before 0.7 and before the rename to GitHub-Grab:
/// %APPDATA%\github-release-archiver or ~/.config/github-release-archiver.
pub fn legacy_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join(LEGACY_APP_DIR))
}

/// A config file left by an earlier version, if there is one and it isn't the current one.
pub fn legacy_config() -> Option<PathBuf> {
    let p = legacy_config_dir()?.join(CONFIG_FILE);
    (p.is_file() && p != config_path()).then_some(p)
}

/// Removes files earlier versions left in per-user folders: the old config
/// and logs, the old download cache, and the copy of the program that setup
/// used to install into %LOCALAPPDATA%\Programs. Only these known names are
/// deleted; a folder is removed only if it's empty afterwards.
pub fn remove_legacy_files() -> Vec<PathBuf> {
    crate::auth::migrate_legacy();
    let mut removed = Vec::new();
    let remove_file = |p: PathBuf, removed: &mut Vec<PathBuf>| {
        if p.is_file() && std::fs::remove_file(&p).is_ok() {
            removed.push(p);
        }
    };
    let current = data_dir();
    if let Some(dir) = legacy_config_dir().filter(|d| *d != current) {
        for name in [CONFIG_FILE, LEGACY_LOG_FILE, "github-release-archiver.log.1"] {
            remove_file(dir.join(name), &mut removed);
        }
        let _ = std::fs::remove_dir(&dir);
    }
    if let Some(cache) = dirs::cache_dir().map(|d| d.join(LEGACY_APP_DIR)) {
        if cache.join("http").is_dir() && std::fs::remove_dir_all(cache.join("http")).is_ok() {
            removed.push(cache.join("http"));
        }
        let _ = std::fs::remove_dir(&cache);
    }
    if let Some(dir) = dirs::data_local_dir().map(|d| d.join("Programs").join(LEGACY_APP_DIR)) {
        if exe_dir().as_deref() != Some(dir.as_path()) {
            remove_file(dir.join(LEGACY_EXE), &mut removed);
            let _ = std::fs::remove_dir(&dir);
        }
    }
    removed
}

/// After the rename to GitHub-Grab: renames old log files next to the program
/// and removes the old program file there, once nothing runs it any more
/// (the caller has just pointed the scheduled task at the new program).
pub fn tidy_after_rename() -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let dir = data_dir();
    for (old, new) in [(LEGACY_LOG_FILE, LOG_FILE), ("github-release-archiver.log.1", "github-grab.log.1")] {
        let (old, new) = (dir.join(old), dir.join(new));
        if old.is_file() {
            if !new.exists() {
                let _ = std::fs::rename(&old, &new);
            } else if std::fs::remove_file(&old).is_ok() {
                removed.push(old);
            }
        }
    }
    if let (Some(exe_dir), Ok(current)) = (exe_dir(), std::env::current_exe()) {
        let old = exe_dir.join(LEGACY_EXE);
        if old.is_file() && current.file_name() != old.file_name() && std::fs::remove_file(&old).is_ok() {
            removed.push(old);
        }
    }
    removed
}

/// Where the program is running from, if that's a poor place to keep it.
pub enum Location {
    Fine,
    /// Opened straight from the zip: Windows runs it from a temporary folder
    /// that disappears.
    InsideZip,
    Downloads,
}

pub fn location() -> Location {
    let Some(dir) = exe_dir() else { return Location::Fine };
    let canon = |p: PathBuf| std::fs::canonicalize(&p).map(|c| PathBuf::from(crate::wintask::clean_path(&c))).unwrap_or(p);
    if dir.starts_with(canon(std::env::temp_dir())) {
        return Location::InsideZip;
    }
    let in_downloads = dirs::download_dir().is_some_and(|d| dir.starts_with(canon(d)))
        || dir.components().any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case("downloads"));
    if in_downloads {
        Location::Downloads
    } else {
        Location::Fine
    }
}

fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".write-test-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}
