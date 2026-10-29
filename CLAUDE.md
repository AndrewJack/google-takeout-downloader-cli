# takeout-dl

Rust CLI that downloads Google Takeout archives (multi-part `.tgz`/`.zip`, up to ~50GB each) with resume, retries and parallel parts.

## Commands
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
- Part URL pattern: the file name ends `-NNN.tgz` and the query has `i=N-1`. `urls::expand` generates parts from any starting part.

## Module map
- `src/main.rs`: clap args, URL collection/dedupe, concurrency (`buffer_unordered`), progress bars, summary/exit code
- `src/cookies.rs`: cookies.txt parser → `reqwest::cookie::Jar` (the jar handles domain matching across redirects)
- `src/urls.rs`: `Part { url, filename }`, part expansion
- `src/download.rs`: `Downloader`. Writes to `<file>.part` and resumes with `Range`. 206 appends, 200 restarts, 416 means already complete. Checks the size against `Content-Range`/`Content-Length`, then renames to the final name. Backoff retries for network errors, 5xx and 429; attempts that made progress don't use up the retry budget.

## Conventions
- `thiserror` enums in library modules, `anyhow` only in `main.rs`.
- Never log or print cookie values. `cookies*.txt` and `*.part` are gitignored.
- A final file existing means it is complete (it is only created by renaming a verified `.part`). Keep that invariant.
- New download behaviour needs a wiremock test in `tests/download.rs`.
