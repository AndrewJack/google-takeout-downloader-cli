//! Resumable, retrying download of a single Takeout part.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures_util::StreamExt;
use indicatif::ProgressBar;
use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, LOCATION, RANGE};
use reqwest::{Client, StatusCode, redirect};
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncWriteExt, BufWriter};

use crate::urls::Part;

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
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl DownloadError {
    fn retryable(&self) -> bool {
        match self {
            Self::Auth | Self::Io(_) => false,
            Self::Status(s) => s.is_server_error() || *s == StatusCode::TOO_MANY_REQUESTS,
            Self::Incomplete { .. } | Self::Http(_) => true,
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
        })
    }

    /// Shortens retry delays; for tests.
    pub fn with_base_backoff(mut self, base: Duration) -> Self {
        self.base_backoff = base;
        self
    }

    pub async fn download(&self, part: &Part, pb: &ProgressBar) -> Result<Outcome, DownloadError> {
        let dest = self.out_dir.join(&part.filename);
        if fs::try_exists(&dest).await? {
            return Ok(Outcome::AlreadyPresent);
        }
        let tmp = self.out_dir.join(format!("{}.part", part.filename));

        let mut failures = 0;
        loop {
            let before = file_len(&tmp).await?;
            match self.attempt(part, &tmp, pb).await {
                Ok(()) => {
                    fs::rename(&tmp, &dest).await?;
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
        part: &Part,
        tmp: &Path,
        pb: &ProgressBar,
    ) -> Result<(), DownloadError> {
        let offset = file_len(tmp).await?;
        let mut req = self.client.get(part.url.clone());
        if offset > 0 {
            req = req.header(RANGE, format!("bytes={offset}-"));
        }
        let resp = req.send().await?;
        let status = resp.status();

        if status.is_redirection() {
            let to_login = resp
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|l| l.contains(LOGIN_HOST));
            return Err(if to_login {
                DownloadError::Auth
            } else {
                DownloadError::Status(status)
            });
        }
        if status == StatusCode::RANGE_NOT_SATISFIABLE && offset > 0 {
            // Everything was already fetched before a previous run was interrupted.
            return Ok(());
        }
        if !status.is_success() {
            return Err(DownloadError::Status(status));
        }
        let is_html = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.starts_with("text/html"));
        if is_html {
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

async fn file_len(path: &Path) -> std::io::Result<u64> {
    match fs::metadata(path).await {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
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
}
