// ABOUTME: Shared Pi launch contract helpers for binary, arguments, paths, stderr, and environment.
// ABOUTME: Keeps native Pi launch inputs consistent across runtime types.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::native_pi_manager::{NativeLaunchSpec, NativeRuntimeType, ReadinessPolicy};
use base64::Engine;

/// `scripts/pi-version.json` baked into the binary at compile time so the
/// host can expose the locked Pi version without re-running fetch logic.
const PI_VERSION_JSON: &str = include_str!("../../scripts/pi-version.json");

/// Locked Pi version string (e.g. "0.77.0"). Resolved lazily on first call.
pub fn locked_pi_version() -> &'static str {
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED.get_or_init(|| {
        // We deliberately do a hand-rolled extraction rather than a full
        // serde_json parse: this string is baked in at compile time, the
        // schema is trivial ({"version": "..."}), and avoiding the
        // dependency makes this fn callable from `const` contexts in the
        // future if needed. If the JSON shape grows, switch to serde_json.
        let needle = "\"version\"";
        let bytes = PI_VERSION_JSON;
        let start = bytes
            .find(needle)
            .expect("pi-version.json: missing \"version\" key");
        let after_key = &bytes[start + needle.len()..];
        let colon = after_key
            .find(':')
            .expect("pi-version.json: malformed \"version\" entry");
        let after_colon = &after_key[colon + 1..];
        let first_quote = after_colon
            .find('"')
            .expect("pi-version.json: \"version\" value not quoted");
        let rest = &after_colon[first_quote + 1..];
        let end_quote = rest
            .find('"')
            .expect("pi-version.json: unterminated \"version\" value");
        rest[..end_quote].to_string()
    })
}

/// Build an augmented PATH for child processes.
///
/// `fix_path_env::fix()` is called at app startup and already merges the
/// user's login-shell PATH into this process.  This function is a second
/// safety net: it appends any well-known tool directories that might still
/// be absent (e.g. nvm-managed node versions, Volta, Bun, Mise shims) so
/// that `npm`, `npx`, and friends are always reachable.
///
/// Directories already present in PATH are not duplicated.
pub(crate) fn build_augmented_path() -> String {
    use std::path::{Path, PathBuf};

    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default();

    #[cfg(not(target_os = "windows"))]
    {
        let mut extras: Vec<PathBuf> = vec![
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/opt/homebrew/sbin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/local/sbin"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ];

        if let Ok(home) = std::env::var("HOME") {
            let h = Path::new(&home);
            extras.push(pi_extension_npm_bin_dir(h));
            extras.push(h.join(".local/bin"));
            extras.push(h.join(".bun/bin"));
            extras.push(h.join(".volta/bin"));
            extras.push(h.join(".cargo/bin"));
            extras.push(h.join(".local/share/mise/shims"));
            // nvm: enumerate all installed node versions
            let nvm_root = h.join(".nvm/versions/node");
            if let Ok(entries) = std::fs::read_dir(nvm_root) {
                for entry in entries.flatten() {
                    let bin = entry.path().join("bin");
                    if bin.is_dir() {
                        extras.push(bin);
                    }
                }
            }
        }

        for extra in extras {
            if !dirs.iter().any(|d| d == &extra) {
                dirs.push(extra);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let mut extras: Vec<PathBuf> = Vec::new();
        if let Ok(appdata) = std::env::var("APPDATA") {
            extras.push(Path::new(&appdata).join("npm"));
        }
        if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
            let h = Path::new(&home);
            extras.push(pi_extension_npm_bin_dir(h));
            extras.push(h.join(".cargo").join("bin"));
            extras.push(h.join(".bun").join("bin"));
            extras.push(h.join("scoop").join("shims"));
        }
        for extra in extras {
            if !dirs.iter().any(|d| d == &extra) {
                dirs.push(extra);
            }
        }
    }

    std::env::join_paths(dirs)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default())
}

pub(crate) fn pi_extension_npm_bin_dir(home: &Path) -> PathBuf {
    home.join(".pi")
        .join("agent")
        .join("npm")
        .join("node_modules")
        .join(".bin")
}

/// Strip a Windows verbatim / extended-length path prefix (`\\?\` or
/// `\\?\UNC\`) from a path string.
///
/// Tauri's `resource_dir()` returns extended-length paths (e.g.
/// `\\?\C:\Users\...\Picot\pi\pi.exe`). The embedded pi (Bun 1.3.10,
/// Windows arm64, compiled standalone) segfaults (`Segmentation fault at
/// address 0x18`) when it is launched with — or asked to load an
/// `--extension` from — a `\\?\`-prefixed path. Passing the plain
/// `C:\Users\...` form avoids the crash. This is a no-op on non-Windows
/// platforms and for paths without the prefix.
pub(crate) fn strip_verbatim_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        // `\\?\UNC\server\share` -> `\\server\share`
        format!(r"\\{}", rest)
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

/// Prepare a Picot extension path for Pi's native RPC process.
///
/// On Windows, the bundled Pi binary cannot load extension paths containing
/// spaces. Mirror such paths into a space-free temporary directory; other
/// platforms use the original path unchanged.
#[cfg(not(target_os = "windows"))]
pub(crate) fn sanitize_extension_path_for_pi(original: &str) -> String {
    original.to_string()
}

#[cfg(target_os = "windows")]
pub(crate) fn sanitize_extension_path_for_pi(original: &str) -> String {
    if !original.contains(' ') {
        return original.to_string();
    }
    match mirror_to_space_free_dir(Path::new(original)) {
        Ok(mirrored) => mirrored.to_string_lossy().to_string(),
        Err(error) => {
            log::warn!("[picot-host] extension path mirror failed ({error}); using original path");
            original.to_string()
        }
    }
}

#[cfg(target_os = "windows")]
fn mirror_to_space_free_dir(src: &Path) -> std::io::Result<PathBuf> {
    let file_name = src.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "extension path has no file name",
        )
    })?;
    let mut dest_dir = std::env::temp_dir();
    if dest_dir.to_string_lossy().contains(' ') {
        dest_dir = PathBuf::from("C:\\ProgramData\\picot");
    }
    dest_dir.push("native-extensions");
    std::fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(file_name);
    std::fs::copy(src, &dest)?;
    Ok(dest)
}

