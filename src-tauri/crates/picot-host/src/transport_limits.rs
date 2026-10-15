// ABOUTME: Centralizes protocol payload limits and bounded transport decisions.
// ABOUTME: Separates physical frame caps from business payload and outbound caps.

use serde_json::Value;
use std::collections::VecDeque;

pub const WS_PHYSICAL_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const HTTP_DEFAULT_BODY_BYTES: usize = 1024 * 1024;
#[allow(dead_code)]
pub const PASTE_BODY_BYTES: usize = 4 * 1024 * 1024;
pub const RUNTIME_COMMAND_BYTES: usize = 1024 * 1024;
pub const DATA_REQUEST_BYTES: usize = 256 * 1024;
pub const HOST_REQUEST_BYTES: usize = 256 * 1024;
// Outbound session payloads (responses, snapshots, events) share the physical
// frame cap — no business ceiling below it, matching the upstream reference
// host and the legacy embedded-server behavior: a session the old runtime
// loaded must load here too. Inbound command caps stay tight — those gate
// untrusted input.
pub const OUTBOUND_PAYLOAD_BYTES: usize = WS_PHYSICAL_FRAME_BYTES;
pub const PROGRESS_BYTES: usize = 64 * 1024;
pub const RAW_FILE_BYTES: usize = 20 * 1024 * 1024;
pub const EXPORT_BYTES: usize = 20 * 1024 * 1024;
pub const SESSION_FILE_BYTES: usize = 20 * 1024 * 1024;
#[allow(dead_code)]
pub const OUTBOUND_QUEUE_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum PayloadKind {
    Command,
    DataRequest,
    HostRequest,
    Response,
    Snapshot,
    Event,
    Progress,
    RawFile,
    Export,
    SessionFile,
}

pub fn serialized_size(value: &Value) -> usize {
    value.to_string().len()
}

pub fn validate(value: &Value, kind: PayloadKind) -> Result<(), &'static str> {
    let limit = match kind {
        PayloadKind::Command => RUNTIME_COMMAND_BYTES,
        PayloadKind::DataRequest => DATA_REQUEST_BYTES,
        PayloadKind::HostRequest => HOST_REQUEST_BYTES,
        PayloadKind::Response | PayloadKind::Snapshot | PayloadKind::Event => {
            OUTBOUND_PAYLOAD_BYTES
        }
        PayloadKind::Progress => PROGRESS_BYTES,
        PayloadKind::RawFile => RAW_FILE_BYTES,
        PayloadKind::Export => EXPORT_BYTES,
        PayloadKind::SessionFile => SESSION_FILE_BYTES,
    };
    if serialized_size(value) > limit {
        Err(match kind {
            PayloadKind::Command => "command_too_large",
            PayloadKind::DataRequest => "data_request_too_large",
            PayloadKind::HostRequest => "host_request_too_large",
            PayloadKind::Response => "response_too_large",
            PayloadKind::Snapshot => "snapshot_too_large",
            PayloadKind::Event => "event_too_large",
            PayloadKind::Progress => "progress_too_large",
            PayloadKind::RawFile => "raw_file_too_large",
            PayloadKind::Export => "export_too_large",
            PayloadKind::SessionFile => "session_file_too_large",
        })
    } else {
        Ok(())
    }
}

/// Bounded outbound policy: terminal frames survive saturation; nonterminal
/// progress frames may be coalesced by key or dropped. This keeps queue-full
/// behavior explicit instead of silently losing a completion response.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundFrame {
    Data { key: String, payload: String },
    Terminal { key: String, payload: String },
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct OutboundQueue {
    capacity: usize,
    frames: VecDeque<OutboundFrame>,
}

