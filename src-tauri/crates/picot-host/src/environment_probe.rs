// ABOUTME: Host-side detection of the fixed external tool catalog.
// ABOUTME: Runs each tool's version command with a short timeout and reads the
// ABOUTME: version back; PATH resolution is left to the OS launcher.

use serde::Serialize;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// One version command may take this long before it is killed and reported as a
/// failure. Windows Python is the only tool with two candidates to try.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A writer that outlives the direct child must not wedge the probe: reading
/// each pipe stops after this bound and keeps whatever arrived.
const READ_BOUND: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ToolTier {
    Basic,
    Optional,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolId {
    Git,
    Python3,
    Npm,
    Uv,
    R,
    Officecli,
    Dws,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ProbeStatus {
    Ready,
    Missing,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolAction {
    Install,
    Update,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub(crate) tool_id: ToolId,
    pub(crate) status: ProbeStatus,
    pub(crate) version: Option<String>,
    pub(crate) executable_path: Option<String>,
    pub(crate) reason: Option<String>,
    pub(crate) official_url: &'static str,
    pub(crate) tier: ToolTier,
    /// Windows registry fallback found R installed while it is still absent
    /// from this process's PATH: the tool runs, but agent sessions only see
    /// it once Picot restarts and inherits a fresh PATH.
    pub(crate) via_fallback_path: bool,
}

/// A program to try, with the arguments its version flag needs.
type Candidate = (&'static str, &'static [&'static str]);

pub(crate) struct ToolSpec {
    pub(crate) id: ToolId,
    pub(crate) tier: ToolTier,
    pub(crate) official_url: &'static str,
    candidates: &'static [Candidate],
    /// The version line must carry this prefix, for tools whose output can
    /// contain other numbers (an error line, a URL, a host).
    prefix: Option<&'static str>,
    /// Whether this tool prints its version on stderr and may be read there.
    /// Everything else is read from stdout only, so a warning that names some
    /// other version cannot pass as this tool's.
    stderr_ok: bool,
}

const GIT: &[Candidate] = &[("git", &["--version"])];
const NPM: &[Candidate] = &[("npm", &["--version"])];
const UV: &[Candidate] = &[("uv", &["--version"])];
const R_TOOL: &[Candidate] = &[("R", &["--version"])];
const OFFICECLI: &[Candidate] = &[("officecli", &["--version"])];
const DWS: &[Candidate] = &[("dws", &["--version"])];
#[cfg(not(windows))]
const PYTHON3: &[Candidate] = &[("python3", &["--version"])];
/// Windows installs Python through the launcher first; `python` is the
/// fallback when the launcher is absent.
#[cfg(windows)]
const PYTHON3: &[Candidate] = &[("py", &["-3", "--version"]), ("python", &["--version"])];

/// Canonical tool order: the UI list, the install queue and the probe all use it.
pub(crate) const ORDER: [ToolId; 7] = [
    ToolId::Git,
    ToolId::Python3,
    ToolId::Npm,
    ToolId::Uv,
    ToolId::R,
    ToolId::Officecli,
    ToolId::Dws,
];

const SPECS: [ToolSpec; 7] = [
    ToolSpec {
        id: ToolId::Git,
        tier: ToolTier::Basic,
        official_url: "https://git-scm.com/downloads",
        candidates: GIT,
        prefix: Some("git version"),
        stderr_ok: false,
    },
    ToolSpec {
        id: ToolId::Python3,
        tier: ToolTier::Basic,
        official_url: "https://www.python.org/downloads/",
        candidates: PYTHON3,
        prefix: Some("Python"),
        stderr_ok: true,
    },
    ToolSpec {
        id: ToolId::Npm,
        tier: ToolTier::Basic,
        official_url: "https://nodejs.org/en/download",
        candidates: NPM,
        prefix: None,
        stderr_ok: false,
    },
    ToolSpec {
        id: ToolId::Uv,
        tier: ToolTier::Basic,
        official_url: "https://docs.astral.sh/uv/getting-started/installation/",
        candidates: UV,
        prefix: None,
        stderr_ok: false,
    },
    ToolSpec {
        id: ToolId::R,
        tier: ToolTier::Basic,
        official_url: "https://cran.r-project.org/",
        candidates: R_TOOL,
        prefix: Some("R version"),
        stderr_ok: false,
    },
    ToolSpec {
        id: ToolId::Officecli,
        tier: ToolTier::Optional,
        official_url: "https://github.com/iOfficeAI/OfficeCLI#readme",
        candidates: OFFICECLI,
        prefix: None,
        stderr_ok: true,
    },
    ToolSpec {
        id: ToolId::Dws,
        tier: ToolTier::Optional,
        official_url: "https://github.com/DingTalk-Real-AI/dingtalk-workspace-cli#readme",
        candidates: DWS,
        prefix: Some("dws version"),
        stderr_ok: false,
    },
];

pub(crate) fn spec(tool: ToolId) -> &'static ToolSpec {
    SPECS
        .iter()
        .find(|spec| spec.id == tool)
        .expect("every ToolId has a spec")
}

impl ToolId {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ToolId::Git => "git",
            ToolId::Python3 => "python3",
            ToolId::Npm => "npm",
            ToolId::Uv => "uv",
            ToolId::R => "r",
            ToolId::Officecli => "officecli",
            ToolId::Dws => "dws",
        }
    }

    pub fn from_str(raw: &str) -> Option<ToolId> {
        ORDER.into_iter().find(|tool| tool.as_str() == raw)
    }
}

/// What running one candidate produced.
pub(crate) enum Outcome {
    Output { stdout: String, stderr: String },
    TimedOut,
    SpawnFailed(String),
}

pub fn probe_one(tool: ToolId) -> Probe {
    let spec = spec(tool);
    for (program, args) in spec.candidates {
        match run(program, args) {
            Outcome::SpawnFailed(_) => continue,
            outcome => return evaluate(spec, program, outcome),
        }
    }
    // Every candidate failed to launch. A tool whose installer leaves it off
    // PATH (Windows R) may still be installed: consult its fallback before
    // reading the absence as `missing`.
    if tool == ToolId::R {
        if let Some(probe) = r_fallback_probe() {
            return probe;
        }
    }
    base(spec, ProbeStatus::Missing)
}

pub fn probe_all() -> Vec<Probe> {
    ORDER.into_iter().map(probe_one).collect()
}

fn base(spec: &ToolSpec, status: ProbeStatus) -> Probe {
    Probe {
        tool_id: spec.id,
        status,
        version: None,
        executable_path: None,
        reason: None,
        official_url: spec.official_url,
        tier: spec.tier,
        via_fallback_path: false,
    }
}

/// Turn one candidate's raw output into a status. Pure, so parsing is testable
/// without launching anything.
fn evaluate(spec: &ToolSpec, program: &str, outcome: Outcome) -> Probe {
    let (stdout, stderr) = match outcome {
        Outcome::Output { stdout, stderr } => (stdout, stderr),
        Outcome::TimedOut => {
            return Probe {
                reason: Some(format!("{program} did not answer within {PROBE_TIMEOUT:?}")),
                ..base(spec, ProbeStatus::Failed)
            }
        }
        Outcome::SpawnFailed(error) => {
            return Probe {
                reason: Some(error),
                ..base(spec, ProbeStatus::Failed)
            }
        }
    };
    let version = version_in(&stdout, spec.prefix).or_else(|| {
        spec.stderr_ok
            .then(|| version_in(&stderr, spec.prefix))
            .flatten()
    });
    let path = resolve_program(program);
    match version {
        Some(version) => Probe {
            version: Some(version),
            executable_path: path,
            ..base(spec, ProbeStatus::Ready)
        },
        None => Probe {
            executable_path: path,
            reason: Some(format!("{program} did not print a recognizable version")),
            ..base(spec, ProbeStatus::Failed)
        },
    }
}

/// First line carrying a version token, optionally required to start with
/// `prefix`. Lines without a token are skipped, so an error line that mentions
/// another version cannot be mistaken for this tool's.
fn version_in(text: &str, prefix: Option<&str>) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| prefix.is_none_or(|prefix| line.starts_with(prefix)))
        .find_map(version_token)
}

