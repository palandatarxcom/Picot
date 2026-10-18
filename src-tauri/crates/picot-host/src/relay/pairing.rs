// ABOUTME: Pairing offer codec (spec 2026-10-17-relay-phase2 §2.6/§3.3, plan T2).
// ABOUTME: Paseo v2 payload shape plus the optional one-scan pairingToken.

//! Responsibilities (spec §2.1, §2.6, §3.3):
//!
//! - Encode/parse the connection offer payload: `v: 2`, `serverId`,
//!   `daemonPublicKeyB64`, `relay { endpoint, useTls }`, and the optional
//!   `pairingToken` / `pairingTokenExpiresAt` (Picot's one-scan addition on
//!   Paseo's v2 schema; Paseo clients ignore unknown fields, so interop is
//!   preserved — token fields serialize only when present).
//! - Render the QR/copy URL: `<base>/#offer=<base64url>` with unpadded
//!   URL-safe base64 (`A-Za-z0-9-_`), matching Paseo
//!   `encodeOfferToFragmentUrl` byte-for-byte in construction.
//! - Assemble the one-scan pairing offer (spec §2.6): daemon identity +
//!   relay endpoint + freshly minted pairing token → `{url, expiresAt}`;
//!   `PICOT_RELAY_ENDPOINT` overrides the default endpoint (spec §4).
//! - Parse a pasted URL (locating `#offer=` anywhere, Paseo
//!   `parseConnectionOfferFromUrl` shape) or a bare base64url payload;
//!   reject `v != 2`, empty `serverId`/`daemonPublicKeyB64`/`relay.endpoint`,
//!   wrong public-key length, non-URL-safe base64, and truncated JSON;
//!   ignore unknown fields (forward-compatible).

use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64_URL_NOPAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

/// Default company relay endpoint (spec §4): 443 TLS is the fixed deployment.
pub const DEFAULT_RELAY_ENDPOINT: &str = "paseo.021521.xyz:443";

/// Placeholder offer URL host (spec §3.3, decision A): format-compatible with
/// the future mobile web client; not a live consumer until that ships.
pub const APP_BASE_URL: &str = "https://picot.021521.xyz";

/// Relay reachability carried inside the offer (Paseo v2 `relay` object).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayEndpoint {
    pub endpoint: String,
    pub use_tls: bool,
}

/// Connection offer payload, Paseo v2 schema plus the optional one-scan
/// pairing-token extension (spec §2.6). Unknown fields are ignored on parse
/// (Paseo/zod strip semantics); token fields are omitted from the wire when
/// absent so Picot offers stay shape-identical to Paseo offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionOfferV2 {
    pub v: u8,
    pub server_id: String,
    pub daemon_public_key_b64: String,
    pub relay: RelayEndpoint,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pairing_token: Option<String>,
    /// Unix epoch seconds when `pairing_token` expires (same clock domain as
    /// `RemoteAuth` pairings; `now == expires_at` is still valid).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pairing_token_expires_at: Option<u64>,
}

/// Encode the offer payload as unpadded URL-safe base64 (the `#offer=`
/// fragment body; identical alphabet to Node `toString("base64url")`).
pub fn encode_offer_payload(offer: &ConnectionOfferV2) -> Result<String, String> {
    let json = serde_json::to_string(offer)
        .map_err(|error| format!("Cannot serialize connection offer: {error}"))?;
    Ok(BASE64_URL_NOPAD.encode(json.as_bytes()))
}

/// Render the QR/copy URL `<base>/#offer=<base64url>` (Paseo
/// `encodeOfferToFragmentUrl` parity: one trailing `/` stripped from `base`).
pub fn offer_url(offer: &ConnectionOfferV2, base: &str) -> Result<String, String> {
    let base = base.strip_suffix('/').unwrap_or(base);
    Ok(format!("{base}/#offer={}", encode_offer_payload(offer)?))
}

