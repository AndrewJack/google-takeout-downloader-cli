//! End-to-end integrity check of a downloaded archive.
//!
//! Takeout publishes no checksums, but gzip ends each member with a CRC-32 and length of its
//! contents, so decompressing the whole file proves it is intact (including across resumes).

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

use flate2::bufread::MultiGzDecoder;

/// Whether [`gzip`] can check a file with this name (`.zip` exports are not checked).
pub fn supported(filename: &str) -> bool {
    filename.ends_with(".tgz") || filename.ends_with(".gz")
}

/// Decompresses the whole file, failing on a bad header, CRC or length.
/// `progress` receives the number of compressed bytes read so far.
pub fn gzip(path: &Path, progress: impl Fn(u64)) -> io::Result<()> {
    let counting = Counting {
        inner: File::open(path)?,
        read: 0,
        progress,
    };
    let mut decoder = MultiGzDecoder::new(BufReader::with_capacity(1 << 20, counting));
    io::copy(&mut decoder, &mut io::sink())?;
    Ok(())
}

/// Whether a [`gzip`] error means the data is bad, rather than that reading the file failed.
pub fn is_corruption(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput | io::ErrorKind::UnexpectedEof
    )
}

struct Counting<R, F> {
    inner: R,
    read: u64,
    progress: F,
}

impl<R: Read, F: Fn(u64)> Read for Counting<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        (self.progress)(self.read);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    fn gz(data: &[u8]) -> Vec<u8> {
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn check(bytes: &[u8]) -> io::Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.tgz");
        std::fs::write(&path, bytes).unwrap();
        gzip(&path, |_| {})
    }

    #[test]
    fn accepts_valid_and_multi_member() {
        let data = vec![7u8; 100_000];
        check(&gz(&data)).unwrap();
        check(&[gz(b"one"), gz(b"two")].concat()).unwrap();
    }

    #[test]
    fn rejects_corrupt_truncated_and_non_gzip() {
        let good = gz(&(0..100_000u32)
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>());

        let mut flipped = good.clone();
        let mid = flipped.len() / 2;
        flipped[mid] ^= 0xff;
        let truncated = &good[..good.len() - 4];

        for bad in [&flipped[..], truncated, b"0123456789"] {
            let e = check(bad).unwrap_err();
            assert!(is_corruption(&e), "{e:?}");
        }
    }

    #[test]
    fn supported_extensions() {
        assert!(supported("takeout-1-001.tgz"));
        assert!(!supported("takeout-1-001.zip"));
    }
}