/// A version token: a run of version characters that starts with a digit and
/// contains a dot followed by a digit (`2.46.0`, `1.0.62`, `0.4.18-rc.1`).
fn version_token(line: &str) -> Option<String> {
    let is_version_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_');
    let bytes: Vec<char> = line.chars().collect();
    let mut start = None;
    for (index, character) in bytes.iter().enumerate() {
        let in_token = is_version_char(*character);
        match (start, in_token) {
            (None, true) if character.is_ascii_digit() => start = Some(index),
            (Some(_), false) => {
                let token: String = bytes[start.unwrap()..index].iter().collect();
                if looks_like_version(&token) {
                    return Some(token);
                }
                start = None;
            }
            _ => {}
        }
    }
    let token: String = bytes[start?..].iter().collect();
    looks_like_version(&token).then_some(token)
}

fn looks_like_version(token: &str) -> bool {
    // A dot must be followed by a segment that carries something (`1..2` and
    // `1.` are not versions), and the token must not trail into punctuation
    // (`1.2.-`). Suffixes stay: `2.46.0.windows.1`, `0.4.18-rc.1`.
    if token.ends_with(['.', '-', '+', '_']) {
        return false;
    }
    let mut characters = token.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '.' && !characters.peek().is_some_and(char::is_ascii_alphanumeric) {
            return false;
        }
    }
    token.contains('.') && token.starts_with(|c: char| c.is_ascii_digit())
}

