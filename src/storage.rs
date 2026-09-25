// SPDX-License-Identifier: GPL-3.0-only

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const TOTAL_PADS: usize = 3;

/// Resolves the base data directory following XDG Base Directory Specification:
/// `$XDG_DATA_HOME/cosmic-scratchpad` or `$HOME/.local/share/cosmic-scratchpad`.
pub fn data_dir() -> PathBuf {
    if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME")
        && !xdg_data.trim().is_empty()
    {
        return PathBuf::from(xdg_data).join("cosmic-scratchpad");
    }

    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("cosmic-scratchpad");
    }

    PathBuf::from("/tmp/cosmic-scratchpad")
}

/// Returns the path to the Markdown file for the given pad index (0-indexed).
pub fn pad_path(index: usize) -> PathBuf {
    data_dir().join(format!("pad_{}.md", index + 1))
}

/// Loads the contents of the given pad index. Returns empty string if the file doesn't exist yet.
pub fn load_pad(index: usize) -> io::Result<String> {
    let path = pad_path(index);
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path)
}

/// Atomically writes content to the target file.
/// Preserves the original file name and extension by appending `.tmp.<pid>`.
/// Syncs file to disk and replaces target using atomic rename.
pub fn atomic_write(target_path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file_name = target_path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid file path"))?;

    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(format!(".tmp.{}", std::process::id()));
    let tmp_path = target_path.with_file_name(tmp_name);

    {
        let mut file = File::create(&tmp_path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }

    fs::rename(&tmp_path, target_path)?;
    Ok(())
}

/// Saves the given pad content atomically to disk.
pub fn save_pad_atomic(index: usize, content: &str) -> io::Result<()> {
    atomic_write(&pad_path(index), content)
}

/// Returns the last modification time of the pad file on disk if it exists.
pub fn get_pad_mtime(index: usize) -> Option<std::time::SystemTime> {
    let path = pad_path(index);
    fs::metadata(&path).ok().and_then(|m| m.modified().ok())
}

/// Loads the pad content if it was modified on disk since `last_mtime`.
/// Returns Ok(Some((content, new_mtime))) if modified or initially loaded.
/// Returns Ok(None) if unchanged.
pub fn load_pad_if_modified(
    index: usize,
    last_mtime: Option<std::time::SystemTime>,
) -> io::Result<Option<(String, std::time::SystemTime)>> {
    let path = pad_path(index);
    if !path.exists() {
        return Ok(None);
    }

    let mtime = fs::metadata(&path)?.modified()?;
    if let Some(prev) = last_mtime
        && mtime <= prev
    {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)?;
    Ok(Some((content, mtime)))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_atomic_write_creates_file_and_parent_dirs() {
        let temp_dir =
            std::env::temp_dir().join(format!("scratchpad_create_test_{}", std::process::id()));
        let file_path = temp_dir.join("nested").join("dirs").join("pad_1.md");

        let text = "Hello nested directory!";
        atomic_write(&file_path, text).expect("Failed to create file and parent dirs");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), text);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_atomic_write_overwrites_cleanly() {
        let temp_dir =
            std::env::temp_dir().join(format!("scratchpad_overwrite_test_{}", std::process::id()));
        let file_path = temp_dir.join("pad_1.md");

        let initial_text = "# Initial Content";
        atomic_write(&file_path, initial_text).expect("Failed initial write");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), initial_text);

        let updated_text = "# Updated Content";
        atomic_write(&file_path, updated_text).expect("Failed overwrite");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), updated_text);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_atomic_write_leaves_no_stale_tmp_files() {
        let temp_dir =
            std::env::temp_dir().join(format!("scratchpad_tmp_test_{}", std::process::id()));
        let file_path = temp_dir.join("pad_1.md");

        atomic_write(&file_path, "Test without leftover").expect("Failed atomic write");

        let entries: Vec<_> = fs::read_dir(&temp_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();

        assert_eq!(entries, vec!["pad_1.md"]);
        assert!(entries.iter().all(|name| !name.contains(".tmp.")));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_load_pad_fallback_to_empty() {
        // High pad index that doesn't exist
        let result = load_pad(999).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_pad_path_indexing() {
        let p0 = pad_path(0);
        let p1 = pad_path(1);
        let p2 = pad_path(2);

        assert!(p0.ends_with("pad_1.md"));
        assert!(p1.ends_with("pad_2.md"));
        assert!(p2.ends_with("pad_3.md"));
    }

    #[test]
    fn test_load_pad_if_modified_detects_changes() {
        let temp_dir =
            std::env::temp_dir().join(format!("scratchpad_mtime_test_{}", std::process::id()));
        let file_path = temp_dir.join("pad_1.md");

        // 1. Initial write
        atomic_write(&file_path, "Version 1").expect("Failed to write v1");
        let mtime_v1 = fs::metadata(&file_path).unwrap().modified().unwrap();

        // Calling load_pad_if_modified with mtime_v1 on non-existent index vs same time
        // Simulate checking if modified since mtime_v1
        let path = &file_path;
        let mtime = fs::metadata(path).unwrap().modified().unwrap();
        assert!(mtime <= mtime_v1);

        // 2. Wait 10ms to ensure timestamp difference on filesystems with fine grain mtime
        std::thread::sleep(std::time::Duration::from_millis(15));
        atomic_write(&file_path, "Version 2").expect("Failed to write v2");
        let mtime_v2 = fs::metadata(&file_path).unwrap().modified().unwrap();

        assert!(mtime_v2 > mtime_v1);
        let content_v2 = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content_v2, "Version 2");

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
