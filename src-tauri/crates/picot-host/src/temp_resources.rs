// ABOUTME: Owns private temporary directories for native ephemeral runtimes.
// ABOUTME: Enforces root containment and token ownership before cleanup.

use rand::rngs::OsRng;
use rand::RngCore;
use std::path::{Path, PathBuf};

const QUICK_CHAT_TEMP_PREFIX: &str = "picot-quick-chat-";
const QUICK_CHAT_TOKEN_BYTES: usize = 16;

/// Ensure Picot's private temp root exists beneath `home`, is owner-only, and
/// returns one canonical path for all create/cleanup operations.
pub fn ensure_picot_tmp_root_in(home: &Path) -> Result<PathBuf, String> {
    let dir = home.join(".pi").join("tmp");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {}", dir.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Cannot restrict {}: {}", dir.display(), e))?;
    }
    dir.canonicalize()
        .map_err(|e| format!("Cannot resolve {}: {}", dir.display(), e))
}

/// Resolve the process-wide private temp root. Cache it so create and cleanup
/// cannot disagree if another thread changes process environment during tests.
pub(crate) fn ensure_picot_tmp_root() -> Result<PathBuf, String> {
    static PICOT_TMP_ROOT: std::sync::OnceLock<Result<PathBuf, String>> =
        std::sync::OnceLock::new();
    PICOT_TMP_ROOT
        .get_or_init(|| match dirs::home_dir() {
            Some(home) => ensure_picot_tmp_root_in(&home),
            None => {
                log::warn!("[picot-native] HOME unavailable; falling back to OS temp root");
                std::env::temp_dir()
                    .canonicalize()
                    .or(Ok(std::env::temp_dir()))
            }
        })
        .clone()
}

pub fn canonical_temp_root() -> PathBuf {
    ensure_picot_tmp_root().unwrap_or_else(|error| {
        log::warn!("[picot-native] temp root unavailable ({error}); using OS temp dir");
        std::env::temp_dir()
    })
}

/// Create one owner-private ephemeral runtime directory and return its token.
pub(crate) fn create_quick_chat_temp_dir() -> Result<(PathBuf, String), String> {
    create_quick_chat_temp_dir_in(&canonical_temp_root())
}

pub fn create_quick_chat_temp_dir_in(root: &Path) -> Result<(PathBuf, String), String> {
    loop {
        let token = random_hex_token();
        let candidate = root.join(format!("{QUICK_CHAT_TEMP_PREFIX}{token}"));
        match std::fs::create_dir(&candidate) {
            Ok(()) => {
                let canonical = candidate.canonicalize().map_err(|e| e.to_string())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        &canonical,
                        std::fs::Permissions::from_mode(0o700),
                    );
                }
                return Ok((canonical, token));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// Delete exactly one owner-private directory after validating every boundary.
pub fn cleanup_quick_chat_dir(
    canonical_temp_root: &Path,
    exact_path: &Path,
    ownership_token: &str,
) -> Result<(), String> {
    let input_meta = std::fs::symlink_metadata(exact_path).map_err(|e| e.to_string())?;
    if input_meta.file_type().is_symlink() {
        return Err("refusing to delete a symlink".to_string());
    }
    let canonical = exact_path.canonicalize().map_err(|e| e.to_string())?;
    let canonical_root = canonical_temp_root
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if canonical == canonical_root {
        return Err("refusing to delete the temporary root".to_string());
    }
    if !canonical.starts_with(&canonical_root) {
        return Err("path is outside the temporary root".to_string());
    }
    let expected_name = format!("{QUICK_CHAT_TEMP_PREFIX}{ownership_token}");
    let actual_name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if actual_name != expected_name {
        return Err("ownership token mismatch".to_string());
    }
    std::fs::remove_dir_all(&canonical).map_err(|e| e.to_string())
}

fn random_hex_token() -> String {
    let mut bytes = [0u8; QUICK_CHAT_TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_cleans_owner_private_directory() {
        let root = tempfile::tempdir().unwrap();
        let (path, token) = create_quick_chat_temp_dir_in(root.path()).unwrap();
        cleanup_quick_chat_dir(root.path(), &path, &token).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn rejects_wrong_token_and_outside_path() {
        let root = tempfile::tempdir().unwrap();
        let (path, token) = create_quick_chat_temp_dir_in(root.path()).unwrap();
        assert!(cleanup_quick_chat_dir(root.path(), &path, "wrong").is_err());
        let outside = tempfile::tempdir().unwrap();
        assert!(cleanup_quick_chat_dir(root.path(), outside.path(), &token).is_err());
        cleanup_quick_chat_dir(root.path(), &path, &token).unwrap();
    }
}
