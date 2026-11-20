# github-release-archiver

Checks GitHub repositories, including everything you've starred, for new
releases and archives their files on a network share, sorted by project and
operating system:

```
\\nas\software\
├── ripgrep\
│   ├── Windows\14.1.0\ripgrep-14.1.0-x86_64-pc-windows-msvc.zip
│   ├── Linux\14.1.0\ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz
│   ├── Linux\14.1.0\ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz.sha256
│   ├── Linux\14.1.0\ripgrep_14.1.0-1_amd64.deb
│   └── macOS\14.1.0\ripgrep-14.1.0-aarch64-apple-darwin.tar.gz
└── KeePassXC\
    ├── Windows\2.7.9\KeePassXC-2.7.9-Win64.msi
    └── Linux\2.7.9\KeePassXC-2.7.9-x86_64.AppImage
```

It's a single program for Windows and Linux.

## Features

- **Starred repos.** Log in once, and every repo you star on GitHub is
  archived automatically. You can also follow another account's public stars.
- **Hourly checks on Windows.** `task install` sets up a Task Scheduler job.
  On Linux, use a systemd timer or cron.
- **Sorted by OS** from file names: `Windows`, `Linux`, `macOS`, `Android`,
  `iOS`, `FreeBSD`, `OpenBSD`, `NetBSD`, `Other`. Checksum and signature files
  go in the same folder as the file they describe. You can add your own rules.
- **Light on GitHub's rate limit.** Responses are cached, and unchanged data
  comes back as "304 Not Modified", which GitHub doesn't count. Hourly checks
  of hundreds of starred repos stay well within the limit.
- **Safe downloads.** Each file is written to `name.part`, then its size and
  SHA-256 checksum are checked, and only then is it renamed. Files already in
  the archive are skipped.
- **Filters:** number of releases, prereleases on or off, include/exclude
  wildcards, `only_os`, optional source zips, and per-repo settings.
- **Retention** (`keep_versions`) only ever deletes folders this tool created.

## Quick start (Windows)

```powershell
# 1. Create a config file in %APPDATA%\github-release-archiver\config.toml
github-release-archiver init --archive-root '\\nas\software'

# 2. Log in to GitHub (needed for your starred repos)
github-release-archiver login

# 3. Edit the config: set [starred] enabled = true and/or add [[repo]] entries
notepad $env:APPDATA\github-release-archiver\config.toml

# 4. Check what would be downloaded
github-release-archiver run --dry-run

# 5. Check hourly from now on
github-release-archiver task install
```

Linux is the same, except the config goes in
`~/.config/github-release-archiver/config.toml` and step 5 uses a systemd
timer (see below).

## Logging in

`github-release-archiver login` saves a GitHub token: in **Windows Credential
Manager** on Windows, and in a file only you can read
(`~/.config/github-release-archiver/token`) on Linux. Logging in lets the tool
read your starred repos and raises GitHub's limit from 60 to 5,000 requests
per hour.

There are two ways to log in:

