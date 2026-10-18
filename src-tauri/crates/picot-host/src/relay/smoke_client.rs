// ABOUTME: Standalone relay pairing smoke client (plan T11, spec §2.6).
// ABOUTME: One-shot client: offer → role=client data socket → E2EE handshake
// ABOUTME: → pairing hello → hello_ack device token → one data-plane probe.
// ABOUTME: Secrets never print in full; the pairing token never rides argv.

//! The library half of `examples/relay-smoke-client.rs`: the example is a
//! thin `main` over [`parse_args`] + [`run_smoke`], so the exact code a
//! manual smoke run executes is what the integration test
//! (`tests/relay_smoke_client.rs`) drives against the mock relay.
//!
//! Isolation (plan T11 / R10-2): reads nothing from user data/config dirs
//! and writes nothing outside stdout/stderr — the offer comes from argv
//! (`--offer-url`, optionally `@file`) or `RELAY_SMOKE_OFFER`, the pairing
//! token from the one-scan offer or `PICOT_SMOKE_TOKEN`, session state stays
//! in memory. A `--pairing-token` argv is rejected outright (argv leaks via
//! ps), and every rendered line passes secret redaction.

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::relay::e2ee;
use crate::relay::pairing::{parse_connection_offer, ConnectionOfferV2};

/// Env var carrying the offer URL (task T11; argv `--offer-url` alternative).
pub const OFFER_ENV: &str = "RELAY_SMOKE_OFFER";
/// Env var holding the pairing token when the offer is a token-less Paseo URL.
pub const DEFAULT_PAIRING_TOKEN_ENV: &str = "PICOT_SMOKE_TOKEN";

/// Default device identity for the pairing exchange (`deviceId`).
const DEFAULT_DEVICE_ID: &str = "smoke-1";

/// Per-step network budget (dial, handshake, hello, probe).
const STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// The relay may drop client frames sent before the daemon dials its data
/// socket (the mock relay does); resend the hello on silence.
const HELLO_RESEND_AFTER: Duration = Duration::from_secs(2);
const HELLO_MAX_SENDS: usize = 3;
/// Head start for the daemon's data dial before the first e2ee_hello.
const PRE_HELLO_DELAY: Duration = Duration::from_millis(250);

pub const USAGE: &str = "usage: relay-smoke-client \
[--endpoint host:port] [--offer-url <url|@file>] [--device-id id] \
[--pairing-token-env NAME] [--target <json|@file>]\n\
offer url: --offer-url or env RELAY_SMOKE_OFFER; pairing token: inside the \
one-scan offer, or env PICOT_SMOKE_TOKEN — never argv";

/// Where the offer text comes from (resolved inside [`run_smoke`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferSource {
    /// Literal URL or payload from argv (or a `@file` path to read).
    Argv(String),
    /// `RELAY_SMOKE_OFFER` at run time.
    Env,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeOptions {
    pub offer_source: OfferSource,
    pub endpoint_override: Option<String>,
    pub device_id: String,
    /// Env var name read when the offer carries no pairingToken.
    pub pairing_token_env: String,
    /// Runtime target for the data-plane probe as a JSON string
    /// (`{"workspaceId":…,"sessionId":…,"instanceId":…}`), or `@file`.
    pub target: Option<String>,
}

