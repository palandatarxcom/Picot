// ABOUTME: Owner-independent filesystem contract for registered workspace data.
// ABOUTME: Enforces canonical containment, bounded reads, and atomic owner-safe writes.

#[cfg(unix)]
use std::ffi::CString;
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::path::{Component, Path, PathBuf};
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    InvalidPath,
    /// A create/rename target that is not a single path component (empty, `.`,
    /// `..`, separator, NUL). Distinct from `InvalidPath` so the WebView can
    /// point at the name field rather than the path.
    InvalidName,
    OutsideWorkspace,
    NotFound,
    AlreadyExists,
    IsDirectory,
    DirectoryNotEmpty,
    TooLarge,
    Conflict,
    PermissionDenied,
    /// Reachable a moment ago, unreachable now: unmounted volume, offline
    /// share, stale network handle. Never a deletion signal.
    TemporarilyUnavailable,
    Io(String),
}

impl FileError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidPath => "invalid_path",
            Self::InvalidName => "invalid_name",
            Self::OutsideWorkspace => "path_outside_workspace",
            Self::NotFound => "file_not_found",
            Self::AlreadyExists => "already_exists",
            Self::IsDirectory => "is_directory",
            Self::DirectoryNotEmpty => "directory_not_empty",
            Self::TooLarge => "file_too_large",
            Self::Conflict => "file_conflict",
            Self::PermissionDenied => "permission_denied",
            Self::TemporarilyUnavailable => "temporarily_unavailable",
            Self::Io(_) => "file_access_failed",
        }
    }
}

/// What a `create` call should produce. Both variants refuse to overwrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContent {
    pub relative_path: String,
    pub bytes: Vec<u8>,
    pub modified_at_ms: u128,
}

pub fn resolve_existing(root: &Path, relative: &str) -> Result<PathBuf, FileError> {
    validate_relative(relative)?;
    let root = root.canonicalize().map_err(io_error)?;
    let path = root.join(relative).canonicalize().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            FileError::NotFound
        } else {
            io_error(error)
        }
    })?;
    if !path.starts_with(&root) {
        return Err(FileError::OutsideWorkspace);
    }
    Ok(path)
}

pub fn read(root: &Path, relative: &str) -> Result<FileContent, FileError> {
    read_with_cap(root, relative, MAX_FILE_BYTES)
}

