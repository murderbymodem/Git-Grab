# github-release-archiver

Rust CLI (Windows + Linux) that archives GitHub release assets into
`<archive_root>/<Project>/<OS>/<tag>/<file>`.

## Commits

- Author every commit as the repo owner: `git config user.name "murderbymodem"` and
  `git config user.email "admin@murderbymodem.net"` (set them in the clone before committing).
- End every commit message with Claude's co-author trailer, so both accounts are credited:
  `Co-Authored-By: Claude <noreply@anthropic.com>` (keep the model name if the session supplies one).
- When squash-merging a PR, keep the `Co-Authored-By` trailer in the final message.

## Commands

- Build: `cargo build`
- Test: `cargo test`
- Lint: `cargo clippy --all-targets -- -D warnings` (CI fails on warnings)
- Windows check from Linux: `cargo clippy --all-targets --target x86_64-pc-windows-gnu`
  (needs `rustup target add x86_64-pc-windows-gnu` and the `gcc-mingw-w64-x86-64` package)

## Layout

- `src/main.rs`: CLI (clap) and command handlers
- `src/config.rs`: TOML config, option merging, unknown-key checks
- `src/targets.rs`: builds the repo list from `[[repo]]` plus starred repos
- `src/archive.rs`: download and sort logic, per-project state file, retention
- `src/github.rs`: REST client with ETag cache, starred list, downloads
- `src/classify.rs`: OS folder detection from file names
- `src/auth.rs`: login (token paste or OAuth device flow), token storage
- `src/wintask.rs`: Windows Task Scheduler XML and `schtasks.exe` calls
- `config.example.toml`: documented example config, also used by `init` and by tests

## Conventions

- Every config key must be added to the key lists in `config.rs` (`TOP_KEYS` etc.),
  or it will be rejected as unknown, and documented in `config.example.toml`.
- Never delete files under `archive_root` that the state file doesn't record.
- Keep platform-specific code behind `cfg(windows)` / `cfg(not(windows))`; the rest must
  compile and be unit-testable on Linux.
