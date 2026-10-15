// ABOUTME: Owns native Pi RPC transport and process-tree lifecycle handles.
// ABOUTME: Terminates only its exact child identity and its platform-owned descendants.

#![allow(dead_code)]

use crate::process_tree::ProcessTree;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex};

#[derive(Debug, Clone, PartialEq)]
pub enum BridgeFrame {
    Event(Value),
    ExtensionUi(Value),
    ProtocolError(String),
    TransportError(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeError {
    ProcessClosed,
    Timeout,
    Transport(String),
}

impl BridgeError {
    pub fn is_process_closed(&self) -> bool {
        matches!(self, Self::ProcessClosed)
    }
}

/// Cap on stderr lines folded into an error message.
const DIAGNOSTIC_TAIL_LINES: usize = 20;

type PendingSender = oneshot::Sender<Result<Value, BridgeError>>;

struct BridgeInner {
    next_id: AtomicU64,
    outbound: mpsc::Sender<Value>,
    frames: Mutex<mpsc::Receiver<BridgeFrame>>,
    pending: Mutex<HashMap<String, PendingSender>>,
}

#[derive(Clone)]
pub struct PiRpcBridge {
    inner: Arc<BridgeInner>,
}

pub struct InMemoryPiProcess {
    outbound: mpsc::Receiver<Value>,
    incoming: Option<mpsc::Sender<Vec<u8>>>,
    fatal_frames: Option<mpsc::Sender<BridgeFrame>>,
}

pub struct PiRpcProcess {
    child: Arc<StdMutex<Child>>,
    diagnostics: std::sync::mpsc::Receiver<String>,
    tree: ProcessTree,
}

pub struct PiRpcProcessObserver {
    child: Arc<StdMutex<Child>>,
}

impl PiRpcBridge {
    pub fn attach(
        mut child: Child,
        max_frame_bytes: usize,
    ) -> Result<(Self, PiRpcProcess), String> {
        let tree = ProcessTree::attach(&mut child).inspect_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
        })?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Pi RPC process stdin is not piped".to_string())?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Pi RPC process stdout is not piped".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "Pi RPC process stderr is not piped".to_string())?;
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Value>(64);
        let (incoming_tx, incoming_rx) = mpsc::channel::<Vec<u8>>(64);
        let (frame_tx, frame_rx) = mpsc::channel(64);
        let inner = Arc::new(BridgeInner {
            next_id: AtomicU64::new(1),
            outbound: outbound_tx,
            frames: Mutex::new(frame_rx),
            pending: Mutex::new(HashMap::new()),
        });
        let parser_frame_tx = frame_tx.clone();
        // attach runs on the Tauri setup thread, outside any ambient tokio
        // runtime; the host runtime owns the process-wide tasks the rest of
        // the app (host server, terminal manager) already uses.
        crate::runtime::spawn(read_frames(
            incoming_rx,
            parser_frame_tx,
            Arc::clone(&inner),
            max_frame_bytes,
        ));

        let writer_frame_tx = frame_tx.clone();
        std::thread::Builder::new()
            .name("picot-pi-rpc-writer".into())
            .spawn(move || {
                while let Some(frame) = outbound_rx.blocking_recv() {
                    let mut encoded = frame.to_string();
                    encoded.push('\n');
                    if stdin.write_all(encoded.as_bytes()).is_err() || stdin.flush().is_err() {
                        let _ = writer_frame_tx.blocking_send(BridgeFrame::TransportError(
                            "Pi RPC writer failed".into(),
                        ));
                        break;
                    }
                }
            })
            .map_err(|error| format!("Cannot start Pi RPC writer: {error}"))?;

        std::thread::Builder::new()
            .name("picot-pi-rpc-reader".into())
            .spawn(move || read_jsonl_stdout(&mut stdout, incoming_tx, max_frame_bytes))
            .map_err(|error| format!("Cannot start Pi RPC reader: {error}"))?;