/// Same contract as [`read`] with a caller-selected byte cap. The generic
/// path stays at MAX_FILE_BYTES; only candidate Office preview reads may
/// raise it (spec 2026-09-17: 32 MiB through the preview branch).
pub fn read_with_cap(
    root: &Path,
    relative: &str,
    max_bytes: u64,
) -> Result<FileContent, FileError> {
    let path = resolve_existing(root, relative)?;
    let metadata = fs::metadata(&path).map_err(io_error)?;
    if metadata.is_dir() {
        return Err(FileError::IsDirectory);
    }
    if metadata.len() > max_bytes {
        return Err(FileError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    // The stat above can race a concurrent writer (the workspace's own Pi
    // agent edits files); take() keeps the read bounded even when the file
    // grows between the check and the read.
    fs::File::open(&path)
        .map_err(io_error)?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > max_bytes {
        return Err(FileError::TooLarge);
    }
    Ok(FileContent {
        relative_path: relative.replace('\\', "/"),
        bytes,
        modified_at_ms: modified_ms(&metadata),
    })
}

/// Create a new empty file or directory inside `parent_relative`.
///
/// `parent_relative` uses `"."` for the workspace root. The target must not
/// exist: both kinds go through an exclusive create, so a concurrent agent
/// writing the same name wins or loses cleanly instead of being overwritten.
pub fn create(
    root: &Path,
    parent_relative: &str,
    name: &str,
    kind: CreateKind,
) -> Result<String, FileError> {
    validate_name(name)?;
    let root = root.canonicalize().map_err(classify_io_error)?;
    let parent = resolve_existing(&root, parent_relative)?;
    if !parent.is_dir() {
        return Err(FileError::InvalidPath);
    }
    let target = parent.join(name);
    match kind {
        CreateKind::File => {
            create_exclusive_file(&target)?;
        }
        CreateKind::Directory => {
            fs::create_dir(&target).map_err(classify_io_error)?;
        }
    }
    relative_within(&root, &target)
}

/// Exclusive create: never truncates an existing file, never follows a symlink
/// onto one. Mode 0600 on unix so a fresh file matches `write`'s atomic path.
fn create_exclusive_file(path: &Path) -> Result<(), FileError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(classify_io_error)?;
    #[cfg(not(unix))]
    restrict_permissions(path)?;
    Ok(())
}

/// Rename within the same parent directory. `name` is a basename, never a path,
/// so this can never act as a move API; the target must not exist.
pub fn rename(root: &Path, relative: &str, name: &str) -> Result<String, FileError> {
    validate_name(name)?;
    let root = root.canonicalize().map_err(classify_io_error)?;
    let source = resolve_existing(&root, relative)?;
    if source == root {
        return Err(FileError::InvalidPath);
    }
    let parent = source.parent().ok_or(FileError::InvalidPath)?;
    let target = parent.join(name);
    match fs::symlink_metadata(&target) {
        Ok(_) => return Err(FileError::AlreadyExists),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(classify_io_error(error)),
    }
    fs::rename(&source, &target).map_err(classify_io_error)?;
    relative_within(&root, &target)
}

/// Delete a file, or an empty directory.
///
/// A non-empty directory is refused with `directory_not_empty`. No recursion,
/// no trash, no undo: that is the safety boundary of this data plane, not a
/// gap to fill later.
pub fn remove(root: &Path, relative: &str) -> Result<String, FileError> {
    let root = root.canonicalize().map_err(classify_io_error)?;
    let target = resolve_existing(&root, relative)?;
    if target == root {
        return Err(FileError::InvalidPath);
    }
    // Resolve the reply path before the delete: afterwards the path is gone and
    // any re-derivation would have nothing to look at.
    let relative_path = relative_within(&root, &target)?;
    let metadata = fs::symlink_metadata(&target).map_err(classify_io_error)?;
    if metadata.is_dir() {
        fs::remove_dir(&target).map_err(classify_io_error)?;
    } else {
        fs::remove_file(&target).map_err(classify_io_error)?;
    }
    Ok(relative_path)
}

pub fn write(
    root: &Path,
    relative: &str,
    bytes: &[u8],
    expected_modified_at_ms: Option<u128>,
) -> Result<u128, FileError> {
    validate_relative(relative)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(FileError::TooLarge);
    }
    let root = root.canonicalize().map_err(io_error)?;
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        let parent = parent.canonicalize().map_err(io_error)?;
        if !parent.starts_with(&root) {
            return Err(FileError::OutsideWorkspace);
        }
    }
    let existing = fs::symlink_metadata(&path).ok();
    if let Some(metadata) = existing {
        if metadata.file_type().is_symlink() {
            return Err(FileError::OutsideWorkspace);
        }
        if metadata.is_dir() {
            return Err(FileError::IsDirectory);
        }
        if expected_modified_at_ms.is_some()
            && expected_modified_at_ms != Some(modified_ms(&metadata))
        {
            return Err(FileError::Conflict);
        }
    } else if expected_modified_at_ms.is_some() {
        return Err(FileError::Conflict);
    }
    #[cfg(unix)]
    {
        atomic_replace_unix(&root, relative, bytes)
    }
    #[cfg(not(unix))]
    let file_name = path.file_name().ok_or(FileError::InvalidPath)?;
    #[cfg(not(unix))]
    {
        let temporary = path.with_file_name(format!(
            ".{}.picot-tmp-{}",
            file_name.to_string_lossy(),
            uuid::Uuid::new_v4()
        ));
        fs::write(&temporary, bytes).map_err(io_error)?;
        if let Err(error) = restrict_permissions(&temporary) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(io_error(error));
        }
        let resolved = path.canonicalize().map_err(io_error)?;
        if resolved.strip_prefix(&root).is_err() {
            return Err(FileError::OutsideWorkspace);
        }
        Ok(modified_ms(&fs::metadata(&resolved).map_err(io_error)?))
    }
}

