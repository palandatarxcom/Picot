// ABOUTME: Keeps spawned pi runtimes from outliving the Picot process that owns them.
// ABOUTME: Records each spawn in a per-supervisor registry and sweeps leftovers at startup.
//
// Three things have to be true at once, because each fails on its own:
//
// 1. `pi` already exits when its stdin reaches EOF, so a child normally dies
//    with its parent. But a wedged child never reads stdin and so never sees
//    that EOF — one such runtime was found spinning at 100% CPU nine days
//    after its Picot was gone, still holding a chat channel's lock.
// 2. Killing the direct child leaves the child's own descendants running. That
//    is `process_tree`'s job (process group on Unix, job object on Windows);
//    this module never re-implements it for live children.
// 3. Neither helps when Picot itself is SIGKILLed or crashes: no teardown code
//    runs at all. That is what [`sweep_orphans`] is for — the registry written
//    at spawn time is the only record that survives a parent that never got to
//    clean up after itself.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

const REGISTRY_DIR: &str = "picot-runtimes";

/// The registry file is read-modify-written by the spawn and stop paths, which
/// run on different threads. Without this lock two concurrent spawns could
/// interleave and drop an entry — the only record a later crash sweep has.
static REGISTRY_LOCK: Mutex<()> = Mutex::new(());

fn lock_registry() -> std::sync::MutexGuard<'static, ()> {
    // A poisoned lock only means some other thread panicked mid-write; the
    // registry is a best-effort record, so recover rather than propagate.
    REGISTRY_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(unix)]
