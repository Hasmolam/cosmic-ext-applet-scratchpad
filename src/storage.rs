// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Result of a conflict-checked save operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveResult {
    Saved(Option<SystemTime>),
    Conflict {
        conflict_path: PathBuf,
        disk_content: String,
        disk_mtime: SystemTime,
    },
}

fn data_dir_from_env(
    xdg_data: Option<&str>,
    home: Option<&str>,
    xdg_runtime: Option<&str>,
) -> io::Result<PathBuf> {
    if let Some(xdg) = xdg_data
        && !xdg.trim().is_empty()
    {
        return Ok(PathBuf::from(xdg).join("cosmic-scratchpad"));
    }

    if let Some(home) = home
        && !home.trim().is_empty()
    {
        return Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("cosmic-scratchpad"));
    }

    if let Some(runtime) = xdg_runtime
        && !runtime.trim().is_empty()
    {
        return Ok(PathBuf::from(runtime).join("cosmic-scratchpad"));
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "could not resolve data directory: XDG_DATA_HOME, HOME, and XDG_RUNTIME_DIR are unset",
    ))
}

/// Resolves the base data directory following XDG Base Directory Specification:
/// `$XDG_DATA_HOME/cosmic-scratchpad`, `$HOME/.local/share/cosmic-scratchpad`,
/// or `$XDG_RUNTIME_DIR/cosmic-scratchpad`.
pub fn data_dir() -> io::Result<PathBuf> {
    let xdg_data = std::env::var("XDG_DATA_HOME").ok();
    let home = std::env::var("HOME").ok();
    let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").ok();
    data_dir_from_env(xdg_data.as_deref(), home.as_deref(), xdg_runtime.as_deref())
}

/// Creates `dir` with mode `0700` if it does not exist yet, leaving parent
/// directories (such as `~/.local/share`) with default permissions.
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    if !dir.exists() {
        if let Some(parent) = dir.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            fs::create_dir_all(parent)?;
        }
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Restricts an existing directory to mode `0700` when group or other bits are set.
pub fn tighten_dir_permissions(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = match fs::metadata(dir) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!(?dir, %e, "Failed to inspect directory permissions");
                return;
            }
        };
        if !meta.is_dir() {
            return;
        }
        let mode = meta.permissions().mode();
        if (mode & 0o077) != 0 {
            if let Err(e) = fs::set_permissions(dir, fs::Permissions::from_mode(0o700)) {
                tracing::warn!(?dir, %e, "Failed to tighten directory permissions to 0700");
            } else {
                tracing::info!(
                    ?dir,
                    old_mode = format_args!("{:04o}", mode & 0o777),
                    "Tightened directory permissions to 0700"
                );
            }
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn parent_dir_or_dot(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
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
    ensure_private_dir(dir)?;

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
        return if chars.len() > 16 {
            let truncated: String = chars.into_iter().take(16).collect();
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
    ensure_private_dir(dir)?;

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

static TRASH_SEQ: AtomicU64 = AtomicU64::new(0);

fn next_trash_path(trash_dir: &Path, file_name: &OsStr) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    loop {
        let seq = TRASH_SEQ.fetch_add(1, Ordering::Relaxed);
        let mut trash_name = OsString::from(format!("{ts}_{seq}_"));
        trash_name.push(file_name);
        let candidate = trash_dir.join(trash_name);
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
}

/// Moves a note file to the `trash` subdirectory and returns its previous content.
/// Leaves the original file untouched if writing the trash copy fails.
pub fn delete_note(path: &Path) -> io::Result<String> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e),
    };

    let trash = parent_dir_or_dot(path).join("trash");
    ensure_private_dir(&trash)?;

    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "note path has no file name"))?;
    let trash_file = next_trash_path(&trash, file_name);

    let is_symlink = fs::symlink_metadata(path)?.file_type().is_symlink();
    if is_symlink || fs::rename(path, &trash_file).is_err() {
        atomic_write(&trash_file, &content)?;
        fs::remove_file(path)?;
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::metadata(&trash_file)
                && (meta.permissions().mode() & 0o077) != 0
                && let Err(e) = fs::set_permissions(&trash_file, fs::Permissions::from_mode(0o600))
            {
                tracing::warn!(
                    ?trash_file,
                    %e,
                    "Failed to tighten trashed note permissions to 0600"
                );
            }
        }
    }

    Ok(content)
}

