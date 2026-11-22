//! Guided setup, shown when the program is double-clicked (or run as `setup`).

use crate::config::Config;
use crate::forge::Location;
use crate::paths::{self, Location as Place};
use crate::{auth, wintask};
use anyhow::{Context, Result};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

pub fn run(config_path: &Path) -> Result<()> {
    println!("Git-Grab {}", env!("CARGO_PKG_VERSION"));
    println!("Saves every file from each release on GitHub, GitLab, Gitea or Forgejo to a");
    println!("folder or network share, as <Site>\\<Project>\\<version>\\<file>.\n");

    match paths::location() {
        Place::InsideZip => {
            println!("It looks like this was opened straight from the zip file, so Windows is");
            println!("running it from a temporary folder. Extract the zip first (right-click it,");
            println!("Extract All), put the folder somewhere permanent such as C:\\Tools, and");
            println!("start the program from there.");
            return Ok(());
        }
        Place::Downloads => {
            println!("This program is in your Downloads folder. Its settings are saved next to it,");
            println!("and the hourly check runs it from where it is, so it's best to move the whole");
            println!("folder somewhere permanent first, for example C:\\Tools\\Git-Grab,");
            println!("then start it from there.");
            if !yes_no("Continue from the Downloads folder anyway?", false) {
                return Ok(());
            }
            println!();
        }
        Place::Fine => {}
    }
    // Everything is kept next to the config, so that folder must be writable.
    let dir = paths::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if !paths::is_writable(&dir) {
        println!("Git-Grab keeps its settings in its own folder, but {} can't be", dir.display());
        println!("written to. Move the program to a folder you can write to, for example");
        println!("C:\\Tools\\Git-Grab, and start it from there.");
        return Ok(());
    }

    if config_path.exists() {
        return menu(config_path);
    }
    first_time(config_path)
}

/// Warns when the scheduled task runs a different copy of the program or
/// config (e.g. after the folder was moved), and offers to fix it.
fn check_task_location(config_path: &Path) {
    let Some((command, args)) = wintask::task_paths(wintask::DEFAULT_NAME) else { return };
    let exe = std::env::current_exe().ok().and_then(|e| wintask::absolute(&e).ok());
    let cfg = wintask::absolute(config_path).ok();
    let same_exe = exe.is_some_and(|e| e.display().to_string().eq_ignore_ascii_case(&command));
    let same_cfg = cfg.is_some_and(|c| args.to_lowercase().contains(&c.display().to_string().to_lowercase()));
    if same_exe && same_cfg {
        return;
    }
    println!("The hourly check runs {command}");
    println!("with different settings than this copy of the program (was the folder moved?).");
    if yes_no("Update it to use this copy?", true) {
        if let Err(e) = schedule(config_path) {
            println!("Couldn't update the scheduled task: {e:#}");
        }
    }
    println!();
}

/// Shown when a config already exists.
fn menu(config_path: &Path) -> Result<()> {
    check_task_location(config_path);
    loop {
        println!("Config: {}\n", config_path.display());
        if let Err(e) = crate::status(config_path) {
            println!("(could not read status: {e:#})");
        }
        println!("\nWhat would you like to do?");
        println!("  1  Check for new releases now");
        println!("  2  Show what would be downloaded (no changes)");
        println!("  3  Edit the config file");
        println!("  4  Log in to GitHub again");
        println!("  5  Set up the hourly background check again");
        println!("  6  Start setup over (replaces the config)");
        println!("  Q  Quit");
        let choice = ask("Choice", "Q").to_ascii_uppercase();
        println!();
        let result = match choice.as_str() {
            "1" | "2" => Config::load(config_path).and_then(|c| crate::run_once(&c, choice == "2").map(|_| ())),
            "3" => open_editor(config_path),
            "4" => crate::login(config_path, None, false).map(|_| ()),
            "5" => schedule(config_path),
            "6" => return first_time(config_path),
            "Q" | "" => return Ok(()),
            _ => {
                println!("Please type a number from the list.");
                Ok(())
            }
        };
        if let Err(e) = result {
            println!("Error: {e:#}");
        }
        println!();
    }
}

