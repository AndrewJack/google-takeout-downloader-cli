# takeout-dl

Rust CLI that downloads Google Takeout archives (multi-part `.tgz`/`.zip`, up to ~50GB each) with resume, retries and parallel parts.

## Commands
- Rust version: pinned in `rust-toolchain.toml` (rustup picks it up locally and in CI). Change it only there.
- Build: `cargo build --release` (binary: `target/release/takeout-dl`)
- Test: `cargo test` (unit tests in `src/*`, wiremock integration tests in `tests/download.rs`)
- Lint/format: `cargo clippy --all-targets -- -D warnings`, `cargo fmt`
- Run: `takeout-dl -c cookies.txt -p 5 -o ~/Takeout '<url of part 001>'`
- Preview the expanded part URLs: add `--dry-run` (no cookies needed)
- Debug logging: `RUST_LOG=takeout_dl=debug`

## How Takeout downloads work
- Links need auth. Without cookies, Google answers `302` → `accounts.google.com/ServiceLogin`. The client stops at that redirect and reports `DownloadError::Auth` (never retried).
- Auth = browser cookies in Netscape `cookies.txt` format. Export with a "Get cookies.txt LOCALLY"-style extension while logged in on takeout.google.com.
- Google may demand a recent password re-entry. If you get auth errors with fresh cookies, click one download link in the browser first, then re-export.
- Links expire (~7 days after the export is created).
- Login is checked only when a request starts; a running download keeps going after the cookies lapse. `takeout-download.usercontent.google.com` links serve the file directly (no redirect, no Set-Cookie), so there's no auth-free URL to save.
- Queue: every part starts at once (`download::Queue`, tickets in part order). The first `-j` unfinished parts download at full speed; the rest trickle at `--trickle-rate` KiB/s until they get a slot. Each part has its own `Client`/connection, because slow streams sharing an HTTP/2 connection would hold its flow-control window and stall the fast ones. `--trickle-rate 0` = old behaviour (`buffer_unordered(jobs)`).
- Part URL pattern: the file name ends `-NNN.tgz` and the query has `i=N-1`. `urls::expand` generates parts from any starting part.

## Module map
- `src/main.rs`: clap args, URL collection/dedupe, concurrency (`buffer_unordered` + `Queue`), progress bars, summary/exit code
- `src/cookies.rs`: cookies.txt parser → `reqwest::cookie::Jar` (the jar handles domain matching across redirects)
- `src/urls.rs`: `Part { url, filename }`, part expansion
- `src/download.rs`: `Downloader`. Writes to `<file>.part` and resumes with `Range` plus `If-Range` (the first response's strong ETag or Last-Modified, saved in `<file>.part.validator`). 206 appends, 200 restarts, 416 means complete only if `Content-Range: bytes */<total>` matches the `.part` size (otherwise it restarts). Checks the size against `Content-Range`/`Content-Length`, fsyncs, optionally runs `verify::gzip` (`--verify`; a failure moves the file to `<file>.corrupt`), then renames to the final name.
- `src/verify.rs`: end-to-end gzip check (full decompress → CRC-32 + length). Backoff retries for network errors, 5xx and 429; attempts that made progress don't use up the retry budget.

## Git workflow
- Claude never commits or pushes directly to `main`; all Claude changes reach `main` through a PR. (The user may commit to main directly. These rules and hooks apply only to Claude.)
- Start from an up-to-date main: `git switch main && git pull`, then `git switch -c <type>/<short-desc>` (e.g. `feat/zip-support`, `fix/resume-416`).
- Commit on the branch, `git push -u origin <branch>`, then `gh pr create`.
- PR checks (`.github/workflows/pr.yml`): fmt, clippy and tests, all on macOS (Apple Silicon is the only supported platform). Run `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test` before pushing.
- Merging is the user's call: don't merge PRs unless asked.
- Enforcement:
  - GitHub ruleset "protect main": PRs only, required checks (`checks / fmt + clippy`, `checks / test (macos-latest)`), no force-push or deletion. Repo admins bypass it, so the user can still push to main, as does the `HOMEBREW_DEPLOY_KEY` deploy key (used only by the release workflow's Homebrew formula commit); Claude's pushes to main are stopped by the local layers below.
  - `.githooks/pre-commit` and `.githooks/pre-push` are the real guard. They only act when run from Claude Code: `CLAUDECODE=1` is set, or a `claude` process is an ancestor (`.githooks/is-claude.sh`; this still catches `env -i`). They do nothing in the user's own terminal. git hands `pre-push` the resolved destination refs, so aliases, `push.default` and upstream config can't get around it. Enable per clone with `git config core.hooksPath .githooks`; a SessionStart hook in `.claude/settings.json` does this automatically.
  - `.claude/hooks/protect-main.sh` (PreToolUse on Bash) is a best-effort early warning: it parses the command text, catches commit/push to main before they run, and blocks attempts to skip the git hooks (`--no-verify`, `commit -n`, overriding `core.hooksPath`, touching `CLAUDECODE`).
  - Never bypass either layer. If a hook blocks legitimate work, ask the user.
- Releases: bump `version` in `Cargo.toml` via PR. Merging it runs `.github/workflows/release.yml`, which builds `aarch64-apple-darwin` (the only release target) and creates the `vX.Y.Z` tag and release with generated notes (only if that release doesn't exist yet). Don't create tags or releases by hand. Re-run the workflow from the Actions tab to retry a failed release.
- Homebrew: `Formula/takeout-dl.rb` (this repo is the tap). After publishing, the release workflow commits the new `url`/`sha256` itself, pushing with the `HOMEBREW_DEPLOY_KEY` deploy key (a secret in the main-only `homebrew` environment, and a ruleset bypass actor; personal repos can't make GitHub Actions one). Don't edit the url/sha256 by hand.

## Conventions
- `thiserror` enums in library modules, `anyhow` only in `main.rs`.
- Never log or print cookie values. `cookies*.txt` and `*.part` are gitignored.
- A final file existing means it is complete (it is only created by renaming a verified `.part`). Keep that invariant.
- New download behaviour needs a wiremock test in `tests/download.rs`.
