//! Resumable, retrying download of a single Takeout part.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures_util::StreamExt;
use indicatif::ProgressBar;
use reqwest::header::{
    CONTENT_RANGE, CONTENT_TYPE, ETAG, HeaderMap, IF_RANGE, LAST_MODIFIED, LOCATION, RANGE,
};
use reqwest::{Client, Response, StatusCode, redirect};
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncWriteExt, BufWriter};
use url::Url;

use crate::urls::Part;
use crate::verify;

const LOGIN_HOST: &str = "accounts.google.com";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const MAX_BACKOFF: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error(
        "not authenticated (Google sent a login page). Re-export cookies.txt, and open one \
         download link in your browser first if Google asks you to re-enter your password"
    )]
    Auth,
    #[error("HTTP {0}")]
    Status(StatusCode),
    #[error("transfer ended early: expected {expected} bytes, have {got}")]
    Incomplete { expected: u64, got: u64 },
    #[error("partial file is {got} bytes but the server's file is {expected}; restarting it")]
    SizeMismatch { expected: u64, got: u64 },
    #[error(
        "failed integrity check ({source}); kept as {}. Run again to re-download it",
        path.display()
    )]
    Corrupt {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl DownloadError {
    fn retryable(&self) -> bool {
        match self {
            Self::Auth | Self::Corrupt { .. } | Self::Io(_) => false,
            Self::Status(s) => s.is_server_error() || *s == StatusCode::TOO_MANY_REQUESTS,
            Self::Incomplete { .. } | Self::SizeMismatch { .. } | Self::Http(_) => true,
        }
    }

    /// Whether a saved pre-authorised link was turned down, so the original link should be used.
    fn rejects_link(&self) -> bool {
        match self {
            Self::Auth => true,
            Self::Status(s) => matches!(s.as_u16(), 400 | 401 | 403 | 404 | 410),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Downloaded,
    AlreadyPresent,
}

pub struct Downloader {
    client: Client,
    out_dir: PathBuf,
    retries: u32,
    base_backoff: Duration,
    verify: bool,
}

impl Downloader {
    pub fn new(
        jar: Arc<reqwest::cookie::Jar>,
        out_dir: PathBuf,
        retries: u32,
    ) -> reqwest::Result<Self> {
        // Stop at the login redirect so it surfaces as an auth error rather than an HTML "download".
        let policy = redirect::Policy::custom(|attempt| {
            if attempt.url().host_str() == Some(LOGIN_HOST) {
                attempt.stop()
            } else if attempt.previous().len() > 10 {
                attempt.error("too many redirects")
            } else {
                attempt.follow()
            }
        });
        let client = Client::builder()
            .cookie_provider(jar)
            .redirect(policy)
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(120))
            // Google serves HTTP/2, so parallel parts share one connection. hyper's fixed
            // 2 MiB stream window caps each part at ~window/RTT (~19 MiB/s at 100 ms);
            // adaptive windows grow to the measured bandwidth-delay product instead.
            .http2_adaptive_window(true)
            .build()?;
        Ok(Self {
            client,
            out_dir,
            retries,
            base_backoff: Duration::from_secs(1),
            verify: false,
        })
    }

    /// Shortens retry delays; for tests.
    pub fn with_base_backoff(mut self, base: Duration) -> Self {
        self.base_backoff = base;
        self
    }

    /// Checks each finished gzip part end to end before giving it its final name.
    pub fn with_verify(mut self, verify: bool) -> Self {
        self.verify = verify;
        self
    }

    /// Pre-authorises a part: follows its link through Google's auth redirect now, while the
    /// cookies are fresh, and saves where it lands in `<file>.part.url`. `download` then uses
    /// that link, so a part that waits in the queue for hours doesn't need to auth again.
    pub async fn resolve(&self, part: &Part) -> Result<(), DownloadError> {
        if fs::try_exists(self.out_dir.join(&part.filename)).await? {
            return Ok(());
        }
        // One byte is enough to see the redirects; the body is never read.
        let resp = self
            .client
            .get(part.url.clone())
            .header(RANGE, "bytes=0-0")
            .send()
            .await?;
        check_redirect(&resp)?;
        let status = resp.status();
        if !status.is_success() && status != StatusCode::RANGE_NOT_SATISFIABLE {
            return Err(DownloadError::Status(status));
        }
        if is_html(&resp) {
            return Err(DownloadError::Auth);
        }
        let link = self.link_file(part);
        if resp.url() == &part.url {
            // No redirect: nothing worth saving.
            remove_if_exists(&link).await
        } else {
            tracing::debug!(file = %part.filename, host = ?resp.url().host_str(), "pre-authorised");
            fs::write(&link, resp.url().as_str()).await
        }?;
        Ok(())
    }

    fn link_file(&self, part: &Part) -> PathBuf {
        self.out_dir.join(format!("{}.part.url", part.filename))
    }

    pub async fn download(&self, part: &Part, pb: &ProgressBar) -> Result<Outcome, DownloadError> {
        let dest = self.out_dir.join(&part.filename);
        if fs::try_exists(&dest).await? {
            return Ok(Outcome::AlreadyPresent);
        }
        let tmp = self.out_dir.join(format!("{}.part", part.filename));
        let validator = self
            .out_dir
            .join(format!("{}.part.validator", part.filename));
        let link = self.link_file(part);

        let mut failures = 0;
        loop {
            let before = file_len(&tmp).await?;
            let saved = read_if_exists(&link)
                .await?
                .and_then(|s| Url::parse(&s).ok());
            let url = saved.as_ref().unwrap_or(&part.url);
            match self.attempt(url, &tmp, &validator, pb).await {
                Err(e) if saved.is_some() && e.rejects_link() => {
                    // The pre-authorised link has expired; fall back to the original one.
                    tracing::info!(file = %part.filename, error = %e, "saved link rejected");
                    fs::remove_file(&link).await?;
                }
                Ok(()) => {
                    // On disk before it gets the final name, so a crash can't leave it truncated.
                    sync(&tmp).await?;
                    if self.verify && verify::supported(&part.filename) {
                        self.check(part, &tmp, &validator, pb).await?;
                    }
                    fs::rename(&tmp, &dest).await?;
                    sync(&self.out_dir).await?;
                    remove_if_exists(&validator).await?;
                    remove_if_exists(&link).await?;
                    return Ok(Outcome::Downloaded);
                }
                Err(e) if e.retryable() && failures < self.retries => {
                    // Attempts that made progress don't count against the retry budget.
                    if file_len(&tmp).await? > before {
                        failures = 0;
                    } else {
                        failures += 1;
                    }
                    let delay = self.backoff(failures);
                    tracing::warn!(file = %part.filename, error = %e, ?delay, "retrying");
                    pb.set_message(format!("retry in {}s: {e}", delay.as_secs()));
                    tokio::time::sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Runs the gzip check on a finished `.part`. A corrupt file is moved aside to
    /// `<name>.corrupt` so the next run downloads it from scratch instead of resuming it.
    async fn check(
        &self,
        part: &Part,
        tmp: &Path,
        validator: &Path,
        pb: &ProgressBar,
    ) -> Result<(), DownloadError> {
        pb.set_length(file_len(tmp).await?);
        pb.set_position(0);
        pb.set_message("verifying");
        let (path, bar) = (tmp.to_path_buf(), pb.clone());
        let checked =
            tokio::task::spawn_blocking(move || verify::gzip(&path, |n| bar.set_position(n)))
                .await
                .map_err(std::io::Error::other)?;
        match checked {
            Ok(()) => Ok(()),
            Err(e) if verify::is_corruption(&e) => {
                let bad = self.out_dir.join(format!("{}.corrupt", part.filename));
                fs::rename(tmp, &bad).await?;
                remove_if_exists(validator).await?;
                remove_if_exists(&self.link_file(part)).await?;
                Err(DownloadError::Corrupt {
                    path: bad,
                    source: e,
                })
            }
            Err(e) => Err(e.into()),
        }
    }

    fn backoff(&self, failures: u32) -> Duration {
        let exp = self.base_backoff.saturating_mul(1 << failures.min(6));
        let jitter_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.subsec_millis() as u64 % 1000)
            .unwrap_or(0);
        let jitter = Duration::from_millis(jitter_ms).min(self.base_backoff);
        exp.min(MAX_BACKOFF) + jitter
    }

    async fn attempt(
        &self,
        url: &Url,
        tmp: &Path,
        validator: &Path,
        pb: &ProgressBar,
    ) -> Result<(), DownloadError> {
        let offset = file_len(tmp).await?;
        let mut req = self.client.get(url.clone());
        if offset > 0 {
            req = req.header(RANGE, format!("bytes={offset}-"));
            // Only resume if the file hasn't changed since the .part was started; otherwise the
            // server ignores the Range and sends the whole file, which restarts it below.
            if let Some(v) = read_if_exists(validator).await? {
                req = req.header(IF_RANGE, v);
            }
        }
        let resp = req.send().await?;
        let status = resp.status();
        check_redirect(&resp)?;
        if status == StatusCode::RANGE_NOT_SATISFIABLE && offset > 0 {
            // Nothing left to fetch. That means the .part is complete only if it's exactly the
            // size the server reports (`bytes */<total>`); if it's bigger, start again.
            let total = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok()?.strip_prefix("bytes */")?.parse().ok());
            return match total {
                Some(expected) if expected != offset => {
                    fs::remove_file(tmp).await?;
                    Err(DownloadError::SizeMismatch {
                        expected,
                        got: offset,
                    })
                }
                _ => Ok(()),
            };
        }
        if !status.is_success() {
            return Err(DownloadError::Status(status));
        }
        if is_html(&resp) {
            return Err(DownloadError::Auth);
        }

        // A 206 must resume exactly where we are; anything else restarts the file.
        let range = (status == StatusCode::PARTIAL_CONTENT)
            .then(|| parse_content_range(resp.headers().get(CONTENT_RANGE)?.to_str().ok()?))
            .flatten();
        let (start, total) = match range {
            Some((start, total)) if start == offset => (start, total),
            _ => (0, resp.content_length()),
        };
        if start == 0 {
            // A fresh file: remember which version of it we're downloading, for If-Range.
            match validator_from(resp.headers()) {
                Some(v) => fs::write(validator, v).await?,
                None => remove_if_exists(validator).await?,
            }
        }

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(start > 0)
            .truncate(start == 0)
            .open(tmp)
            .await?;
        let mut writer = BufWriter::with_capacity(1 << 20, file);

        if let Some(total) = total {
            pb.set_length(total);
        }
        pb.set_position(start);
        // The jump to the resume offset isn't transfer speed; don't let it inflate the rate/ETA.
        pb.reset_eta();
        pb.set_message("");

        let mut written = start;
        let mut stream = resp.bytes_stream();
        let result = async {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                writer.write_all(&chunk).await?;
                written += chunk.len() as u64;
                pb.set_position(written);
            }
            Ok::<_, DownloadError>(())
        }
        .await;
        // Keep what arrived so the next attempt can resume from it.
        writer.flush().await?;
        result?;

        match total {
            Some(expected) if written != expected => Err(DownloadError::Incomplete {
                expected,
                got: written,
            }),
            _ => Ok(()),
        }
    }
}

/// A redirect the client stopped at: the login page means the cookies didn't work.
fn check_redirect(resp: &Response) -> Result<(), DownloadError> {
    let status = resp.status();
    if !status.is_redirection() {
        return Ok(());
    }
    let to_login = resp
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|l| l.contains(LOGIN_HOST));
    Err(if to_login {
        DownloadError::Auth
    } else {
        DownloadError::Status(status)
    })
}

/// Google answers with an HTML sign-in page rather than the archive when auth fails.
fn is_html(resp: &Response) -> bool {
    resp.headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("text/html"))
}

async fn file_len(path: &Path) -> std::io::Result<u64> {
    match fs::metadata(path).await {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

async fn read_if_exists(path: &Path) -> std::io::Result<Option<String>> {
    match fs::read_to_string(path).await {
        Ok(s) => Ok(Some(s.trim().to_owned()).filter(|s| !s.is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

async fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path).await {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Flushes a file, or a directory's entries, to disk (`F_FULLFSYNC` on macOS).
async fn sync(path: &Path) -> std::io::Result<()> {
    fs::File::open(path).await?.sync_all().await
}

/// The value to send as `If-Range`: a strong ETag, else Last-Modified (weak ETags aren't allowed).
fn validator_from(headers: &HeaderMap) -> Option<String> {
    let get = |name| headers.get(name)?.to_str().ok().map(str::to_owned);
    get(ETAG)
        .filter(|e| !e.starts_with("W/"))
        .or_else(|| get(LAST_MODIFIED))
}

/// Parses `bytes <start>-<end>/<total>` into (start, total).
fn parse_content_range(value: &str) -> Option<(u64, Option<u64>)> {
    let rest = value.strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let start = range.split_once('-')?.0.parse().ok()?;
    Some((start, total.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range() {
        assert_eq!(
            parse_content_range("bytes 100-199/200"),
            Some((100, Some(200)))
        );
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, None)));
        assert_eq!(parse_content_range("garbage"), None);
    }

    #[test]
    fn if_range_validator() {
        let headers = |pairs: &[(&'static str, &'static str)]| {
            let mut h = HeaderMap::new();
            for (k, v) in pairs {
                h.insert(*k, v.parse().unwrap());
            }
            h
        };
        let lm = "Wed, 28 Oct 2026 22:24:38 GMT";
        assert_eq!(
            validator_from(&headers(&[("etag", "\"abc\""), ("last-modified", lm)])),
            Some("\"abc\"".into())
        );
        assert_eq!(
            validator_from(&headers(&[("etag", "W/\"abc\""), ("last-modified", lm)])),
            Some(lm.into())
        );
        assert_eq!(validator_from(&headers(&[("etag", "W/\"abc\"")])), None);
        assert_eq!(validator_from(&HeaderMap::new()), None);
    }
}