fn first_time(config_path: &Path) -> Result<()> {
    // 1. Where to save
    println!("Step 1 of 5: Where should releases be saved?");
    println!("  A local folder like D:\\Software, or a network share like \\\\nas\\software.");
    let archive_root = loop {
        if let Some(f) = ask_folder("Folder") {
            break f;
        }
    };
    println!("\n  You can also keep a copy in a second place, e.g. a local archive mirrored");
    println!("  to a network share. Files are downloaded once and then copied there.");
    let mut mirrors: Vec<String> = Vec::new();
    loop {
        let label = if mirrors.is_empty() { "Second folder (Enter to skip)" } else { "Another folder (Enter when done)" };
        match ask_folder(label) {
            Some(f) if f.eq_ignore_ascii_case(&archive_root) || mirrors.iter().any(|m| m.eq_ignore_ascii_case(&f)) => {
                println!("  That folder is already used.");
            }
            Some(f) => mirrors.push(f),
            None => break,
        }
    }

    // 2. Login
    println!("\nStep 2 of 5: Log in to GitHub");
    println!("  Needed to archive your starred repos, and raises GitHub's limit from 60 to");
    println!("  5,000 checks per hour.");
    let mut login = auth::resolve(None).and_then(|(token, _)| {
        let client = crate::github::Client::new("https://api.github.com", Some(token), None).ok()?;
        client.current_user().ok().map(|u| u.login)
    });
    let prompt = match &login {
        Some(user) => format!("Already logged in as {user}. Log in again as someone else?"),
        None => "Log in now?".to_string(),
    };
    if yes_no(&prompt, login.is_none()) {
        loop {
            match crate::login(config_path, None, false) {
                Ok(user) => {
                    login = Some(user);
                    break;
                }
                Err(e) => {
                    println!("  Login failed: {e:#}");
                    if !yes_no("  Try again?", true) {
                        break;
                    }
                }
            }
        }
    }

    // 3. What to archive
    println!("\nStep 3 of 5: Which repos?");
    let starred_user = match &login {
        Some(user) => yes_no(&format!("Archive every repo starred by {user}?"), true).then_some(None),
        None => {
            let u = ask("GitHub username whose starred repos to archive (blank to skip)", "");
            (!u.is_empty()).then_some(Some(u))
        }
    };
    let remove_unstarred = starred_user.is_some()
        && yes_no("When you unstar a repo on GitHub, delete its archived files too?", false);
    println!("Add specific projects, one per line: owner/name for GitHub (e.g. BurntSushi/ripgrep),");
    println!("or paste a link from GitHub, GitLab, Gitea, Forgejo or Codeberg.");
    println!("Press Enter on an empty line when done.");
    let mut repos = Vec::new();
    loop {
        let r = ask("Project", "");
        if r.is_empty() {
            break;
        }
        match Location::parse(&r, 2) {
            Ok(l) => repos.push(spec(&l)),
            Err(e) => println!("  {e:#}"),
        }
    }
    println!("\nYou can also follow every project of a user or organization, on any of those");
    println!("sites. Paste their page, e.g. https://git.magicbean.dev/MagicBeanDev");
    let mut accounts = Vec::new();
    loop {
        let a = ask("Account (Enter when done)", "");
        if a.is_empty() {
            break;
        }
        match Location::parse(&a, 1) {
            Ok(l) => accounts.push(spec(&l)),
            Err(e) => println!("  {e:#}"),
        }
    }
    // Other sites only need a login for private projects.
    let mut asked = std::collections::HashSet::new();
    for (s, n) in repos.iter().map(|r| (r, 2)).chain(accounts.iter().map(|a| (a, 1))) {
        let Ok(l) = Location::parse(s, n) else { continue };
        if l.host == crate::forge::GITHUB_HOST || !asked.insert(l.host.clone()) {
            continue;
        }
        if yes_no(&format!("Log in to {}? Only needed for private projects.", l.host), false) {
            if let Err(e) = crate::login_site(config_path, &l.base, false) {
                println!("  Login failed: {e:#}");
            }
        }
    }

    // 4. Options
    println!("\nStep 4 of 5: Options");
    let releases: usize = loop {
        match ask("How many of the newest releases to keep per project?", "1").parse() {
            Ok(n) if n >= 1 => break n,
            _ => println!("  Enter a number, 1 or more."),
        }
    };
    let prune = yes_no(&format!("Delete older releases once there are more than {releases}?"), false);

    let text = build_config(&Answers {
        archive_root: &archive_root,
        mirrors: &mirrors,
        starred_user: starred_user.as_ref().map(|u| u.as_deref()),
        remove_unstarred,
        repos: &repos,
        accounts: &accounts,
        releases,
        prune,
    });
    Config::parse(&text).context("the generated config is invalid (please report this)")?;
    if let Some(dir) = config_path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(config_path, text)?;
    println!("\nSaved the config to {}", config_path.display());
    println!("(Every other option is explained in config.example.toml.)");

    // 5. Background checks
    println!("\nStep 5 of 5: Background checks");
    if cfg!(windows) {
        if yes_no("Check for new releases every hour in the background?", true) {
            if let Err(e) = schedule(config_path) {
                println!("Couldn't create the scheduled task: {e:#}");
            }
        }
    } else {
        println!("  On Linux, use a systemd timer or cron to run `run` hourly (see the README).");
    }

    if yes_no("\nShow what would be downloaded now?", true) {
        let cfg = Config::load(config_path)?;
        crate::run_once(&cfg, true)?;
        if yes_no("Download it now? (Otherwise the background check will.)", true) {
            crate::run_once(&cfg, false)?;
        }
    }
    println!("\nAll set. Double-click the program again any time for the menu.");
    if let Some(dir) = paths::exe_dir() {
        println!("Keep it in {} (the hourly check runs it from there).", dir.display());
        println!("If you move the folder, start the program from its new place and it will offer");
        println!("to update the hourly check.");
    }
    Ok(())
}

