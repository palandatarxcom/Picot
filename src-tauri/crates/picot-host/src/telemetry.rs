// ABOUTME: Defines anonymous rollout telemetry schema and fail-closed sampling.
// ABOUTME: Emits only allowlisted coarse runtime facts; transport stays non-blocking.
//
// D10 cohort threshold is undecided (waits Gate D telemetry plan): this schema
// ships ahead of its wiring and stays intentionally unwired until D10 lands.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const SUCCESS_SAMPLE_RATE: f64 = 0.01;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientClass {
    NativeDesktop,
    PairedRemote,
    UnpairedBrowser,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    Legacy,
    Native,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlagState {
    Off,
    Dogfood,
    Cohort,
    DefaultOn,
    InvalidFallback,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteFamily {
    ExistingShell,
    NativeShell,
    Bootstrap,
    Static,
    V2Ws,
    LegacyApi,
    CompatApi,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationFamily {
    Bootstrap,
    Chat,
    Session,
    Files,
    Git,
    Terminal,
    Settings,
    Ephemeral,
    Cost,
    Search,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Failure,
    Cancelled,
    Timeout,
    Reconnect,
    Crash,
    Fallback,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LatencyBucket {
    #[serde(rename = "0_100")]
    ZeroTo100,
    #[serde(rename = "101_500")]
    OneOhOneTo500,
    #[serde(rename = "501_2000")]
    FiveOhOneTo2000,
    #[serde(rename = "2001_10000")]
    TwoThousandOneTo10000,
    #[serde(rename = "over_10000")]
    Over10000,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CountBucket {
    #[serde(rename = "0")]
    Zero,
    #[serde(rename = "1_10")]
    OneTo10,
    #[serde(rename = "11_100")]
    ElevenTo100,
    #[serde(rename = "over_100")]
    Over100,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SizeBucket {
    #[serde(rename = "0_1k")]
    ZeroTo1k,
    #[serde(rename = "1k_64k")]
    OneTo64k,
    #[serde(rename = "64k_1m")]
    SixtyFourKTo1m,
    #[serde(rename = "over_1m")]
    Over1m,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BuildChannel {
    Dev,
    Stable,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OsFamily {
    Macos,
    Windows,
    Linux,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryEvent {
    pub schema_version: u8,
    pub anonymous_client_class: ClientClass,
    pub runtime_mode: RuntimeMode,
    pub flag_state: FlagState,
    pub protocol_version: u8,
    pub route_family: RouteFamily,
    pub operation_family: OperationFamily,
    pub outcome: Outcome,
    pub failure_code: Option<String>,
    pub latency_bucket_ms: LatencyBucket,
    pub event_count_bucket: CountBucket,
    pub payload_size_bucket: SizeBucket,
    pub sequence_gap: bool,
    pub build_channel: BuildChannel,
    pub host_os_family: OsFamily,
    pub created_at_bucket: String,
}

#[derive(Debug, Clone, Copy)]
pub struct TelemetryConfig {
    pub enabled: bool,
    pub success_sample_rate: f64,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            success_sample_rate: SUCCESS_SAMPLE_RATE,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AnonymousTelemetryEmitter {
    config: TelemetryConfig,
}

impl AnonymousTelemetryEmitter {
    pub fn new(config: TelemetryConfig) -> Self {
        Self { config }
    }

    pub fn emit(&self, event: &TelemetryEvent) -> Option<Value> {
        if !self.config.enabled
            || !self.config.success_sample_rate.is_finite()
            || !(0.0..=1.0).contains(&self.config.success_sample_rate)
            || event.schema_version != 1
            || !valid_date_bucket(&event.created_at_bucket)
            || event
                .failure_code
                .as_ref()
                .is_some_and(|code| !stable_failure_code(code))
        {
            return None;
        }
        let always_sample = !matches!(event.outcome, Outcome::Success);
        if !always_sample && !sample(event, self.config.success_sample_rate) {
            return None;
        }
        serde_json::to_value(event).ok()
    }
}

fn valid_date_bucket(value: &str) -> bool {
    value.len() == 10
        && value.as_bytes().iter().enumerate().all(|(i, b)| {
            if matches!(i, 4 | 7) {
                *b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

fn stable_failure_code(value: &str) -> bool {
    matches!(
        value,
        "handshake_rejected"
            | "runtime_crashed"
            | "event_sequence_gap"
            | "command_too_large"
            | "startup_failed"
    )
}

fn sample(event: &TelemetryEvent, rate: f64) -> bool {
    let bytes = Sha256::digest(serde_json::to_vec(event).unwrap_or_default());
    let bucket = u64::from_be_bytes(bytes[..8].try_into().unwrap()) as f64 / u64::MAX as f64;
    bucket < rate
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(outcome: Outcome) -> TelemetryEvent {
        TelemetryEvent {
            schema_version: 1,
            anonymous_client_class: ClientClass::NativeDesktop,
            runtime_mode: RuntimeMode::Native,
            flag_state: FlagState::Off,
            protocol_version: 1,
            route_family: RouteFamily::NativeShell,
            operation_family: OperationFamily::Chat,
            outcome,
            failure_code: None,
            latency_bucket_ms: LatencyBucket::ZeroTo100,
            event_count_bucket: CountBucket::Zero,
            payload_size_bucket: SizeBucket::ZeroTo1k,
            sequence_gap: false,
            build_channel: BuildChannel::Dev,
            host_os_family: OsFamily::Macos,
            created_at_bucket: "2026-08-30".into(),
        }
    }

    #[test]
    fn disabled_by_default_and_success_sampling_is_bounded() {
        assert!(AnonymousTelemetryEmitter::default()
            .emit(&event(Outcome::Failure))
            .is_none());
        let emitter = AnonymousTelemetryEmitter::new(TelemetryConfig {
            enabled: true,
            success_sample_rate: 0.0,
        });
        assert!(emitter.emit(&event(Outcome::Success)).is_none());
        assert!(emitter.emit(&event(Outcome::Failure)).is_some());
    }

    #[test]
    fn rejects_invalid_config_and_failure_text() {
        let bad_rate = AnonymousTelemetryEmitter::new(TelemetryConfig {
            enabled: true,
            success_sample_rate: 2.0,
        });
        assert!(bad_rate.emit(&event(Outcome::Failure)).is_none());
        let mut invalid = event(Outcome::Failure);
        invalid.failure_code = Some("raw cwd /token prompt".into());
        assert!(AnonymousTelemetryEmitter::new(TelemetryConfig {
            enabled: true,
            success_sample_rate: 1.0
        })
        .emit(&invalid)
        .is_none());
    }

    #[test]
    fn serialized_event_has_no_sensitive_fields_or_values() {
        let mut item = event(Outcome::Failure);
        item.failure_code = Some("runtime_crashed".into());
        let value = AnonymousTelemetryEmitter::new(TelemetryConfig {
            enabled: true,
            success_sample_rate: 1.0,
        })
        .emit(&item)
        .unwrap();
        let object = value.as_object().unwrap();
        for forbidden in [
            "capability",
            "token",
            "root",
            "session",
            "path",
            "prompt",
            "cwd",
            "port",
            "url",
            "credential",
        ] {
            assert!(
                !object
                    .keys()
                    .any(|key| key.to_ascii_lowercase().contains(forbidden)),
                "{forbidden}"
            );
        }
        let text = value.to_string();
        for forbidden in ["/Users/", "PI_STUDIO_", "raw cwd", "secret"] {
            assert!(!text.contains(forbidden));
        }
    }
}