/// Deletes regular files in `trash_dir` whose mtime is at least `retention_days`
/// older than `now`. Returns the number of removed files.
pub fn purge_old_trash(
    trash_dir: &Path,
    retention_days: u32,
    now: SystemTime,
) -> io::Result<usize> {
    if retention_days == 0 || !trash_dir.exists() {
        return Ok(0);
    }

    let cutoff = Duration::from_secs(u64::from(retention_days) * 86_400);
    let mut removed = 0usize;

    for entry in fs::read_dir(trash_dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(?trash_dir, %e, "Failed to read trash entry");
                continue;
            }
        };
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(?path, %e, "Failed to inspect trash entry");
                continue;
            }
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let modified = match meta.modified() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(?path, %e, "Failed to read trash file mtime");
                continue;
            }
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age >= cutoff {
            match fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(e) => tracing::warn!(?path, %e, "Failed to remove expired trash file"),
            }
        }
    }

    Ok(removed)
}

/// Atomically writes content to the target file.
/// Preserves symlinks by writing to the canonical path.
/// Preserves existing file permissions, or uses `0600` for new files.
/// Syncs file to disk and replaces target using atomic rename.
fn atomic_write(target_path: &Path, content: &str) -> io::Result<()> {
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

    if let Some(parent) = real_target.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        fs::create_dir_all(parent)?;
    }

    let file_name = real_target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid file path"))?;

    // Each write gets its own temp file. A shared name lets two concurrent
    // writes of the same note truncate each other's data before the rename.
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(format!(".tmp.{}.{}", std::process::id(), seq));
    let tmp_path = real_target.with_file_name(tmp_name);

    let result = write_tmp_and_rename(
        &tmp_path,
        &real_target,
        content,
        existing_permissions.as_ref(),
    );
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn write_tmp_and_rename(
    tmp_path: &Path,
    target: &Path,
    content: &str,
    permissions: Option<&fs::Permissions>,
) -> io::Result<()> {
    {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(tmp_path)?;
        if let Some(perms) = permissions
            && let Err(e) = file.set_permissions(perms.clone())
        {
            tracing::warn!(?tmp_path, %e, "Failed to preserve file permissions");
        }
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(tmp_path, target)
}

static CONFLICT_SEQ: AtomicU64 = AtomicU64::new(0);

fn next_conflict_path(path: &Path) -> PathBuf {
    let parent = parent_dir_or_dot(path);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_else(|| std::borrow::Cow::Borrowed("note"));
    let ext = path.extension().map(|e| e.to_string_lossy());
    let unix_secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    loop {
        let seq = CONFLICT_SEQ.fetch_add(1, Ordering::Relaxed);
        let name = match (&ext, seq) {
            (Some(ext), 0) => format!("{stem}.conflict-{unix_secs}.{ext}"),
            (Some(ext), n) => format!("{stem}.conflict-{unix_secs}_{n}.{ext}"),
            (None, 0) => format!("{stem}.conflict-{unix_secs}"),
            (None, n) => format!("{stem}.conflict-{unix_secs}_{n}"),
        };
        let candidate = parent.join(name);
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
}

/// Saves `content` to `path` if the file on disk has not changed since
/// `expected_mtime`. If an external edit changed the file to different content,
/// writes `content` to a sibling `.conflict-<secs>[_<seq>].<ext>` file with
/// mode `0600` and returns [`SaveResult::Conflict`].
pub fn save_note_checked(
    path: &Path,
    content: &str,
    expected_mtime: Option<SystemTime>,
) -> io::Result<SaveResult> {
    match fs::metadata(path) {
        Ok(meta) => {
            let disk_mtime = meta.modified().ok();
            let externally_changed = (expected_mtime.is_some() && disk_mtime != expected_mtime)
                || (expected_mtime.is_none() && meta.len() > 0);
            if externally_changed {
                let disk_content = fs::read_to_string(path)?;
                if disk_content == content {
                    return Ok(SaveResult::Saved(disk_mtime));
                }
                let conflict_path = next_conflict_path(path);
                atomic_write(&conflict_path, content)?;
                return Ok(SaveResult::Conflict {
                    conflict_path,
                    disk_content,
                    disk_mtime: disk_mtime.unwrap_or_else(SystemTime::now),
                });
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    atomic_write(path, content)?;
    Ok(SaveResult::Saved(note_mtime(path)))
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

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
            derive_title("Plain text line\nSecond line", "Fallback"),
            "Plain text line"
        );
        assert_eq!(
            derive_title("   \n\n#    Spaced Title   \n", "Fallback"),
            "Spaced Title"
        );
        assert_eq!(derive_title("", "Fallback"), "Fallback");
        assert_eq!(derive_title("   \n\t  ", "Fallback"), "Fallback");

        // Long title truncation (capped at 16 chars + ellipsis)
        let long_line = "This is a remarkably long line designed to test title truncation behavior";
        let derived = derive_title(long_line, "Fallback");
        assert!(derived.ends_with('…'));
        assert!(derived.chars().count() <= 17);

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
    fn test_data_dir_from_env_precedence_and_error() {
        assert_eq!(
            data_dir_from_env(Some("/xdg/data"), Some("/home/u"), Some("/run/user/1000")).unwrap(),
            PathBuf::from("/xdg/data/cosmic-scratchpad")
        );
        assert_eq!(
            data_dir_from_env(Some("   "), Some("/home/u"), Some("/run/user/1000")).unwrap(),
            PathBuf::from("/home/u/.local/share/cosmic-scratchpad")
        );
        assert_eq!(
            data_dir_from_env(None, Some(""), Some("/run/user/1000")).unwrap(),
            PathBuf::from("/run/user/1000/cosmic-scratchpad")
        );

        let err = data_dir_from_env(None, Some("  "), None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn test_list_and_create_notes_in_dir() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join("notes");

        // 1. Calling create_new_note_in_dir directly in empty directory creates pad_1.md only
        let p1 = create_new_note_in_dir(&data_dir, None).expect("Failed to create initial note");
        assert!(p1.ends_with("pad_1.md"));
        assert_eq!(
            fs::metadata(&data_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&p1).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let notes_after_init = list_notes_in_dir(&data_dir).unwrap();
        assert_eq!(notes_after_init.len(), 1);
        assert_eq!(notes_after_init[0], p1);

        // 2. Create pad_2.md with title
        let p2 = create_new_note_in_dir(&data_dir, Some("Meeting Notes"))
            .expect("Failed to create note");
        assert!(p2.ends_with("pad_2.md"));
        let p2_content = fs::read_to_string(&p2).unwrap();
        assert_eq!(p2_content, "# Meeting Notes\n\n");

        // 3. Create pad_3.md without title
        let p3 = create_new_note_in_dir(&data_dir, None).expect("Failed to create note 3");
        assert!(p3.ends_with("pad_3.md"));
        assert_eq!(fs::read_to_string(&p3).unwrap(), "");

        // 4. Verify list order
        let notes_all = list_notes_in_dir(&data_dir).unwrap();
        assert_eq!(notes_all.len(), 3);
        assert!(notes_all[0].ends_with("pad_1.md"));
        assert!(notes_all[1].ends_with("pad_2.md"));
        assert!(notes_all[2].ends_with("pad_3.md"));

        // 5. Delete note and verify private trash dir and file permissions
        fs::set_permissions(&p2, fs::Permissions::from_mode(0o644)).unwrap();
        let deleted_content = delete_note(&p2).expect("Failed to delete note");
        assert_eq!(deleted_content, "# Meeting Notes\n\n");
        assert!(!p2.exists());

        let trash = data_dir.join("trash");
        assert_eq!(
            fs::metadata(&trash).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let trash_entries: Vec<_> = fs::read_dir(&trash)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        let trashed_p2 = trash_entries
            .iter()
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .ends_with("_pad_2.md")
            })
            .expect("trashed pad_2.md missing");
        assert_eq!(
            fs::metadata(trashed_p2).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn test_ensure_private_dir_leaves_parent_mode_untouched() {
        let temp_dir = tempfile::tempdir().unwrap();
        let parent = temp_dir.path().join("share");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();

        let target = parent.join("cosmic-scratchpad");
        ensure_private_dir(&target).unwrap();

        assert_eq!(
            fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn test_tighten_dir_permissions() {
        let temp_dir = tempfile::tempdir().unwrap();
        let loose = temp_dir.path().join("loose");
        fs::create_dir(&loose).unwrap();
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o755)).unwrap();

        tighten_dir_permissions(&loose);
        assert_eq!(
            fs::metadata(&loose).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let ro = temp_dir.path().join("readonly");
        fs::create_dir(&ro).unwrap();
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o500)).unwrap();
        tighten_dir_permissions(&ro);
        assert_eq!(
            fs::metadata(&ro).unwrap().permissions().mode() & 0o777,
            0o500
        );
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o700)).unwrap();

        let missing = temp_dir.path().join("does_not_exist");
        tighten_dir_permissions(&missing);
        assert!(!missing.exists());
    }

    #[test]
    fn test_delete_note_on_unwritable_trash_preserves_original() {
        let temp_dir = tempfile::tempdir().unwrap();
        let note_path = temp_dir.path().join("pad_1.md");
        atomic_write(&note_path, "keep me").unwrap();

        let trash = temp_dir.path().join("trash");
        fs::create_dir(&trash).unwrap();
        fs::set_permissions(&trash, fs::Permissions::from_mode(0o500)).unwrap();

        // Root ignores 0o500 directory permissions.
        let probe = trash.join(".probe");
        if OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .is_ok()
        {
            let _ = fs::remove_file(&probe);
            fs::set_permissions(&trash, fs::Permissions::from_mode(0o700)).unwrap();
            return;
        }

        let res = delete_note(&note_path);
        fs::set_permissions(&trash, fs::Permissions::from_mode(0o700)).unwrap();

        assert!(res.is_err());
        assert_eq!(fs::read_to_string(&note_path).unwrap(), "keep me");
    }

    #[test]
    fn test_purge_old_trash() {
        let temp_dir = tempfile::tempdir().unwrap();
        let trash = temp_dir.path().join("trash");
        let missing = temp_dir.path().join("missing_trash");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);

        assert_eq!(purge_old_trash(&missing, 30, now).unwrap(), 0);

        fs::create_dir(&trash).unwrap();
        let old_file = trash.join("1_pad_1.md");
        let boundary_file = trash.join("2_pad_2.md");
        let recent_file = trash.join("3_pad_3.md");
        let nested_dir = trash.join("subdir");

        fs::write(&old_file, "old").unwrap();
        fs::write(&boundary_file, "boundary").unwrap();
        fs::write(&recent_file, "recent").unwrap();
        fs::create_dir(&nested_dir).unwrap();

        fs::File::options()
            .write(true)
            .open(&old_file)
            .unwrap()
            .set_modified(now - Duration::from_secs(31 * 86_400))
            .unwrap();
        fs::File::options()
            .write(true)
            .open(&boundary_file)
            .unwrap()
            .set_modified(now - Duration::from_secs(30 * 86_400))
            .unwrap();
        fs::File::options()
            .write(true)
            .open(&recent_file)
            .unwrap()
            .set_modified(now - Duration::from_secs(5 * 86_400))
            .unwrap();

        assert_eq!(purge_old_trash(&trash, 0, now).unwrap(), 0);
        assert!(old_file.exists());
        assert!(boundary_file.exists());
        assert!(recent_file.exists());

        assert_eq!(purge_old_trash(&trash, 30, now).unwrap(), 2);
        assert!(!old_file.exists());
        assert!(!boundary_file.exists());
        assert!(recent_file.exists());
        assert!(nested_dir.exists());
    }

    #[test]
    fn test_save_note_checked_clean_and_conflict() {
        let temp_dir = tempfile::tempdir().unwrap();
        let note_path = temp_dir.path().join("pad_1.md");

        // 1. Clean initial write
        let res = save_note_checked(&note_path, "v1", None).unwrap();
        let mtime1 = match res {
            SaveResult::Saved(Some(t)) => t,
            other => panic!("expected Saved with mtime, got {other:?}"),
        };
        assert_eq!(fs::read_to_string(&note_path).unwrap(), "v1");
        assert_eq!(
            fs::metadata(&note_path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        // 2. Clean overwrite with matching expected_mtime
        let res = save_note_checked(&note_path, "v2", Some(mtime1)).unwrap();
        let mtime2 = match res {
            SaveResult::Saved(Some(t)) => t,
            other => panic!("expected Saved with mtime, got {other:?}"),
        };
        assert_eq!(fs::read_to_string(&note_path).unwrap(), "v2");

        // 3. External edit with different content and changed mtime triggers conflict
        fs::write(&note_path, "external edit").unwrap();
        let external_mtime = mtime2 + Duration::from_secs(10);
        fs::File::options()
            .write(true)
            .open(&note_path)
            .unwrap()
            .set_modified(external_mtime)
            .unwrap();

        let conflict = save_note_checked(&note_path, "local draft", Some(mtime2)).unwrap();
        match conflict {
            SaveResult::Conflict {
                conflict_path,
                disk_content,
                disk_mtime,
            } => {
                assert_eq!(disk_content, "external edit");
                assert_eq!(disk_mtime, external_mtime);
                assert_eq!(conflict_path.parent(), Some(temp_dir.path()));
                let fname = conflict_path.file_name().unwrap().to_string_lossy();
                assert!(fname.starts_with("pad_1.conflict-"));
                assert!(fname.ends_with(".md"));
                assert_eq!(fs::read_to_string(&conflict_path).unwrap(), "local draft");
                assert_eq!(
                    fs::metadata(&conflict_path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                assert_eq!(fs::read_to_string(&note_path).unwrap(), "external edit");
            }
            other => panic!("expected Conflict, got {other:?}"),
        }

        // 4. External mtime change with identical content returns Saved without conflict file
        let count_before = fs::read_dir(temp_dir.path()).unwrap().count();
        let res = save_note_checked(&note_path, "external edit", Some(mtime2)).unwrap();
        assert_eq!(res, SaveResult::Saved(Some(external_mtime)));
        assert_eq!(fs::read_dir(temp_dir.path()).unwrap().count(), count_before);

        // 5. expected_mtime == None on non-empty disk file triggers conflict
        let res = save_note_checked(&note_path, "blind overwrite", None).unwrap();
        let conflict2_path = match res {
            SaveResult::Conflict { conflict_path, .. } => conflict_path,
            other => panic!("expected Conflict, got {other:?}"),
        };
        assert_eq!(fs::read_to_string(&note_path).unwrap(), "external edit");
        assert_eq!(
            fs::read_to_string(&conflict2_path).unwrap(),
            "blind overwrite"
        );
        // Distinct conflict files so repeated conflicts do not overwrite each other
        assert_ne!(
            fs::read_dir(temp_dir.path())
                .unwrap()
                .filter(|e| e
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".conflict-"))
                .count(),
            1
        );
    }

    #[test]
    fn test_delete_note_on_symlink_preserves_target_and_copies_to_trash() {
        let temp_dir = tempfile::tempdir().unwrap();
        let real_file = temp_dir.path().join("target.md");
        let symlink_file = temp_dir.path().join("link_note.md");

        atomic_write(&real_file, "real note content").unwrap();
        std::os::unix::fs::symlink(&real_file, &symlink_file).unwrap();

        let deleted = delete_note(&symlink_file).unwrap();
        assert_eq!(deleted, "real note content");
        assert!(fs::symlink_metadata(&symlink_file).is_err());
        assert_eq!(fs::read_to_string(&real_file).unwrap(), "real note content");

        let trash = temp_dir.path().join("trash");
        let trashed: Vec<_> = fs::read_dir(&trash)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(trashed.len(), 1);
        assert!(
            !fs::symlink_metadata(&trashed[0])
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(&trashed[0]).unwrap(),
            "real note content"
        );
        assert_eq!(
            fs::metadata(&trashed[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn test_atomic_write_creates_file_and_parent_dirs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("nested").join("dirs").join("pad_1.md");

        let text = "Hello nested directory!";
        atomic_write(&file_path, text).expect("Failed to create file and parent dirs");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), text);
    }

    #[test]
    fn test_atomic_write_overwrites_cleanly() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("pad_1.md");

        let initial_text = "# Initial Content";
        atomic_write(&file_path, initial_text).expect("Failed initial write");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), initial_text);

        let updated_text = "# Updated Content";
        atomic_write(&file_path, updated_text).expect("Failed overwrite");
        assert_eq!(fs::read_to_string(&file_path).unwrap(), updated_text);
    }

    #[test]
    fn test_atomic_write_leaves_no_stale_tmp_files() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("pad_1.md");

        atomic_write(&file_path, "Test without leftover").expect("Failed atomic write");

        let entries: Vec<_> = fs::read_dir(temp_dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();

        assert_eq!(entries, vec!["pad_1.md"]);
        assert!(entries.iter().all(|name| !name.contains(".tmp.")));
    }

    #[test]
    fn test_concurrent_atomic_writes_do_not_collide() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("pad_1.md");
        atomic_write(&file_path, "seed").unwrap();

        let contents: Vec<String> = (0..16).map(|i| format!("{i}").repeat(4096)).collect();
        std::thread::scope(|scope| {
            let handles: Vec<_> = contents
                .iter()
                .map(|content| {
                    let path = &file_path;
                    scope.spawn(move || atomic_write(path, content))
                })
                .collect();
            for handle in handles {
                handle.join().unwrap().unwrap();
            }
        });

        // Every write succeeds against its own temp file, so the result is one
        // complete payload rather than an interleaving of several.
        let on_disk = fs::read_to_string(&file_path).unwrap();
        assert!(contents.contains(&on_disk));

        let leftovers: Vec<_> = fs::read_dir(temp_dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "stale temp files: {leftovers:?}");
    }

    #[test]
    fn test_atomic_write_failure_returns_err_and_cleans_up() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("pad_1.md");
        atomic_write(&file_path, "original").unwrap();

        fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let result = atomic_write(&file_path, "lost?");
        fs::set_permissions(temp_dir.path(), fs::Permissions::from_mode(0o700)).unwrap();

        // Root ignores directory permissions; nothing to assert in that case.
        if result.is_ok() {
            return;
        }
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "original");
        let names: Vec<_> = fs::read_dir(temp_dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["pad_1.md"]);
    }

    #[test]
    fn test_atomic_write_preserves_symlink() {
        let temp_dir = tempfile::tempdir().unwrap();
        let real_file = temp_dir.path().join("real_pad.md");
        let symlink_file = temp_dir.path().join("symlink_pad.md");

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
    }

    #[test]
    fn test_atomic_write_preserves_permissions() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("restricted.md");

        atomic_write(&file_path, "Secret notes").expect("Failed initial write");
        assert_eq!(
            fs::metadata(&file_path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o640))
            .expect("Failed to set 0640 mode");

        // Overwrite and check permissions
        atomic_write(&file_path, "Updated secret notes").expect("Failed overwrite");
        let perms = fs::metadata(&file_path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o640);
    }

    #[test]
    fn test_create_new_note_on_empty_dir_creates_single_file() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Creating on an empty dir should produce pad_1.md and nothing else
        let note = create_new_note_in_dir(temp_dir.path(), Some("First Note"))
            .expect("Failed to create note in empty dir");
        assert!(note.ends_with("pad_1.md"));

        let files = list_notes_in_dir(temp_dir.path()).expect("Failed to list notes");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], note);
    }

    #[test]
    fn test_trash_retention_multiple_deletions() {
        let temp_dir = tempfile::tempdir().unwrap();

        let p1 = create_new_note_in_dir(temp_dir.path(), Some("Note V1")).unwrap();
        delete_note(&p1).unwrap();

        // Create another note with same name (pad_1.md)
        let p1_again = create_new_note_in_dir(temp_dir.path(), Some("Note V2")).unwrap();
        assert_eq!(p1, p1_again);
        delete_note(&p1_again).unwrap();

        let trash_dir = temp_dir.path().join("trash");
        let trashed: Vec<_> = fs::read_dir(trash_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();

        // Ensure both versions exist in trash and did not overwrite each other
        assert_eq!(trashed.len(), 2);
        assert!(trashed.iter().all(|f| f.ends_with("_pad_1.md")));
    }
}
