//! Where the program keeps its files.
//!
//! Git-Grab is portable: everything it keeps (other than the archive itself)
//! lives in one folder, the folder that holds the config file. By default
//! that's the program's own folder; with `-c path/to/config.toml` it's that
//! file's folder. That covers config.toml, the log, the download cache and
//! saved logins.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const CONFIG_FILE: &str = "config.toml";
pub const LOG_FILE: &str = "git-grab.log";

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The folder that holds the running executable.
pub fn exe_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let clean = PathBuf::from(crate::wintask::clean_path(&exe));
    clean.parent().map(Path::to_path_buf)
}

/// config.toml next to the program.
pub fn default_config() -> PathBuf {
    exe_dir().unwrap_or_else(|| PathBuf::from(".")).join(CONFIG_FILE)
}

/// Uses the folder of `config_path` for everything else. Call once, at start-up.
pub fn init(config_path: &Path) {
    let dir = config_path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let dir = std::fs::canonicalize(dir).map(|d| PathBuf::from(crate::wintask::clean_path(&d))).unwrap_or_else(|_| dir.to_path_buf());
    let _ = DATA_DIR.set(dir);
}

/// The folder for the log, cache and logins (the config file's folder).
pub fn data_dir() -> PathBuf {
    DATA_DIR.get().cloned().unwrap_or_else(|| exe_dir().unwrap_or_else(|| PathBuf::from(".")))
}

pub fn cache_dir() -> PathBuf {
    data_dir().join("cache")
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

pub fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".write-test-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}
