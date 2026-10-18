// ABOUTME: Relay E2EE core: persistent daemon identity keypair (spec §2.3)
// ABOUTME: plus, in later T1 slices, the Paseo-wire-format NaCl box helpers.

//! Responsibilities (spec `2026-10-17-relay-phase2-pair-device-design` §2.1,
//! §2.3, §2.4):
//!
//! - **Daemon identity** (this slice): one X25519 keypair + a uuid `serverId`,
//!   created once and persisted in the metadata DB under the reserved
//!   `relay.daemon.identity` preference key. Load-or-create runs read → create
//!   → write under the store mutex, so concurrent first-enable yields a single
//!   identity; a corrupt row (invalid JSON, missing fields, bad key lengths)
//!   returns an error and is **never silently rebuilt** — the secret key is the
//!   trust anchor, and a quiet re-key would silently orphan every paired
//!   device. The secret never appears in `Debug` output.
//! - **NaCl box crypto** (T1 remainder): `derive_shared_key` / `seal` /
//!   `seal_with_nonce` / `open` and the base64 `nonce || ciphertext` wire
//!   frames, byte-compatible with Paseo's tweetnacl `nacl.box`; rejects
//!   all-zero shared secrets and low-order/all-zero peer keys.
//! - **Handshake frames** (T1 remainder): plaintext `e2ee_hello` →
//!   `e2ee_ready` exchange gating every data frame.

use crate::metadata_store::SharedMetadataStore;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use crypto_box::SecretKey;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Reserved-namespace preferences key holding the daemon identity JSON.
pub const IDENTITY_PREFERENCE_KEY: &str = "relay.daemon.identity";

/// X25519 public/secret key length in bytes (`crypto_box::KEY_BYTES`).
const KEY_BYTES: usize = 32;

/// The daemon's persistent relay identity (spec §2.3): one keypair per daemon,
/// created on first enable, never rotated in v1.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonIdentity {
    pub server_id: String,
    pub public_key_b64: String,
    secret_key_b64: String,
}

impl std::fmt::Debug for DaemonIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The secret key is the trust anchor; it never reaches logs or
        // debug output (spec §2.3, `tests/token_redaction.rs` precedent).
        f.debug_struct("DaemonIdentity")
            .field("server_id", &self.server_id)
            .field("public_key_b64", &self.public_key_b64)
            .field("secret_key_b64", &"<redacted>")
            .finish()
    }
}

impl DaemonIdentity {
    fn generate() -> Self {
        let secret_key = SecretKey::generate(&mut OsRng);
        let public_key = secret_key.public_key();
        Self {
            server_id: Uuid::new_v4().to_string(),
            public_key_b64: BASE64_STANDARD.encode(public_key.as_bytes()),
            secret_key_b64: BASE64_STANDARD.encode(secret_key.to_bytes()),
        }
    }

    /// Structural validation for a loaded row (spec §2.3): the key fields must
    /// decode to exactly 32 bytes and `serverId` must be non-empty. Used to
    /// reject corrupt rows instead of silently re-keying.
    fn validate(&self) -> Result<(), String> {
        if self.server_id.is_empty() {
            return Err("daemon identity serverId is empty".to_string());
        }
        decode_key(&self.public_key_b64, "publicKeyB64")?;
        decode_key(&self.secret_key_b64, "secretKeyB64")?;
        Ok(())
    }
}

fn decode_key(value_b64: &str, field: &str) -> Result<[u8; KEY_BYTES], String> {
    let bytes = BASE64_STANDARD
        .decode(value_b64)
        .map_err(|error| format!("daemon identity {field} is not valid base64: {error}"))?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        format!(
            "daemon identity {field} is {} bytes, expected {KEY_BYTES}",
            bytes.len()
        )
    })
}