/// Resolve the directory used by Pi for its agent state and extensions.
///
/// An explicit non-empty `PI_CODING_AGENT_DIR` takes precedence. Otherwise the
/// user's home directory is used as the parent of `.pi/agent`. The directory
/// is created before canonicalization so the child always receives a stable,
/// existing absolute path.
pub(crate) fn resolve_pi_agent_root() -> Result<PathBuf, String> {
    let root = match std::env::var("PI_CODING_AGENT_DIR") {
        Ok(path) if !path.trim().is_empty() => PathBuf::from(path),
        _ => {
            let home = std::env::var("HOME")
                .ok()
                .filter(|path| !path.is_empty())
                .or_else(|| {
                    std::env::var("USERPROFILE")
                        .ok()
                        .filter(|path| !path.is_empty())
                })
                .ok_or_else(|| {
                    "cannot resolve Pi agent root: HOME and USERPROFILE are unset".to_string()
                })?;
            PathBuf::from(home).join(".pi").join("agent")
        }
    };

    std::fs::create_dir_all(&root)
        .map_err(|error| format!("failed to create Pi agent root {}: {error}", root.display()))?;
    let canonicalized = root.canonicalize().map_err(|error| {
        format!(
            "failed to canonicalize Pi agent root {}: {error}",
            root.display()
        )
    })?;
    // `canonicalize` on Windows returns a `\\?\`-prefixed extended-length
    // path. Bun (the bundled Pi runtime) cannot resolve modules from such a
    // path, so every package extension fails with
    // `Cannot find module '\\?\C:\...\node_modules\<pkg>\dist\index.js'`,
    // which presents as a health-check timeout with no window. Strip the
    // prefix so pi receives the plain `C:\...` form, matching macOS/Linux.
    Ok(PathBuf::from(strip_verbatim_prefix(
        &canonicalized.to_string_lossy(),
    )))
}