#[allow(dead_code)]
impl OutboundQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            frames: VecDeque::new(),
        }
    }

    pub fn push_data(&mut self, key: impl Into<String>, payload: impl Into<String>) -> bool {
        let key = key.into();
        if let Some(existing) = self.frames.iter_mut().find(
            |frame| matches!(frame, OutboundFrame::Data { key: existing, .. } if existing == &key),
        ) {
            *existing = OutboundFrame::Data {
                key,
                payload: payload.into(),
            };
            return true;
        }
        if self.frames.len() >= self.capacity {
            return false;
        }
        self.frames.push_back(OutboundFrame::Data {
            key,
            payload: payload.into(),
        });
        true
    }

    pub fn push_terminal(&mut self, key: impl Into<String>, payload: impl Into<String>) -> bool {
        let key = key.into();
        let payload = payload.into();
        // One terminal outcome per request/operation key. A repeated outcome
        // updates in place, preventing retries from growing the queue.
        if let Some(existing) = self.frames.iter_mut().find(
            |frame| matches!(frame, OutboundFrame::Terminal { key: existing, .. } if existing == &key),
        ) {
            *existing = OutboundFrame::Terminal { key, payload };
            return true;
        }
        // Terminal outcomes have priority over coalescible data, but remain
        // bounded: evict oldest data first, then oldest terminal outcome.
        if self.frames.len() >= self.capacity {
            if let Some(index) = self
                .frames
                .iter()
                .position(|frame| matches!(frame, OutboundFrame::Data { .. }))
            {
                self.frames.remove(index);
            } else {
                self.frames.pop_front();
            }
        }
        self.frames
            .push_back(OutboundFrame::Terminal { key, payload });
        true
    }

    pub fn pop(&mut self) -> Option<OutboundFrame> {
        self.frames.pop_front()
    }
    pub fn len(&self) -> usize {
        self.frames.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn business_limit_is_separate_from_physical_limit() {
        let value = json!({"message": "x".repeat(RUNTIME_COMMAND_BYTES)});
        assert_eq!(
            validate(&value, PayloadKind::Command),
            Err("command_too_large")
        );
        assert!(
            WS_PHYSICAL_FRAME_BYTES
                .to_string()
                .parse::<usize>()
                .unwrap()
                > RUNTIME_COMMAND_BYTES
        );
    }

    #[test]
    fn queue_coalesces_data_but_retains_terminal_frames() {
        let mut queue = OutboundQueue::new(1);
        assert!(queue.push_data("progress", "first"));
        assert!(queue.push_data("progress", "latest"));
        assert!(!queue.push_data("other", "dropped"));
        assert!(queue.push_terminal("request", "completed"));
        assert_eq!(queue.len(), 1);
        assert_eq!(
            queue.pop(),
            Some(OutboundFrame::Terminal {
                key: "request".into(),
                payload: "completed".into()
            })
        );
    }

    #[test]
    fn terminal_outcomes_are_keyed_and_bounded() {
        let mut queue = OutboundQueue::new(2);
        assert!(queue.push_terminal("request-1", "first"));
        assert!(queue.push_terminal("request-1", "latest"));
        assert_eq!(queue.len(), 1);
        assert!(queue.push_terminal("request-2", "second"));
        assert!(queue.push_terminal("request-3", "third"));
        assert_eq!(queue.len(), 2);
        assert_eq!(
            queue.pop(),
            Some(OutboundFrame::Terminal {
                key: "request-2".into(),
                payload: "second".into()
            })
        );
        assert_eq!(
            queue.pop(),
            Some(OutboundFrame::Terminal {
                key: "request-3".into(),
                payload: "third".into()
            })
        );
    }

    #[test]
    fn terminal_outcome_evicts_data_before_terminal() {
        let mut queue = OutboundQueue::new(2);
        assert!(queue.push_data("progress", "latest"));
        assert!(queue.push_terminal("request-1", "first"));
        assert!(queue.push_terminal("request-2", "second"));
        assert_eq!(queue.len(), 2);
        assert_eq!(
            queue.pop(),
            Some(OutboundFrame::Terminal {
                key: "request-1".into(),
                payload: "first".into()
            })
        );
        assert_eq!(
            queue.pop(),
            Some(OutboundFrame::Terminal {
                key: "request-2".into(),
                payload: "second".into()
            })
        );
    }

    #[test]
    fn route_specific_limits_fail_closed() {
        assert_eq!(
            validate(
                &json!({"bytes": "x".repeat(RAW_FILE_BYTES)}),
                PayloadKind::RawFile
            ),
            Err("raw_file_too_large")
        );
        assert_eq!(
            validate(
                &json!({"bytes": "x".repeat(EXPORT_BYTES)}),
                PayloadKind::Export
            ),
            Err("export_too_large")
        );
        assert_eq!(
            validate(
                &json!({"bytes": "x".repeat(SESSION_FILE_BYTES)}),
                PayloadKind::SessionFile
            ),
            Err("session_file_too_large")
        );
    }
}
