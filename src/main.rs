mod archive;
mod auth;
mod classify;
mod config;
mod github;
mod logging;
mod targets;
mod util;
mod wintask;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use config::{Config, APP_DIR};
use log::{error, info, warn};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

/// Archives GitHub release files into <root>/<Project>/<OS>/<tag>/ folders.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Config file [default: ./config.toml if present, else the per-user config folder]
    #[arg(short, long, env = "GITHUB_RELEASE_ARCHIVER_CONFIG", global = true)]
    config: Option<PathBuf>,
    /// Append log output to this file instead of the console
    #[arg(long, global = true)]
    log_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a starter config file
    Init {
        /// Where the archive should go, e.g. \\nas\software or /mnt/software
        #[arg(long)]
        archive_root: Option<String>,
        /// Overwrite an existing config file
        #[arg(long)]
        force: bool,
    },
    /// Log in to GitHub so starred repos can be listed and the rate limit is higher
    Login {
        /// OAuth App client ID for browser login (device flow). Without one, you paste a token
        #[arg(long)]
        client_id: Option<String>,
        /// Read the token from standard input instead of prompting
        #[arg(long)]
        with_token: bool,
    },
    /// Remove the saved GitHub login
    Logout,
    /// Show the config, login and archive status
    Status,
    /// Check every repo once and exit
    Run {
        /// Show what would be downloaded without writing anything
        #[arg(long)]
        dry_run: bool,
        /// Used by the scheduled task: hide the console window
        #[arg(long, hide = true)]
        scheduled: bool,
    },
    /// Keep running, checking every `interval_minutes`
    Watch,
    /// Show which OS folder each file name would be sorted into
    Classify {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Manage the Windows Task Scheduler job
    #[command(subcommand)]
    Task(TaskCmd),
}

#[derive(Subcommand)]
enum TaskCmd {
    /// Create (or replace) a scheduled task that runs a check automatically
    Install {
        /// Minutes between checks
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=44640))]
        every: u32,
        /// Also run while you're logged off (asks for your Windows password, which Windows stores)
        #[arg(long)]
        run_when_logged_off: bool,
        #[arg(long, default_value = wintask::DEFAULT_NAME)]
        name: String,
    },
    /// Delete the scheduled task
    Remove {
        #[arg(long, default_value = wintask::DEFAULT_NAME)]
        name: String,
    },
    /// Show the scheduled task's details and last result
    Status {
        #[arg(long, default_value = wintask::DEFAULT_NAME)]
        name: String,
    },
    /// Start the scheduled task now
    Run {
        #[arg(long, default_value = wintask::DEFAULT_NAME)]
        name: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::Run { scheduled: true, .. } = cli.command {
        wintask::hide_console();
    }
    if let Err(e) = logging::init(cli.log_file.as_deref()) {
        eprintln!("{e:#}");
        return ExitCode::from(2);
    }
    match real_main(cli) {
        Ok(code) => code,
        Err(e) => {
            error!("{e:#}");
            ExitCode::from(2)
        }
    }
}

fn real_main(cli: Cli) -> Result<ExitCode> {
    let config_path = cli.config.clone().unwrap_or_else(config::default_path);
    match cli.command {
        Command::Init { archive_root, force } => init(&config_path, archive_root.as_deref(), force)?,
        Command::Login { client_id, with_token } => login(&config_path, client_id, with_token)?,
        Command::Logout => {
            if auth::delete()? {
                println!("Saved login removed. (The token itself stays valid until you revoke it on GitHub.)");
            } else {
                println!("No saved login.");
            }
        }
        Command::Status => status(&config_path)?,
        Command::Run { dry_run, .. } => {
            let cfg = Config::load(&config_path)?;
            if run_once(&cfg, dry_run)? {
                return Ok(ExitCode::FAILURE);
            }
        }
        Command::Watch => watch(&config_path)?,
        Command::Classify { names } => {
            // The config is optional here; use its os_rules if it loads.
            let rules = Config::load(&config_path).map(|c| c.os_rules).unwrap_or_default();
            for n in names {
                println!("{:<10} {n}", classify::classify(&n, &rules));
            }
        }
        Command::Task(cmd) => task(cmd, &config_path, cli.log_file.as_deref())?,
    }
    Ok(ExitCode::SUCCESS)
}

