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

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_atomic_write_creates_and_overwrites() {
        let temp_dir = std::env::temp_dir().join(format!("scratchpad_test_{}", std::process::id()));
        let file_path = temp_dir.join("pad_1.md");

        let initial_text = "# Hello World\nTesting initial atomic write.";
        atomic_write(&file_path, initial_text).expect("Failed initial atomic write");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), initial_text);

        let updated_text = "# Hello Again\nTesting overwrite.";
        atomic_write(&file_path, updated_text).expect("Failed overwrite atomic write");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), updated_text);

        // Verify no leftover .tmp files
        let entries: Vec<_> = fs::read_dir(&temp_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["pad_1.md"]);

        let _ = fs::remove_dir_all(&temp_dir);
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
}