/// Effective relay endpoint (spec §4): a non-empty `PICOT_RELAY_ENDPOINT`
/// overrides the company-relay constant (dev/self-hosted smoke; never in UI).
/// Same env-consumption shape as `vcc_config::config_file`.
pub fn relay_endpoint() -> String {
    std::env::var("PICOT_RELAY_ENDPOINT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_RELAY_ENDPOINT.to_string())
}

/// The `relay_pairing_offer` op result (spec §4.1): the QR/copy URL plus the
/// token's expiry, so the UI can grey the code out the instant it dies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingOffer {
    pub url: String,
    pub expires_at: u64,
}

/// Assemble the one-scan pairing offer (spec §2.6): the daemon identity
/// (E2EE trust anchor), the relay endpoint (with the dial plan's TLS flag,
/// so a plain-WS dev endpoint is not advertised as TLS), and the freshly
/// minted pairing token rendered as the placeholder-host offer URL.
pub fn build_pairing_offer(
    identity: &crate::relay::e2ee::DaemonIdentity,
    endpoint: &str,
    use_tls: bool,
    pairing: &crate::remote_auth::Pairing,
) -> Result<PairingOffer, String> {
    let offer = ConnectionOfferV2 {
        v: 2,
        server_id: identity.server_id.clone(),
        daemon_public_key_b64: identity.public_key_b64.clone(),
        relay: RelayEndpoint {
            endpoint: endpoint.to_string(),
            use_tls,
        },
        pairing_token: Some(pairing.token.clone()),
        pairing_token_expires_at: Some(pairing.expires_at),
    };
    Ok(PairingOffer {
        url: offer_url(&offer, APP_BASE_URL)?,
        expires_at: pairing.expires_at,
    })
}

/// Parse a connection offer from a full URL (anywhere containing `#offer=`)
/// or a bare base64url payload. Non-URL-safe base64 (`+`, `/`, `=`) is
/// rejected with an explicit error: our own URLs never contain those
/// characters, so their presence means the payload was encoded with the
/// wrong variant.
pub fn parse_connection_offer(input: &str) -> Result<ConnectionOfferV2, String> {
    const FRAGMENT_MARKER: &str = "#offer=";
    let trimmed = input.trim();
    let payload = match trimmed.find(FRAGMENT_MARKER) {
        Some(index) => trimmed[index + FRAGMENT_MARKER.len()..].trim(),
        None => trimmed,
    };
    if payload.is_empty() {
        return Err("Connection offer payload is empty".to_string());
    }
    if let Some(bad) = payload
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() && *c != '-' && *c != '_')
    {
        return Err(format!(
            "Connection offer payload is not URL-safe base64 (found {bad:?}; \
             expected only A-Za-z0-9-_ with no padding)"
        ));
    }
    let bytes = BASE64_URL_NOPAD
        .decode(payload)
        .map_err(|error| format!("Connection offer payload is not valid base64url: {error}"))?;
    let json = String::from_utf8(bytes)
        .map_err(|_| "Connection offer payload is not valid UTF-8".to_string())?;
    let offer: ConnectionOfferV2 = serde_json::from_str(&json)
        .map_err(|error| format!("Connection offer payload is not valid JSON: {error}"))?;
    offer.validate()?;
    Ok(offer)
}