        let (diagnostic_tx, diagnostic_rx) = std::sync::mpsc::sync_channel(64);
        std::thread::Builder::new()
            .name("picot-pi-rpc-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let bounded: String = line.chars().take(4096).collect();
                    let _ = diagnostic_tx.try_send(bounded);
                }
            })
            .map_err(|error| format!("Cannot start Pi RPC stderr reader: {error}"))?;

        Ok((
            Self { inner },
            PiRpcProcess {
                child: Arc::new(StdMutex::new(child)),
                diagnostics: diagnostic_rx,
                tree,
            },
        ))
    }

    #[cfg(any(test, feature = "test-seams"))]
    pub(crate) fn in_memory(max_frame_bytes: usize) -> (Self, InMemoryPiProcess) {
        let (outbound_tx, outbound_rx) = mpsc::channel(32);
        let (incoming_tx, incoming_rx) = mpsc::channel(32);
        let (frame_tx, frame_rx) = mpsc::channel(32);
        let inner = Arc::new(BridgeInner {
            next_id: AtomicU64::new(1),
            outbound: outbound_tx,
            frames: Mutex::new(frame_rx),
            pending: Mutex::new(HashMap::new()),
        });
        let fatal_frames = frame_tx.clone();
        tokio::spawn(read_frames(
            incoming_rx,
            frame_tx,
            Arc::clone(&inner),
            max_frame_bytes,
        ));
        (
            Self { inner },
            InMemoryPiProcess {
                outbound: outbound_rx,
                incoming: Some(incoming_tx),
                fatal_frames: Some(fatal_frames),
            },
        )
    }

    pub async fn request(
        &self,
        mut command: Value,
        timeout: Duration,
    ) -> Result<Value, BridgeError> {
        let id = format!(
            "picot-{}",
            self.inner.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let object = command
            .as_object_mut()
            .ok_or_else(|| BridgeError::Transport("RPC command must be an object".into()))?;
        object.insert("id".into(), Value::String(id.clone()));

        let (response_tx, response_rx) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .await
            .insert(id.clone(), response_tx);
        if self.inner.outbound.send(command).await.is_err() {
            self.inner.pending.lock().await.remove(&id);
            return Err(BridgeError::ProcessClosed);
        }

        match tokio::time::timeout(timeout, response_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(BridgeError::ProcessClosed),
            Err(_) => {
                self.inner.pending.lock().await.remove(&id);
                Err(BridgeError::Timeout)
            }
        }
    }

    pub async fn send_frame(&self, frame: Value) -> Result<(), BridgeError> {
        self.inner
            .outbound
            .send(frame)
            .await
            .map_err(|_| BridgeError::ProcessClosed)
    }

    pub async fn next_frame(&self) -> Option<BridgeFrame> {
        self.inner.frames.lock().await.recv().await
    }

    pub(crate) async fn reject_pending(&self) {
        for (_, sender) in self.inner.pending.lock().await.drain() {
            let _ = sender.send(Err(BridgeError::ProcessClosed));
        }
    }
}

impl PiRpcProcess {
    /// Process id of the attached child, if it has not been reaped yet.
    pub fn pid(&self) -> Option<u32> {
        self.child.lock().ok().map(|child| child.id())
    }

    pub fn observer(&self) -> PiRpcProcessObserver {
        PiRpcProcessObserver {
            child: Arc::clone(&self.child),
        }
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, String> {
        self.child
            .lock()
            .map_err(|_| "Pi RPC process lock poisoned".to_string())?
            .try_wait()
            .map_err(|error| format!("Cannot inspect Pi RPC process: {error}"))
    }

    pub fn wait(&mut self) -> Result<ExitStatus, String> {
        self.child
            .lock()
            .map_err(|_| "Pi RPC process lock poisoned".to_string())?
            .wait()
            .map_err(|error| format!("Cannot wait for Pi RPC process: {error}"))
    }

    pub fn kill(&mut self) -> Result<bool, String> {
        let mut child = self
            .child
            .lock()
            .map_err(|_| "Pi RPC process lock poisoned".to_string())?;
        self.tree
            .terminate(&mut child)
            .map_err(|error| format!("Cannot stop Pi RPC process tree: {error}"))
    }

    /// Everything `pi` has written to stderr and we have not reported yet.
    /// The tail explains why a closed process rejected its last RPC request.
    pub fn drain_diagnostics(&self) -> Option<String> {
        let lines: Vec<String> = self.diagnostics.try_iter().collect();
        if lines.is_empty() {
            return None;
        }
        let start = lines.len().saturating_sub(DIAGNOSTIC_TAIL_LINES);
        Some(lines[start..].join("\n"))
    }
}

impl PiRpcProcessObserver {
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, String> {
        self.child
            .lock()
            .map_err(|_| "Pi RPC process lock poisoned".to_string())?
            .try_wait()
            .map_err(|error| format!("Cannot inspect Pi RPC process: {error}"))
    }
}

fn read_jsonl_stdout(
    stdout: &mut impl Read,
    incoming: mpsc::Sender<Vec<u8>>,
    max_frame_bytes: usize,
) {
    let mut chunk = [0_u8; 8192];
    let mut frame = Vec::new();
    let mut oversized = false;
    loop {
        let read = match stdout.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        for byte in &chunk[..read] {
            if *byte == b'\n' {
                if oversized {
                    if incoming
                        .blocking_send(vec![0; max_frame_bytes + 1])
                        .is_err()
                    {
                        return;
                    }
                } else {
                    if frame.last() == Some(&b'\r') {
                        frame.pop();
                    }
                    if incoming.blocking_send(std::mem::take(&mut frame)).is_err() {
                        return;
                    }
                }
                frame.clear();
                oversized = false;
            } else if !oversized {
                frame.push(*byte);
                if frame.len() > max_frame_bytes {
                    frame.clear();
                    oversized = true;
                }
            }
        }
    }
}

async fn read_frames(
    mut incoming: mpsc::Receiver<Vec<u8>>,
    frames: mpsc::Sender<BridgeFrame>,
    inner: Arc<BridgeInner>,
    max_frame_bytes: usize,
) {
    while let Some(raw) = incoming.recv().await {
        if raw.len() > max_frame_bytes {
            let _ = frames
                .send(BridgeFrame::ProtocolError(format!(
                    "Pi RPC frame exceeded {max_frame_bytes} bytes"
                )))
                .await;
            // A status frame with raw newlines (pi 0.84.2 multi-line
            // statusText) or an oversized frame loses that one frame; the
            // reader must keep serving later frames. Killing the reader here
            // turns one cosmetic frame into a dead runtime.
            continue;
        }
        let parsed = match serde_json::from_slice::<Value>(&raw) {
            Ok(value) => value,
            Err(error) => {
                let _ = frames
                    .send(BridgeFrame::ProtocolError(format!(
                        "Invalid Pi RPC JSONL frame: {error}"
                    )))
                    .await;
                continue;
            }
        };
        if let Some(id) = parsed.get("id").and_then(Value::as_str) {
            if let Some(sender) = inner.pending.lock().await.remove(id) {
                let _ = sender.send(Ok(parsed));
                continue;
            }
        }
        let frame = if parsed
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.starts_with("extension_ui"))
        {
            BridgeFrame::ExtensionUi(parsed)
        } else {
            BridgeFrame::Event(parsed)
        };
        let _ = frames.send(frame).await;
    }

