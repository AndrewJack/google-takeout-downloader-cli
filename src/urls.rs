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
    #[error("no parts to download: --from {from} is after --to {to}")]
    EmptyRange { from: u32, to: u32 },
    #[error("part numbers start at 1")]
    PartZero,
}

/// Which parts to download, relative to a given part's URL.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Range {
    /// Number of parts, counting from the URL's part number.
    pub parts: Option<u32>,
    /// First part number (default: the URL's).
    pub from: Option<u32>,
    /// Last part number, inclusive. Takes precedence over `parts`.
    pub to: Option<u32>,
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

/// Returns the parts selected by `range`, built from the given part's URL. With no range
/// (or `parts` of 0 or 1) the URL is returned as is, even if it has no part number.
///
/// The last part is `to`, else the URL's part number plus `parts - 1`, else the first part.
pub fn expand(raw: &str, range: &Range) -> Result<Vec<Part>, UrlError> {
    let url = Url::parse(raw.trim()).map_err(|e| UrlError::Invalid(raw.to_string(), e))?;
    let first = Part::from_url(url)?;
    let parts = range.parts.filter(|&n| n > 1);
    if parts.is_none() && range.from.is_none() && range.to.is_none() {
        return Ok(vec![first]);
    }

    let (prefix, width, ext) = split_part_number(&first.filename)
        .ok_or_else(|| UrlError::NoPartNumber(first.filename.clone()))?;
    let own: u32 = first.filename[prefix.len()..prefix.len() + width]
        .parse()
        .expect("digits checked");
    let start = range.from.unwrap_or(own);
    let end = match (range.to, parts) {
        (Some(to), _) => to,
        (None, Some(n)) => own.saturating_add(n - 1),
        (None, None) => start,
    };
    if start == 0 {
        return Err(UrlError::PartZero);
    }
    if end < start {
        return Err(UrlError::EmptyRange {
            from: start,
            to: end,
        });
    }

    let mut out = Vec::with_capacity((end - start + 1) as usize);
    for n in start..=end {
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

    fn parts(n: u32) -> Range {
        Range {
            parts: Some(n),
            ..Range::default()
        }
    }

    fn names(parts: &[Part]) -> Vec<&str> {
        parts.iter().map(|p| p.filename.as_str()).collect()
    }

    const URL: &str = "https://takeout-download.usercontent.google.com/download/takeout-20261028T223002Z-1-001.tgz?j=0a1b2c3d&i=0&user=123456789012&authuser=0";

    #[test]
    fn single_part_unchanged() {
        let parts = expand(URL, &Range::default()).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].filename, "takeout-20261028T223002Z-1-001.tgz");
        assert_eq!(parts[0].url.as_str(), URL);
    }

    #[test]
    fn expands_filenames_and_index() {
        let parts = expand(URL, &parts(3)).unwrap();
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
        let parts = expand(&url, &parts(2)).unwrap();
        assert_eq!(parts[1].filename, "takeout-20261028T223002Z-1-011.tgz");
        assert!(parts[1].url.as_str().contains("&i=10&"));
    }

    #[test]
    fn rejects_expansion_without_part_number() {
        let err = expand("https://example.com/file.tgz", &parts(2)).unwrap_err();
        assert!(matches!(err, UrlError::NoPartNumber(_)));
    }

    #[test]
    fn from_to_selects_range_from_first_part_link() {
        let range = Range {
            from: Some(4),
            to: Some(10),
            ..Range::default()
        };
        let parts = expand(URL, &range).unwrap();
        assert_eq!(parts.len(), 7);
        assert_eq!(parts[0].filename, "takeout-20261028T223002Z-1-004.tgz");
        assert!(parts[0].url.as_str().contains("&i=3&"));
        assert_eq!(parts[6].filename, "takeout-20261028T223002Z-1-010.tgz");
        assert!(parts[6].url.as_str().contains("&i=9&"));
    }

    #[test]
    fn from_with_parts_ends_at_total() {
        let range = Range {
            parts: Some(10),
            from: Some(4),
            to: None,
        };
        let with_to = Range {
            parts: None,
            from: Some(4),
            to: Some(10),
        };
        assert_eq!(expand(URL, &range).unwrap(), expand(URL, &with_to).unwrap());
    }

    #[test]
    fn from_alone_is_one_part() {
        let range = Range {
            from: Some(5),
            ..Range::default()
        };
        let parts = expand(URL, &range).unwrap();
        assert_eq!(names(&parts), ["takeout-20261028T223002Z-1-005.tgz"]);
    }

    #[test]
    fn to_alone_counts_from_link() {
        let range = Range {
            to: Some(3),
            ..Range::default()
        };
        assert_eq!(
            expand(URL, &range).unwrap(),
            expand(URL, &parts(3)).unwrap()
        );
    }

    #[test]
    fn rejects_backwards_range() {
        let range = Range {
            from: Some(5),
            to: Some(4),
            ..Range::default()
        };
        let err = expand(URL, &range).unwrap_err();
        assert!(matches!(err, UrlError::EmptyRange { from: 5, to: 4 }));
    }

    #[test]
    fn rejects_part_zero() {
        let range = Range {
            from: Some(0),
            to: Some(2),
            ..Range::default()
        };
        assert!(matches!(expand(URL, &range), Err(UrlError::PartZero)));
    }

    #[test]
    fn rejects_range_without_part_number() {
        let range = Range {
            from: Some(2),
            ..Range::default()
        };
        let err = expand("https://example.com/file.tgz", &range).unwrap_err();
        assert!(matches!(err, UrlError::NoPartNumber(_)));
    }
}
