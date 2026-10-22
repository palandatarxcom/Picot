// ABOUTME: Emits PICOT_BUILD_ID, the host build identity shared by every crate member.
// ABOUTME: git describe --tags --always --dirty, with a compile-timestamp fallback when git is unavailable.

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // Deliberately no `cargo:rerun-if-changed` line: cargo then reruns this
    // script whenever any file of the package changes, so the `-dirty` suffix
    // tracks local edits. Consumers read the value through
    // `picot_host::BUILD_ID`, which keeps one build id per cargo invocation
    // across the app, the host crate, and the daemon binary.
    println!("cargo:rustc-env=PICOT_BUILD_ID={}", build_id());

    // The source-tree `src-tauri` directory: this package sits two levels below
    // it, so the dev-mode resource lookups and test fixtures that used to read
    // `CARGO_MANIFEST_DIR` read this instead and keep their relative paths.
    println!(
        "cargo:rustc-env=PICOT_SRC_TAURI_DIR={}",
        src_tauri_dir().display()
    );
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