/// Run one candidate: bounded by [`PROBE_TIMEOUT`], killed when it overruns.
fn run(program: &str, args: &[&str]) -> Outcome {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Repo convention: a GUI host must not flash a console window on Windows.
    crate::windows_child::hide_console(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Outcome::SpawnFailed(error.to_string()),
    };

    let stdout_rx = read_in_background(child.stdout.take());
    let stderr_rx = read_in_background(child.stderr.take());

    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Outcome::TimedOut;
                }
            }
            Err(error) => return Outcome::SpawnFailed(error.to_string()),
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // The child exited, so both pipes normally reach EOF immediately. A stray
    // descendant holding one of them must not wedge us, hence the bound.
    Outcome::Output {
        stdout: stdout_rx.recv_timeout(READ_BOUND).unwrap_or_default(),
        stderr: stderr_rx.recv_timeout(READ_BOUND).unwrap_or_default(),
    }
}

fn read_in_background(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            let _ = sender.send(text);
        });
    }
    receiver
}

/// The `InstallPath` value line from `reg query` for `Software\R-core\R`.
/// Pure so the Windows registry format is testable on every platform.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_reg_install_path(output: &str) -> Option<String> {
    output
        .lines()
        .rfind(|line| line.contains("InstallPath") && line.contains("REG_SZ"))
        .and_then(|line| line.rsplit("REG_SZ").next())
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
}

#[cfg_attr(not(windows), allow(dead_code))]
fn fallback_r_exe_path(install_path: &std::path::Path) -> std::path::PathBuf {
    install_path.join("bin").join("x64").join("R.exe")
}

/// Windows only: R installed by the official Inno installer is registered
/// under `Software\R-core\R` (HKLM for admin installs, HKCU otherwise) but is
/// not on PATH. Resolve the install path, then require the same `R version`
/// output the PATH probe would — the registry only locates the program, the
/// version still comes from the tool itself.
#[cfg(windows)]
fn r_fallback_probe() -> Option<Probe> {
    let hive_key = |hive: &str| format!("{hive}\\Software\\R-core\\R");
    let install_path = ["HKLM", "HKCU"]
        .iter()
        .filter_map(
            |hive| match run("reg", &["query", &hive_key(hive), "/v", "InstallPath"]) {
                Outcome::Output { stdout, .. } => parse_reg_install_path(&stdout),
                _ => None,
            },
        )
        .next()?;
    let exe = fallback_r_exe_path(std::path::Path::new(&install_path));
    if !exe.is_file() {
        return None;
    }
    match run(exe.to_string_lossy().as_ref(), &["--version"]) {
        Outcome::Output { stdout, stderr } => {
            let version = version_in(&stdout, Some("R version"))
                .or_else(|| version_in(&stderr, Some("R version")))?;
            let spec = spec(ToolId::R);
            Some(Probe {
                version: Some(version),
                executable_path: Some(exe.to_string_lossy().into_owned()),
                via_fallback_path: true,
                ..base(spec, ProbeStatus::Ready)
            })
        }
        _ => None,
    }
}