pub fn pid_is_alive(pid: u32) -> bool {
    // ESRCH means gone; EPERM means alive but not ours to signal.
    unsafe {
        libc::kill(pid as i32, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(not(unix))]
pub fn pid_is_alive(_pid: u32) -> bool {
    false
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeEntry {
    pub pid: u32,
    /// Process start time, as the OS reports it. A pid alone is not safe to
    /// kill after a reboot or a pid wrap: this pins the identity.
    pub started_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RuntimeRegistry {
    supervisor_pid: u32,
    entries: Vec<RuntimeEntry>,
}

fn registry_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".pi").join(REGISTRY_DIR))
}

fn registry_path_for(supervisor_pid: u32) -> Option<PathBuf> {
    registry_dir().map(|dir| dir.join(format!("{supervisor_pid}.json")))
}

/// Start time of `pid` as the OS reports it, used to tell a live runtime from
/// an unrelated process that inherited its pid.
pub fn process_start_time(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn read_registry(path: &Path) -> Option<RuntimeRegistry> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_registry(path: &Path, registry: &RuntimeRegistry) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let encoded = match serde_json::to_string(registry) {
        Ok(encoded) => encoded,
        Err(error) => {
            log::warn!("[picot-native] cannot encode the runtime registry: {error}");
            return;
        }
    };
    // Publish atomically. A plain write leaves a window where a concurrent
    // reader's `read_registry` sees a truncated document, fails to parse it, and
    // treats the file as garbage — deleting the record of a supervisor that is
    // alive and merely mid-write, so its runtimes are never swept. The
    // in-process lock cannot close that window: the other reader is another
    // Picot process. Renaming a complete file leaves no such window.
    let temp = path.with_extension("json.tmp");
    if let Err(error) = std::fs::write(&temp, encoded) {
        log::warn!(
            "[picot-native] cannot write the runtime registry {}: {error}",
            temp.display()
        );
        return;
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        log::warn!(
            "[picot-native] cannot publish the runtime registry {}: {error}",
            path.display()
        );
        let _ = std::fs::remove_file(&temp);
    }
}

/// Record a runtime we just spawned, so a future Picot can clean it up if this
/// one dies without running any teardown.
pub fn record_runtime(pid: u32) {
    let _guard = lock_registry();
    let Some(path) = registry_path_for(std::process::id()) else {
        return;
    };
    let mut registry = read_registry(&path).unwrap_or(RuntimeRegistry {
        supervisor_pid: std::process::id(),
        entries: Vec::new(),
    });
    registry.supervisor_pid = std::process::id();
    registry.entries.retain(|entry| entry.pid != pid);
    // A record without a verifiable start time is worse than no record: the
    // sweep would have to kill it on pid alone, and after a pid wrap that is
    // some unrelated process. Leave the orphan behind instead.
    let Some(started_at) = process_start_time(pid) else {
        return;
    };
    registry.entries.push(RuntimeEntry { pid, started_at });
    write_registry(&path, &registry);
}

/// Drop a runtime we stopped ourselves. Removing the file once it is empty
/// keeps a clean shutdown from leaving sweep work for the next launch.
pub fn forget_runtime(pid: u32) {
    let _guard = lock_registry();
    let Some(path) = registry_path_for(std::process::id()) else {
        return;
    };
    let Some(mut registry) = read_registry(&path) else {
        return;
    };
    registry.entries.retain(|entry| entry.pid != pid);
    if registry.entries.is_empty() {
        let _ = std::fs::remove_file(&path);
    } else {
        write_registry(&path, &registry);
    }
}

pub fn clear_registry() {
    let _guard = lock_registry();
    if let Some(path) = registry_path_for(std::process::id()) {
        let _ = std::fs::remove_file(path);
    }
}

/// Kill runtimes left behind by Picot processes that are no longer running.
/// Returns how many were killed. Called once at startup, before any runtime
/// of this process exists.
pub fn sweep_orphans() -> usize {
    sweep_orphans_in(
        registry_dir(),
        std::process::id(),
        &pid_is_alive,
        &|pid| {
            #[cfg(unix)]
            unsafe {
                // The sweep has no `Child` handle to hand `process_tree`, so it
                // signals the recorded group directly: a wedged pi may have left
                // its own descendants behind too.
                libc::kill(-(pid as i32), libc::SIGKILL);
                libc::kill(pid as i32, libc::SIGKILL);
            }
            #[cfg(not(unix))]
            {
                // Windows runtimes die with their job object when Picot's handles
                // close, so a surviving registry entry there needs no signal.
                let _ = pid;
            }
        },
        &process_start_time,
    )
}

/// Split out for tests: `alive` and `kill` are the only OS-touching parts.
fn sweep_orphans_in(
    dir: Option<PathBuf>,
    self_pid: u32,
    alive: &dyn Fn(u32) -> bool,
    kill: &dyn Fn(u32),
    start_time: &dyn Fn(u32) -> Option<String>,
) -> usize {
    let Some(dir) = dir else {
        return 0;
    };
    let Ok(listing) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut killed = 0;
    for item in listing.flatten() {
        let path = item.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(registry) = read_registry(&path) else {
            let _ = std::fs::remove_file(&path);
            continue;
        };
        // Another Picot is still running and owns these runtimes.
        if registry.supervisor_pid == self_pid || alive(registry.supervisor_pid) {
            continue;
        }
        for entry in &registry.entries {
            // pid 0 is not a process: `kill(0, SIGKILL)` signals the caller's
            // own process group. A registry file we did not write must not be
            // able to make the sweep kill Picot itself.
            if entry.pid == 0 {
                continue;
            }
            if !alive(entry.pid) {
                continue;
            }
            // Identity check: a pid on its own can belong to anything after a
            // reboot or a pid wrap, and killing the wrong process is worse
            // than leaving an orphan behind. An entry with no recorded start
            // time (written by an older Picot whose `ps` probe failed) gets
            // the same benefit of the doubt.
            if entry.started_at.is_empty() {
                continue;
            }
            if start_time(entry.pid).as_deref() != Some(entry.started_at.as_str()) {
                continue;
            }
            kill(entry.pid);
            killed += 1;
        }
        let _ = std::fs::remove_file(&path);
    }
    killed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn registry_with(dir: &Path, supervisor_pid: u32, entries: Vec<RuntimeEntry>) {
        write_registry(
            &dir.join(format!("{supervisor_pid}.json")),
            &RuntimeRegistry {
                supervisor_pid,
                entries,
            },
        );
    }

    /// Entry start times the injected probe confirms, so the kill path runs.
    fn matching_start_time(pid: u32) -> Option<String> {
        Some(format!("start-{pid}"))
    }

    fn entry(pid: u32, started_at: &str) -> RuntimeEntry {
        RuntimeEntry {
            pid,
            started_at: started_at.into(),
        }
    }

    #[test]
    fn kills_runtimes_whose_supervisor_is_gone() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        registry_with(&dir, 4242, vec![entry(7001, "start-7001")]);
        let killed = RefCell::new(Vec::new());
        let count = sweep_orphans_in(
            Some(dir.clone()),
            1,
            &|pid| pid == 7001,
            &|pid| killed.borrow_mut().push(pid),
            &matching_start_time,
        );
        assert_eq!(count, 1);
        assert_eq!(*killed.borrow(), vec![7001]);
        assert!(!dir.join("4242.json").exists());
    }

    #[test]
    fn skips_entries_whose_start_time_was_never_recorded() {
        // An older Picot whose `ps` probe failed wrote an empty start time.
        // Killing on pid alone is exactly the pid-wrap hazard the identity
        // check exists for, so the sweep leaves the orphan behind.
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        registry_with(&dir, 4242, vec![entry(7001, "")]);
        let killed = RefCell::new(Vec::new());
        let count = sweep_orphans_in(
            Some(dir.clone()),
            1,
            &|pid| pid == 7001,
            &|pid| killed.borrow_mut().push(pid),
            &matching_start_time,
        );
        assert_eq!(count, 0);
        assert!(killed.borrow().is_empty());
    }

    #[test]
    fn leaves_runtimes_of_a_live_supervisor_alone() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        registry_with(&dir, 4242, vec![entry(7001, "start-7001")]);
        let killed = RefCell::new(Vec::new());
        let count = sweep_orphans_in(
            Some(dir.clone()),
            1,
            &|_| true,
            &|pid| killed.borrow_mut().push(pid),
            &matching_start_time,
        );
        assert_eq!(count, 0);
        assert!(killed.borrow().is_empty());
        // The registry must survive: those runtimes still have an owner.
        assert!(dir.join("4242.json").exists());
    }

    #[test]
    fn never_kills_a_pid_that_was_recycled() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        registry_with(&dir, 4242, vec![entry(7001, "recorded-start")]);
        let killed = RefCell::new(Vec::new());
        // The pid is alive, but it is some other process now: start times differ.
        let count = sweep_orphans_in(
            Some(dir.clone()),
            1,
            &|pid| pid == 7001,
            &|pid| killed.borrow_mut().push(pid),
            &|pid| Some(format!("different-start-{pid}")),
        );
        assert_eq!(count, 0);
        assert!(killed.borrow().is_empty());
    }

    #[test]
    fn ignores_our_own_registry_file() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        registry_with(&dir, 99, vec![entry(7001, "start-7001")]);
        let killed = RefCell::new(Vec::new());
        let count = sweep_orphans_in(
            Some(dir.clone()),
            99,
            &|_| false,
            &|pid| killed.borrow_mut().push(pid),
            &matching_start_time,
        );
        assert_eq!(count, 0);
        assert!(dir.join("99.json").exists());
    }

    /// The production path with nothing mocked: a real child in its own
    /// process group, the real `ps` identity probe, and a real kill.
    #[cfg(unix)]
    #[test]
    fn sweeps_a_real_orphan_in_its_own_process_group() {
        use std::os::unix::process::CommandExt;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        let mut command = Command::new("sleep");
        command.arg("30");
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        // A supervisor pid that is provably gone, so the sweep cannot skip the
        // entry by finding a live owner.
        let dead_supervisor = (30_000..40_000u32)
            .find(|candidate| !pid_is_alive(*candidate))
            .expect("a dead pid exists");
        registry_with(
            &dir,
            dead_supervisor,
            vec![entry(
                pid,
                &process_start_time(pid).expect("probe the live child"),
            )],
        );

        let killed = sweep_orphans_in(
            Some(dir.clone()),
            std::process::id(),
            &pid_is_alive,
            &|pid| unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
                libc::kill(pid as i32, libc::SIGKILL);
            },
            &process_start_time,
        );

        assert_eq!(killed, 1);
        let mut reaped = false;
        for _ in 0..50 {
            if child.try_wait().unwrap().is_some() {
                reaped = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(reaped, "the swept orphan was still running");
        assert!(!dir.join(format!("{dead_supervisor}.json")).exists());
    }

    #[test]
    fn publishing_the_registry_leaves_no_partial_file_behind() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("1234.json");
        write_registry(
            &path,
            &RuntimeRegistry {
                supervisor_pid: 1234,
                entries: vec![RuntimeEntry {
                    pid: 7001,
                    started_at: "Thu Sep  3 11:59:30 2026".into(),
                }],
            },
        );

        let names = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        // The temp file must be renamed away, not left for the sweep to skip.
        assert_eq!(names, vec!["1234.json".to_string()]);
        assert_eq!(read_registry(&path).unwrap().entries.len(), 1);
    }

    #[test]
    fn a_live_process_reads_as_alive() {
        assert!(pid_is_alive(std::process::id()));
    }
}
