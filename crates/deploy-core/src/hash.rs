//! Content hashing. Port of @facetlayer/file-manifest's `getFileHash`.

use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::Result;
use sha2::{Digest, Sha256};

/// 64 KiB, matching the old daemon. Deploy bundles routinely carry large
/// binaries, so files are hashed streaming and never held in memory.
const READ_BUFFER_SIZE: usize = 64 * 1024;

/// SHA-256 hex digest of a file's contents.
///
/// A missing file is `Ok(None)`, not an error — callers use this to ask "does
/// this file exist, and if so what is it" in one step. Any other IO failure is
/// a real error and propagates (this mirrors the JS, which resolved `null` only
/// on ENOENT and rejected otherwise).
pub fn get_file_hash(path: &Path) -> Result<Option<String>> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };

    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; READ_BUFFER_SIZE];

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(Some(hex::encode(hasher.finalize())))
}

/// SHA-256 hex digest of an in-memory buffer, for content that never touches
/// the disk (an upload body being verified against its claimed hash).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("deploy-core-hash-{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn returns_consistent_hash_for_same_content() {
        let dir = temp_dir("consistent");
        let path = dir.join("hash-test-1.txt");
        fs::write(&path, "hello world").unwrap();

        let hash1 = get_file_hash(&path).unwrap().unwrap();
        let hash2 = get_file_hash(&path).unwrap().unwrap();
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn returns_different_hash_for_different_content() {
        let dir = temp_dir("different");
        let a = dir.join("hash-test-2a.txt");
        let b = dir.join("hash-test-2b.txt");
        fs::write(&a, "content A").unwrap();
        fs::write(&b, "content B").unwrap();

        assert_ne!(get_file_hash(&a).unwrap(), get_file_hash(&b).unwrap());
    }

    #[test]
    fn returns_none_for_non_existent_file() {
        let dir = temp_dir("missing");
        assert_eq!(get_file_hash(&dir.join("does-not-exist.txt")).unwrap(), None);
    }

    #[test]
    fn produces_correct_sha256_hash() {
        let dir = temp_dir("known");
        let path = dir.join("hash-test-known.txt");
        fs::write(&path, "test").unwrap();

        assert_eq!(
            get_file_hash(&path).unwrap().unwrap(),
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }

    #[test]
    fn handles_empty_file() {
        let dir = temp_dir("empty");
        let path = dir.join("hash-test-empty.txt");
        fs::write(&path, "").unwrap();

        assert_eq!(
            get_file_hash(&path).unwrap().unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn handles_binary_content() {
        let dir = temp_dir("binary");
        let path = dir.join("hash-test-binary.bin");
        fs::write(&path, [0x00u8, 0x01, 0x02, 0xff, 0xfe, 0xfd]).unwrap();

        assert_eq!(get_file_hash(&path).unwrap().unwrap().len(), 64);
    }

    #[test]
    fn hashes_content_larger_than_the_read_buffer() {
        let dir = temp_dir("large");
        let path = dir.join("large.bin");
        let content = vec![0xabu8; READ_BUFFER_SIZE * 3 + 17];
        fs::write(&path, &content).unwrap();

        assert_eq!(
            get_file_hash(&path).unwrap().unwrap(),
            sha256_hex(&content)
        );
    }
}