/// Build native launch inputs without constructing a port-addressed manager.
pub(crate) fn native_launch_spec(
    static_dir: &Path,
    cwd: &str,
    session_path: Option<&str>,
) -> Result<NativeLaunchSpec, String> {
    let spec = native_launch_spec_for(
        static_dir,
        NativeRuntimeType::Primary,
        Path::new(cwd),
        session_path.map(Path::new),
    )?;
    // Every wrapper caller (open_workspace / restart_runtime / workspace
    // transition) launches on a registry-verified workspace root: that launch
    // is Picot's explicit trust gesture. Record it BEFORE spawn so Pi's RPC
    // startup — no UI, undecided extension, no saved decision — resolves the
    // project as trusted and loads its project-local resources. Ephemeral
    // runtimes call native_launch_spec_for directly and stay untrusted.
    crate::project_trust::trust_registered_workspace(cwd);
    // Registry-verified launch only: the config bridge treats this canonical
    // root as Picot's explicit permission for project MCP reads and writes.
    let mut spec = spec;
    spec.mcp_project_root = Some(canonical_project_root(Path::new(cwd)));
    Ok(spec)
}

/// Canonical project root for the host marker. `canonicalize` resolves
/// symlinks so the bridge can compare real paths; if the directory vanished
/// between admission and launch the already-resolved absolute cwd is used and
/// the bridge's own realpath comparison still decides.
fn canonical_project_root(cwd: &Path) -> PathBuf {
    let resolved = cwd
        .canonicalize()
        .map(|path| PathBuf::from(strip_verbatim_prefix(&path.to_string_lossy())))
        .unwrap_or_else(|_| PathBuf::from(strip_verbatim_prefix(&cwd.to_string_lossy())));
    resolved
}
/// Build native launch inputs for a specific runtime type.
///
/// Session and tool flags follow the Gate C launch contract per type:
/// Dedicated requires an explicit session; SideChat, QuickChat, Standby,
/// and Config are sessionless (`command_description` emits `--no-session`);
/// QuickChat and Config are always toolless (Config is the landing bridge
/// service: configuration ops never call agent tools). Standby callers may
/// still enable `no_tools` on the returned spec for the side-chat standby
/// pool variant.
pub(crate) fn native_launch_spec_for(
    static_dir: &Path,
    runtime_type: NativeRuntimeType,
    cwd: &Path,
    session_path: Option<&Path>,
) -> Result<NativeLaunchSpec, String> {
    if runtime_type == NativeRuntimeType::Dedicated && session_path.is_none() {
        return Err("Dedicated runtime requires an explicit session path".into());
    }
    if matches!(
        runtime_type,
        NativeRuntimeType::SideChat
            | NativeRuntimeType::QuickChat
            | NativeRuntimeType::Standby
            | NativeRuntimeType::Config
    ) && session_path.is_some()
    {
        return Err("Sessionless runtime types cannot carry a session path".into());
    }
    let binary = resolve_bundled_pi(static_dir)?;
    let bridge = resolve_picot_bridge_extension_path(static_dir)?;
    let mut secret_bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut secret_bytes);
    let spec = NativeLaunchSpec {
        binary,
        cwd: PathBuf::from(strip_verbatim_prefix(&cwd.to_string_lossy())),
        session_path: session_path
            .map(|path| PathBuf::from(strip_verbatim_prefix(&path.to_string_lossy()))),
        extensions: vec![bridge],
        pi_version: locked_pi_version().to_owned(),
        path_env: build_augmented_path(),
        agent_root: Some(resolve_pi_agent_root()?),
        static_dir: Some(static_dir.to_path_buf()),
        install_secret: Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret_bytes)),
        runtime_type,
        no_tools: matches!(
            runtime_type,
            NativeRuntimeType::QuickChat | NativeRuntimeType::Config
        ),
        readiness: ReadinessPolicy::default(),
        cleanup: crate::native_pi_manager::NativeCleanupResources::default(),
        // Ephemeral runtimes (Config/Quick/Side/Standby) never admit project
        // MCP operations; only the primary wrapper sets the marker.
        mcp_project_root: None,
    };
    Ok(spec)
}