    for (_, sender) in inner.pending.lock().await.drain() {
        let _ = sender.send(Err(BridgeError::ProcessClosed));
    }
}

#[cfg(windows)]
mod windows_job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    #[derive(Debug)]
    pub struct JobHandle(HANDLE);
    // SAFETY: handle is an opaque kernel object; Windows serializes operations on it.
    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    impl Drop for JobHandle {
        fn drop(&mut self) {
            unsafe {
                // Job was configured with KILL_ON_JOB_CLOSE; this also cleans
                // descendants when native bridge observes an unexpected exit.
                CloseHandle(self.0);
            }
        }
    }

    pub fn create_and_assign(pid: u32) -> Option<JobHandle> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) != 0;
            if !configured {
                CloseHandle(job);
                return None;
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() || AssignProcessToJobObject(job, process) == 0 {
                if !process.is_null() {
                    CloseHandle(process);
                }
                CloseHandle(job);
                return None;
            }
            CloseHandle(process);
            Some(JobHandle(job))
        }
    }

    pub fn terminate(job: &JobHandle) -> Result<(), String> {
        if unsafe { TerminateJobObject(job.0, 1) } == 0 {
            return Err(format!(
                "TerminateJobObject failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
}

/// Test seam: drives the bridge without a Pi child process. Compiled for this
/// crate's tests and for consuming packages that enable `test-seams`.
#[cfg(any(test, feature = "test-seams"))]
impl InMemoryPiProcess {
    pub async fn read_request(&mut self) -> Option<Value> {
        self.outbound.recv().await
    }

    pub fn try_read_request(&mut self) -> Option<Value> {
        self.outbound.try_recv().ok()
    }

    pub async fn write_frame(&mut self, value: Value) -> Result<(), BridgeError> {
        self.write_raw(format!("{}\n", value)).await
    }

    pub async fn write_raw(&mut self, raw: String) -> Result<(), BridgeError> {
        self.incoming
            .as_ref()
            .ok_or(BridgeError::ProcessClosed)?
            .send(raw.trim_end_matches('\n').as_bytes().to_vec())
            .await
            .map_err(|_| BridgeError::ProcessClosed)
    }

    pub async fn close(&mut self) {
        self.incoming.take();
        self.fatal_frames.take();
    }

    /// Close only the host→child request pipe. The bridge writer task then
    /// observes a failed send — the writer-fail fault class — while the
    /// child→host direction stays alive.
    pub fn fail_outbound(&mut self) {
        self.outbound.close();
        if let Some(fatal_frames) = self.fatal_frames.take() {
            let _ =
                fatal_frames.try_send(BridgeFrame::TransportError("Pi RPC writer failed".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BridgeFrame, PiRpcBridge};
    use serde_json::json;
    #[cfg(unix)]
    use std::process::{Command, Stdio};
    use std::time::Duration;

    #[tokio::test]
    async fn correlates_responses_and_surfaces_events() {
        let (bridge, mut process) = PiRpcBridge::in_memory(1024);

        let request = tokio::spawn({
            let bridge = bridge.clone();
            async move {
                bridge
                    .request(json!({ "type": "get_state" }), Duration::from_secs(1))
                    .await
            }
        });

        let outbound = process.read_request().await.expect("request frame");
        assert_eq!(outbound["type"], "get_state");
        let id = outbound["id"].as_str().expect("native request id");

        process
            .write_frame(json!({ "id": id, "type": "response", "success": true }))
            .await
            .expect("response frame");
        assert_eq!(request.await.unwrap().unwrap()["success"], true);

        process
            .write_frame(json!({ "type": "agent_start" }))
            .await
            .expect("event frame");
        assert_eq!(
            bridge.next_frame().await,
            Some(BridgeFrame::Event(json!({ "type": "agent_start" })))
        );
    }

    #[tokio::test]
    async fn rejects_oversized_frames_and_pending_requests_on_exit() {
        let (bridge, mut process) = PiRpcBridge::in_memory(32);
        let request = tokio::spawn({
            let bridge = bridge.clone();
            async move {
                bridge
                    .request(json!({ "type": "get_state" }), Duration::from_secs(1))
                    .await
            }
        });

        process.read_request().await.expect("request frame");
        process
            .write_raw(format!("{{\"value\":\"{}\"}}\n", "x".repeat(64)))
            .await
            .expect("oversized frame");

        assert!(matches!(
            bridge.next_frame().await,
            Some(BridgeFrame::ProtocolError(_))
        ));
        process.close().await;
        assert!(request.await.unwrap().unwrap_err().is_process_closed());
    }

    #[tokio::test]
    async fn invalid_jsonl_lines_do_not_kill_the_reader() {
        let (bridge, mut process) = PiRpcBridge::in_memory(1024);
        let request = tokio::spawn({
            let bridge = bridge.clone();
            async move {
                bridge
                    .request(json!({ "type": "get_tree" }), Duration::from_secs(2))
                    .await
            }
        });

        let outbound = process.read_request().await.expect("request frame");
        let id = outbound["id"].as_str().expect("request id").to_owned();
        // Pi 0.84.2 emits status frames whose statusText carries raw
        // newlines; the JSONL line splits into two invalid halves. The
        // reader must drop them and keep serving later responses.
        process
            .write_raw("{\"type\":\"extension_ui_request\",\"statusText\":\"line one\n".into())
            .await
            .expect("first half");
        process
            .write_raw("line two\"}\n".into())
            .await
            .expect("second half");
        let first_garbage =
            tokio::time::timeout(Duration::from_millis(200), bridge.next_frame()).await;
        assert!(
            matches!(first_garbage, Ok(Some(BridgeFrame::ProtocolError(_)))),
            "garbage lines must be reported, not swallowed silently"
        );

        process
            .write_frame(json!({
                "id": id,
                "type": "response",
                "command": "get_tree",
                "success": true
            }))
            .await
            .expect("response after garbage");
        assert_eq!(
            request
                .await
                .unwrap()
                .expect("reader must survive garbage lines")["command"],
            "get_tree"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_tree_uses_child_group_and_escalates_to_termination() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("trap '' TERM; while :; do sleep 1; done")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        let child = command.spawn().unwrap();
        let (bridge, mut process) = PiRpcBridge::attach(child, 1024).unwrap();
        process.kill().unwrap();
        let status = process.wait().unwrap();
        assert!(!status.success());
        drop(bridge);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn attaches_to_a_real_jsonl_subprocess() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("IFS= read -r line; printf '%s\\n' '{\"id\":\"picot-1\",\"type\":\"response\",\"command\":\"get_state\",\"success\":true}'")
            .stdin(Stdio::piped());
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = command.spawn().unwrap();
        let (bridge, mut process) = PiRpcBridge::attach(child, 1024).unwrap();

        let response = bridge
            .request(json!({ "type": "get_state" }), Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(response["command"], "get_state");
        assert!(process.wait().unwrap().success());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retains_stderr_tail_for_a_closed_process() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("echo 'session resume failed' >&2; exit 1")
            .stdin(Stdio::piped());
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = command.spawn().unwrap();
        let (_bridge, process) = PiRpcBridge::attach(child, 1024).unwrap();

        std::thread::sleep(Duration::from_millis(25));
        assert_eq!(
            process.drain_diagnostics().as_deref(),
            Some("session resume failed")
        );
    }
}
