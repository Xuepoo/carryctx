//! Checksum helpers for ctxpack payloads.
//!
//! The v1 `manifest.json` has no checksum field (see `manifest.rs`
//! fail-closed notes). This module provides the pure SHA-256 helpers that
//! future pack versions, external SDKs, and streaming transports can use
//! without pulling in SQLite/Git/CLI. Checksums are computed over the raw
//! bytes as written (JSONL lines, not re-serialized values).

use sha2::{Digest, Sha256};
use std::io::{Read, Write};

/// SHA-256 of `bytes`, hex-encoded (64 chars).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Wrap a writer so callers can stream bytes and finalize a hex digest.
///
/// The hasher sees exactly the bytes written; no framing is added.
pub struct ChecksumWriter<W> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> ChecksumWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    pub fn finalize(self) -> (W, String) {
        let digest = self.hasher.finalize();
        (self.inner, hex::encode(digest))
    }
}

impl<W: Write> Write for ChecksumWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Wrap a reader so callers can stream bytes and finalize a hex digest.
pub struct ChecksumReader<R> {
    inner: R,
    hasher: Sha256,
}

impl<R: Read> ChecksumReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    pub fn finalize(self) -> (R, String) {
        let digest = self.hasher.finalize();
        (self.inner, hex::encode(digest))
    }
}

impl<R: Read> Read for ChecksumReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.hasher.update(&buf[..n]);
        }
        Ok(n)
    }
}

/// Convenience: create a checksum writer.
pub fn checksum_writer<W: Write>(inner: W) -> ChecksumWriter<W> {
    ChecksumWriter::new(inner)
}

/// Convenience: create a checksum reader.
pub fn checksum_reader<R: Read>(inner: R) -> ChecksumReader<R> {
    ChecksumReader::new(inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Write};

    #[test]
    fn sha256_hex_is_stable() {
        // echo -n hello | sha256sum
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn checksum_writer_matches_sha256_hex() {
        let mut writer = checksum_writer(Vec::new());
        writer.write_all(b"hello ").unwrap();
        writer.write_all(b"world").unwrap();
        let (buf, digest) = writer.finalize();
        assert_eq!(buf, b"hello world");
        assert_eq!(digest, sha256_hex(b"hello world"));
    }

    #[test]
    fn checksum_reader_matches_sha256_hex() {
        let reader = checksum_reader(Cursor::new(b"hello world".to_vec()));
        let mut reader = reader;
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"hello world");
        let (_, digest) = reader.finalize();
        assert_eq!(digest, sha256_hex(b"hello world"));
    }
}
