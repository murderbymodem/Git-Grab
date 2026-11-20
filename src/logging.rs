//! Log output to the console, or to a file for scheduled runs.

use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::path::Path;

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

pub fn init(log_file: Option<&Path>) -> Result<()> {
    let mut b = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if let Some(path) = log_file {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        // Keep one previous log: app.log -> app.log.1 once it passes 5 MiB.
        if fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
            let mut old = path.as_os_str().to_owned();
            old.push(".1");
            let _ = fs::rename(path, old);
        }
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("cannot open log file {}", path.display()))?;
        b.target(env_logger::Target::Pipe(Box::new(f))).write_style(env_logger::WriteStyle::Never);
    }
    b.init();
    Ok(())
}
