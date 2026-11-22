//! Windows Task Scheduler integration: registers a task that runs `run` on a
//! schedule (hourly by default), using `schtasks.exe` with a task XML file.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_NAME: &str = "Git-Grab";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Logon {
    /// Runs only while the user is logged on. No password is stored.
    Interactive,
    /// Runs whether or not the user is logged on. Windows stores the password
    /// so the task can reach network shares. No console window appears.
    Password,
}

pub struct TaskSpec {
    pub name: String,
    pub exe: PathBuf,
    pub args: String,
    pub workdir: PathBuf,
    pub interval_minutes: u32,
    pub user: String,
    pub logon: Logon,
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// Task definition. The trigger starts in the past and repeats indefinitely;
/// StartWhenAvailable catches up on runs missed while the PC was off, and
/// IgnoreNew stops a slow run from overlapping the next one.
pub fn task_xml(t: &TaskSpec) -> String {
    let logon = match t.logon {
        Logon::Interactive => "InteractiveToken",
        Logon::Password => "Password",
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Checks for new releases and archives them. Created by Git-Grab.</Description>
  </RegistrationInfo>
  <Triggers>
    <TimeTrigger>
      <Repetition>
        <Interval>PT{interval}M</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <StartBoundary>2025-01-01T00:05:00</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>{logon}</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>true</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <ExecutionTimeLimit>PT12H</ExecutionTimeLimit>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>{args}</Arguments>
      <WorkingDirectory>{workdir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#,
        interval = t.interval_minutes,
        user = xml_escape(&t.user),
        logon = logon,
        exe = xml_escape(&clean_path(&t.exe)),
        args = xml_escape(&t.args),
        workdir = xml_escape(&clean_path(&t.workdir)),
    )
}

/// Removes the `\\?\` prefix `canonicalize` adds on Windows, which Task Scheduler rejects.
pub fn clean_path(p: &Path) -> String {
    let s = p.display().to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s
    }
}

pub fn absolute(p: &Path) -> Result<PathBuf> {
    let abs = std::fs::canonicalize(p).with_context(|| format!("cannot find {}", p.display()))?;
    Ok(PathBuf::from(clean_path(&abs)))
}

/// "DOMAIN\user" for the current Windows user.
pub fn current_user() -> Result<String> {
    let user = std::env::var("USERNAME").context("USERNAME is not set")?;
    Ok(match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() => format!(r"{d}\{user}"),
        _ => user,
    })
}

pub fn ensure_windows() -> Result<()> {
    if !cfg!(windows) {
        bail!("Task Scheduler is only available on Windows. On Linux, use a systemd timer or cron (see the README)");
    }
    Ok(())
}

fn schtasks(args: &[&str]) -> Result<String> {
    let out = Command::new("schtasks.exe").args(args).output().context("cannot run schtasks.exe")?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        bail!("schtasks failed: {}", text.trim());
    }
    Ok(text)
}

pub fn install(spec: &TaskSpec, password: Option<&str>) -> Result<()> {
    ensure_windows()?;
    // schtasks wants the XML as UTF-16 with a byte-order mark.
    let xml = task_xml(spec);
    let mut bytes = vec![0xFF, 0xFE];
    for unit in xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let tmp = std::env::temp_dir().join(format!("git-grab-task-{}.xml", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    let tmp_s = tmp.display().to_string();
    let mut args = vec!["/Create", "/TN", spec.name.as_str(), "/XML", tmp_s.as_str(), "/F"];
    if let Some(pw) = password {
        args.extend(["/RU", spec.user.as_str(), "/RP", pw]);
    }
    let result = schtasks(&args);
    let _ = std::fs::remove_file(&tmp);
    result.map(|_| ())
}

pub fn uninstall(name: &str) -> Result<()> {
    ensure_windows()?;
    schtasks(&["/Delete", "/TN", name, "/F"]).map(|_| ())
}

pub fn status(name: &str) -> Result<String> {
    ensure_windows()?;
    schtasks(&["/Query", "/TN", name, "/FO", "LIST", "/V"])
}

/// The program path and arguments the scheduled task runs, if it exists.
pub fn task_paths(name: &str) -> Option<(String, String)> {
    if !cfg!(windows) {
        return None;
    }
    let xml = schtasks(&["/Query", "/TN", name, "/XML"]).ok()?;
    Some((xml_field(&xml, "Command")?, xml_field(&xml, "Arguments").unwrap_or_default()))
}

fn xml_field(xml: &str, tag: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = start + xml[start..].find(&format!("</{tag}>"))?;
    let v = &xml[start..end];
    Some(v.replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&"))
}

pub fn run_now(name: &str) -> Result<()> {
    ensure_windows()?;
    schtasks(&["/Run", "/TN", name]).map(|_| ())
}

/// True when the program got a console window of its own, which is what
/// happens when it's double-clicked in Explorer. When it's started from
/// PowerShell or cmd, the shell shares the console, so more than one process
/// is attached.
pub fn launched_from_explorer() -> bool {
    #[cfg(windows)]
    unsafe {
        let mut pids = [0u32; 4];
        windows_sys::Win32::System::Console::GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) == 1
    }
    #[cfg(not(windows))]
    false
}

/// Detaches from the console window. Used by scheduled runs, so the window
/// Task Scheduler opens for interactive tasks closes right away.
pub fn hide_console() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::FreeConsole();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_xml() {
        let spec = TaskSpec {
            name: DEFAULT_NAME.into(),
            exe: PathBuf::from(r"\\?\C:\Tools\gg\git-grab.exe"),
            args: r#"--config "C:\Tools\gg\config.toml" run"#.into(),
            workdir: PathBuf::from(r"C:\Tools\gg"),
            interval_minutes: 60,
            user: r"PC\eric & co".into(),
            logon: Logon::Password,
        };
        let xml = task_xml(&spec);
        assert!(xml.contains("<Interval>PT60M</Interval>"));
        assert!(xml.contains(r"<Command>C:\Tools\gg\git-grab.exe</Command>"));
        assert!(xml.contains("<Arguments>--config &quot;C:\\Tools\\gg\\config.toml&quot; run</Arguments>"));
        assert!(xml.contains(r"<UserId>PC\eric &amp; co</UserId>"));
        assert!(xml.contains("<LogonType>Password</LogonType>"));
    }

    #[test]
    fn reads_task_xml_fields() {
        let spec = TaskSpec {
            name: DEFAULT_NAME.into(),
            exe: PathBuf::from(r"C:\Tools\A & B\git-grab.exe"),
            args: r#"--config "C:\Tools\A & B\config.toml" run"#.into(),
            workdir: PathBuf::from(r"C:\Tools"),
            interval_minutes: 60,
            user: "me".into(),
            logon: Logon::Interactive,
        };
        let xml = task_xml(&spec);
        assert_eq!(xml_field(&xml, "Command").unwrap(), r"C:\Tools\A & B\git-grab.exe");
        assert_eq!(xml_field(&xml, "Arguments").unwrap(), r#"--config "C:\Tools\A & B\config.toml" run"#);
    }

    #[test]
    fn cleans_verbatim_paths() {
        assert_eq!(clean_path(Path::new(r"\\?\C:\x")), r"C:\x");
        assert_eq!(clean_path(Path::new(r"\\?\UNC\nas\share")), r"\\nas\share");
        assert_eq!(clean_path(Path::new("/usr/bin")), "/usr/bin");
    }
}
