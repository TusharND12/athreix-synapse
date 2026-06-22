//! Content-addressed snapshot store — the Time Machine backend.
//!
//! Each captured file version is stored once, keyed by the SHA-256 of its bytes
//! (automatic dedupe), zlib-compressed on disk under `.synapse/snapshots/blobs/`.
//! A checkpoint (see `store`) is just a map of `path -> blob hash`; restoring is
//! writing those blobs back to the working tree. This reuses git's content-
//! addressed idea in pure Rust, with no dependency on the user's own `.git`.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct Snapshots {
    blobs_dir: PathBuf,
}

impl Snapshots {
    /// Open (and create) the snapshot store under `<synapse_dir>/snapshots`.
    pub fn open(synapse_dir: &Path) -> std::io::Result<Self> {
        let blobs_dir = synapse_dir.join("snapshots").join("blobs");
        fs::create_dir_all(&blobs_dir)?;
        Ok(Self { blobs_dir })
    }

    fn path_for(&self, hash: &str) -> PathBuf {
        // Fan out by the first two hex chars to avoid huge flat directories.
        self.blobs_dir.join(&hash[0..2]).join(&hash[2..])
    }

    pub fn hash(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        hex::encode(h.finalize())
    }

    pub fn has_blob(&self, hash: &str) -> bool {
        self.path_for(hash).exists()
    }

    /// Store bytes, returning their content hash. No-op if already present.
    pub fn write_blob(&self, bytes: &[u8]) -> std::io::Result<String> {
        let hash = Self::hash(bytes);
        let path = self.path_for(&hash);
        if path.exists() {
            return Ok(hash);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(bytes)?;
        let compressed = enc.finish()?;
        // Write to a temp file then rename for atomicity.
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, &compressed)?;
        fs::rename(&tmp, &path)?;
        Ok(hash)
    }

    pub fn read_blob(&self, hash: &str) -> std::io::Result<Vec<u8>> {
        let compressed = fs::read(self.path_for(hash))?;
        let mut dec = ZlibDecoder::new(&compressed[..]);
        let mut out = Vec::new();
        dec.read_to_end(&mut out)?;
        Ok(out)
    }

    /// Convenience: blob bytes decoded as UTF-8 text, if valid.
    pub fn read_text(&self, hash: &str) -> Option<String> {
        self.read_blob(hash).ok().and_then(|b| String::from_utf8(b).ok())
    }

    /// Delete every blob not present in `referenced` (garbage collection). Also
    /// removes any stray `.tmp` files. Returns the number of files removed.
    pub fn gc(&self, referenced: &std::collections::HashSet<String>) -> std::io::Result<usize> {
        let mut removed = 0usize;
        let subs = match fs::read_dir(&self.blobs_dir) {
            Ok(s) => s,
            Err(_) => return Ok(0),
        };
        for sub in subs.flatten() {
            if !sub.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let prefix = sub.file_name().to_string_lossy().to_string();
            if let Ok(files) = fs::read_dir(sub.path()) {
                for f in files.flatten() {
                    let rest = f.file_name().to_string_lossy().to_string();
                    let hash = format!("{prefix}{rest}");
                    if !referenced.contains(&hash) && fs::remove_file(f.path()).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("syn-snap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn blob_roundtrip_and_dedupe() {
        let dir = temp();
        let s = Snapshots::open(&dir).unwrap();
        let data = b"fn main() {\n    println!(\"hi\");\n}\n";
        let h1 = s.write_blob(data).unwrap();
        let h2 = s.write_blob(data).unwrap(); // identical content
        assert_eq!(h1, h2, "same content must hash identically (dedupe)");
        assert!(s.has_blob(&h1));
        assert_eq!(s.read_blob(&h1).unwrap(), data);
        assert_eq!(s.read_text(&h1).as_deref(), Some(std::str::from_utf8(data).unwrap()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn distinct_content_distinct_hash() {
        let dir = temp();
        let s = Snapshots::open(&dir).unwrap();
        assert_ne!(s.write_blob(b"a").unwrap(), s.write_blob(b"b").unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }
}