fn api_url(config_path: &Path) -> String {
    Config::load(config_path).map(|c| c.api_url).unwrap_or_else(|_| "https://api.github.com".into())
}

fn cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join(APP_DIR).join("http"))
}

fn init(path: &Path, archive_root: Option<&str>, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!("{} already exists (use --force to overwrite)", path.display());
    }
    let mut text = config::example().to_string();
    if let Some(root) = archive_root {
        if root.contains('\'') {
            bail!("archive_root can't contain a single quote");
        }
        text = text.replacen("archive_root = '/mnt/software'", &format!("archive_root = '{root}'"), 1);
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)?;
    println!("Created {}", path.display());
    println!("Next: edit it (archive_root, repos, [starred]), then run `login` and `run --dry-run`.");
    Ok(())
}

fn login(config_path: &Path, client_id: Option<String>, with_token: bool) -> Result<()> {
    let cfg = Config::load(config_path).ok();
    let api = cfg.as_ref().map(|c| c.api_url.clone()).unwrap_or_else(|| "https://api.github.com".into());
    let client_id = client_id
        .or_else(|| cfg.as_ref().and_then(|c| c.oauth_client_id.clone()))
        .or_else(|| option_env!("GRA_OAUTH_CLIENT_ID").map(str::to_string))
        .filter(|c| !c.is_empty());

    let token = if with_token {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        line.trim().to_string()
    } else if let Some(id) = client_id {
        auth::device_login(&api, &id)?
    } else {
        auth::prompt_token()?
    };
    if token.is_empty() {
        bail!("no token given");
    }
    let user = github::Client::new(&api, Some(token.clone()), None)?
        .current_user()
        .context("the token didn't work")?;
    let place = auth::save(&token)?;
    println!("Logged in as {}. Token saved in {place}.", user.login);
    Ok(())
}

fn status(config_path: &Path) -> Result<()> {
    println!("Config:       {}", config_path.display());
    let cfg = match Config::load(config_path) {
        Ok(c) => Some(c),
        Err(e) => {
            println!("              {e:#}");
            None
        }
    };
    if let Some(c) = &cfg {
        let ok = if c.archive_root.is_dir() { "reachable" } else { "NOT reachable" };
        println!("Archive root: {} ({ok})", c.archive_root.display());
        println!("Repos:        {} listed", c.repos.len());
        match c.starred_enabled() {
            Some(s) => println!("Starred:      on ({})", s.user.as_deref().unwrap_or("logged-in account")),
            None => println!("Starred:      off"),
        }
    }
    let api = cfg.as_ref().map(|c| c.api_url.clone()).unwrap_or_else(|| api_url(config_path));
    match auth::resolve(cfg.as_ref().and_then(|c| c.github_token.as_deref())) {
        None => println!("GitHub login: none (60 requests/hour; run `login`)"),
        Some((token, source)) => {
            let from = match source {
                auth::Source::Config => "github_token in config".to_string(),
                auth::Source::Env(v) => format!("{v} environment variable"),
                auth::Source::Saved => "saved login".to_string(),
            };
            match github::Client::new(&api, Some(token), None)?.current_user() {
                Ok(u) => println!("GitHub login: {} (from {from})", u.login),
                Err(e) => println!("GitHub login: token from {from} doesn't work: {e:#}"),
            }
        }
    }
    if cfg!(windows) {
        let state = wintask::status(wintask::DEFAULT_NAME).map(|t| {
            t.lines()
                .filter(|l| ["Status:", "Next Run Time:", "Last Run Time:", "Last Result:"].iter().any(|k| l.trim_start().starts_with(k)))
                .map(|l| format!("\n              {}", l.trim()))
                .collect::<String>()
        });
        println!("Scheduled task: {}", state.unwrap_or_else(|_| " not installed (run `task install`)".into()));
    }
    Ok(())
}