/// Resolve the Picot bridge extension used by native Pi runtimes.
///
/// Native launch owns this resolution directly; it never depends on a
/// port-based manager or a Pi-served HTTP/WS extension.
pub(crate) fn resolve_picot_bridge_extension_path(static_dir: &Path) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Some(parent) = static_dir.parent() {
        candidates.push(parent.join("extensions").join("picot-bridge.mjs"));
    }
    if cfg!(debug_assertions) {
        candidates.push(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("extensions")
                .join("dist")
                .join("picot-bridge.mjs"),
        );
        candidates.push(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("extensions")
                .join("picot-bridge.ts"),
        );
    }
    candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .map(|bridge| {
            PathBuf::from(sanitize_extension_path_for_pi(&strip_verbatim_prefix(
                &bridge.to_string_lossy(),
            )))
        })
        .ok_or_else(|| {
            format!(
                "Could not find picot-bridge extension. Tried:\n{}",
                candidates
                    .iter()
                    .map(|path| format!("  - {}", path.display()))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
}

pub(crate) fn resolve_bundled_pi(static_dir: &Path) -> Result<PathBuf, String> {
    let bin_name = if cfg!(target_os = "windows") {
        "pi.exe"
    } else {
        "pi"
    };

    // Explicit override (rare; useful when smoke-testing a hand-built pi).
    if let Ok(explicit) = std::env::var("PI_BIN") {
        let candidate = PathBuf::from(explicit.trim());
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    let mut tried: Vec<PathBuf> = Vec::new();
    let bundled = static_dir
        .parent()
        .map(|parent| parent.join("pi").join(bin_name));
    if let Some(p) = bundled.clone() {
        if p.is_file() {
            return Ok(p);
        }
        tried.push(p);
    }

    if cfg!(debug_assertions) {
        let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("pi")
            .join(bin_name);
        if dev_path.is_file() {
            return Ok(dev_path);
        }
        tried.push(dev_path);
    }

    let tried_str = tried
        .iter()
        .map(|p| format!("  - {}", p.display()))
        .collect::<Vec<_>>()
        .join("\n");
    Err(format!(
        "Could not find bundled pi binary. Tried:\n{}\n\n\
             For dev: run `bun run fetch:pi` from the repo root.\n\
             For release: the .app bundle is missing `resources/pi/{}`. \
             Reinstall Picot.",
        tried_str, bin_name
    ))
}

#[cfg(test)]
mod launch_spec_tests {
    use super::*;

    fn static_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources")
    }

    fn spec_for(runtime_type: NativeRuntimeType, session_path: Option<&Path>) -> NativeLaunchSpec {
        let cwd = std::env::temp_dir();
        native_launch_spec_for(&static_dir(), runtime_type, &cwd, session_path)
            .expect("builder must produce a launch spec")
    }

    #[test]
    fn native_launch_paths_strip_windows_verbatim_prefixes() {
        assert_eq!(strip_verbatim_prefix(r"\\?\C:\workspace"), r"C:\workspace");
        assert_eq!(
            strip_verbatim_prefix(r"\\?\UNC\server\share\workspace"),
            r"\\server\share\workspace"
        );
        assert_eq!(strip_verbatim_prefix(r"C:\workspace"), r"C:\workspace");

        let cwd = Path::new(r"\\?\C:\workspace");
        let session = Path::new(r"\\?\C:\sessions\chat.jsonl");
        let spec = native_launch_spec_for(
            &static_dir(),
            NativeRuntimeType::Dedicated,
            cwd,
            Some(session),
        )
        .expect("verbatim-prefixed paths should produce a launch spec");
        assert_eq!(spec.cwd, PathBuf::from(r"C:\workspace"));
        assert_eq!(
            spec.session_path,
            Some(PathBuf::from(r"C:\sessions\chat.jsonl"))
        );
    }

    #[test]
    fn dedicated_requires_and_carries_explicit_session() {
        assert!(native_launch_spec_for(
            &static_dir(),
            NativeRuntimeType::Dedicated,
            &std::env::temp_dir(),
            None
        )
        .is_err());
        let session = std::env::temp_dir().join("picot-launch-spec-dedicated.jsonl");
        std::fs::write(&session, b"").unwrap();
        let spec = spec_for(NativeRuntimeType::Dedicated, Some(&session));
        let description = spec.command_description();
        assert_eq!(description.runtime_type, NativeRuntimeType::Dedicated);
        assert!(description.args.contains(&"--session".to_string()));
        assert!(description
            .args
            .contains(&session.to_string_lossy().into_owned()));
        assert!(!description.args.contains(&"--no-session".to_string()));
        assert!(!description.args.contains(&"--no-tools".to_string()));
    }

    #[test]
    fn side_chat_is_sessionless() {
        let spec = spec_for(NativeRuntimeType::SideChat, None);
        let description = spec.command_description();
        assert_eq!(description.runtime_type, NativeRuntimeType::SideChat);
        assert!(description.args.contains(&"--no-session".to_string()));
        assert!(!description.args.contains(&"--session".to_string()));
        assert!(!description.args.contains(&"--no-tools".to_string()));
    }

    #[test]
    fn quick_chat_is_sessionless_and_toolless_with_secret() {
        let spec = spec_for(NativeRuntimeType::QuickChat, None);
        let description = spec.command_description();
        assert_eq!(description.runtime_type, NativeRuntimeType::QuickChat);
        assert!(description.args.contains(&"--no-session".to_string()));
        assert!(description.args.contains(&"--no-tools".to_string()));
        assert!(description
            .environment
            .contains_key("PI_STUDIO_SKILL_INSTALL_SECRET"));
    }

    #[test]
    fn standby_is_sessionless_and_tool_flag_is_caller_controlled() {
        let mut spec = spec_for(NativeRuntimeType::Standby, None);
        let description = spec.command_description();
        assert_eq!(description.runtime_type, NativeRuntimeType::Standby);
        assert!(description.args.contains(&"--no-session".to_string()));
        assert!(!description.args.contains(&"--no-tools".to_string()));

        // Side-chat standby pool variant: toolless standby.
        spec.no_tools = true;
        let description = spec.command_description();
        assert!(description.args.contains(&"--no-tools".to_string()));
    }

    #[test]
    fn ephemeral_runtimes_carry_no_project_root_marker() {
        for runtime_type in [
            NativeRuntimeType::Primary,
            NativeRuntimeType::Config,
            NativeRuntimeType::QuickChat,
            NativeRuntimeType::SideChat,
            NativeRuntimeType::Standby,
        ] {
            let spec = spec_for(runtime_type, None);
            assert!(
                spec.mcp_project_root.is_none(),
                "{runtime_type:?} must not claim a host-verified project root"
            );
            let description = spec.command_description();
            assert!(!description
                .environment
                .contains_key(crate::native_pi_manager::MCP_PROJECT_ROOT_ENV));
        }
    }

    #[test]
    fn only_a_set_project_root_is_exported_to_the_child() {
        let mut spec = spec_for(NativeRuntimeType::Primary, None);
        spec.mcp_project_root = Some(PathBuf::from("/workspace/with space"));
        let description = spec.command_description();
        assert_eq!(
            description
                .environment
                .get(crate::native_pi_manager::MCP_PROJECT_ROOT_ENV),
            Some(&"/workspace/with space".to_string())
        );
    }

    #[test]
    fn canonical_project_root_is_absolute_and_resolved() {
        let dir = std::env::temp_dir();
        let resolved = canonical_project_root(&dir);
        assert!(resolved.is_absolute());
        assert_eq!(resolved, dir.canonicalize().expect("temp dir must resolve"));
    }

    #[test]
    fn sessionless_types_reject_session_paths() {
        let session = std::env::temp_dir().join("picot-launch-spec-reject.jsonl");
        std::fs::write(&session, b"").unwrap();
        for runtime_type in [
            NativeRuntimeType::SideChat,
            NativeRuntimeType::QuickChat,
            NativeRuntimeType::Standby,
        ] {
            assert!(native_launch_spec_for(
                &static_dir(),
                runtime_type,
                &std::env::temp_dir(),
                Some(&session)
            )
            .is_err());
        }
    }
}