/// Load the persisted identity, creating it once if absent (spec §2.3).
///
/// Concurrent callers sharing one store get the identical identity because
/// the whole read → create → write sequence runs under the store mutex. A
/// corrupt row errors out instead of rebuilding: re-keying would silently
/// break every already-paired device.
pub fn load_or_create_identity(store: &SharedMetadataStore) -> Result<DaemonIdentity, String> {
    // Read → create → write under one lock hold (spec §2.3): concurrent
    // first-enable yields a single identity, and a corrupt row errors out
    // rather than being rebuilt — the secret key is the trust anchor and a
    // silent re-key would orphan every already-paired device.
    let guard = store
        .lock()
        .map_err(|_| "Picot metadata lock poisoned".to_string())?;
    if let Some(value) = guard.relay_pref_get(IDENTITY_PREFERENCE_KEY)? {
        let identity: DaemonIdentity = serde_json::from_value(value)
            .map_err(|error| format!("daemon identity row is corrupt: {error}"))?;
        identity.validate()?;
        return Ok(identity);
    }
    let identity = DaemonIdentity::generate();
    let value = serde_json::to_value(&identity)
        .map_err(|error| format!("Cannot serialize daemon identity: {error}"))?;
    guard.relay_pref_set(IDENTITY_PREFERENCE_KEY, &value)?;
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::{load_or_create_identity, DaemonIdentity, IDENTITY_PREFERENCE_KEY};
    use crate::metadata_store::{MetadataStore, SharedMetadataStore};
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    // Parallel tests may sample identical timestamps; the monotonic
    // per-call sequence guarantees a unique directory.
    fn temp_dir() -> PathBuf {
        let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "picot-relay-e2ee-{}-{sequence}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn shared_store(temp: &Path) -> SharedMetadataStore {
        Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ))
    }

    #[test]
    fn identity_is_created_once_and_idempotent_across_reopen() {
        let temp = temp_dir();
        let store = shared_store(&temp);
        let first = load_or_create_identity(&store).unwrap();
        // Second call on the live handle: same identity, not a re-key.
        let second = load_or_create_identity(&store).unwrap();
        assert_eq!(first, second);

        // Survives a fresh store handle on the same database file.
        drop(store);
        let reopened = shared_store(&temp);
        let third = load_or_create_identity(&reopened).unwrap();
        assert_eq!(first, third);

        // Stored shape follows spec §2.3 (camelCase keys, both b64 fields).
        let row = reopened
            .lock()
            .unwrap()
            .relay_pref_get(IDENTITY_PREFERENCE_KEY)
            .unwrap()
            .expect("identity row must be persisted");
        assert_eq!(row["serverId"], json!(first.server_id));
        assert_eq!(row["publicKeyB64"], json!(first.public_key_b64));
        assert!(row.get("secretKeyB64").is_some());

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn concurrent_first_create_yields_one_identity() {
        let temp = temp_dir();
        let store = shared_store(&temp);
        let left_store = Arc::clone(&store);
        let right_store = Arc::clone(&store);
        let left = std::thread::spawn(move || load_or_create_identity(&left_store).unwrap());
        let right = std::thread::spawn(move || load_or_create_identity(&right_store).unwrap());
        assert_eq!(left.join().unwrap(), right.join().unwrap());
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn corrupt_identity_row_is_rejected_not_rebuilt() {
        let temp = temp_dir();
        let store = shared_store(&temp);

        // Wrong shape (missing key fields) through the internal accessor.
        store
            .lock()
            .unwrap()
            .relay_pref_set(IDENTITY_PREFERENCE_KEY, &json!({ "serverId": "x" }))
            .unwrap();
        assert!(load_or_create_identity(&store).is_err());
        // The corrupt row is left in place — no silent rebuild happened.
        assert!(store
            .lock()
            .unwrap()
            .relay_pref_get(IDENTITY_PREFERENCE_KEY)
            .unwrap()
            .is_some());

        // Bad key length (decodes, but not 32 bytes).
        store
            .lock()
            .unwrap()
            .relay_pref_set(
                IDENTITY_PREFERENCE_KEY,
                &json!({
                    "serverId": "id",
                    "publicKeyB64": "short",
                    "secretKeyB64": "short"
                }),
            )
            .unwrap();
        assert!(load_or_create_identity(&store).is_err());

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn daemon_identity_debug_redacts_the_secret_key() {
        let temp = temp_dir();
        let store = shared_store(&temp);
        let identity: DaemonIdentity = load_or_create_identity(&store).unwrap();
        let rendered = format!("{identity:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains(&identity.secret_key_b64));
        // The public half stays visible for log correlation.
        assert!(rendered.contains(&identity.public_key_b64));
        fs::remove_dir_all(&temp).unwrap();
    }
}
