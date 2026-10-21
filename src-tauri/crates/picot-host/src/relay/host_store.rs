// ABOUTME: Desktop-client host store: profiles + device tokens in one 0600 JSON (plan T3).
// ABOUTME: Fails closed on corruption and publishes atomically.

//! # Host store (spec `2026-10-19-relay-phase2-5-desktop-host-client-design` §2.5, §2.6)
//!
//! One app-private document, `<app_config_dir>/picot-hosts.json` (0600, the same
//! directory convention as `picot-daemon-settings.json`): the install-unique app
//! device id, plus every paired host's profile and its **plaintext bearer**
//! device token. The token is a live remote data-plane credential, so the file is
//! heavier than the daemon's token hashes (§2.6) and never prints: [`DeviceToken`]
//! redacts itself in `Debug`.
//!
//! ## Failure semantics
//!
//! - **Missing file** — a fresh empty document, minted install id, published at
//!   once so the id is stable across launches (§2.3 R8: a random per-connection
//!   device id would make the remote overwrite its own device record).
//! - **Corrupt file** — [`StoreError::Corrupt`] and nothing else. There is no
//!   fallback document (§2.6 R9: this file is the TOFU trust baseline, unlike the
//!   quit policy): the bytes stay untouched, every later `load` fails with the
//!   same verdict, and because a store cannot be constructed there is no object
//!   whose `save` could overwrite the evidence. Recovery is the user's explicit
//!   act — restore or delete the file — and the UI says so.
//! - **Write failure** — the previous document survives (temp + `rename`, no
//!   partial publish), and in-memory state follows only a durable publish.
//!   A failed pairing write is reported to the caller: the remote token cannot
//!   be rolled back, so the host has to pair again (never claim it works).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::daemon::instance::write_atomic_bytes;
use crate::relay::pairing::{ConnectionOfferV2, RelayEndpoint};

/// Store file name inside the app config directory (§2.6).
pub const HOSTS_FILE_NAME: &str = "picot-hosts.json";

/// Where the host store lives for a given `app_config_dir`.
pub fn hosts_path(config_dir: &Path) -> PathBuf {
    config_dir.join(HOSTS_FILE_NAME)
}

/// Display label for a host whose label the user cannot edit (v1): the relay
/// endpoint's host part, from the offer (§3.1).
pub fn default_label(endpoint: &str) -> String {
    // shortcut: an IPv6 literal endpoint keeps its bracketed form; upgrade when
    // a v6 relay endpoint actually ships.
    match endpoint.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            host.to_string()
        }
        _ => endpoint.to_string(),
    }
}

/// The remote's bearer credential for this device. Redacts itself in `Debug`
/// so an accidental `{:?}` cannot put a live token in a log line (§5).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceToken(String);

impl DeviceToken {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw token: the one place it leaves this module, for the relay hello.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DeviceToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DeviceToken(<redacted>)")
    }
}

/// One saved host: the trust anchors, a display label, and when it was added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostProfile {
    pub server_id: String,
    pub label: String,
    pub relay: RelayEndpoint,
    pub daemon_public_key_b64: String,
    /// Unix epoch seconds; the offer carries no timestamp of its own.
    pub added_at: u64,
}

impl HostProfile {
    /// The profile an offer would save (label defaults to the endpoint host).
    pub fn from_offer(offer: &ConnectionOfferV2, added_at: u64) -> Self {
        Self {
            server_id: offer.server_id.clone(),
            label: default_label(&offer.relay.endpoint),
            relay: offer.relay.clone(),
            daemon_public_key_b64: offer.daemon_public_key_b64.clone(),
            added_at,
        }
    }
}

/// A saved host plus the token that authenticates this app as one of its devices.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredHost {
    pub profile: HostProfile,
    /// Absent after a revoke (`credentialInvalid`, §2.5) until a re-pair lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_token: Option<DeviceToken>,
}

impl StoredHost {
    pub fn has_token(&self) -> bool {
        self.device_token.is_some()
    }
}

impl fmt::Debug for StoredHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredHost")
            .field("profile", &self.profile)
            .field("device_token", &self.device_token)
            .finish()
    }
}

