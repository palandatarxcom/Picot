// ABOUTME: Emits PICOT_BUILD_ID, the host build identity shared by every crate member.
// ABOUTME: git describe --tags --always --dirty, with a compile-timestamp fallback when git is unavailable.

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    emit_rerun_paths();
    // Consumers read the value through `picot_host::BUILD_ID`, which keeps one
    // build id per cargo invocation across the app, the host crate, and the
    // daemon binary.
    println!("cargo:rustc-env=PICOT_BUILD_ID={}", build_id());

    // The source-tree `src-tauri` directory: this package sits two levels below
    // it, so the dev-mode resource lookups and test fixtures that used to read
    // `CARGO_MANIFEST_DIR` read this instead and keep their relative paths.
    println!(
        "cargo:rustc-env=PICOT_SRC_TAURI_DIR={}",
        src_tauri_dir().display()
    );
}

/// Re-run triggers for `git describe --tags --always --dirty`.
///
/// Watching the repository alone would miss every input that moves the string:
/// a normal commit writes `refs/heads/<branch>` and leaves `HEAD`'s content
/// unchanged, a new tag moves the describe base, and a staged change flips
/// `-dirty`. Paths come from `git rev-parse`, because a linked worktree keeps
/// `HEAD` in `.git/worktrees/<name>` while the refs it resolves live in the
/// common directory.
fn emit_rerun_paths() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    // Any `rerun-if-changed` disables cargo's default "watch the whole package"
    // rule, so the `-dirty` half of the identity needs it re-emitted.
    println!("cargo:rerun-if-changed=.");
    let Some((git_dir, common_dir)) = git_dirs(manifest_dir) else {
        return;
    };
    for path in [
        git_dir.join("HEAD"),
        git_dir.join("index"),
        // Recursive: covers loose refs for branches and tags.
        common_dir.join("refs"),
        common_dir.join("packed-refs"),
    ] {
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

/// The worktree-local git directory and the shared one, absolute.
fn git_dirs(manifest_dir: &str) -> Option<(PathBuf, PathBuf)> {
    let read = |flag: &str| -> Option<PathBuf> {
        let output = Command::new("git")
            .args(["rev-parse", flag])
            .current_dir(manifest_dir)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let printed = String::from_utf8(output.stdout).ok()?.trim().to_string();
        if printed.is_empty() {
            return None;
        }
        let path = PathBuf::from(&printed);
        // `--git-common-dir` prints `.git` for the main worktree.
        let absolute = if path.is_absolute() {
            path
        } else {
            PathBuf::from(manifest_dir).join(path)
        };
        Some(std::fs::canonicalize(&absolute).unwrap_or(absolute))
    };
    Some((read("--absolute-git-dir")?, read("--git-common-dir")?))
}

fn src_tauri_dir() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let joined = manifest_dir.join("..").join("..");
    std::fs::canonicalize(&joined).unwrap_or(joined)
}

fn build_id() -> String {
    git_describe().unwrap_or_else(|| {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        format!("t{seconds}")
    })
}

fn git_describe() -> Option<String> {
    let output = Command::new("git")
        // `--tags` matters: release tags are lightweight (`git tag "$TAG"` in
        // scripts/release.sh), and a plain describe only counts annotated tags,
        // which anchored every build id to an old v0.3.6.1 while the app itself
        // reported 0.5.x.
        .args(["describe", "--tags", "--always", "--dirty"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let id = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}
