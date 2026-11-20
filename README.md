# github-release-archiver

Checks GitHub repositories, including everything you've starred, for new
releases and archives every file from each release on a network share, one
folder per project and one per version:

```
\\nas\software\
├── ripgrep\
│   ├── README.md
│   ├── 14.1.0\
│   │   ├── ripgrep-14.1.0-x86_64-pc-windows-msvc.zip
│   │   ├── ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz
│   │   ├── ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz.sha256
│   │   ├── ripgrep-14.1.0-aarch64-apple-darwin.tar.gz
│   │   └── ripgrep_14.1.0-1_amd64.deb
│   └── 14.0.3\
│       └── ...
└── KeePassXC\
    ├── README.md
    └── 2.7.9\
        ├── KeePassXC-2.7.9-Win64.msi
        └── KeePassXC-2.7.9-x86_64.AppImage
```

Version folders use the release's tag name exactly as the project wrote it
(`v1.2.0`, `1.2.0`, `release-2024.05`, …), with characters Windows doesn't allow
replaced by `_`.

It's a single program for Windows and Linux.

## Features

- **Starred repos.** Log in once, and every repo you star on GitHub is
  archived automatically. You can also follow another account's public stars.
- **Hourly checks on Windows.** `task install` sets up a Task Scheduler job.
  On Linux, use a systemd timer or cron.
- **Light on GitHub's rate limit.** Responses are cached, and unchanged data
  comes back as "304 Not Modified", which GitHub doesn't count. Hourly checks
  of hundreds of starred repos stay well within the limit.
- **Safe downloads.** Each file is written to `name.part`, then its size and
  SHA-256 checksum are checked, and only then is it renamed. Files already in
  the archive are skipped.
- **Everything by default.** Every file in a release is saved. Optional
  `include`/`exclude` wildcards skip files you don't want, and you can choose how
  many releases to keep, whether to include prereleases, and whether to save
  source zips, globally or per repo.
- **Mirrors.** Keep the archive in two (or more) places at once, such as a
  local drive plus a network share. Files are downloaded once and copied to
  each mirror. A mirror that's offline catches up on the next run.
- **README copies.** Each project folder gets a copy of the repo's README, so you
  can tell what a project is. It's refreshed when the README changes on GitHub.
  Turn it off with `save_readme = false`, globally or per repo.
- **Retention** (`keep_versions`) only ever deletes folders this tool created.

## Quick start (Windows)

1. Download `github-release-archiver-<version>-windows-x64.zip` from
   [Releases](https://github.com/murderbymodem/github-release-archiver/releases) and extract it.
2. **Double-click `github-release-archiver.exe`.** A guided setup asks for:
   - where to save releases (for example `\\nas\software`)
   - your GitHub login
   - whether to archive your starred repos, plus any other repos to include
   - which systems to keep, and how many versions
   - whether to check hourly in the background

   It then copies the program to `%LOCALAPPDATA%\Programs\github-release-archiver\`,
   creates the scheduled task, and offers to run a first check.
3. Double-click it again any time for a menu: check now, edit the config, log in
   again, and so on.

Windows may show a SmartScreen warning the first time, because the program isn't
code-signed. Click **More info → Run anyway**.

You can do the same from a terminal: run `github-release-archiver setup`, or the
individual steps `init`, `login`, `run --dry-run` and `task install`. On Linux,
the config goes in `~/.config/github-release-archiver/config.toml` and hourly
checks use a systemd timer (see below).

## Mirrors

```toml
archive_root = 'D:\Software'          # main archive: downloads go here
mirrors = ['\\nas\software']         # gets a copy of everything
```

Each run copies new or changed files from `archive_root` to every mirror, using
the same `<Project>\<version>\` layout. Copies are written to a temporary file
and renamed when complete. When `keep_versions` removes an old release from the
main archive, it's removed from the mirrors too. Like everywhere else, only
folders this program created are ever deleted. Files you put in a mirror
yourself are left alone.

If a mirror can't be reached (the NAS is off, or a laptop is away from the
network), it's skipped with a warning, and the next run copies whatever it
missed. Adding a mirror to an existing archive copies everything on the next run.

`status` shows whether each mirror is reachable. For the scheduled task with
`--run-when-logged-off`, use UNC paths (`\\server\share`) rather than mapped
drive letters.

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
| `setup` | Guided setup, or a menu if already set up (also what double-clicking runs) |
| `init [--archive-root PATH]` | Create a starter config |
| `login` / `logout` / `status` | Manage the GitHub login and show the current setup |
| `run [--dry-run]` | Check everything once |
| `watch` | Keep running, checking every `interval_minutes` and re-reading the config each time |
| `task install/run/status/remove` | Windows scheduled task |

Global options: `-c/--config PATH` (or the `GITHUB_RELEASE_ARCHIVER_CONFIG`
environment variable) and `--log-file PATH`. Exit codes: `0` success, `1` some
repos or files failed (the rest continued), `2` the run couldn't start.
Set `RUST_LOG=debug` for detailed logs.

Every option is explained in [`config.example.toml`](config.example.toml).

## Upgrading from 0.4 or earlier

Earlier versions sorted files into OS folders (`ripgrep\Windows\14.1.0\`). The
first run of 0.5 moves those files into `ripgrep\14.1.0\` and removes the empty
OS folders, so nothing is downloaded again. It only touches folders it recorded
itself. The old `only_os` and `os_rules` settings are ignored with a warning, and
can be deleted from the config.

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
