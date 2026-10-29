//! Netscape `cookies.txt` parsing (the format exported by browser extensions).

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::cookie::Jar;
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum CookieError {
    #[error("failed to read cookies file {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("line {line}: expected 7 tab-separated fields")]
    Malformed { line: usize },
    #[error("no usable (unexpired) cookies found")]
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub domain: String,
    pub include_subdomains: bool,
    pub path: String,
    pub secure: bool,
    /// Unix seconds; 0 means a session cookie.
    pub expires: u64,
    pub name: String,
    pub value: String,
}

impl Cookie {
    fn is_expired(&self, now: u64) -> bool {
        self.expires != 0 && self.expires < now
    }
}

/// Parses Netscape cookies.txt contents, dropping expired cookies.
pub fn parse(contents: &str, now: u64) -> Result<Vec<Cookie>, CookieError> {
    let mut cookies = Vec::new();
    for (idx, raw) in contents.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        let line = match line.strip_prefix("#HttpOnly_") {
            Some(rest) => rest,
            None if line.starts_with('#') || line.trim().is_empty() => continue,
            None => line,
        };
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 7 {
            return Err(CookieError::Malformed { line: idx + 1 });
        }
        let cookie = Cookie {
            domain: fields[0].to_string(),
            include_subdomains: fields[1].eq_ignore_ascii_case("TRUE"),
            path: fields[2].to_string(),
            secure: fields[3].eq_ignore_ascii_case("TRUE"),
            expires: fields[4].parse().unwrap_or(0),
            name: fields[5].to_string(),
            value: fields[6].to_string(),
        };
        if !cookie.is_expired(now) {
            cookies.push(cookie);
        }
    }
    if cookies.is_empty() {
        return Err(CookieError::Empty);
    }
    Ok(cookies)
}

/// Builds a reqwest cookie jar so domain matching also applies across redirects.
pub fn into_jar(cookies: &[Cookie]) -> Arc<Jar> {
    let jar = Jar::default();
    for c in cookies {
        let host = c.domain.trim_start_matches('.');
        let Ok(url) = Url::parse(&format!("https://{host}{}", c.path)) else {
            continue;
        };
        let mut header = format!("{}={}; Path={}", c.name, c.value, c.path);
        if c.include_subdomains || c.domain.starts_with('.') {
            header.push_str(&format!("; Domain={host}"));
        }
        if c.secure {
            header.push_str("; Secure");
        }
        jar.add_cookie_str(&header, &url);
    }
    Arc::new(jar)
}

pub fn load(path: &Path) -> Result<Arc<Jar>, CookieError> {
    let contents = std::fs::read_to_string(path).map_err(|source| CookieError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(into_jar(&parse(&contents, now)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::cookie::CookieStore;

    const SAMPLE: &str = "# Netscape HTTP Cookie File\n\
        \n\
        .google.com\tTRUE\t/\tTRUE\t2000000000\tSID\tabc\n\
        #HttpOnly_.google.com\tTRUE\t/\tTRUE\t0\tHSID\tdef\n\
        .google.com\tTRUE\t/\tTRUE\t100\tOLD\texpired\n\
        accounts.google.com\tFALSE\t/\tTRUE\t2000000000\tLSID\tghi\n";

    #[test]
    fn parses_comments_httponly_and_expiry() {
        let cookies = parse(SAMPLE, 1_000_000_000).unwrap();
        let names: Vec<_> = cookies.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["SID", "HSID", "LSID"]);
        assert!(cookies[0].include_subdomains);
        assert_eq!(cookies[1].expires, 0);
    }

    #[test]
    fn rejects_malformed_line() {
        let err = parse("only\tthree\tfields\n", 0).unwrap_err();
        assert!(matches!(err, CookieError::Malformed { line: 1 }));
    }

    #[test]
    fn empty_file_is_error() {
        assert!(matches!(parse("# nothing\n", 0), Err(CookieError::Empty)));
    }

    #[test]
    fn jar_matches_subdomains_but_not_host_only() {
        let jar = into_jar(&parse(SAMPLE, 1_000_000_000).unwrap());
        let url = Url::parse("https://takeout-download.usercontent.google.com/download/x").unwrap();
        let header = jar.cookies(&url).unwrap();
        let header = header.to_str().unwrap();
        assert!(header.contains("SID=abc"));
        assert!(header.contains("HSID=def"));
        assert!(!header.contains("LSID"));
    }
}