#[cfg(unix)]
fn atomic_replace_unix(root: &Path, relative: &str, bytes: &[u8]) -> Result<u128, FileError> {
    use std::io::Write;

    let root_bytes =
        CString::new(root.as_os_str().as_bytes()).map_err(|_| FileError::InvalidPath)?;
    let root_fd = unsafe {
        libc::open(
            root_bytes.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    let mut directory = unsafe { std::fs::File::from_raw_fd(root_fd) };
    let components: Vec<_> = Path::new(relative).components().collect();
    let file_name = components
        .last()
        .and_then(|component| match component {
            Component::Normal(name) => Some(*name),
            _ => None,
        })
        .ok_or(FileError::InvalidPath)?;
    for component in &components[..components.len() - 1] {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = CString::new(name.as_bytes()).map_err(|_| FileError::InvalidPath)?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    let temp_name = format!(
        ".{}.picot-tmp-{}",
        file_name.to_string_lossy(),
        uuid::Uuid::new_v4()
    );
    let temp_name = CString::new(temp_name.as_bytes()).map_err(|_| FileError::InvalidPath)?;
    let target_name = CString::new(file_name.as_bytes()).map_err(|_| FileError::InvalidPath)?;
    let temp_fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            temp_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if temp_fd < 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    let mut temporary = unsafe { std::fs::File::from_raw_fd(temp_fd) };
    if let Err(error) = temporary
        .write_all(bytes)
        .and_then(|_| temporary.sync_all())
    {
        unsafe { libc::unlinkat(directory.as_raw_fd(), temp_name.as_ptr(), 0) };
        return Err(io_error(error));
    }
    drop(temporary);
    if unsafe {
        libc::renameat(
            directory.as_raw_fd(),
            temp_name.as_ptr(),
            directory.as_raw_fd(),
            target_name.as_ptr(),
        )
    } < 0
    {
        unsafe { libc::unlinkat(directory.as_raw_fd(), temp_name.as_ptr(), 0) };
        return Err(io_error(std::io::Error::last_os_error()));
    }
    let target_fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            target_name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if target_fd < 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    let target = unsafe { std::fs::File::from_raw_fd(target_fd) };
    Ok(modified_ms(&target.metadata().map_err(io_error)?))
}

fn validate_relative(value: &str) -> Result<(), FileError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(FileError::InvalidPath);
    }
    Ok(())
}

/// A create/rename target must be exactly one path component. `\` is rejected
/// alongside `/` so the contract reads identically on every platform, and `\0`
/// is rejected because the OS would fail on it with an opaque error.
fn validate_name(name: &str) -> Result<(), FileError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(FileError::InvalidName);
    }
    Ok(())
}

/// Reply path for a target the caller has already resolved inside `root`.
/// Containment is by construction (canonical parent + single-component name);
/// this only projects it back onto the wire as a workspace-relative path.
fn relative_within(root: &Path, path: &Path) -> Result<String, FileError> {
    path.strip_prefix(root)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .map_err(|_| FileError::OutsideWorkspace)
}

/// Map an OS error onto the closed code set. The transient bucket exists so an
/// unmounted volume or an offline share never reads as "the user deleted this":
/// the WebView keeps its cache on the former and invalidates on the latter.
fn classify_io_error(error: std::io::Error) -> FileError {
    match error.kind() {
        std::io::ErrorKind::NotFound => FileError::NotFound,
        std::io::ErrorKind::AlreadyExists => FileError::AlreadyExists,
        std::io::ErrorKind::PermissionDenied => FileError::PermissionDenied,
        std::io::ErrorKind::DirectoryNotEmpty => FileError::DirectoryNotEmpty,
        std::io::ErrorKind::NotConnected
        | std::io::ErrorKind::TimedOut
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::ConnectionReset => FileError::TemporarilyUnavailable,
        _ if is_transient_os_error(&error) => FileError::TemporarilyUnavailable,
        _ => io_error(error),
    }
}

/// Errno values that mean "reachable a moment ago, unreachable now" rather than
/// "this path is wrong". Unix-only: Windows reports these through `ErrorKind`.
#[cfg(unix)]
fn is_transient_os_error(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ESTALE)
            | Some(libc::EIO)
            | Some(libc::ENXIO)
            | Some(libc::ENOTCONN)
            | Some(libc::EHOSTDOWN)
            | Some(libc::EHOSTUNREACH)
            | Some(libc::ETIMEDOUT)
    )
}

#[cfg(not(unix))]
fn is_transient_os_error(_error: &std::io::Error) -> bool {
    false
}

fn modified_ms(metadata: &fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_millis())
}

