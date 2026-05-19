use std::path::{Path, PathBuf};
use tokio::fs;
use crate::error::AppError;

/// Returns the path for a stored file, bucketed by first 4 hex chars.
/// e.g. hash "abcdef123..." -> "<storage>/ab/cd/abcdef123....gif"
pub fn file_path(storage: &str, filename: &str) -> PathBuf {
    // `get` (vs slicing) avoids a panic on names shorter than 4 bytes or
    // with a non-ASCII boundary. Stored names are server UUIDs so this is
    // defensive; the fallback bucket is deterministic for a given name.
    let bucket1 = filename.get(0..2).unwrap_or("__");
    let bucket2 = filename.get(2..4).unwrap_or("__");
    Path::new(storage).join(bucket1).join(bucket2).join(filename)
}

pub async fn write_file(storage: &str, filename: &str, data: &[u8]) -> Result<(), AppError> {
    let path = file_path(storage, filename);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(&path, data).await?;
    Ok(())
}

pub async fn delete_file(storage: &str, filename: &str) -> Result<(), AppError> {
    let path = file_path(storage, filename);
    if path.exists() {
        fs::remove_file(path).await?;
    }
    Ok(())
}

pub async fn read_file(storage: &str, filename: &str) -> Result<Vec<u8>, AppError> {
    let path = file_path(storage, filename);
    fs::read(&path).await.map_err(|_| AppError::NotFound)
}

/// Sum the size of every regular file under `storage`, recursively.
/// Used at startup to reconcile on-disk bytes against the DB total.
pub fn dir_size(storage: &str) -> std::io::Result<u64> {
    fn walk(dir: &Path, total: &mut u64) -> std::io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_dir() {
                walk(&entry.path(), total)?;
            } else if ft.is_file() {
                *total += entry.metadata()?.len();
            }
        }
        Ok(())
    }
    let mut total = 0;
    walk(Path::new(storage), &mut total)?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_path_buckets_by_first_four_chars() {
        let path = file_path("storage", "abcd1234.gif");
        assert_eq!(path, Path::new("storage/ab/cd/abcd1234.gif"));
    }

    #[test]
    fn file_path_different_filenames_different_buckets() {
        let a = file_path("storage", "aabbcc.gif");
        let b = file_path("storage", "zzyyxx.gif");
        assert_ne!(a.parent(), b.parent());
    }

    #[test]
    fn file_path_handles_short_names_without_panicking() {
        assert_eq!(file_path("s", "abc"), Path::new("s/ab/__/abc"));
        assert_eq!(file_path("s", "a"), Path::new("s/__/__/a"));
    }

    #[tokio::test]
    async fn write_and_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().to_str().unwrap();
        let filename = "abcd1234test.gif";
        let data = b"fake gif data";

        write_file(storage, filename, data).await.unwrap();
        let read = read_file(storage, filename).await.unwrap();
        assert_eq!(read, data);
    }

    #[tokio::test]
    async fn read_missing_file_returns_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_file(dir.path().to_str().unwrap(), "abcdnonexistent.gif").await;
        assert!(matches!(result, Err(AppError::NotFound)));
    }
}
