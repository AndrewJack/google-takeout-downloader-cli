# takeout-dl

A command-line downloader for [Google Takeout](https://takeout.google.com) archives.

Takeout exports are split into many large parts (up to 50GB each), and browser downloads of them often fail partway through. `takeout-dl` downloads every part from a single link, resumes interrupted downloads, retries on errors, and fetches several parts at once.

## Features

- **Resume**: partial downloads are kept as `<name>.part` and continue where they stopped, including after Ctrl-C. If Google's copy of the file has changed since the `.part` was started, it restarts instead of joining two different files.
- **Retries**: network errors, 5xx and 429 responses are retried with exponential backoff.
- **Parallel**: downloads several parts at full speed at once (`-j`).
- **No lapsed logins for queued parts**: Google only checks your login when a download starts. So every part starts straight away, and parts waiting for their turn download slowly (`--trickle-rate`, 64 KiB/s each by default) until a full-speed slot frees up. That way a part that waits hours in the queue doesn't fail because the cookies stopped working.
- **Part expansion**: give the link to part 001 and `-p N`, and it works out the links for parts 002…N. Use `--from`/`--to` to download only some of them.
- **Safe output**: a file only gets its final name once its size has been checked and it has been flushed to disk. Parts that already exist are skipped.
- **Integrity check** (`--verify`): decompresses each `.tgz` part to check gzip's built-in checksum before giving it its final name.
- **Clear auth errors**: if Google sends a login page, it says so instead of saving HTML as a `.tgz`.

## Install

`takeout-dl` supports macOS on Apple Silicon. It may build elsewhere from source, but other platforms aren't tested.

### Homebrew (macOS, Apple Silicon)

```sh
brew tap andrewjack/takeout-dl https://github.com/AndrewJack/google-takeout-downloader-cli
brew install takeout-dl
```

Upgrade later with `brew update && brew upgrade takeout-dl`.

### From source

Requires a Rust toolchain ([rustup.rs](https://rustup.rs)). The repo pins the Rust version in `rust-toolchain.toml`, and rustup installs it on first build.

```sh
git clone https://github.com/AndrewJack/google-takeout-downloader-cli.git
cd google-takeout-downloader-cli
cargo install --path .
```

This installs the `takeout-dl` binary into `~/.cargo/bin`. To update, `git pull` and run `cargo install --path .` again.

Check it works with `takeout-dl --version`.

## Usage

### 1. Export your Google cookies

Takeout download links only work when you're logged in, so `takeout-dl` uses your browser's cookies.

1. Install a cookie export extension that writes the Netscape `cookies.txt` format, for example *Get cookies.txt LOCALLY* (Chrome/Firefox).
2. While logged in, open [takeout.google.com](https://takeout.google.com) → **Manage exports**.
3. Use the extension to export cookies for the current site, and save them as `cookies.txt`.

> `cookies.txt` gives access to your Google account. Keep it private and delete it when you're done.

### 2. Get the download link

On the **Manage exports** page, right-click the **Download** button for part 1 and copy the link address. It looks like:

```
https://takeout-download.usercontent.google.com/download/takeout-20261028T223002Z-1-001.tgz?j=...&i=0&user=...&authuser=0
```

Note how many parts the export has (shown on the same page).

### 3. Download

Always quote the URL: it contains `&`.

```sh
# Preview the part links that will be downloaded (no cookies needed)
takeout-dl --dry-run -p 5 'https://takeout-download.usercontent.google.com/download/takeout-...-001.tgz?...'

# Download all 5 parts into ~/Takeout, checking each one
takeout-dl -c cookies.txt -p 5 -o ~/Takeout --verify 'https://takeout-download.usercontent.google.com/download/takeout-...-001.tgz?...'
```

If the download is interrupted, run the same command again. Finished parts are skipped and partial ones continue where they stopped.

### More examples

```sh
# Download parts 6-10 only (any part's link works, e.g. part 001)
takeout-dl -c cookies.txt --from 6 --to 10 'https://...-001.tgz?...'

# Parts 6 to the end of a 20-part export
takeout-dl -c cookies.txt -p 20 --from 6 'https://...-001.tgz?...'

# Read links from a file (one per line, # comments allowed)
takeout-dl -c cookies.txt --url-file links.txt

# More parallel downloads and more retries
takeout-dl -c cookies.txt -p 20 -j 4 --retries 10 '<url>'

# Verbose logging
RUST_LOG=takeout_dl=debug takeout-dl -c cookies.txt '<url>'
```

## Options

| Option | Default | Description |
| --- | --- | --- |
| `[URLS]...` | | One or more Takeout download links |
| `--url-file <FILE>` | | File with one link per line |
| `-p, --parts <N>` | | Total parts to download, counting from each given link's part number |
| `--from <N>` | link's part | First part number to download |
| `--to <N>` | | Last part number to download (inclusive). Can't be combined with `-p` |
| `-c, --cookies <FILE>` | required | Netscape-format `cookies.txt` |
| `-o, --out-dir <DIR>` | `.` | Where to save the archives |
| `-j, --jobs <N>` | `2` | Parts to download at full speed at the same time |
| `--trickle-rate <KIB/S>` | `64` | Speed of each part waiting for its turn. `0` makes waiting parts not start until their turn, which can fail if the login lapses in the meantime |
| `--retries <N>` | `5` | Retries per part. Attempts that made progress don't count against this |
| `--verify` | | Check each `.tgz` part end to end (gzip CRC) after it downloads. Adds up to a few minutes per 50 GB part. `.zip` parts aren't checked |
| `--dry-run` | | Print the expanded part links and exit |

`takeout-dl` exits non-zero if any part failed. Run it again to retry only the failed parts.

## Troubleshooting

**`not authenticated (Google sent a login page)`**
- Cookies are missing or expired: export `cookies.txt` again.
- Google may require a recent password check before downloads. Click one **Download** button in your browser, enter your password if asked, cancel the download, then export the cookies again.
- If the first parts download and later ones fail, the login lapsed before those parts started. Don't use `--trickle-rate 0`, and run again with fresh cookies.
- Make sure the cookies come from the same Google account as the export (the `authuser`/`user` values in the link).

**`failed integrity check (...); kept as <name>.corrupt`**: the downloaded part is damaged. Run the same command again to download it from scratch, then delete the `.corrupt` file.

**Checking parts you already downloaded**: `--verify` only checks parts as they finish. To check existing files, run `gzip -t <file>.tgz` (or `unzip -tq <file>.zip`).

**`HTTP 404` / `HTTP 410`**: the download links expire, usually 7 days after the export was created. Create a new export.

**Downloads are slow or throttled**: lower `-j`. Google may limit how many downloads one account can run at once.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
```

See [CLAUDE.md](CLAUDE.md) for architecture notes.

## License

[MIT](LICENSE)
