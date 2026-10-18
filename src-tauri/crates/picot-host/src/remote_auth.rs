#![cfg_attr(not(test), allow(dead_code))]
// ABOUTME: LAN pairing/token exchange backed by the shared Picot metadata
// store so pairing writes and workspace/preference controls observe one
// database without a second SQLite connection.

use crate::metadata_store::{MetadataStore, SharedMetadataStore};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const PAIRING_LIFETIME_SECONDS: u64 = 5 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    pub token: String,
    pub expires_at: u64,
}

/// The identity a device token resolves to (Gate B, spec §2.8): the paired
/// device plus the owner whose pairing minted it. `owner_id` is `None` for
/// rows written before the binding existed (legacy LAN pairings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicePrincipal {
    pub device_id: String,
    pub owner_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingError {
    InvalidOrUsed,
    Expired,
    Storage(String),
}

pub struct RemoteAuth {
    store: Arc<Mutex<MetadataStore>>,
    pending: HashMap<Vec<u8>, PendingPairing>,
}

/// One unconsumed pairing token: its expiry plus the minting owner the
/// exchange will stamp onto the device token record (Gate B, spec §2.8).
struct PendingPairing {
    expires_at: u64,
    owner: Option<String>,
}

impl RemoteAuth {
    pub fn new(store: SharedMetadataStore) -> Self {
        Self {
            store,
            pending: HashMap::new(),
        }
    }