/// Every way the store can refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The file exists but is not our document. Blocking: never overwritten.
    Corrupt { path: PathBuf, detail: String },
    /// The filesystem refused a read, encode, or publish.
    Io { path: PathBuf, detail: String },
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt { path, detail } => write!(
                formatter,
                "host store {} is corrupt ({detail}); restore or delete it to recover",
                path.display()
            ),
            Self::Io { path, detail } => {
                write!(formatter, "host store {} failed: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for StoreError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostsDocument {
    /// Install-unique app device id (§2.3 R8): stable across launches, so a
    /// re-pair overwrites this app's own device record instead of adding one.
    app_device_id: String,
    #[serde(default)]
    hosts: Vec<StoredHost>,
}

/// The owner of the host document. Every mutation publishes the whole file
/// first and only then moves memory, so a failed write leaves the saved state
/// (and the caller's next read) exactly as before.
pub struct HostStore {
    path: PathBuf,
    document: Mutex<HostsDocument>,
}

impl fmt::Debug for HostStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The document holds bearer tokens: only the location is printable.
        formatter
            .debug_struct("HostStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl HostStore {
    /// Load the store, initialising it when the file is absent.
    ///
    /// A corrupt file is [`StoreError::Corrupt`] — the only instance of the
    /// blocked state: no store exists, so nothing can save over the evidence.
    pub fn load(path: PathBuf) -> Result<Self, StoreError> {
        let document = match std::fs::read_to_string(&path) {
            Ok(raw) => {
                let document: HostsDocument =
                    serde_json::from_str(&raw).map_err(|error| StoreError::Corrupt {
                        path: path.clone(),
                        detail: error.to_string(),
                    })?;
                if document.app_device_id.trim().is_empty() {
                    return Err(StoreError::Corrupt {
                        path,
                        detail: "appDeviceId is empty".to_string(),
                    });
                }
                document
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let store = Self {
                    path,
                    document: Mutex::new(HostsDocument {
                        app_device_id: uuid::Uuid::new_v4().to_string(),
                        hosts: Vec::new(),
                    }),
                };
                // Publish at once: the install id must not change on the next launch.
                let document = store.lock().clone();
                store.write(&document)?;
                return Ok(store);
            }
            Err(error) => {
                return Err(StoreError::Io {
                    path,
                    detail: error.to_string(),
                })
            }
        };
        Ok(Self {
            path,
            document: Mutex::new(document),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn app_device_id(&self) -> String {
        self.lock().app_device_id.clone()
    }

    pub fn hosts(&self) -> Vec<StoredHost> {
        self.lock().hosts.clone()
    }

    pub fn host(&self, server_id: &str) -> Option<StoredHost> {
        self.lock()
            .hosts
            .iter()
            .find(|host| host.profile.server_id == server_id)
            .cloned()
    }

    /// Delete one host profile and its token (§2.5: the remote still lists the
    /// device — revoking it is the owner's act on the remote side).
    pub fn delete(&self, server_id: &str) -> Result<bool, StoreError> {
        let mut document = self.lock();
        let Some(index) = document
            .hosts
            .iter()
            .position(|host| host.profile.server_id == server_id)
        else {
            return Ok(false);
        };
        let mut next = document.clone();
        next.hosts.remove(index);
        self.write(&next)?;
        *document = next;
        Ok(true)
    }

    /// Explicit remote rejection clears the bearer but keeps the trust anchors.
    pub fn clear_token(&self, server_id: &str) -> Result<(), StoreError> {
        let mut document = self.lock();
        let Some(index) = document
            .hosts
            .iter()
            .position(|host| host.profile.server_id == server_id && host.device_token.is_some())
        else {
            return Ok(());
        };
        let mut next = document.clone();
        next.hosts[index].device_token = None;
        self.write(&next)?;
        *document = next;
        Ok(())
    }

    /// Replace or insert one host with the token this device just received, then
    /// publish. A failed write leaves the saved profile as it was.
    pub fn upsert(
        &self,
        profile: &HostProfile,
        token: DeviceToken,
    ) -> Result<StoredHost, StoreError> {
        let mut document = self.lock();
        let index = document
            .hosts
            .iter()
            .position(|host| host.profile.server_id == profile.server_id);
        let stored = StoredHost {
            profile: profile.clone(),
            device_token: Some(token),
        };
        let mut next = document.clone();
        match index {
            Some(index) => next.hosts[index] = stored.clone(),
            None => next.hosts.push(stored.clone()),
        }
        self.write(&next)?;
        *document = next;
        Ok(stored)
    }

    fn write(&self, document: &HostsDocument) -> Result<(), StoreError> {
        let encoded = serde_json::to_vec_pretty(document).map_err(|error| StoreError::Io {
            path: self.path.clone(),
            detail: format!("cannot encode the host store: {error}"),
        })?;
        write_atomic_bytes(&self.path, &encoded).map_err(|detail| StoreError::Io {
            path: self.path.clone(),
            detail,
        })
    }

    fn lock(&self) -> MutexGuard<'_, HostsDocument> {
        // A panicking holder left the document whole (it only moves after a
        // durable publish), so the poisoned lock is still the truth.
        self.document
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