- **Paste a token** (the default). Create a
  [fine-grained token](https://github.com/settings/personal-access-tokens/new)
  with *Account permissions → Starring: Read-only*, which is all it needs, then
  paste it when asked. To script it: `... login --with-token < token.txt`.
- **Browser login.** `login` shows a code that you approve on github.com.
  This needs a GitHub OAuth App:
  1. Go to [github.com/settings/applications/new](https://github.com/settings/applications/new).
  2. Enter any name and homepage. The callback URL is not used: enter the homepage again.
  3. Register it, then tick **Enable Device Flow**.
  4. Put the app's **Client ID** in the config (`oauth_client_id = "Ov23li..."`),
     or build it in with `GRA_OAUTH_CLIENT_ID=... cargo build --release`.

  A client ID is not a secret, and no client secret is used.

The token is looked up in this order: `github_token` in the config, then the
`GITHUB_TOKEN` or `GH_TOKEN` environment variable, then the saved login.
`status` shows which one is in use. `logout` deletes the saved token.

## Starred repos

```toml
[starred]
enabled = true
# user = "someone"             # someone else's public stars; leave out for your own
ignore = ["someorg/*"]         # owner/name wildcards to skip
include_forks = true
include_archived = true
releases = 1                   # settings for starred repos only
only_os = ["Windows", "Linux"]
```

The list is fetched on every run, so a newly starred repo is picked up on the
next check. Unstarring a repo stops new downloads but keeps what's already
archived. A repo that's both starred and listed under `[[repo]]` uses its
`[[repo]]` settings.

If two starred repos have the same name (`alice/tool` and `bob/tool`), the
second one goes in `tool (bob)`. Each project folder records which repo it
belongs to, so the folder names stay the same from run to run.

## Windows: scheduled task

```powershell
github-release-archiver task install                        # hourly, while you're logged on
github-release-archiver task install --every 30             # every 30 minutes
github-release-archiver task install --run-when-logged-off  # all the time (asks for your password)
github-release-archiver task run      # start it now to test
github-release-archiver task status   # last run time and result
github-release-archiver task remove
```

- The task runs `run` with the config file you used when installing it, and
  writes its log to `github-release-archiver.log` next to that config. When the
  log passes 5 MB, the old one is kept as `.log.1`.
- **By default** it runs only while you're logged on, and no password is
  stored. A console window flashes briefly each time.
- **`--run-when-logged-off`** also runs when nobody is logged on, with no
  window. Windows stores your password with the task so it can reach network
  shares. Use a UNC path (`\\server\share`) for `archive_root`, because mapped
  drive letters don't exist when you're logged off. The account needs the
  *Log on as a batch job* right, which administrators have by default.
- Missed runs (PC asleep or off) happen at the next opportunity. A run never
  overlaps a previous one that's still going.

## Linux: systemd timer

`/etc/systemd/system/github-release-archiver.service`

```ini
[Unit]
Description=Archive GitHub releases
Wants=network-online.target
After=network-online.target remote-fs.target
RequiresMountsFor=/mnt/software

[Service]
Type=oneshot
User=archiver
ExecStart=/usr/local/bin/github-release-archiver -c /etc/github-release-archiver/config.toml run
```

`/etc/systemd/system/github-release-archiver.timer`

```ini
[Unit]
Description=Run github-release-archiver hourly

[Timer]
OnBootSec=5min
OnUnitActiveSec=1h
Persistent=true

[Install]
WantedBy=timers.target
```

```bash
sudo -u archiver github-release-archiver login
sudo systemctl enable --now github-release-archiver.timer
```

The first command saves the token for the `archiver` user. Logs:
`journalctl -u github-release-archiver`.

## Commands

| Command | What it does |
|---|---|
| `init [--archive-root PATH]` | Create a starter config |
| `login` / `logout` / `status` | Manage the GitHub login and show the current setup |
| `run [--dry-run]` | Check everything once |
| `watch` | Keep running, checking every `interval_minutes` and re-reading the config each time |
| `classify NAME…` | Show which OS folder file names would go in |
| `task install/run/status/remove` | Windows scheduled task |

Global options: `-c/--config PATH` (or the `GITHUB_RELEASE_ARCHIVER_CONFIG`
environment variable) and `--log-file PATH`. Exit codes: `0` success, `1` some
repos or files failed (the rest continued), `2` the run couldn't start.
Set `RUST_LOG=debug` for detailed logs.

Every option is explained in [`config.example.toml`](config.example.toml).

## How the OS folder is chosen

1. Your `os_rules`: per-repo rules first, then global ones, in order. The first
   matching wildcard wins.
2. Built-in guesses from words in the file name and its extension, checked in
   this order (order matters, since `aarch64-linux-android` contains "linux"):
   - **Android**: `android`, `.apk`, `.aab`
   - **iOS**: `ios`, `.ipa`
   - **macOS**: `darwin`, `apple`, `macos`, `osx`, `mac`, `.dmg`, `.pkg`
   - **Windows**: `windows`, `win`, `win32`, `win64`, `msvc`, `mingw`, `.exe`, `.msi`, `.msix`
   - **FreeBSD / OpenBSD / NetBSD**
   - **Linux**: `linux*`, `gnu`, `musl`, distro names, `.deb`, `.rpm`, `.AppImage`, `.flatpak`, `.snap`
   - **Other**: anything else

## Building

```bash
cargo build --release
```

Ready-made programs are on the
[Releases](https://github.com/murderbymodem/github-release-archiver/releases) page:

- `github-release-archiver-<version>-windows-x64.zip` for 64-bit Windows 10/11
- `github-release-archiver-<version>-linux-x64.tar.gz` for 64-bit Linux. It's a
  single self-contained file that runs on any distro with nothing to install.

Publishing a release on GitHub (with a new `v*` tag) builds and attaches both
automatically. To rebuild an existing release, use **Actions → Release → Run
workflow** (see `.github/workflows/release.yml`).

## License

GPL-3.0. See [LICENSE](LICENSE).