    fn with_store<T>(
        &self,
        operation: impl FnOnce(&mut MetadataStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut guard = self
            .store
            .lock()
            .map_err(|_| "Picot metadata lock poisoned".to_string())?;
        operation(&mut guard)
    }

    pub fn create_pairing(&mut self, now: u64, owner: Option<&str>) -> Pairing {
        // Lazy cleanup shares the rejection boundary (`now > expires_at` is
        // dead): a token at now == expires_at is still redeemable (spec
        // 2026-10-17-relay-phase2 §2.10).
        self.pending.retain(|_, pending| pending.expires_at >= now);
        let token = format!("picot_pair_{}", Uuid::new_v4().simple());
        let expires_at = now + PAIRING_LIFETIME_SECONDS;
        self.pending.insert(
            hash(&token),
            PendingPairing {
                expires_at,
                owner: owner.map(str::to_owned),
            },
        );
        Pairing { token, expires_at }
    }

    /// Invalidate every unconsumed pairing token (spec §2.10 `superseded`
    /// on re-mint and the relay-off state). Paired device tokens survive.
    pub fn revoke_pending(&mut self) {
        self.pending.clear();
    }

    /// Mint a pairing token superseding any unconsumed one first, so at
    /// most one unconsumed token exists at any time (spec §2.6/§2.10; both
    /// mint points share this pending pool, spec §6).
    pub fn mint_fresh_pairing_token(&mut self, now: u64, owner: Option<&str>) -> Pairing {
        self.revoke_pending();
        self.create_pairing(now, owner)
    }

    pub fn exchange(
        &mut self,
        pairing_token: &str,
        device_id: &str,
        now: u64,
    ) -> Result<String, PairingError> {
        let token_hash = hash(pairing_token);
        let pending = self
            .pending
            .remove(&token_hash)
            .ok_or(PairingError::InvalidOrUsed)?;
        if now > pending.expires_at {
            return Err(PairingError::Expired);
        }
        let device_token = format!(
            "picot_device_{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        self.store
            .lock()
            .map_err(|_| PairingError::Storage("Picot metadata lock poisoned".to_string()))?
            .store_device_token(device_id, &device_token, pending.owner.as_deref())
            .map_err(PairingError::Storage)?;
        Ok(device_token)
    }

    pub fn authorize(&self, device_token: &str) -> Result<bool, String> {
        Ok(self.authenticate(device_token)?.is_some())
    }

    /// Resolve a device token to its principal (spec §2.8): `None` when the
    /// token is unknown or revoked.
    pub fn authenticate(&self, device_token: &str) -> Result<Option<DevicePrincipal>, String> {
        let Some((device_id, owner_id)) = self
            .store
            .lock()
            .map_err(|_| "Picot metadata lock poisoned".to_string())?
            .device_token_record(device_token)?
        else {
            return Ok(None);
        };
        Ok(Some(DevicePrincipal {
            device_id,
            owner_id,
        }))
    }

    pub fn revoke(&mut self, device_id: &str) -> Result<(), String> {
        self.with_store(|store| store.revoke_device(device_id))
    }
}

fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

#[cfg(test)]
mod tests {
    use super::{PairingError, RemoteAuth};
    use crate::metadata_store::{MetadataStore, SharedMetadataStore};
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_DIR_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn shared_auth() -> (RemoteAuth, SharedMetadataStore, std::path::PathBuf) {
        // Parallel tests may sample identical timestamps; sequence keeps dirs unique.
        let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp = std::env::temp_dir().join(format!(
            "picot-remote-auth-{}-{sequence}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&temp).unwrap();
        let store: SharedMetadataStore = Arc::new(Mutex::new(
            MetadataStore::open(&temp.join("picot.sqlite3")).unwrap(),
        ));
        (RemoteAuth::new(Arc::clone(&store)), store, temp)
    }

    #[test]
    fn remote_auth_and_registry_handles_observe_the_same_shared_state() {
        let (mut auth, store, temp) = shared_auth();

        // Registry-side write through the second handle.
        let workspace = temp.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let (row, added) = store.lock().unwrap().add_workspace(&workspace).unwrap();
        assert!(added);

        // Pairing-side write through RemoteAuth lands in the same database:
        // verify via a fresh handle onto the shared store's file path through
        // authorize (reads paired_devices) and registry row visibility.
        let pairing = auth.create_pairing(1_000, None);
        let device_token = auth.exchange(&pairing.token, "phone", 1_001).unwrap();
        assert!(auth.authorize(&device_token).unwrap());

        let seen = store
            .lock()
            .unwrap()
            .get_workspace(&row.workspace_id)
            .unwrap();
        assert_eq!(
            seen.map(|seen| seen.canonical_path),
            Some(row.canonical_path)
        );
        assert!(store
            .lock()
            .unwrap()
            .verify_device_token(&device_token)
            .unwrap());
    }

    #[test]
    fn exchange_stamps_the_minting_owner_onto_the_device_record() {
        let (mut auth, _store, temp) = shared_auth();
        // Gate B (spec §2.8): the pending token carries the minting owner;
        // exchange stamps it onto the device token record, so the device's
        // principal needs no runtime guessing.
        let pairing = auth.create_pairing(1_000, Some("owner-7"));
        let device_token = auth.exchange(&pairing.token, "phone", 1_001).unwrap();
        assert_eq!(
            auth.authenticate(&device_token).unwrap(),
            Some(super::DevicePrincipal {
                device_id: "phone".into(),
                owner_id: Some("owner-7".into()),
            })
        );
        // A pending token minted without an owner (the legacy LAN shape)
        // still exchanges; its record carries no binding.
        let legacy = auth.create_pairing(2_000, None);
        let legacy_token = auth.exchange(&legacy.token, "old-phone", 2_001).unwrap();
        assert_eq!(
            auth.authenticate(&legacy_token)
                .unwrap()
                .map(|principal| principal.owner_id),
            Some(None)
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn authenticate_returns_none_for_unknown_or_revoked_tokens() {
        let (mut auth, _store, temp) = shared_auth();
        assert_eq!(auth.authenticate("picot_device_none").unwrap(), None);
        let pairing = auth.create_pairing(1_000, Some("owner-7"));
        let token = auth.exchange(&pairing.token, "phone", 1_001).unwrap();
        auth.revoke("phone").unwrap();
        assert_eq!(auth.authenticate(&token).unwrap(), None);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn exchanges_a_five_minute_single_use_pairing_token_for_a_device_token() {
        let (mut auth, _store, temp) = shared_auth();
        let pairing = auth.create_pairing(1_000, None);
        assert_eq!(pairing.expires_at, 1_300);

        let device_token = auth.exchange(&pairing.token, "phone", 1_001).unwrap();
        assert!(auth.authorize(&device_token).unwrap());
        assert_eq!(
            auth.exchange(&pairing.token, "second-phone", 1_002),
            Err(PairingError::InvalidOrUsed)
        );
        assert!(
            !String::from_utf8_lossy(&fs::read(temp.join("picot.sqlite3")).unwrap())
                .contains(&device_token)
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn revoke_pending_supersedes_every_unconsumed_token() {
        let (mut auth, _store, temp) = shared_auth();
        let first = auth.create_pairing(1_000, None);
        let second = auth.create_pairing(1_100, None);
        auth.revoke_pending();
        assert_eq!(
            auth.exchange(&first.token, "phone", 1_200),
            Err(PairingError::InvalidOrUsed)
        );
        assert_eq!(
            auth.exchange(&second.token, "phone", 1_201),
            Err(PairingError::InvalidOrUsed)
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn mint_fresh_pairing_token_leaves_at_most_one_unconsumed_token() {
        let (mut auth, _store, temp) = shared_auth();
        let stale = auth.mint_fresh_pairing_token(1_000, None);
        let fresh = auth.mint_fresh_pairing_token(1_100, None);
        assert_eq!(
            auth.exchange(&stale.token, "phone", 1_101),
            Err(PairingError::InvalidOrUsed)
        );
        assert!(auth.exchange(&fresh.token, "phone", 1_102).is_ok());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn now_equal_to_expires_at_is_still_redeemable_for_cleanup_and_exchange() {
        let (mut auth, _store, temp) = shared_auth();
        let pairing = auth.create_pairing(4_000, None);
        assert_eq!(pairing.expires_at, 4_300);
        // The lazy cleanup inside create_pairing runs at now == expires_at
        // and must keep the token: cleanup and rejection share one boundary
        // (spec 2026-10-17-relay-phase2 §2.10, `now == expires_at` usable).
        let _next = auth.create_pairing(4_300, None);
        assert!(auth.exchange(&pairing.token, "phone", 4_300).is_ok());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn rejects_expired_pairing_tokens_and_supports_device_revocation() {
        let (mut auth, _store, temp) = shared_auth();
        let expired = auth.create_pairing(2_000, None);
        assert_eq!(
            auth.exchange(&expired.token, "phone", 2_301),
            Err(PairingError::Expired)
        );

        let pairing = auth.create_pairing(3_000, None);
        let token = auth.exchange(&pairing.token, "phone", 3_001).unwrap();
        auth.revoke("phone").unwrap();
        assert!(!auth.authorize(&token).unwrap());
        fs::remove_dir_all(temp).unwrap();
    }
}
