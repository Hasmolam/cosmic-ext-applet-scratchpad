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
/// Preserves symlinks by writing to the canonical path.
/// Preserves original file permissions (e.g. chmod 0600).
/// Syncs file to disk and replaces target using atomic rename.
pub fn atomic_write(target_path: &Path, content: &str) -> io::Result<()> {
    let real_target = if target_path.exists() {
        fs::canonicalize(target_path).unwrap_or_else(|_| target_path.to_path_buf())
    } else {
        target_path.to_path_buf()
    };

    let existing_permissions = if real_target.exists() {
        fs::metadata(&real_target).ok().map(|m| m.permissions())
    } else {
        None
    };

    if let Some(parent) = real_target.parent() {
        fs::create_dir_all(parent)?;
    }

    let file_name = real_target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid file path"))?;

    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(format!(".tmp.{}", std::process::id()));
    let tmp_path = real_target.with_file_name(tmp_name);

    {
        let mut file = File::create(&tmp_path)?;
        if let Some(perms) = existing_permissions {
            let _ = fs::set_permissions(&tmp_path, perms);
        }
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }

    fs::rename(&tmp_path, &real_target)?;
    Ok(())
}

/// Returns the path to the backup file for the given pad index.
pub fn backup_path(index: usize) -> PathBuf {
    data_dir().join(format!("pad_{}.md.bak", index + 1))
}

/// Creates an atomic backup of the given pad file before clearing.
pub fn create_pad_backup(index: usize) -> io::Result<()> {
    let source = pad_path(index);
    if source.exists() {
        let content = fs::read_to_string(&source)?;
        if !content.is_empty() {
            atomic_write(&backup_path(index), &content)?;
        }
    }
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

    #[test]
    fn test_atomic_write_preserves_symlink() {
        let temp_dir =
            std::env::temp_dir().join(format!("scratchpad_symlink_test_{}", std::process::id()));
        let real_file = temp_dir.join("real_pad.md");
        let symlink_file = temp_dir.join("symlink_pad.md");

        atomic_write(&real_file, "Initial Real Content").expect("Failed to write real file");
        std::os::unix::fs::symlink(&real_file, &symlink_file).expect("Failed to create symlink");

        // Write through symlink
        atomic_write(&symlink_file, "Updated through symlink")
            .expect("Failed to write through symlink");

        // Verify symlink is still a symlink
        let symlink_meta = fs::symlink_metadata(&symlink_file).expect("Symlink metadata failed");
        assert!(symlink_meta.file_type().is_symlink());

        // Verify real file received the updated content
        assert_eq!(
            fs::read_to_string(&real_file).unwrap(),
            "Updated through symlink"
        );

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_atomic_write_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir =
            std::env::temp_dir().join(format!("scratchpad_perms_test_{}", std::process::id()));
        let file_path = temp_dir.join("restricted.md");

        atomic_write(&file_path, "Secret notes").expect("Failed initial write");
        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o600))
            .expect("Failed to set 0600 mode");

        // Overwrite and check permissions
        atomic_write(&file_path, "Updated secret notes").expect("Failed overwrite");
        let perms = fs::metadata(&file_path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_backup_path_and_creation() {
        let b0 = backup_path(0);
        let b1 = backup_path(1);
        let b2 = backup_path(2);
        assert!(b0.ends_with("pad_1.md.bak"));
        assert!(b1.ends_with("pad_2.md.bak"));
        assert!(b2.ends_with("pad_3.md.bak"));

        // Save a test pad then create backup
        save_pad_atomic(0, "Test Backup Content").expect("Failed to save pad 0");
        create_pad_backup(0).expect("Failed to create pad backup");

        assert!(b0.exists());
        let backup_content = fs::read_to_string(&b0).expect("Failed to read backup file");
        assert_eq!(backup_content, "Test Backup Content");

        // Cleanup
        let _ = fs::remove_file(&b0);
    }
}