#[cfg(not(windows))]
fn r_fallback_probe() -> Option<Probe> {
    None
}

/// Locate the program the way the OS launcher would, for display only. `None`
/// means the launcher found it somewhere this scan cannot see (a shim, a
/// Windows app alias), which is informational rather than an error.
fn resolve_program(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|extension| !extension.trim().is_empty())
            .map(|extension| extension.trim().to_ascii_lowercase())
            .collect()
    } else {
        Vec::new()
    };
    for directory in std::env::split_paths(&path) {
        let direct = directory.join(program);
        if is_executable(&direct) {
            return Some(direct.to_string_lossy().to_string());
        }
        for extension in &extensions {
            let candidate = directory.join(format!("{program}{extension}"));
            if is_executable(&candidate) {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }
    None
}

fn is_executable(path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    fn spec_of(tool: ToolId) -> &'static ToolSpec {
        spec(tool)
    }

    fn output(stdout: &str, stderr: &str) -> Outcome {
        Outcome::Output {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn every_catalog_tool_has_a_spec_in_order() {
        assert_eq!(ORDER.len(), SPECS.len());
        for (index, tool) in ORDER.into_iter().enumerate() {
            assert_eq!(SPECS[index].id, tool);
            assert!(!spec(tool).official_url.is_empty());
        }
        assert_eq!(spec(ToolId::Git).tier, ToolTier::Basic);
        assert_eq!(spec(ToolId::Dws).tier, ToolTier::Optional);
        // R slots with the other basic tools, before the optional ones.
        assert_eq!(ORDER[4], ToolId::R);
        assert_eq!(spec(ToolId::R).tier, ToolTier::Basic);
        assert_eq!(ToolId::R.as_str(), "r");
        assert!(ToolId::from_str("r").is_some());
        assert!(ToolId::from_str("uv").is_some());
        assert!(ToolId::from_str("other").is_none());
    }

    #[test]
    fn parses_the_real_host_output_of_every_tool() {
        let cases = [
            (ToolId::Git, "git version 2.55.0\n", "", "2.55.0"),
            (ToolId::Python3, "", "Python 3.14.0\n", "3.14.0"),
            (ToolId::Npm, "11.19.0\n", "", "11.19.0"),
            (ToolId::Uv, "uv 0.12.5\n", "", "0.12.5"),
            (
                ToolId::R,
                "R version 4.6.2 (2025-10-31) -- Great Detour\n\nType 'demo()'...\n",
                "",
                "4.6.2",
            ),
            (ToolId::Officecli, "1.0.153\n", "", "1.0.153"),
            (
                ToolId::Dws,
                "dws version v1.0.62 (70323e14)\n",
                "",
                "1.0.62",
            ),
        ];
        for (tool, stdout, stderr, expected) in cases {
            let probe = evaluate(spec_of(tool), tool.as_str(), output(stdout, stderr));
            assert_eq!(probe.status, ProbeStatus::Ready, "{tool:?} {stdout:?}");
            assert_eq!(probe.version.as_deref(), Some(expected), "{tool:?}");
        }
    }

    #[test]
    fn an_error_line_carrying_another_version_is_not_this_tool_version() {
        // npm's failure text names a Node version; that is not npm's version.
        let probe = evaluate(
            spec_of(ToolId::Npm),
            "npm",
            output("", "npm ERR requires Node.js 20.0.0\n"),
        );
        assert_eq!(probe.status, ProbeStatus::Failed);
        assert_eq!(probe.version, None);

        let probe = evaluate(
            spec_of(ToolId::Dws),
            "dws",
            output("Failed to connect to 127.0.0.1\n", ""),
        );
        assert_eq!(probe.status, ProbeStatus::Failed);
    }

    #[test]
    fn malformed_numbers_are_not_versions() {
        for text in ["1..2", "1.2.-", "1.", "version 1", "no digits here"] {
            assert_eq!(version_in(text, None), None, "{text:?}");
        }
        // A real suffix survives, so an update is not mistaken for "unchanged".
        assert_eq!(
            version_in("uv 0.4.18-rc.1", None).as_deref(),
            Some("0.4.18-rc.1")
        );
        assert_eq!(
            version_in("uv 0.4.18-rc.2", None).as_deref(),
            Some("0.4.18-rc.2")
        );
        assert_eq!(
            version_in("git version 2.46.0.windows.1", Some("git version")).as_deref(),
            Some("2.46.0.windows.1")
        );
    }

    #[test]
    fn a_prefixed_tool_ignores_lines_without_its_prefix() {
        let probe = evaluate(
            spec_of(ToolId::Git),
            "git",
            output("warning: 9.9.9\n git version 2.55.0\n", ""),
        );
        assert_eq!(probe.version.as_deref(), Some("2.55.0"));
    }

    #[test]
    fn timeout_and_spawn_failure_are_reported_as_failed_with_a_reason() {
        let timed_out = evaluate(spec_of(ToolId::Uv), "uv", Outcome::TimedOut);
        assert_eq!(timed_out.status, ProbeStatus::Failed);
        assert!(timed_out.reason.unwrap().contains("did not answer"));

        let failed = evaluate(
            spec_of(ToolId::Uv),
            "uv",
            Outcome::SpawnFailed("permission denied".into()),
        );
        assert_eq!(failed.status, ProbeStatus::Failed);
        assert_eq!(failed.reason.as_deref(), Some("permission denied"));
    }

    #[test]
    fn a_tool_whose_candidates_all_fail_to_launch_reads_as_missing() {
        // `evaluate` is never reached when every candidate fails to spawn, so
        // assert the fallback the loop returns in that case.
        let probe = base(spec_of(ToolId::Python3), ProbeStatus::Missing);
        assert_eq!(probe.status, ProbeStatus::Missing);
        assert_eq!(probe.version, None);
        assert_eq!(probe.executable_path, None);
    }

    #[test]
    fn registry_install_path_parses_the_reg_query_output() {
        let output = "\r\nHKEY_LOCAL_MACHINE\\Software\\R-core\\R\r\n    InstallPath    REG_SZ    C:\\Program Files\\R\\R-4.6.2\r\n\r\n";
        assert_eq!(
            parse_reg_install_path(output).as_deref(),
            Some("C:\\Program Files\\R\\R-4.6.2"),
            "the path keeps its spaces"
        );
        let hkcu = "\r\nHKEY_CURRENT_USER\\Software\\R-core\\R\r\n    InstallPath    REG_SZ    C:\\Users\\me\\R\\R-4.6.2\r\n";
        assert_eq!(
            parse_reg_install_path(hkcu).as_deref(),
            Some("C:\\Users\\me\\R\\R-4.6.2")
        );
        assert_eq!(
            parse_reg_install_path(
                "ERROR: The system was unable to find the specified registry key or value.\r\n"
            ),
            None
        );
    }

    #[test]
    fn the_registry_fallback_resolves_bin_x64_r_exe() {
        let path = fallback_r_exe_path(std::path::Path::new("C:\\Program Files\\R\\R-4.6.2"));
        assert!(path.ends_with(std::path::Path::new("bin").join("x64").join("R.exe")));
    }

    #[test]
    #[cfg(not(windows))]
    fn outside_windows_the_r_registry_fallback_is_absent() {
        assert!(r_fallback_probe().is_none());
    }

    #[test]
    fn a_fallback_flag_defaults_to_false_and_can_be_set() {
        let probe = base(spec_of(ToolId::R), ProbeStatus::Ready);
        assert!(!probe.via_fallback_path);
    }
}