fn io_error(error: std::io::Error) -> FileError {
    FileError::Io(error.to_string())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), FileError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn read_with_cap_raises_the_limit_for_candidate_office_reads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        // 9 MiB: above the generic 8 MiB cap, inside a 32 MiB candidate cap.
        let payload = vec![0u8; 9 * 1024 * 1024];
        fs::write(root.join("big.docx"), &payload).unwrap();
        assert_eq!(
            read(&root, "big.docx"),
            Err(FileError::TooLarge),
            "the generic 8 MiB read cap must stay untouched"
        );
        let content = read_with_cap(&root, "big.docx", 32 * 1024 * 1024).unwrap();
        assert_eq!(content.bytes.len(), 9 * 1024 * 1024);
        assert_eq!(content.relative_path, "big.docx");
    }

    #[test]
    fn read_with_cap_preserves_validation_and_failure_modes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("small.docx"), b"hi").unwrap();
        assert_eq!(
            read_with_cap(&root, "../escape", 32 * 1024 * 1024),
            Err(FileError::InvalidPath)
        );
        assert_eq!(
            read_with_cap(&root, "missing.docx", 32 * 1024 * 1024),
            Err(FileError::NotFound)
        );
        assert_eq!(
            read_with_cap(&root, "small.docx", 1),
            Err(FileError::TooLarge),
            "the explicit cap is enforced, not the 8 MiB default"
        );
        assert_eq!(
            read_with_cap(&root, "small.docx", MAX_FILE_BYTES)
                .unwrap()
                .bytes,
            b"hi"
        );
    }

    #[test]
    fn rejects_traversal_and_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        assert_eq!(
            resolve_existing(&root, "../outside/secret"),
            Err(FileError::InvalidPath)
        );
        #[cfg(unix)]
        assert_eq!(
            resolve_existing(&root, "link/secret"),
            Err(FileError::OutsideWorkspace)
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_rejects_final_symlink_without_touching_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside.txt");
        fs::create_dir(&root).unwrap();
        fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("target.txt")).unwrap();
        assert_eq!(
            write(&root, "target.txt", b"inside", None),
            Err(FileError::OutsideWorkspace)
        );
        assert_eq!(fs::read(&outside).unwrap(), b"outside");
    }

    #[test]
    fn atomic_write_enforces_conflict_and_mode() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("root")).unwrap();
        let root = dir.path().join("root");
        let stamp = write(&root, "a.txt", b"one", None).unwrap();
        assert_eq!(read(&root, "a.txt").unwrap().bytes, b"one");
        assert_eq!(
            write(&root, "a.txt", b"two", Some(stamp + 1)),
            Err(FileError::Conflict)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(root.join("a.txt"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    fn workspace() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        (dir, root)
    }

    #[test]
    fn create_rejects_names_that_are_not_single_components() {
        let (_dir, root) = workspace();
        for name in ["", ".", "..", "a/b", "a\\b", "a\0b"] {
            assert_eq!(
                create(&root, ".", name, CreateKind::File),
                Err(FileError::InvalidName),
                "{name:?} must not be a create target"
            );
            assert_eq!(
                create(&root, ".", name, CreateKind::Directory),
                Err(FileError::InvalidName),
                "{name:?} must not be a create target"
            );
        }
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }

    #[test]
    fn create_rejects_parents_outside_the_workspace() {
        let (dir, root) = workspace();
        fs::create_dir(dir.path().join("outside")).unwrap();
        for parent in ["..", "../outside", "/etc", "src/../../outside"] {
            assert_eq!(
                create(&root, parent, "x", CreateKind::File),
                Err(FileError::InvalidPath),
                "{parent:?} must not be a create parent"
            );
        }
        assert_eq!(fs::read_dir(dir.path().join("outside")).unwrap().count(), 0);
    }

    #[test]
    fn create_makes_empty_files_and_directories_with_relative_replies() {
        let (_dir, root) = workspace();
        assert_eq!(
            create(&root, ".", "notes", CreateKind::Directory),
            Ok("notes".to_owned())
        );
        assert_eq!(
            create(&root, "notes", "a.md", CreateKind::File),
            Ok("notes/a.md".to_owned())
        );
        assert!(root.join("notes").is_dir());
        assert_eq!(fs::read(root.join("notes/a.md")).unwrap(), b"");
        // The root is spelled "." and nothing else: an empty parent is a bad path.
        assert_eq!(
            create(&root, "", "a.md", CreateKind::File),
            Err(FileError::InvalidPath)
        );
        assert_eq!(
            create(&root, "notes/a.md", "deep", CreateKind::Directory),
            Err(FileError::InvalidPath),
            "a file is not a parent directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_keeps_the_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, root) = workspace();
        create(&root, ".", "a.txt", CreateKind::File).unwrap();
        assert_eq!(
            fs::metadata(root.join("a.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn create_never_overwrites() {
        let (_dir, root) = workspace();
        fs::write(root.join("keep.txt"), b"original").unwrap();
        assert_eq!(
            create(&root, ".", "keep.txt", CreateKind::File),
            Err(FileError::AlreadyExists)
        );
        assert_eq!(fs::read(root.join("keep.txt")).unwrap(), b"original");
        fs::create_dir(root.join("keep-dir")).unwrap();
        assert_eq!(
            create(&root, ".", "keep-dir", CreateKind::Directory),
            Err(FileError::AlreadyExists)
        );
    }

    #[test]
    fn rename_stays_in_the_parent_directory() {
        let (dir, root) = workspace();
        fs::create_dir(root.join("notes")).unwrap();
        fs::write(root.join("notes/a.md"), b"body").unwrap();
        assert_eq!(
            rename(&root, "notes/a.md", "b.md"),
            Ok("notes/b.md".to_owned())
        );
        assert!(!root.join("notes/a.md").exists());
        assert_eq!(fs::read(root.join("notes/b.md")).unwrap(), b"body");
        // A separator in the name would turn rename into a move API.
        assert_eq!(
            rename(&root, "notes/b.md", "../escaped.md"),
            Err(FileError::InvalidName)
        );
        assert!(!dir.path().join("escaped.md").exists());
        assert_eq!(
            rename(&root, "notes/b.md", "deep/escaped.md"),
            Err(FileError::InvalidName)
        );
        assert_eq!(
            rename(&root, "notes/b.md", "b.md"),
            Err(FileError::AlreadyExists)
        );
        assert_eq!(
            rename(&root, "notes/ghost.md", "c.md"),
            Err(FileError::NotFound)
        );
    }

    #[test]
    fn rename_refuses_the_workspace_root() {
        let (_dir, root) = workspace();
        assert_eq!(rename(&root, ".", "renamed"), Err(FileError::InvalidPath));
        assert!(root.is_dir());
    }

    #[test]
    fn remove_deletes_files_and_empty_directories_only() {
        let (_dir, root) = workspace();
        fs::write(root.join("a.txt"), b"x").unwrap();
        assert_eq!(remove(&root, "a.txt"), Ok("a.txt".to_owned()));
        assert!(!root.join("a.txt").exists());

        fs::create_dir(root.join("empty")).unwrap();
        assert_eq!(remove(&root, "empty"), Ok("empty".to_owned()));
        assert!(!root.join("empty").exists());

        fs::create_dir(root.join("full")).unwrap();
        fs::write(root.join("full/keep.txt"), b"x").unwrap();
        assert_eq!(
            remove(&root, "full"),
            Err(FileError::DirectoryNotEmpty),
            "no recursion, no trash, no undo"
        );
        assert!(root.join("full/keep.txt").exists());

        assert_eq!(remove(&root, "ghost"), Err(FileError::NotFound));
        assert_eq!(remove(&root, "."), Err(FileError::InvalidPath));
        assert!(root.is_dir());
    }

    #[test]
    fn a_vanished_workspace_root_reads_as_not_found_for_mutations() {
        // §8.1: a mutation that hits ENOENT answers `not_found`. Only listings
        // separate transient unreachability, because only the tree holds a cache
        // that a deletion signal would throw away.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("unmounted");
        assert_eq!(
            create(&root, ".", "a.txt", CreateKind::File),
            Err(FileError::NotFound)
        );
        assert_eq!(remove(&root, "a.txt"), Err(FileError::NotFound));
        assert_eq!(rename(&root, "a.txt", "b.txt"), Err(FileError::NotFound));
    }

    #[test]
    fn classify_io_error_separates_transient_from_permanent() {
        use std::io::{Error, ErrorKind};
        assert_eq!(
            classify_io_error(Error::from(ErrorKind::NotFound)),
            FileError::NotFound
        );
        assert_eq!(
            classify_io_error(Error::from(ErrorKind::AlreadyExists)),
            FileError::AlreadyExists
        );
        assert_eq!(
            classify_io_error(Error::from(ErrorKind::PermissionDenied)),
            FileError::PermissionDenied
        );
        assert_eq!(
            classify_io_error(Error::from(ErrorKind::DirectoryNotEmpty)),
            FileError::DirectoryNotEmpty
        );
        assert_eq!(
            classify_io_error(Error::from(ErrorKind::NotConnected)),
            FileError::TemporarilyUnavailable
        );
        #[cfg(unix)]
        assert_eq!(
            classify_io_error(Error::from_raw_os_error(libc::ESTALE)),
            FileError::TemporarilyUnavailable
        );
        assert!(matches!(
            classify_io_error(Error::other("boom")),
            FileError::Io(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn mutations_refuse_to_follow_a_symlink_out_of_the_workspace() {
        use std::os::unix::fs::symlink;
        let (dir, root) = workspace();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"secret").unwrap();
        symlink(&outside, root.join("link")).unwrap();
        assert_eq!(
            create(&root, "link", "new.txt", CreateKind::File),
            Err(FileError::OutsideWorkspace)
        );
        assert_eq!(
            rename(&root, "link/secret.txt", "renamed.txt"),
            Err(FileError::OutsideWorkspace)
        );
        assert_eq!(
            remove(&root, "link/secret.txt"),
            Err(FileError::OutsideWorkspace)
        );
        assert!(outside.join("secret.txt").exists());
    }
}
