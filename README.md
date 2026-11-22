# Git-Grab

Checks projects on GitHub, GitLab, Gitea, Forgejo and Codeberg for new
releases, including everything you've starred on GitHub or every project of a
user or organization, and archives every file from each release on a network
share: one folder per site, then one per project and one per version.

```
\\nas\software\
├── GitHub\
│   ├── ripgrep\
│   │   ├── README.md
│   │   ├── 14.1.0\
│   │   │   ├── ripgrep-14.1.0-x86_64-pc-windows-msvc.zip
│   │   │   ├── ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz
│   │   │   ├── ripgrep-14.1.0-aarch64-apple-darwin.tar.gz
│   │   │   └── ripgrep_14.1.0-1_amd64.deb
│   │   └── 14.0.3\
│   └── KeePassXC\
│       └── 2.7.9\
├── GitLab\                  (gitlab.com)
│   └── inkscape\1.4\
├── Codeberg\                (codeberg.org)
│   └── forgejo\...
└── git.magicbean.dev\       (each Gitea/Forgejo or self-hosted GitLab site)
    └── tool\
        ├── README.md
        └── v1.0\
```

The site folders are `GitHub`, `GitLab` (gitlab.com) and `Codeberg`. Any other
site uses its host name, such as `git.magicbean.dev`. To use a different name,
set `folder` in a `[[site]]` entry, for example `folder = "MagicBean"`.

Version folders use the release's tag name exactly as the project wrote it
(`v1.2.0`, `1.2.0`, `release-2024.05`, …), with characters Windows doesn't allow
replaced by `_`.

It's a single program for Windows and Linux.

## Features

- **Not just GitHub.** Projects on GitLab (gitlab.com or self-hosted), Gitea,
  Forgejo (Codeberg, or a site like git.magicbean.dev), and GitHub Enterprise.
  The kind of site is detected automatically.
- **Whole accounts.** Follow every project of a user or organization, such as
  `https://git.magicbean.dev/MagicBeanDev`, so new projects are picked up as
  they appear.
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
- **Prereleases when there's nothing else.** Prereleases (betas, nightlies) are
  skipped by default. A project that only publishes prereleases gets its newest
  ones archived instead of nothing (`prereleases_if_no_release`, on by default).
  A project with no releases at all gets no folder.
- **Mirrors.** Keep the archive in two (or more) places at once, such as a
  local drive plus a network share. Files are downloaded once and copied to
  each mirror. A mirror that's offline catches up on the next run.
- **README copies.** Each project folder gets a copy of the project's README, so
  you can tell what a project is. It's refreshed when the README changes.
  Turn it off with `save_readme = false`, globally or per repo.
- **Retention** (`keep_versions`) only ever deletes folders this tool created.
- **Portable.** Settings, logs, cache and logins all live in the program's
  folder. Nothing goes in AppData or the registry.

## Quick start (Windows)

