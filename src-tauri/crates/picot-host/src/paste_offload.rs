// ABOUTME: Writes bounded composer paste payloads into workspace-local temporary storage.
// ABOUTME: Enforces containment, private permissions, expiry cleanup, and aggregate quota.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const MAX_PASTE_BYTES: usize = 4 * 1024 * 1024;
const MAX_DIRECTORY_BYTES: u64 = 32 * 1024 * 1024;
const EXPIRY: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, PartialEq, Eq)]
pub enum PasteError {
    TooLarge,
    InvalidWorkspace,
    Symlink,
    QuotaExceeded,
    Io,
}

/// Legacy parity: `<tmp>/.gitignore` with `*` + `!.gitignore` keeps pasted
/// content out of `git status` (legacy `paste-offload.ts:80-83`). Skipped
/// when the entry is a symlink or unwritable — never a hard failure.
fn maintain_self_ignore(tmp: &std::path::Path) {
    let gitignore = tmp.join(".gitignore");
    if gitignore
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return;
    }
    if gitignore.is_file() {
        return;
    }
    let _ = fs::write(&gitignore, "*\n!.gitignore\n");
}

pub fn write(workspace: &Path, content: &str, now: SystemTime) -> Result<String, PasteError> {
    if content.len() > MAX_PASTE_BYTES {
        return Err(PasteError::TooLarge);
    }
    let root = workspace
        .canonicalize()
        .map_err(|_| PasteError::InvalidWorkspace)?;
    if !root.is_dir() {
        return Err(PasteError::InvalidWorkspace);
    }
    let pi = checked_directory(&root, &root.join(".pi"))?;
    let tmp = checked_directory(&root, &pi.join("tmp"))?;
    cleanup(&tmp, now);
    maintain_self_ignore(&tmp);

    let current = directory_bytes(&tmp);
    if current.saturating_add(content.len() as u64) > MAX_DIRECTORY_BYTES {
        return Err(PasteError::QuotaExceeded);
    }
    let stamp = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| PasteError::Io)?
        .as_secs();
    for index in 0..100u32 {
        let suffix = if index == 0 {
            String::new()
        } else {
            format!("-{index}")
        };
        let name = format!("paste-{stamp}{suffix}.txt");
        let path = tmp.join(&name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(PasteError::Io),
        };
        // The directory containment checks ran before the open; a directory
        // swapped for a symlink in between would redirect this file outside
        // the workspace. Re-resolve before any content is written so a
        // redirected paste never leaves secret bytes outside the workspace.
        if !std::fs::canonicalize(&path)
            .map_err(|_| PasteError::Io)?
            .starts_with(&tmp)
        {
            let _ = fs::remove_file(&path);
            return Err(PasteError::Symlink);
        }
        if file
            .write_all(content.as_bytes())
            .and_then(|_| file.sync_all())
            .is_err()
        {
            let _ = fs::remove_file(&path);
            return Err(PasteError::Io);
        }
        return Ok(format!(".pi/tmp/{name}"));
    }
    Err(PasteError::QuotaExceeded)
}

fn checked_directory(root: &Path, directory: &Path) -> Result<PathBuf, PasteError> {
    if directory.exists() {
        if fs::symlink_metadata(directory)
            .map_err(|_| PasteError::Io)?
            .file_type()
            .is_symlink()
        {
            return Err(PasteError::Symlink);
        }
        if !directory.is_dir() {
            return Err(PasteError::InvalidWorkspace);
        }
    } else {
        fs::create_dir(directory).map_err(|_| PasteError::Io)?;
        set_private(directory)?;
    }
    let canonical = directory.canonicalize().map_err(|_| PasteError::Io)?;
    if !canonical.starts_with(root) {
        return Err(PasteError::InvalidWorkspace);
    }
    Ok(canonical)
}

fn cleanup(directory: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_file()
            || !path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("paste-"))
        {
            continue;
        }
        if meta
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= EXPIRY)
        {
            let _ = fs::remove_file(path);
        }
    }
}

fn directory_bytes(directory: &Path) -> u64 {
    fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let meta = fs::symlink_metadata(entry.path()).ok()?;
            meta.file_type().is_file().then_some(meta.len())
        })
        .sum()
}

fn set_private(path: &Path) -> Result<(), PasteError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| PasteError::Io)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn writes_private_relative_file_and_expires_old_files() {
        let dir = tempdir().unwrap();
        let now = SystemTime::now() + EXPIRY + Duration::from_secs(1);
        let old = dir.path().join(".pi/tmp");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("paste-old.txt"), "old").unwrap();
        let path = write(dir.path(), "hello", now).unwrap();
        let stamp = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(path, format!(".pi/tmp/paste-{stamp}.txt"));
        assert!(!old.join("paste-old.txt").exists());
        assert_eq!(fs::read_to_string(dir.path().join(path)).unwrap(), "hello");
    }

    #[test]
    fn rejects_oversized_content_and_symlinked_tmp() {
        let dir = tempdir().unwrap();
        assert_eq!(
            write(
                dir.path(),
                &"x".repeat(MAX_PASTE_BYTES + 1),
                SystemTime::now()
            ),
            Err(PasteError::TooLarge)
        );
        let outside = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".pi")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), dir.path().join(".pi/tmp")).unwrap();
        #[cfg(unix)]
        assert_eq!(
            write(dir.path(), "x", SystemTime::now()),
            Err(PasteError::Symlink)
        );
    }
}