/// Parse CLI flags. Pure (no env reads) so the contract is unit-testable:
/// `--pairing-token` hard-errors (R10: tokens never ride argv), values may
/// use the `@file` form, no positional arguments exist.
pub fn parse_args(args: &[String]) -> Result<SmokeOptions, String> {
    let mut offer_source: Option<OfferSource> = None;
    let mut endpoint_override = None;
    let mut device_id = DEFAULT_DEVICE_ID.to_string();
    let mut pairing_token_env = DEFAULT_PAIRING_TOKEN_ENV.to_string();
    let mut target = None;
    let mut values = args.iter();
    while let Some(flag) = values.next() {
        let mut take = |slot: &mut Option<String>| -> Result<(), String> {
            let value = values
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            *slot = Some(value.clone());
            Ok(())
        };
        match flag.as_str() {
            "--endpoint" => take(&mut endpoint_override)?,
            "--offer-url" | "--offer" => {
                let mut slot = None;
                take(&mut slot)?;
                offer_source = Some(OfferSource::Argv(slot.unwrap()));
            }
            "--device-id" => {
                let mut slot = None;
                take(&mut slot)?;
                device_id = slot.unwrap();
            }
            "--pairing-token-env" => {
                let mut slot = None;
                take(&mut slot)?;
                pairing_token_env = slot.unwrap();
            }
            "--target" => take(&mut target)?,
            // Exists only to fail loudly: argv leaks via ps (plan T11 step 1).
            "--pairing-token" => {
                return Err(
                    "pairing token must not be passed via argv; use the one-scan offer or \
                     the PICOT_SMOKE_TOKEN env var"
                        .to_string(),
                )
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if device_id.is_empty() {
        return Err("device id must not be empty".to_string());
    }
    let offer_source = offer_source.unwrap_or(OfferSource::Env);
    Ok(SmokeOptions {
        offer_source,
        endpoint_override,
        device_id,
        pairing_token_env,
        target,
    })
}

/// Everything the smoke run established, secrets kept private so [`render`]
/// cannot leak them by construction.
pub struct SmokeReport {
    pub endpoint: String,
    pub use_tls: bool,
    pub server_id: String,
    /// The minted device token from hello_ack — never rendered in full.
    device_token: Option<String>,
    pairing_token: String,
    probe_request_id: String,
    /// Wire `type` of the correlated probe response (e.g. `runtime_subscribed`).
    pub probe_frame_type: String,
    /// `error.code` when the probe response is a refusal (e.g. `forbidden_class`).
    pub probe_error_code: Option<String>,
}

impl std::fmt::Debug for SmokeReport {
    /// Manual Debug: token fields render redacted — a derived impl would
    /// hand the secrets to any `{:?}` sink.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmokeReport")
            .field("endpoint", &self.endpoint)
            .field("use_tls", &self.use_tls)
            .field("server_id", &self.server_id)
            .field("device_token", &self.device_token.as_deref().map(preview))
            .field("pairing_token", &preview(&self.pairing_token))
            .field("probe_request_id", &self.probe_request_id)
            .field("probe_frame_type", &self.probe_frame_type)
            .field("probe_error_code", &self.probe_error_code)
            .finish()
    }
}

impl SmokeReport {
    /// The minted device token, exposed for test assertions only — callers
    /// must never print it (use [`render`], which redacts).
    pub fn device_token(&self) -> Option<&str> {
        self.device_token.as_deref()
    }

    /// Success = pairing exchange + a correlated response frame; the probe's
    /// positivity (`runtime_subscribed`) additionally needs a live target.
    pub fn ok(&self) -> bool {
        !self.probe_frame_type.is_empty()
    }

    pub fn render(&self) -> String {
        let pairing = match (&self.device_token, self.pairing_token.is_empty()) {
            (Some(_), _) => format!(
                "pairing: one-scan exchange ok (device token {} received, redacted)",
                preview(self.device_token.as_deref().unwrap_or(""))
            ),
            (None, false) => format!(
                "pairing: returning device token {}",
                preview(&self.pairing_token)
            ),
            (None, true) => "pairing: (no token present)".to_string(),
        };
        let probe = match &self.probe_error_code {
            Some(code) => format!(
                "data-plane probe: {} -> error {} (requestId {})",
                self.probe_frame_type, code, self.probe_request_id
            ),
            None => format!(
                "data-plane probe: {} (requestId {})",
                self.probe_frame_type, self.probe_request_id
            ),
        };
        let mut lines = format!(
            "picot relay smoke client (plan T11)\nendpoint: {} ({})\nserverId: {}\ne2ee handshake: ready\n{pairing}\n{probe}\n",
            self.endpoint,
            if self.use_tls { "tls" } else { "plain" },
            self.server_id,
        );
        if self.ok() {
            lines.push_str("SMOKE OK\n");
        }
        // Belt and suspenders: even though no secret was interpolated above,
        // scan the rendered text for the live secrets before it leaves.
        redact(&mut lines, &self.pairing_token);
        if let Some(token) = &self.device_token {
            redact(&mut lines, token);
        }
        lines
    }
}

/// First characters + an explicit redaction marker — enough to correlate
/// runs (`picot_device…` vs `picot_pairin…` — both fixed public prefixes),
/// never enough to reuse.
fn preview(secret: &str) -> String {
    let head: String = secret.chars().take(12).collect();
    format!("\"{head}…\"")
}

/// Overwrite any occurrence of `secret` in `text` in place.
fn redact(text: &mut String, secret: &str) {
    if secret.len() >= 8 {
        *text = text.replace(secret, "<redacted>");
    }
}

type ClientStream = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send_text(stream: &mut ClientStream, text: &str) -> Result<(), String> {
    stream
        .send(Message::Text(text.to_string()))
        .await
        .map_err(|error| format!("socket send failed: {error}"))
}

async fn recv_text(stream: &mut ClientStream) -> Result<String, String> {
    loop {
        let frame = tokio::time::timeout(STEP_TIMEOUT, stream.next())
            .await
            .map_err(|_| "timed out waiting for a relay frame".to_string())?
            .transpose()
            .map_err(|error| format!("socket read failed: {error}"))?;
        match frame {
            Some(Message::Text(text)) => return Ok(text),
            // ping/pong is liveness, not content
            Some(Message::Ping(_) | Message::Pong(_)) => continue,
            Some(Message::Close(_)) | None => return Err("socket closed by the relay".to_string()),
            Some(_) => return Err("unexpected non-text relay frame".to_string()),
        }
    }
}

fn send_sealed(shared: &[u8; 32], plaintext: &str) -> String {
    BASE64_STANDARD.encode(e2ee::seal(shared, plaintext.as_bytes()))
}

fn open_sealed(shared: &[u8; 32], wire: &str) -> Result<String, String> {
    let bundle = BASE64_STANDARD
        .decode(wire)
        .map_err(|_| "relay frame is not valid base64".to_string())?;
    let plaintext = e2ee::open(shared, &bundle)?;
    String::from_utf8(plaintext).map_err(|_| "relay plaintext is not UTF-8".to_string())
}

fn resolve_arg_value(value: &str) -> Result<String, String> {
    match value.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read offer file {path:?}: {error}")),
        None => Ok(value.to_string()),
    }
}

/// Full smoke run (the example's and the test's entire body):
/// resolve offer → dial `ws(s)://…?serverId=…&role=client&v=2` → E2EE
/// handshake → pairing hello → one data-plane probe → report.
pub async fn run_smoke(options: &SmokeOptions) -> Result<SmokeReport, String> {
    let offer_text = match &options.offer_source {
        OfferSource::Argv(value) => resolve_arg_value(value)?,
        OfferSource::Env => std::env::var(OFFER_ENV)
            .map_err(|_| format!("no offer: pass --offer-url or set {OFFER_ENV}"))?,
    };
    let offer = parse_connection_offer(&offer_text)?;
    let (endpoint, use_tls) = match &options.endpoint_override {
        Some(endpoint) => (endpoint.clone(), offer.relay.use_tls),
        None => (offer.relay.endpoint.clone(), offer.relay.use_tls),
    };
    let pairing_token = match &offer.pairing_token {
        Some(token) => token.clone(),
        None => std::env::var(&options.pairing_token_env).map_err(|_| {
            format!(
                "offer carries no pairing token and env {} is unset",
                options.pairing_token_env
            )
        })?,
    };
    // The token stays owned here for error redaction; smoke_run gets a copy.
    let report = smoke_run(&offer, &endpoint, use_tls, options, pairing_token.clone())
        .await
        .map_err(|error| redact_string(error, &pairing_token))?;
    Ok(report)
}

fn redact_string(mut text: String, secret: &str) -> String {
    redact(&mut text, secret);
    text
}

async fn smoke_run(
    offer: &ConnectionOfferV2,
    endpoint: &str,
    use_tls: bool,
    options: &SmokeOptions,
    pairing_token: String,
) -> Result<SmokeReport, String> {
    let scheme = if use_tls { "wss" } else { "ws" };
    let server_id = utf8_percent_encode(&offer.server_id, NON_ALPHANUMERIC);
    let url = format!("{scheme}://{endpoint}/ws?serverId={server_id}&role=client&v=2");
    let request = url
        .into_client_request()
        .map_err(|error| format!("invalid relay url: {error}"))?;
    let dial = tokio_tungstenite::connect_async_tls_with_config(
        request,
        None,
        false,
        None::<tokio_tungstenite::Connector>,
    );
    let dial_outcome = tokio::time::timeout(STEP_TIMEOUT, dial)
        .await
        .map_err(|_| "timed out dialing the relay".to_string())?;
    let (mut stream, _response) =
        dial_outcome.map_err(|error| format!("relay dial failed: {error}"))?;

    // Fresh throwaway keypair per run (plan T11): nothing persisted anywhere.
    let client_secret = crypto_box::SecretKey::generate(&mut rand::rngs::OsRng);
    let client_public_b64 = BASE64_STANDARD.encode(client_secret.public_key().as_bytes());
    let shared = e2ee::derive_shared_key(&client_secret.to_bytes(), &offer.daemon_public_key_b64)?;

    // e2ee_hello → e2ee_ready. Frames sent before the daemon's data dial can
    // be dropped (mock-relay pipe semantics), so the hello resends on silence.
    let hello = format!("{{\"type\":\"e2ee_hello\",\"key\":\"{client_public_b64}\"}}");
    tokio::time::sleep(PRE_HELLO_DELAY).await;
    send_text(&mut stream, &hello).await?;
    let ready = 'ready: {
        for sent in 1..=HELLO_MAX_SENDS {
            match tokio::time::timeout(HELLO_RESEND_AFTER, stream.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    break 'ready text;
                }
                Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
                Ok(Some(Ok(Message::Close(_)))) | Ok(None) => {
                    return Err("socket closed before e2ee_ready".to_string());
                }
                Ok(Some(Ok(_))) => return Err("unexpected non-text frame pre-ready".to_string()),
                Ok(Some(Err(error))) => {
                    return Err(format!("socket read failed pre-ready: {error}"));
                }
                Err(_) if sent < HELLO_MAX_SENDS => {
                    send_text(&mut stream, &hello).await?;
                }
                Err(_) => return Err("timed out waiting for e2ee_ready".to_string()),
            }
        }
        return Err("timed out waiting for e2ee_ready".to_string());
    };
    if ready != e2ee::e2ee_ready_frame() {
        return Err(format!(
            "unexpected pre-ready frame (expected e2ee_ready): {}",
            preview(&ready)
        ));
    }

    // One-scan pairing hello (spec §2.6): the daemon exchanges the token and
    // hello_ack carries the minted device token back.
    let sealed_hello = send_sealed(
        &shared,
        &json!({
            "type": "hello",
            "protocolVersion": 2,
            "clientType": "remote",
            "clientId": options.device_id,
            "deviceId": options.device_id,
            "pairingToken": pairing_token,
        })
        .to_string(),
    );
    send_text(&mut stream, &sealed_hello).await?;
    let ack_wire = recv_text(&mut stream).await?;
    let ack: Value = serde_json::from_str(&open_sealed(&shared, &ack_wire)?)
        .map_err(|_| "hello_ack is not JSON".to_string())?;
    if ack.get("type").and_then(Value::as_str) != Some("hello_ack") {
        let code = ack["error"]["code"].as_str().unwrap_or("unknown");
        return Err(format!("pairing rejected: {code}"));
    }
    let device_token = ack
        .get("deviceToken")
        .and_then(Value::as_str)
        .map(str::to_owned);

    // One data-plane round-trip: runtime_subscribe against the given target
    // (a live owner runtime yields runtime_subscribed; a placeholder target
    // still proves the encrypted request/response pipe with a correlated
    // error frame).
    let probe_request_id = "smoke-1".to_string();
    let target_text = match &options.target {
        Some(value) => resolve_arg_value(value)?,
        None => "{\"workspaceId\":\"picot-smoke\",\"sessionId\":\"picot-smoke\",\"instanceId\":\"picot-smoke\"}".to_string(),
    };
    let target: Value =
        serde_json::from_str(&target_text).map_err(|_| "target is not valid JSON".to_string())?;
    send_text(
        &mut stream,
        &send_sealed(
            &shared,
            &json!({
                "type": "runtime_subscribe",
                "requestId": probe_request_id,
                "target": target,
            })
            .to_string(),
        ),
    )
    .await?;
    let probe_wire = recv_text(&mut stream).await?;
    let probe: Value = serde_json::from_str(&open_sealed(&shared, &probe_wire)?)
        .map_err(|_| "probe response is not JSON".to_string())?;
    let correlated = probe.get("requestId").and_then(Value::as_str)
        == Some(probe_request_id.as_str())
        || probe.get("type").and_then(Value::as_str) == Some("runtime_subscribed");
    if !correlated {
        return Err(format!(
            "probe response does not correlate with {probe_request_id}: {}",
            preview(&probe_wire)
        ));
    }
    let probe_frame_type = probe
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let probe_error_code = probe
        .pointer("/error/code")
        .and_then(Value::as_str)
        .map(str::to_owned);

    Ok(SmokeReport {
        endpoint: endpoint.to_string(),
        use_tls,
        server_id: offer.server_id.clone(),
        device_token,
        pairing_token,
        probe_request_id,
        probe_frame_type,
        probe_error_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parse_args_reads_offer_endpoint_device_and_target() {
        let options = parse_args(&argv(&[
            "--endpoint",
            "127.0.0.1:9001",
            "--offer-url",
            "https://picot.021521.xyz/#offer=abc",
            "--device-id",
            "smoke-2",
            "--target",
            "@target.json",
        ]))
        .unwrap();
        assert_eq!(
            options.offer_source,
            OfferSource::Argv("https://picot.021521.xyz/#offer=abc".to_string())
        );
        assert_eq!(options.endpoint_override.as_deref(), Some("127.0.0.1:9001"));
        assert_eq!(options.device_id, "smoke-2");
        assert_eq!(options.pairing_token_env, "PICOT_SMOKE_TOKEN");
        assert_eq!(options.target.as_deref(), Some("@target.json"));
    }

    #[test]
    fn parse_args_defaults_offer_to_env_and_applies_defaults() {
        let options = parse_args(&[]).unwrap();
        assert_eq!(options.offer_source, OfferSource::Env);
        assert_eq!(options.device_id, "smoke-1");
        assert_eq!(options.pairing_token_env, DEFAULT_PAIRING_TOKEN_ENV);
        assert!(options.target.is_none());
    }

    #[test]
    fn parse_args_hard_errors_on_argv_pairing_token() {
        let error = parse_args(&argv(&["--pairing-token", "picot_pair_secret"]))
            .expect_err("argv token must hard-error");
        assert!(error.contains("must not be passed via argv"), "{error}");
    }

    #[test]
    fn parse_args_rejects_unknown_flags_missing_values_and_positionals() {
        assert!(parse_args(&argv(&["--nonsense"])).is_err());
        assert!(parse_args(&argv(&["--endpoint"])).is_err());
        assert!(parse_args(&argv(&["bare-positional"])).is_err());
        assert!(parse_args(&argv(&["--device-id", ""])).is_err());
    }

    fn report_with(device_token: Option<&str>, pairing_token: &str) -> SmokeReport {
        SmokeReport {
            endpoint: "relay.example:443".into(),
            use_tls: true,
            server_id: "server-1".into(),
            device_token: device_token.map(str::to_owned),
            pairing_token: pairing_token.to_string(),
            probe_request_id: "smoke-1".into(),
            probe_frame_type: "runtime_subscribed".into(),
            probe_error_code: None,
        }
    }

    #[test]
    fn render_never_prints_full_secrets() {
        let pairing_token = "picot_pair_0123456789abcdef0123456789abcdef";
        let device_token = "picot_device_0123456789abcdef0123456789abcdef";
        let report = report_with(Some(device_token), pairing_token);
        let rendered = report.render();
        assert!(rendered.contains("SMOKE OK"), "{rendered}");
        assert!(!rendered.contains(device_token), "{rendered}");
        assert!(!rendered.contains(pairing_token), "{rendered}");
        // The preview shows a fixed short head, never the full value.
        assert!(rendered.contains("picot_device…"), "{rendered}");
    }

    #[test]
    fn render_reports_probe_refusals_and_stays_secret_free() {
        let mut report = report_with(
            Some("picot_device_0123456789abcdef"),
            "picot_pair_0123456789abcdef",
        );
        report.probe_frame_type = "host_response".into();
        report.probe_error_code = Some("forbidden_class".into());
        let rendered = report.render();
        assert!(rendered.contains("error forbidden_class"), "{rendered}");
        // The public `picot_device` prefix may show (it carries no entropy);
        // the secrets' random tails must not.
        assert!(
            !rendered.contains("picot_device_0123456789abcdef"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("picot_pair_0123456789abcdef"),
            "{rendered}"
        );
    }

    #[test]
    fn redact_replaces_secret_occurrences_in_free_text() {
        let secret = "picot_pair_0123456789abcdef";
        let mut text = format!("failed while using {secret} somewhere");
        redact(&mut text, secret);
        assert_eq!(text, "failed while using <redacted> somewhere");
        // Short values stay untouched (a blanket match would mangle output).
        let mut text = "plain diagnostics".to_string();
        redact(&mut text, "diag");
        assert_eq!(text, "plain diagnostics");
    }

    #[test]
    fn preview_shows_only_the_head() {
        assert_eq!(preview("picot_device_abcdefgh"), "\"picot_device…\"");
        assert_eq!(preview("short"), "\"short…\"");
    }
}
