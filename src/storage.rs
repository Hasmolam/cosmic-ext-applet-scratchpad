// SPDX-License-Identifier: GPL-3.0-only

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

/// Directory for soft-deleted / trashed notes.
pub fn trash_dir() -> PathBuf {
    data_dir().join("trash")
}

/// Returns the path to the Markdown file for the given pad index (0-indexed).
pub fn pad_path(index: usize) -> PathBuf {
    data_dir().join(format!("pad_{}.md", index + 1))
}

/// Natural sorting helper for note files:
/// pad_1.md, pad_2.md, pad_10.md are sorted numerically.
/// Other files follow alphabetically.
fn note_sort_key(filename: &str) -> (u8, u64, String) {
    if let Some(rest) = filename.strip_prefix("pad_")
        && let Some(num_str) = rest.strip_suffix(".md")
        && let Ok(num) = num_str.parse::<u64>()
    {
        return (0, num, String::new());
    }
    (1, 0, filename.to_lowercase())
}

/// Lists all Markdown note files in the specified directory.
/// If directory is empty, creates `pad_1.md` and returns it so at least one note always exists.
pub fn list_notes_in_dir(dir: &Path) -> io::Result<Vec<PathBuf>> {
    fs::create_dir_all(dir)?;

    let mut notes = Vec::new();
    let entries = fs::read_dir(dir)?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let filename = entry.file_name().to_string_lossy().to_string();
            if filename.ends_with(".md")
                && !filename.starts_with('.')
                && !filename.ends_with(".bak")
            {
                notes.push(path);
            }
        }
    }

    notes.sort_by(|a, b| {
        let name_a = a.file_name().unwrap_or_default().to_string_lossy();
        let name_b = b.file_name().unwrap_or_default().to_string_lossy();
        note_sort_key(&name_a).cmp(&note_sort_key(&name_b))
    });

    if notes.is_empty() {
        let initial_file = dir.join("pad_1.md");
        atomic_write(&initial_file, "")?;
        notes.push(initial_file);
    }

    Ok(notes)
}

/// Lists all notes in the default data directory.
pub fn list_notes() -> io::Result<Vec<PathBuf>> {
    list_notes_in_dir(&data_dir())
}

/// Derives a clean note title from note content:
/// Scans for first non-empty line, removes markdown headers/list symbols, and truncates if necessary.
pub fn derive_title(content: &str, fallback: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut cleaned = trimmed.trim_start_matches('#').trim();
        for prefix in ["- ", "* ", "+ ", "> "] {
            if let Some(rest) = cleaned.strip_prefix(prefix) {
                cleaned = rest.trim();
                break;
            }
        }

        if cleaned.is_empty() {
            continue;
        }

        let chars: Vec<char> = cleaned.chars().collect();
        return if chars.len() > 22 {
            let truncated: String = chars.into_iter().take(22).collect();
            format!("{}…", truncated.trim_end())
        } else {
            cleaned.to_string()
        };
    }

    fallback.to_string()
}

/// Creates a new note in the specified directory.
/// If `title_opt` is supplied, pre-populates note with `# <title>\n\n`.
pub fn create_new_note_in_dir(dir: &Path, title_opt: Option<&str>) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;

    let mut max_idx = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Some(fname) = path.file_name().and_then(|f| f.to_str())
                && let Some(rest) = fname.strip_prefix("pad_")
                && let Some(num_str) = rest.strip_suffix(".md")
                && let Ok(num) = num_str.parse::<u64>()
            {
                max_idx = max_idx.max(num);
            }
        }
    }

    let next_idx = max_idx + 1;
    let new_path = dir.join(format!("pad_{}.md", next_idx));
    let initial_content = if let Some(title) = title_opt
        && !title.trim().is_empty()
    {
        format!("# {}\n\n", title.trim())
    } else {
        String::new()
    };

    atomic_write(&new_path, &initial_content)?;
    Ok(new_path)
}