/// Asks for a folder and checks it can be reached. `None` = left blank.
fn ask_folder(label: &str) -> Option<String> {
    loop {
        let answer = ask(label, "");
        if answer.is_empty() {
            return None;
        }
        let p = PathBuf::from(&answer);
        if p.is_dir() {
            return Some(answer);
        }
        println!("  That folder doesn't exist or can't be reached.");
        if yes_no("  Create it?", true) {
            match std::fs::create_dir_all(&p) {
                Ok(()) => return Some(answer),
                Err(e) => println!("  Couldn't create it: {e}"),
            }
        } else if yes_no("  Use it anyway (e.g. the share isn't connected right now)?", false) {
            return Some(answer);
        }
    }
}

fn schedule(config_path: &Path) -> Result<()> {
    wintask::ensure_windows()?;
    println!("By default the check runs while you're logged on, and a window flashes briefly.");
    println!("Running it when you're logged off too hides the window, but Windows needs your");
    println!("password to store with the task.");
    let logged_off = yes_no("Also run while you're logged off?", false);
    crate::install_task(config_path, 60, logged_off, wintask::DEFAULT_NAME, None, None)
}

fn open_editor(path: &Path) -> Result<()> {
    let editor = if cfg!(windows) { "notepad".to_string() } else { std::env::var("EDITOR").unwrap_or_else(|_| "nano".into()) };
    std::process::Command::new(&editor).arg(path).status().with_context(|| format!("cannot start {editor}"))?;
    if let Err(e) = Config::load(path) {
        println!("Warning: the config now has a problem: {e:#}");
    }
    Ok(())
}

pub struct Answers<'a> {
    pub archive_root: &'a str,
    pub mirrors: &'a [String],
    /// None = starred off; Some(None) = the logged-in account; Some(Some(u)) = user u.
    pub starred_user: Option<Option<&'a str>>,
    /// Delete archived files of repos that get unstarred.
    pub remove_unstarred: bool,
    pub repos: &'a [String],
    pub accounts: &'a [String],
    pub releases: usize,
    pub prune: bool,
}

/// How a project or account is written in the config: owner/name for
/// github.com, otherwise its full address.
fn spec(l: &Location) -> String {
    if l.host == crate::forge::GITHUB_HOST {
        l.path.clone()
    } else {
        format!("{}/{}", l.base, l.path)
    }
}

/// TOML string with correct quoting/escaping for any input.
fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

