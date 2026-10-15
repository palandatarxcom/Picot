// ABOUTME: Issues and validates short-lived desktop capabilities for host windows.
// ABOUTME: Credentials remain in host memory and are revoked with window lifecycle.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::rngs::OsRng;
use rand::RngCore;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[allow(dead_code)]
const CAPABILITY_BYTES: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopCapability {
    pub owner_id: String,
    pub window_label: String,
    pub generation: u64,
    pub expires_at: Option<Instant>,
}

#[derive(Default)]
#[allow(dead_code)]
pub struct HostCapabilityStore {
    records: HashMap<String, DesktopCapability>,
}

#[allow(dead_code)]
impl HostCapabilityStore {
    pub fn mint(
        &mut self,
        owner_id: impl Into<String>,
        window_label: impl Into<String>,
        generation: u64,
        ttl: Option<Duration>,
    ) -> String {
        let mut bytes = [0u8; CAPABILITY_BYTES];
        OsRng.fill_bytes(&mut bytes);
        let token = URL_SAFE_NO_PAD.encode(bytes);
        self.records.insert(
            token.clone(),
            DesktopCapability {
                owner_id: owner_id.into(),
                window_label: window_label.into(),
                generation,
                expires_at: ttl.map(|value| Instant::now() + value),
            },
        );
        token
    }

    pub fn validate(&mut self, token: &str, owner_id: &str, generation: u64) -> bool {
        let Some(record) = self.records.get(token) else {
            return false;
        };
        if record.owner_id != owner_id || record.generation != generation {
            return false;
        }
        if record
            .expires_at
            .is_some_and(|expires| Instant::now() >= expires)
        {
            self.records.remove(token);
            return false;
        }
        true
    }

    pub fn lookup(&mut self, token: &str) -> Option<DesktopCapability> {
        let record = self.records.get(token)?.clone();
        if record
            .expires_at
            .is_some_and(|expires| Instant::now() >= expires)
        {
            self.records.remove(token);
            return None;
        }
        Some(record)
    }

    pub fn revoke(&mut self, token: &str) -> bool {
        self.records.remove(token).is_some()
    }
    #[allow(dead_code)]
    pub fn revoke_owner(&mut self, owner_id: &str) {
        self.records.retain(|_, record| record.owner_id != owner_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_validate_scope_and_revoke() {
        let mut store = HostCapabilityStore::default();
        let token = store.mint("owner-a", "window-a", 7, None);
        assert!(store.validate(&token, "owner-a", 7));
        assert!(!store.validate(&token, "owner-b", 7));
        assert!(!store.validate(&token, "owner-a", 8));
        assert!(store.revoke(&token));
        assert!(!store.validate(&token, "owner-a", 7));
    }

    #[test]
    fn expired_capability_is_removed() {
        let mut store = HostCapabilityStore::default();
        let token = store.mint("owner", "window", 0, Some(Duration::ZERO));
        assert!(store.lookup(&token).is_none());
    }

    #[test]
    fn window_close_revokes_all_capabilities_for_that_owner() {
        let mut store = HostCapabilityStore::default();
        let first = store.mint("owner-a", "window-a", 1, None);
        let second = store.mint("owner-a", "window-a", 2, None);
        let other = store.mint("owner-b", "window-b", 1, None);

        store.revoke_owner("owner-a");

        assert!(!store.validate(&first, "owner-a", 1));
        assert!(!store.validate(&second, "owner-a", 2));
        assert!(store.validate(&other, "owner-b", 1));
    }
}