/// Returns true if anything failed.
fn run_once(cfg: &Config, dry_run: bool) -> Result<bool> {
    let token = auth::resolve(cfg.github_token.as_deref()).map(|(t, _)| t);
    if token.is_none() {
        warn!("not logged in; limited to 60 GitHub API requests/hour (run `login`)");
    }
    if !dry_run && !cfg.archive_root.is_dir() {
        bail!(
            "archive_root {} does not exist or is not reachable (is the share mounted?)",
            cfg.archive_root.display()
        );
    }
    let client = github::Client::new(&cfg.api_url, token, cache_dir())?;
    let mut s = archive::Summary::default();
    let targets = match targets::build(cfg, &client) {
        Ok(t) => t,
        Err(e) => {
            // Still check the [[repo]] entries if the starred list can't be fetched.
            warn!("cannot list starred repos: {e:#}");
            s.failed += 1;
            explicit_targets(cfg)
        }
    };
    archive::run(&cfg.archive_root, &targets, &client, dry_run, &mut s);
    info!(
        "done: {} {}, {} already archived, {} failed, {} old release(s) pruned, {} transferred",
        s.downloaded,
        if dry_run { "to download" } else { "downloaded" },
        s.skipped,
        s.failed,
        s.pruned,
        archive::human(s.bytes)
    );
    Ok(s.failed > 0)
}

fn explicit_targets(cfg: &Config) -> Vec<archive::Target> {
    cfg.repos
        .iter()
        .map(|r| archive::Target { repo: r.repo.clone(), folder: cfg.project_folder(r), opts: cfg.effective(&r.options, &r.os_rules) })
        .collect()
}

fn watch(path: &Path) -> Result<()> {
    // Fail fast if the config is broken at startup.
    let mut cfg = Config::load(path)?;
    loop {
        if let Err(e) = run_once(&cfg, false) {
            error!("{e:#}");
        }
        let mins = cfg.interval_minutes;
        info!("next check in {mins} minute(s)");
        std::thread::sleep(Duration::from_secs(mins * 60));
        // Pick up config edits without a restart; keep the old config if the new one is broken.
        match Config::load(path) {
            Ok(c) => cfg = c,
            Err(e) => error!("config reload failed, keeping previous config: {e:#}"),
        }
    }
}

fn task(cmd: TaskCmd, config_path: &Path, log_file: Option<&Path>) -> Result<()> {
    wintask::ensure_windows()?;
    match cmd {
        TaskCmd::Install { every, run_when_logged_off, name } => {
            let cfg = Config::load(config_path)?; // refuse to schedule a broken config
            let config_abs = wintask::absolute(config_path)?;
            let workdir = config_abs.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
            let log = match log_file {
                Some(l) => l.to_path_buf(),
                None => workdir.join("github-release-archiver.log"),
            };
            let exe = wintask::absolute(&std::env::current_exe()?)?;
            let user = wintask::current_user()?;
            let args = format!(
                r#"--config "{}" --log-file "{}" run --scheduled"#,
                config_abs.display(),
                wintask::clean_path(&log)
            );
            if auth::resolve(cfg.github_token.as_deref()).is_none() {
                warn!("not logged in: scheduled runs will be limited to 60 API requests/hour. Run `login` first");
            }
            let root = cfg.archive_root.display().to_string();
            if run_when_logged_off && !root.starts_with(r"\\") && !root.to_ascii_uppercase().starts_with("C:") {
                warn!("archive_root is {root}: mapped drive letters don't exist when you're logged off. Use a UNC path like \\\\server\\share");
            }
            let (logon, password) = if run_when_logged_off {
                let pw = rpassword::prompt_password(format!("Windows password for {user}: "))?;
                (wintask::Logon::Password, Some(pw))
            } else {
                (wintask::Logon::Interactive, None)
            };
            let spec = wintask::TaskSpec { name: name.clone(), exe, args, workdir, interval_minutes: every, user, logon };
            wintask::install(&spec, password.as_deref())?;
            println!("Scheduled task \"{name}\" created: checks every {every} minute(s).");
            println!("Log file: {}", log.display());
            println!("Test it now with: github-release-archiver task run");
            if !run_when_logged_off {
                println!("It runs only while you're logged on. Use --run-when-logged-off to run at all times.");
            }
        }
        TaskCmd::Remove { name } => {
            wintask::uninstall(&name)?;
            println!("Scheduled task \"{name}\" removed.");
        }
        TaskCmd::Status { name } => print!("{}", wintask::status(&name)?),
        TaskCmd::Run { name } => {
            wintask::run_now(&name)?;
            println!("Started \"{name}\". Watch the log file for progress.");
        }
    }
    Ok(())
}
