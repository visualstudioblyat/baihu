// Atomic file writes — write .tmp, fsync, rename.
//
// Prevents corruption from crashes or power loss during writes.
// The rename is atomic on both POSIX (rename(2)) and Windows (MoveFileEx).

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::Path;

/// Atomically writes `data` to `path` via a temp file + rename.
/// Ensures data is fsynced to disk before the rename occurs.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let tmp_path = path.with_extension("tmp");

    // Write to temp file with explicit fsync
    let result = (|| -> Result<()> {
        let mut file = fs::File::create(&tmp_path)
            .with_context(|| format!("Failed to create temp file: {}", tmp_path.display()))?;
        file.write_all(data)
            .context("Failed to write data to temp file")?;
        file.sync_all().context("Failed to fsync temp file")?;
        Ok(())
    })();

    if let Err(e) = result {
        // Cleanup temp file on failure
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    // Atomic rename
    fs::rename(&tmp_path, path).with_context(|| {
        // Cleanup on rename failure too
        let _ = fs::remove_file(&tmp_path);
        format!(
            "Failed to rename {} -> {}",
            tmp_path.display(),
            path.display()
        )
    })
}

/// Async version for tokio contexts. Delegates to blocking threadpool
/// since fsync and rename must happen synchronously.
pub async fn atomic_write_async(path: &Path, data: Vec<u8>) -> Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || atomic_write(&path, &data))
        .await
        .context("Atomic write task panicked")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn creates_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        atomic_write(&path, b"hello").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn overwrites_existing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    }

    #[test]
    fn no_tmp_left_on_success() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        atomic_write(&path, b"data").unwrap();
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn empty_data_works() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("empty.txt");
        atomic_write(&path, b"").unwrap();
        assert_eq!(fs::read(&path).unwrap().len(), 0);
    }

    #[test]
    fn missing_parent_returns_error() {
        let path = std::path::PathBuf::from(if cfg!(windows) {
            r"C:\nonexistent_dir_xyz\file.txt"
        } else {
            "/nonexistent_dir_xyz/file.txt"
        });
        assert!(atomic_write(&path, b"data").is_err());
    }

    #[tokio::test]
    async fn async_creates_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("async.txt");
        atomic_write_async(&path, b"async data".to_vec())
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "async data");
    }
}
