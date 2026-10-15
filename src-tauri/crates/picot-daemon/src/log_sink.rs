// ABOUTME: The daemon's log sink: one append-only 0600 file under the data dir plus stderr.
// ABOUTME: Level from PICOT_DAEMON_LOG (default info); every line carries the pid for `ps` correlation.

//! The app names `<data_dir>/picot-daemon.log` in every "the daemon did not come
//! up" error (plan Task 4), so the daemon has to actually write there — a
//! detached process with a closed stderr would otherwise leave no trace at all.
//!
//! Deliberately a ~50-line `log::Log` implementation instead of a logging crate:
//! the daemon needs a file **and** stderr from one sink, with a 0600 file, and
//! no config surface the app has to set up. `PICOT_DAEMON_LOG` (`off`/`error`/
//! `warn`/`info`/`debug`/`trace`) is the only knob.
//!
//! The token never reaches this sink: the token-bearing types redact themselves
//! (`daemon::instance::InstanceFile`), and `tests/token_redaction.rs` fails the
//! build if a token-formatting log line ever appears in the daemon sources.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use log::{LevelFilter, Log, Metadata, Record};

/// The installed sink, kept alive for the whole process (the logger holds it).
pub struct DaemonLog {
    level: LevelFilter,
    file: Mutex<File>,
}

/// Install the sink. Returns the level actually used.
pub fn install(path: &Path, default_level: LevelFilter) -> Result<LevelFilter, String> {
    let level = std::env::var("PICOT_DAEMON_LOG")
        .ok()
        .as_deref()
        .and_then(parse_level)
        .unwrap_or(default_level);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let sink: &'static DaemonLog = Box::leak(Box::new(DaemonLog {
        level,
        file: Mutex::new(file),
    }));
    if log::set_logger(sink).is_ok() {
        log::set_max_level(level);
    }
    Ok(level)
}

fn parse_level(value: &str) -> Option<LevelFilter> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" | "warning" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

impl Log for DaemonLog {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} [{}] pid={} {}\n",
            chrono_like_timestamp(),
            record.level(),
            std::process::id(),
            record.args()
        );
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
        // A detached daemon's stderr may be /dev/null or a pipe; writing is
        // best effort either way.
        let _ = std::io::stderr().write_all(line.as_bytes());
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

/// RFC 3339 UTC, seconds precision — the same format the instance file uses, so
/// a log excerpt and an instance record can be compared by eye.
fn chrono_like_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    picot_host::daemon::instance::format_timestamp(seconds)
}