pub fn build_config(a: &Answers) -> String {
    let mut t = String::new();
    t.push_str("# Created by Git-Grab setup. All options: see config.example.toml\n\n");
    t.push_str(&format!("archive_root = {}\n", q(a.archive_root)));
    if a.mirrors.is_empty() {
        t.push_str("# mirrors = ['\\\\nas\\software']   # extra folders that get a copy of the archive\n\n");
    } else {
        let list: Vec<String> = a.mirrors.iter().map(|m| q(m)).collect();
        t.push_str("# Extra folders that get a copy of the archive\n");
        t.push_str(&format!("mirrors = [{}]\n\n", list.join(", ")));
    }
    t.push_str("# How many of the newest releases to archive per project\n");
    t.push_str(&format!("releases = {}\n", a.releases));
    if a.prune {
        t.push_str("# Delete older releases (only ones this program downloaded)\n");
        t.push_str(&format!("keep_versions = {}\n", a.releases));
    } else {
        t.push_str(&format!("# keep_versions = {}   # uncomment to delete older releases\n", a.releases));
    }
    t.push_str("include_prereleases = false\n");
    t.push_str("# exclude = [\"*.sig\"]   # wildcard patterns of files to skip (default: download everything)\n");
    t.push_str("\n[starred]\n");
    match a.starred_user {
        None => t.push_str("enabled = false\n"),
        Some(None) => t.push_str("enabled = true   # repos starred by the logged-in account\n"),
        Some(Some(u)) => t.push_str(&format!("enabled = true\nuser = {}\n", q(u))),
    }
    t.push_str(&format!(
        "remove_unstarred = {}   # delete a repo's archived files once you unstar it\n",
        a.remove_unstarred
    ));
    t.push_str("# ignore = [\"someorg/*\"]   # starred repos to skip\n");
    for r in a.repos {
        t.push_str(&format!("\n[[repo]]\nrepo = {}\n", q(r)));
    }
    for acc in a.accounts {
        t.push_str(&format!("\n[[account]]   # every project of this user or organization\nurl = {}\n", q(acc)));
    }
    t
}

// ---- Console prompts ----

fn read_line() -> Option<String> {
    let mut s = String::new();
    match io::stdin().lock().read_line(&mut s) {
        Ok(0) | Err(_) => None, // end of input: fall back to defaults
        Ok(_) => Some(s.trim().to_string()),
    }
}

pub fn ask(question: &str, default: &str) -> String {
    if default.is_empty() {
        print!("{question}: ");
    } else {
        print!("{question} [{default}]: ");
    }
    let _ = io::stdout().flush();
    match read_line() {
        Some(s) if !s.is_empty() => s,
        _ => default.to_string(),
    }
}

pub fn yes_no(question: &str, default: bool) -> bool {
    loop {
        print!("{question} [{}]: ", if default { "Y/n" } else { "y/N" });
        let _ = io::stdout().flush();
        match read_line().map(|s| s.to_ascii_lowercase()) {
            None => return default,
            Some(s) if s.is_empty() => return default,
            Some(s) if s == "y" || s == "yes" => return true,
            Some(s) if s == "n" || s == "no" => return false,
            _ => println!("  Please answer y or n."),
        }
    }
}

/// Keeps a double-clicked window open until Enter is pressed.
pub fn pause() {
    print!("\nPress Enter to close this window...");
    let _ = io::stdout().flush();
    let _ = read_line();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_config_parses() {
        let repos = vec!["BurntSushi/ripgrep".to_string()];
        let text = build_config(&Answers {
            archive_root: r"\\nas\eric's share",
            mirrors: &[r"D:\Archive".to_string()],
            starred_user: Some(None),
            remove_unstarred: true,
            repos: &repos,
            accounts: &["https://git.magicbean.dev/MagicBeanDev".to_string()],
            releases: 2,
            prune: true,
        });
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(cfg.archive_root, PathBuf::from(r"\\nas\eric's share"));
        assert_eq!(cfg.mirrors, [PathBuf::from(r"D:\Archive")]);
        assert_eq!(cfg.defaults.keep_versions, Some(2));
        assert!(cfg.starred_enabled().unwrap().user.is_none());
        assert!(cfg.starred_enabled().unwrap().remove_unstarred);
        assert_eq!(cfg.repos[0].repo, "BurntSushi/ripgrep");
        assert_eq!(cfg.accounts[0].url, "https://git.magicbean.dev/MagicBeanDev");

        let text = build_config(&Answers {
            archive_root: "/mnt/x",
            mirrors: &[],
            starred_user: Some(Some("someone")),
            remove_unstarred: false,
            repos: &[],
            accounts: &[],
            releases: 1,
            prune: false,
        });
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(cfg.starred_enabled().unwrap().user.as_deref(), Some("someone"));
        assert!(cfg.defaults.exclude.is_empty() && cfg.defaults.keep_versions.is_none());
    }
}