/// Creates a new note in the default data directory.
pub fn create_new_note(title_opt: Option<&str>) -> io::Result<PathBuf> {
    create_new_note_in_dir(&data_dir(), title_opt)
}

/// Deletes a note file safely by copying it to the trash directory and then removing the original.
/// Returns the content that was deleted (for undo support).
pub fn delete_note(path: &Path) -> io::Result<String> {
    let content = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };

    if path.exists() {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let trash = parent.join("trash");
        let _ = fs::create_dir_all(&trash);

        if let Some(file_name) = path.file_name() {
            let ts = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let trash_file = trash.join(format!("{}_{}", ts, file_name.to_string_lossy()));
            let _ = atomic_write(&trash_file, &content);
        }

        fs::remove_file(path)?;
    }

    Ok(content)
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

/// Returns the last modification time of the file on disk if it exists.
pub fn note_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Loads note content if modified on disk since `last_mtime`.
pub fn load_note_if_modified(
    path: &Path,
    last_mtime: Option<SystemTime>,
) -> io::Result<Option<(String, SystemTime)>> {
    if !path.exists() {
        return Ok(None);
    }

    let mtime = fs::metadata(path)?.modified()?;
    if let Some(prev) = last_mtime
        && mtime <= prev
    {
        return Ok(None);
    }

    let content = fs::read_to_string(path)?;
    Ok(Some((content, mtime)))
}

/// Saves the given note content atomically to disk.
pub fn save_note_atomic(path: &Path, content: &str) -> io::Result<()> {
    atomic_write(path, content)
}

// Backward-compatible wrappers for legacy 3-pad indexing
pub fn load_pad(index: usize) -> io::Result<String> {
    let path = pad_path(index);
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path)
}

pub fn save_pad_atomic(index: usize, content: &str) -> io::Result<()> {
    atomic_write(&pad_path(index), content)
}

pub fn get_pad_mtime(index: usize) -> Option<SystemTime> {
    note_mtime(&pad_path(index))
}

pub fn load_pad_if_modified(
    index: usize,
    last_mtime: Option<SystemTime>,
) -> io::Result<Option<(String, SystemTime)>> {
    load_note_if_modified(&pad_path(index), last_mtime)
}

