//! Takeout part URL handling: `...-001.tgz?...&i=0` → `...-00N.tgz?...&i=N-1`.

use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum UrlError {
    #[error("invalid URL {0}: {1}")]
    Invalid(String, url::ParseError),
    #[error("URL has no file name: {0}")]
    NoFilename(String),
    #[error("cannot expand parts: file name {0} does not end in -NNN.<ext>")]
    NoPartNumber(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub url: Url,
    pub filename: String,
}

impl Part {
    pub fn from_url(url: Url) -> Result<Self, UrlError> {
        let filename = url
            .path_segments()
            .and_then(|mut s| s.next_back())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| UrlError::NoFilename(url.to_string()))?
            .to_string();
        Ok(Self { url, filename })
    }
}

/// Splits `takeout-...-001.tgz` into (`takeout-...-`, 3, `.tgz`).
fn split_part_number(filename: &str) -> Option<(&str, usize, &str)> {
    let dot = filename.find('.')?;
    let (stem, ext) = filename.split_at(dot);
    let dash = stem.rfind('-')?;
    let digits = &stem[dash + 1..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((&stem[..=dash], digits.len(), ext))
}

/// Returns the given part plus the following parts up to `parts` in total, numbering from
/// the part number in the URL's file name.
pub fn expand(raw: &str, parts: Option<u32>) -> Result<Vec<Part>, UrlError> {
    let url = Url::parse(raw.trim()).map_err(|e| UrlError::Invalid(raw.to_string(), e))?;
    let first = Part::from_url(url)?;
    let Some(parts) = parts.filter(|&n| n > 1) else {
        return Ok(vec![first]);
    };

    let (prefix, width, ext) = split_part_number(&first.filename)
        .ok_or_else(|| UrlError::NoPartNumber(first.filename.clone()))?;
    let start: u32 = first.filename[prefix.len()..prefix.len() + width]
        .parse()
        .expect("digits checked");

    let mut out = Vec::with_capacity(parts as usize);
    for n in start..start + parts {
        let filename = format!("{prefix}{n:0width$}{ext}");
        let mut url = first.url.clone();
        url.path_segments_mut()
            .expect("http URL has path")
            .pop()
            .push(&filename);
        let pairs: Vec<(String, String)> = first
            .url
            .query_pairs()
            .map(|(k, v)| {
                let v = if k == "i" {
                    (n - 1).to_string()
                } else {
                    v.into_owned()
                };
                (k.into_owned(), v)
            })
            .collect();
        url.query_pairs_mut().clear().extend_pairs(pairs);
        out.push(Part { url, filename });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://takeout-download.usercontent.google.com/download/takeout-20261028T223002Z-1-001.tgz?j=0a1b2c3d&i=0&user=123456789012&authuser=0";

    #[test]
    fn single_part_unchanged() {
        let parts = expand(URL, None).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].filename, "takeout-20261028T223002Z-1-001.tgz");
        assert_eq!(parts[0].url.as_str(), URL);
    }

    #[test]
    fn expands_filenames_and_index() {
        let parts = expand(URL, Some(3)).unwrap();
        let names: Vec<_> = parts.iter().map(|p| p.filename.as_str()).collect();
        assert_eq!(
            names,
            [
                "takeout-20261028T223002Z-1-001.tgz",
                "takeout-20261028T223002Z-1-002.tgz",
                "takeout-20261028T223002Z-1-003.tgz",
            ]
        );
        assert_eq!(
            parts[2].url.as_str(),
            "https://takeout-download.usercontent.google.com/download/takeout-20261028T223002Z-1-003.tgz?j=0a1b2c3d&i=2&user=123456789012&authuser=0"
        );
    }

    #[test]
    fn expands_from_later_part() {
        let url = URL.replace("-001.tgz", "-010.tgz").replace("i=0", "i=9");
        let parts = expand(&url, Some(2)).unwrap();
        assert_eq!(parts[1].filename, "takeout-20261028T223002Z-1-011.tgz");
        assert!(parts[1].url.as_str().contains("&i=10&"));
    }

    #[test]
    fn rejects_expansion_without_part_number() {
        let err = expand("https://example.com/file.tgz", Some(2)).unwrap_err();
        assert!(matches!(err, UrlError::NoPartNumber(_)));
    }
}