impl ConnectionOfferV2 {
    /// Structural validation shared by both ingest paths: `v == 2`, non-empty
    /// identity fields, a canonical 32-byte X25519 public key, and a
    /// non-empty relay endpoint. Mirrors Paseo `ConnectionOfferV2Schema`
    /// bounds plus the key-length check Paseo defers to handshake time.
    fn validate(&self) -> Result<(), String> {
        if self.v != 2 {
            return Err(format!(
                "Unsupported connection offer version {} (expected 2)",
                self.v
            ));
        }
        if self.server_id.is_empty() {
            return Err("Connection offer serverId is empty".to_string());
        }
        if self.daemon_public_key_b64.is_empty() {
            return Err("Connection offer daemonPublicKeyB64 is empty".to_string());
        }
        crate::relay::e2ee::validate_public_key_b64(&self.daemon_public_key_b64)?;
        if self.relay.endpoint.is_empty() {
            return Err("Connection offer relay endpoint is empty".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata_store::{MetadataStore, SharedMetadataStore};
    use crate::relay::e2ee::load_or_create_identity;
    use crate::remote_auth::Pairing;
    use std::sync::{Arc, Mutex};

    /// DaemonIdentity's secret key is module-private, and the loader is the
    /// production construction path — tests use it verbatim on a temp store.
    fn test_identity() -> crate::relay::e2ee::DaemonIdentity {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!(
            "picot-pairing-offer-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let store: SharedMetadataStore = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        load_or_create_identity(&store).unwrap()
    }

    /// Paseo daemon public key literal: canonical padded standard base64 of
    /// 32 bytes (tweetnacl `nacl.util.encodeBase64` output shape).
    const PUBLIC_KEY_B64: &str = "3H1QXjHfKZWDqjXxrIhCNBQCGi5PZaXTVO06FSTBRHU=";

    fn offer_without_token() -> ConnectionOfferV2 {
        ConnectionOfferV2 {
            v: 2,
            server_id: "0d9d0e70-2ca1-4a2f-a6b2-6f6c1b1f6b7a".to_string(),
            daemon_public_key_b64: PUBLIC_KEY_B64.to_string(),
            relay: RelayEndpoint {
                endpoint: DEFAULT_RELAY_ENDPOINT.to_string(),
                use_tls: true,
            },
            pairing_token: None,
            pairing_token_expires_at: None,
        }
    }

    fn offer_with_token() -> ConnectionOfferV2 {
        ConnectionOfferV2 {
            pairing_token: Some("picot_pair_0123456789abcdef0123456789abcdef".to_string()),
            pairing_token_expires_at: Some(1_771_000_000),
            ..offer_without_token()
        }
    }

    /// Encode `payload` as unpadded URL-safe base64 (test-side, independent of
    /// `encode_offer_payload`).
    fn b64url(payload: &str) -> String {
        BASE64_URL_NOPAD.encode(payload.as_bytes())
    }

    #[test]
    fn url_round_trips_with_and_without_token() {
        for offer in [offer_with_token(), offer_without_token()] {
            let url = offer_url(&offer, APP_BASE_URL).unwrap();
            assert!(url.starts_with("https://picot.021521.xyz/#offer="));
            assert_eq!(parse_connection_offer(&url).unwrap(), offer);
        }
    }

    #[test]
    fn encoded_fragment_uses_unpadded_url_safe_alphabet() {
        for offer in [offer_with_token(), offer_without_token()] {
            let url = offer_url(&offer, APP_BASE_URL).unwrap();
            let fragment = url.split("#offer=").nth(1).unwrap();
            assert!(
                fragment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "fragment must be URL-safe base64 without padding: {fragment}"
            );
            assert!(!fragment.contains('='));
        }
    }

    #[test]
    fn token_fields_are_absent_from_wire_when_none() {
        let payload = encode_offer_payload(&offer_without_token()).unwrap();
        let json = String::from_utf8(BASE64_URL_NOPAD.decode(payload).unwrap()).unwrap();
        assert!(!json.contains("pairingToken"));
        // And present (both) when set.
        let payload = encode_offer_payload(&offer_with_token()).unwrap();
        let json = String::from_utf8(BASE64_URL_NOPAD.decode(payload).unwrap()).unwrap();
        assert!(json.contains("\"pairingToken\""));
        assert!(json.contains("\"pairingTokenExpiresAt\""));
    }

    /// Reference vector generated by Paseo's exact encode path
    /// (`JSON.stringify` + Node `toString("base64url")`, scratch bun script
    /// 2026-10-18): a Paseo v2 offer without the Picot extension must parse.
    #[test]
    fn paseo_reference_url_parses_without_token() {
        const PASEO_ENCODED: &str = "eyJ2IjoyLCJzZXJ2ZXJJZCI6IjBkOWQwZTcwLTJjYTEtNGEyZi1hNmIyLTZmNmMxYjFmNmI3YSIsImRhZW1vblB1YmxpY0tleUI2NCI6IjNIMVFYakhmS1pXRHFqWHhySWhDTkJRQ0dpNVBaYVhUVk8wNkZTVEJSSFU9IiwicmVsYXkiOnsiZW5kcG9pbnQiOiJwYXNlby4wMjE1MjEueHl6OjQ0MyIsInVzZVRscyI6dHJ1ZX19";
        let parsed =
            parse_connection_offer(&format!("https://app.paseo.sh/#offer={PASEO_ENCODED}"))
                .unwrap();
        assert_eq!(parsed, offer_without_token());
    }

    #[test]
    fn parse_accepts_bare_payload_and_surrounding_whitespace() {
        let payload = encode_offer_payload(&offer_with_token()).unwrap();
        assert_eq!(
            parse_connection_offer(&format!("  {payload} \n")).unwrap(),
            offer_with_token()
        );
        // A URL fragment appearing anywhere in the input wins (Paseo shape:
        // indexOf, not startsWith).
        assert_eq!(
            parse_connection_offer(&format!("pair via https://x/#offer={payload}")).unwrap(),
            offer_with_token()
        );
    }

    #[test]
    fn offer_url_strips_one_trailing_slash_from_base() {
        assert_eq!(
            offer_url(&offer_without_token(), "https://h.example/").unwrap(),
            offer_url(&offer_without_token(), "https://h.example").unwrap()
        );
    }

    #[test]
    fn parse_rejects_wrong_version() {
        for v in [1u8, 3] {
            let mut offer = offer_without_token();
            offer.v = v;
            let err = parse_connection_offer(&encode_offer_payload(&offer).unwrap())
                .expect_err("non-v2 offer must be rejected");
            assert!(err.contains("version"), "{err}");
        }
    }

    #[test]
    fn parse_rejects_missing_or_empty_identity_fields() {
        let mut offer = offer_without_token();
        offer.server_id = String::new();
        assert!(parse_connection_offer(&encode_offer_payload(&offer).unwrap()).is_err());

        let mut offer = offer_without_token();
        offer.daemon_public_key_b64 = String::new();
        assert!(parse_connection_offer(&encode_offer_payload(&offer).unwrap()).is_err());

        // Missing fields entirely (hand-built JSON).
        let missing_key =
            b64url(r#"{"v":2,"serverId":"abc","relay":{"endpoint":"e:1","useTls":true}}"#);
        assert!(parse_connection_offer(&missing_key).is_err());
        let missing_server =
            b64url(r#"{"v":2,"daemonPublicKeyB64":"k","relay":{"endpoint":"e:1","useTls":true}}"#);
        assert!(parse_connection_offer(&missing_server).is_err());
    }

    #[test]
    fn parse_rejects_wrong_public_key_length() {
        let mut offer = offer_without_token();
        offer.daemon_public_key_b64 = BASE64_URL_NOPAD.encode([0u8; 31]); // 31 bytes
        assert!(parse_connection_offer(&encode_offer_payload(&offer).unwrap()).is_err());
    }

    #[test]
    fn parse_rejects_empty_relay_endpoint() {
        let mut offer = offer_without_token();
        offer.relay.endpoint = String::new();
        assert!(parse_connection_offer(&encode_offer_payload(&offer).unwrap()).is_err());
    }

    #[test]
    fn parse_rejects_non_url_safe_base64_with_explicit_error() {
        // Standard (non-URL) base64 uses `+`/`/` and pads with `=`; inject a
        // `+` deterministically (this JSON's standard base64 happens to avoid
        // both) so the wrong-variant rejection path is exercised.
        let mut wrong_variant = encode_offer_payload(&offer_without_token()).unwrap();
        wrong_variant.replace_range(0..1, "+");
        let err = parse_connection_offer(&format!("https://x/#offer={wrong_variant}"))
            .expect_err("standard base64 fragment must be rejected");
        assert!(err.contains("not URL-safe base64"), "{err}");

        // Padded URL-safe base64 (trailing '=') is equally rejected.
        let padded = format!("{}=", encode_offer_payload(&offer_without_token()).unwrap());
        assert!(parse_connection_offer(&padded).is_err());
    }

    #[test]
    fn parse_rejects_garbage_and_truncated_payloads() {
        assert!(parse_connection_offer("https://x/#offer=!!!not-base64!!!").is_err());
        assert!(parse_connection_offer("https://x/#offer=").is_err());
        assert!(parse_connection_offer("").is_err());
        // Valid base64url, truncated JSON.
        let truncated = b64url(r#"{"v":2,"serverId":"abc""#);
        assert!(parse_connection_offer(&truncated).is_err());
        // Valid base64url, non-UTF8 bytes.
        assert!(parse_connection_offer(&BASE64_URL_NOPAD.encode([0xff, 0xfe, 0x00])).is_err());
    }

    #[test]
    fn parse_ignores_unknown_fields() {
        // Paseo forward-compat: unknown extra fields are stripped by zod on
        // their side; we accept them so newer Paseo/Picot offers keep parsing.
        let extended = b64url(
            r#"{"v":2,"serverId":"sid","daemonPublicKeyB64":"KEY","relay":{"endpoint":"e:1","useTls":true,"futureRelayField":7},"futureField":[1,2],"pairingToken":"picot_pair_x"}"#
                .replace("KEY", PUBLIC_KEY_B64)
                .as_str(),
        );
        let parsed = parse_connection_offer(&extended).unwrap();
        assert_eq!(parsed.pairing_token.as_deref(), Some("picot_pair_x"));
        assert_eq!(parsed.pairing_token_expires_at, None);
    }

    #[test]
    fn build_pairing_offer_round_trips_all_fields() {
        let identity = test_identity();
        let pairing = Pairing {
            token: "picot_pair_0123456789abcdef".to_string(),
            expires_at: 1_771_000_500,
        };
        let offer = build_pairing_offer(&identity, DEFAULT_RELAY_ENDPOINT, true, &pairing).unwrap();
        assert!(offer.url.starts_with("https://picot.021521.xyz/#offer="));
        assert_eq!(offer.expires_at, 1_771_000_500);

        let parsed = parse_connection_offer(&offer.url).unwrap();
        assert_eq!(parsed.v, 2);
        assert_eq!(parsed.server_id, identity.server_id);
        assert_eq!(parsed.daemon_public_key_b64, identity.public_key_b64);
        assert_eq!(parsed.relay.endpoint, DEFAULT_RELAY_ENDPOINT);
        assert!(parsed.relay.use_tls);
        assert_eq!(
            parsed.pairing_token.as_deref(),
            Some("picot_pair_0123456789abcdef")
        );
        assert_eq!(parsed.pairing_token_expires_at, Some(1_771_000_500));
    }

    #[test]
    fn build_pairing_offer_carries_endpoint_override_and_expires_at() {
        let identity = test_identity();
        let pairing = Pairing {
            token: "picot_pair_override".to_string(),
            expires_at: 42,
        };
        let offer = build_pairing_offer(&identity, "localhost:9443", true, &pairing).unwrap();
        assert_eq!(offer.expires_at, 42);
        let parsed = parse_connection_offer(&offer.url).unwrap();
        assert_eq!(parsed.relay.endpoint, "localhost:9443");
        assert_eq!(parsed.pairing_token_expires_at, Some(42));
    }
}