pub fn backup_path(index: usize) -> PathBuf {
    data_dir().join(format!("pad_{}.md.bak", index + 1))
}

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

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_derive_title() {
        assert_eq!(
            derive_title("# Project Setup\nSome content", "Fallback"),
            "Project Setup"
        );
        assert_eq!(derive_title("### Sub Header", "Fallback"), "Sub Header");
        assert_eq!(derive_title("* Bullet item", "Fallback"), "Bullet item");
        assert_eq!(
            derive_title("- Another bullet", "Fallback"),
            "Another bullet"
        );
        assert_eq!(derive_title("> Quote line", "Fallback"), "Quote line");
        assert_eq!(
            derive_title("Plain text first line\nSecond line", "Fallback"),
            "Plain text first line"
        );
        assert_eq!(
            derive_title("   \n\n#    Spaced Title   \n", "Fallback"),
            "Spaced Title"
        );
        assert_eq!(derive_title("", "Fallback"), "Fallback");
        assert_eq!(derive_title("   \n\t  ", "Fallback"), "Fallback");

        // Long title truncation (capped at 22 chars + ellipsis)
        let long_line = "This is a remarkably long line designed to test title truncation behavior";
        let derived = derive_title(long_line, "Fallback");
        assert!(derived.ends_with('…'));
        assert!(derived.chars().count() <= 23);

        // Markdown bold / formatting preservation
        assert_eq!(derive_title("**Bold Title**", "Fallback"), "**Bold Title**");
        assert_eq!(derive_title("`code snippet`", "Fallback"), "`code snippet`");

        // Unicode and Turkish character handling
        let turkish_line = "Şekerli çay ve öğleden sonra toplantısı";
        let turkish_derived = derive_title(turkish_line, "Fallback");
        assert!(turkish_derived.ends_with('…'));
        assert!(turkish_derived.starts_with("Şekerli çay"));
    }

    #[test]
    fn test_list_and_create_notes_in_dir() {
        let temp_dir =
            std::env::temp_dir().join(format!("scratchpad_list_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);

        // 1. Calling create_new_note_in_dir directly in empty directory creates pad_1.md only
        let p1 = create_new_note_in_dir(&temp_dir, None).expect("Failed to create initial note");
        assert!(p1.ends_with("pad_1.md"));
        let notes_after_init = list_notes_in_dir(&temp_dir).unwrap();
        assert_eq!(notes_after_init.len(), 1);
        assert_eq!(notes_after_init[0], p1);

        // 2. Create pad_2.md with title
        let p2 = create_new_note_in_dir(&temp_dir, Some("Meeting Notes"))
            .expect("Failed to create note");
        assert!(p2.ends_with("pad_2.md"));
        let p2_content = fs::read_to_string(&p2).unwrap();
        assert_eq!(p2_content, "# Meeting Notes\n\n");

        // 3. Create pad_3.md without title
        let p3 = create_new_note_in_dir(&temp_dir, None).expect("Failed to create note 3");
        assert!(p3.ends_with("pad_3.md"));
        assert_eq!(fs::read_to_string(&p3).unwrap(), "");

        // 4. Verify list order
        let notes_all = list_notes_in_dir(&temp_dir).unwrap();
        assert_eq!(notes_all.len(), 3);
        assert!(notes_all[0].ends_with("pad_1.md"));
        assert!(notes_all[1].ends_with("pad_2.md"));
        assert!(notes_all[2].ends_with("pad_3.md"));

        // 5. Delete note and verify timestamped trash backup
        let deleted_content = delete_note(&p2).expect("Failed to delete note");
        assert_eq!(deleted_content, "# Meeting Notes\n\n");
        assert!(!p2.exists());
        let trash_entries: Vec<_> = fs::read_dir(temp_dir.join("trash"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert!(trash_entries.iter().any(|name| name.ends_with("_pad_2.md")));

        let _ = fs::remove_dir_all(&temp_dir);
    }

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
    fn test_pad_path_indexing() {
        let p0 = pad_path(0);
        let p1 = pad_path(1);
        let p2 = pad_path(2);

        assert!(p0.ends_with("pad_1.md"));
        assert!(p1.ends_with("pad_2.md"));
        assert!(p2.ends_with("pad_3.md"));
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
    fn test_create_new_note_on_empty_dir_creates_single_file() {
        let temp_dir =
            std::env::temp_dir().join(format!("scratchpad_empty_dir_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);

        // Creating on an empty dir should produce pad_1.md and nothing else
        let note = create_new_note_in_dir(&temp_dir, Some("First Note"))
            .expect("Failed to create note in empty dir");
        assert!(note.ends_with("pad_1.md"));

        let files = list_notes_in_dir(&temp_dir).expect("Failed to list notes");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], note);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_trash_retention_multiple_deletions() {
        let temp_dir = std::env::temp_dir().join(format!(
            "scratchpad_multi_trash_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp_dir);

        let p1 = create_new_note_in_dir(&temp_dir, Some("Note V1")).unwrap();
        delete_note(&p1).unwrap();

        // Create another note with same name (pad_1.md)
        let p1_again = create_new_note_in_dir(&temp_dir, Some("Note V2")).unwrap();
        assert_eq!(p1, p1_again);
        delete_note(&p1_again).unwrap();

        let trash_dir = temp_dir.join("trash");
        let trashed: Vec<_> = fs::read_dir(trash_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();

        // Ensure both versions exist in trash and did not overwrite each other
        assert_eq!(trashed.len(), 2);
        assert!(trashed.iter().all(|f| f.ends_with("_pad_1.md")));

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