1. Download `git-grab-<version>-windows-x64.zip` from
   [Releases](https://github.com/murderbymodem/Git-Grab/releases).
2. **Extract it to a permanent folder**, not Downloads. For example
   `C:\Tools\Git-Grab\`. The program is portable: it keeps its
   settings in that folder, and the hourly check runs it from there.
3. **Double-click `git-grab.exe`.** A guided setup asks for:
   - where to save releases (for example `D:\Software`), plus an optional mirror
     such as `\\nas\software`
   - your GitHub login
   - whether to archive your starred repos, plus any other repos to include
   - how many versions to keep
   - whether to check hourly in the background

   It then creates the scheduled task and offers to run a first check.
4. Double-click it again any time for a menu: check now, edit the config, log in
   again, and so on.

Windows may show a SmartScreen warning the first time, because the program isn't
code-signed. Click **More info → Run anyway**.

You can do the same from a terminal: run `git-grab setup`, or the
individual steps `init`, `login`, `run --dry-run` and `task install`. On Linux,
hourly checks use a systemd timer (see below).

## Portable: everything in one folder

Git-Grab keeps everything it needs (other than the archive itself) in the folder
that holds its config. By default the config is next to the program, so that
means the program's own folder:

| File | What it is |
|---|---|
| `config.toml` | Your settings |
| `git-grab.log` | Log of the scheduled checks (`.log.1` is the previous one) |
| `cache\` | Cached site responses, so unchanged data doesn't use up GitHub's rate limit |
| `token` | Your saved GitHub login |
| `token-<host>` | Saved logins for other sites, e.g. `token-git.magicbean.dev` |

Nothing is written to `%APPDATA%`, the registry, or Windows Credential Manager.
The scheduled task is the only thing outside the folder.

**Saved logins are protected.** On Windows, the token files are encrypted with
Windows' data protection (DPAPI), so only your Windows account on that PC can
read them. On Linux, only their owner can read them. If you copy the folder to
another PC or user account, everything else works, and you just run `login`
there again.

**Moving the folder:** move it, then double-click the program in its new place.
It notices that the hourly check still points at the old location and offers to
update it.

The folder must be writable, so don't put Git-Grab in `C:\Program Files`. To
keep the program and its files apart (for example on a Linux server), use
`-c /path/to/config.toml`. The log, cache and logins then live next to that
config file instead.

## Mirrors

```toml
archive_root = 'D:\Software'          # main archive: downloads go here
mirrors = ['\\nas\software']         # gets a copy of everything
```

Each run copies new or changed files from `archive_root` to every mirror, using
the same `<Site>\<Project>\<version>\` layout. Copies are written to a temporary file
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

`git-grab login` saves a GitHub token in the `token` file next to the config.
It's encrypted for your Windows account on Windows, and readable only by you on
Linux. Logging in lets the tool
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
     or build it in with `GIT_GRAB_OAUTH_CLIENT_ID=... cargo build --release`.

  A client ID is not a secret, and no client secret is used.

The token is looked up in this order: `github_token` in the config, then the
`GITHUB_TOKEN` or `GH_TOKEN` environment variable, then the saved login.
`status` shows which one is in use. `logout` deletes the saved token.

## Other sites: GitLab, Gitea, Forgejo, Codeberg

Anywhere a project goes in the config, you can use `owner/name` for GitHub or
paste a link from any supported site:

```toml
[[repo]]
repo = "BurntSushi/ripgrep"                                # GitHub
[[repo]]
repo = "https://gitlab.com/inkscape/inkscape"              # GitLab (subgroups work)
[[repo]]
repo = "https://codeberg.org/forgejo/forgejo"              # Codeberg (Forgejo)

# Every project of a user or organization, on any of these sites.
[[account]]
url = "https://git.magicbean.dev/MagicBeanDev"
ignore = ["MagicBeanDev/old-*"]   # optional, like [starred]
include_forks = false              # optional
```

Git-Grab finds out what kind of site it is talking to by asking the site.
github.com, gitlab.com and codeberg.org are known. If a site can't be
identified, or you'd rather not wait for the check, say what it is:

```toml
[[site]]
url = "https://git.magicbean.dev"
type = "forgejo"        # "github" (Enterprise), "gitlab", "gitea" or "forgejo"
folder = "MagicBean"    # optional: the site's folder in the archive (default: git.magicbean.dev)
```

**Logging in** isn't needed for public projects. For private ones, run
`git-grab login --site https://git.magicbean.dev` and paste a token. It
shows where to create one: GitLab needs the `read_api` scope, and Gitea/Forgejo
needs read access to repositories. Each site's token is stored separately and
only ever sent to that site. Release files hosted somewhere else get no token.
`git-grab logout --site …` removes it. You can also put `token = "…"` in the
site's `[[site]]` entry.

Differences between sites:
- GitLab doesn't report file sizes for release links, so a file that's already
  archived is recognised by name. GitHub and Gitea also compare sizes.
- Only GitHub provides checksums, which are verified.
- GitLab's "upcoming" releases count as prereleases.
- Starred repos are GitHub only. For other sites, use `[[account]]` or list
  the projects.

## Starred repos

```toml
[starred]
enabled = true
# user = "someone"             # someone else's public stars; leave out for your own
ignore = ["someorg/*"]         # owner/name wildcards to skip
include_forks = true
include_archived = true
remove_unstarred = false       # true: delete a repo's archived files once you unstar it
releases = 1                   # settings for starred repos only
```

The list is fetched on every run, so a newly starred repo is picked up on the
next check. Unstarring a repo stops new downloads. By default, what's already
archived is kept.

**`remove_unstarred = true`** deletes an unstarred repo's folder on the next run,
in the main archive and in every mirror. `run --dry-run` shows what it would
remove first. To be safe:
- Only repos archived *because* they were starred are removed. Anything in a
  `[[repo]]` or `[[account]]` entry stays, even if you also unstar it.
- Nothing is removed in a run where the star list couldn't be fetched, or came
  back completely empty.
- Only what Git-Grab downloaded is deleted: the release folders, README and
  state file. Files you added yourself stay, and so does the folder holding them. A repo that's both starred and listed under `[[repo]]` uses its
`[[repo]]` settings.

If two starred repos have the same name (`alice/tool` and `bob/tool`), the
second one goes in `tool (bob)`. Each project folder records which repo it
belongs to, so the folder names stay the same from run to run.

## Windows: scheduled task

```powershell
git-grab task install                        # hourly, while you're logged on
git-grab task install --every 30             # every 30 minutes
git-grab task install --run-when-logged-off  # all the time (asks for your password)
git-grab task run      # start it now to test
git-grab task status   # last run time and result
git-grab task remove
```

- The task runs `run` with the config file you used when installing it, and
  writes its log to `git-grab.log` next to that config. When the
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

`/etc/systemd/system/git-grab.service`

```ini
[Unit]
Description=Archive releases with Git-Grab
Wants=network-online.target
After=network-online.target remote-fs.target
RequiresMountsFor=/mnt/software

[Service]
Type=oneshot
User=archiver
ExecStart=/usr/local/bin/git-grab -c /var/lib/git-grab/config.toml run
```

`/etc/systemd/system/git-grab.timer`

```ini
[Unit]
Description=Run git-grab hourly

[Timer]
OnBootSec=5min
OnUnitActiveSec=1h
Persistent=true

[Install]
WantedBy=timers.target
```

```bash
sudo install -d -o archiver -m 700 /var/lib/git-grab
sudo -u archiver git-grab -c /var/lib/git-grab/config.toml init --archive-root /mnt/software
sudo -u archiver git-grab -c /var/lib/git-grab/config.toml login
sudo systemctl enable --now git-grab.timer
```

These commands create a folder only the `archiver` user can access, put the
config there, and save the login next to it. The cache lives there too. Logs:
`journalctl -u git-grab`.

## Commands

| Command | What it does |
|---|---|
| `setup` | Guided setup, or a menu if already set up (also what double-clicking runs) |
| `init [--archive-root PATH]` | Create a starter config |
| `login` / `logout [--site URL]` / `status` | Manage logins (GitHub, or another site with `--site`) and show the current setup |
| `run [--dry-run]` | Check everything once |
| `watch` | Keep running, checking every `interval_minutes` and re-reading the config each time |
| `task install/run/status/remove` | Windows scheduled task |

Global options: `-c/--config PATH` (or the `GIT_GRAB_CONFIG`
environment variable) and `--log-file PATH`. Exit codes: `0` success, `1` some
repos or files failed (the rest continued), `2` the run couldn't start.
Set `RUST_LOG=debug` for detailed logs.

Every option is explained in [`config.example.toml`](config.example.toml).

## Building

```bash
cargo build --release
```

Ready-made programs are on the
[Releases](https://github.com/murderbymodem/Git-Grab/releases) page:

- `git-grab-<version>-windows-x64.zip` for 64-bit Windows 10/11
- `git-grab-<version>-linux-x64.tar.gz` for 64-bit Linux. It's a
  single self-contained file that runs on any distro with nothing to install.

Publishing a release on GitHub (with a new `v*` tag) builds and attaches both
automatically. To rebuild an existing release, use **Actions → Release → Run
workflow** (see `.github/workflows/release.yml`).

## License

GPL-3.0. See [LICENSE](LICENSE).
