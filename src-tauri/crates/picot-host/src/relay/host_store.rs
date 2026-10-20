// ABOUTME: Desktop-client host store: profiles + device tokens in one 0600 JSON (plan T3).
// ABOUTME: Fails closed on corruption, publishes atomically, and lands TOFU changes
// ABOUTME: only through a generation-guarded staged transaction.

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
//!
//! ## Staged TOFU transaction
//!
//! A changed or new connection is never written directly: [`StagedPairing::begin`]
//! holds the candidate profile in memory, [`StagedPairing::remote_authenticated`]
//! records the token the remote minted, and [`StagedPairing::commit`] performs the
//! generation-checked replace. Cancel, a failed remote authentication, a stale
//! confirmation response, or a store write failure all leave the saved profile —
//! and the live relay connection built from it — exactly as they were (§2.3.3).
//!
//! **The honest part (§5.2 contract 3, R2-06):** a remote token cannot be rolled
//! back. When the exchange succeeded but the local write failed, the staged
//! candidate keeps its token in memory and `commit` may be retried; the remote
//! already counts this device. If the process dies before that retry succeeds,
//! the token is gone and the host has to pair again — never claim the old profile
//! still works. The user-facing copy for that state belongs to the add-host flow.

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
    /// Committed changes so far; the staged-commit guard (R2-09).
    pub generation: u64,
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
            .field("generation", &self.generation)
            .finish()
    }
}

/// Every way the store or a staged transaction can refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The file exists but is not our document. Blocking: never overwritten.
    Corrupt { path: PathBuf, detail: String },
    /// The filesystem refused a read, encode, or publish.
    Io { path: PathBuf, detail: String },
    /// A staged replace found a different generation than the caller expected.
    GenerationMismatch {
        server_id: String,
        expected: Option<u64>,
        actual: Option<u64>,
    },
    /// The staged transaction is not in a state that allows this step (commit
    /// before the remote accepted, after cancel, or after a successful commit).
    InvalidStagedState {
        server_id: String,
        state: &'static str,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt { path, detail } => write!(
                formatter,
                "host store {} is corrupt ({detail}); restore or delete it to recover",
                path.display()
            ),
            Self::Io { path, detail } => write!(formatter, "host store {} failed: {detail}", path.display()),
            Self::GenerationMismatch {
                server_id,
                expected,
                actual,
            } => write!(
                formatter,
                "host {server_id} changed under a staged commit (expected generation {expected:?}, found {actual:?})"
            ),
            Self::InvalidStagedState { server_id, state } => write!(
                formatter,
                "the staged change for host {server_id} cannot commit while {state}"
            ),
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

    /// Generation-checked replace, then publish. `expected` is `None` when the
    /// host must not exist yet (a first pairing).
    fn commit_profile(
        &self,
        profile: &HostProfile,
        expected: Option<u64>,
        token: DeviceToken,
    ) -> Result<StoredHost, StoreError> {
        let mut document = self.lock();
        let index = document
            .hosts
            .iter()
            .position(|host| host.profile.server_id == profile.server_id);
        let actual = index.map(|index| document.hosts[index].generation);
        if actual != expected {
            return Err(StoreError::GenerationMismatch {
                server_id: profile.server_id.clone(),
                expected,
                actual,
            });
        }
        let stored = StoredHost {
            profile: profile.clone(),
            device_token: Some(token),
            generation: actual.unwrap_or(0) + 1,
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

/// A TOFU change waiting for the two things it needs: a remote authentication
/// that minted a token, and a store write that actually landed.
///
/// ```text
/// begin ──▶ AwaitingRemoteAuth ──remote_authenticated──▶ ReadyToCommit
///                                                          │ commit (ok)           ──▶ Committed
///                                                          │ commit (write fails)  ──▶ SaveFailed ──commit retry──▶ Committed
///        └── cancel ──▶ Abandoned (candidate + token dropped; saved profile untouched)
/// ```
#[derive(Debug)]
pub struct StagedPairing {
    profile: HostProfile,
    expected_generation: Option<u64>,
    token: Option<DeviceToken>,
    state: StagedState,
}

/// Where a [`StagedPairing`] stands. Pinned by tests: the UI reads it to tell
/// "pairing failed, nothing saved" from "the remote accepted us but the write
/// did not, retry" (§5.2 contract 3, R2-06).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedState {
    AwaitingRemoteAuth,
    ReadyToCommit,
    /// The remote minted a token but the local write failed. The candidate and
    /// its token live in memory only: `commit` retries the save, and dropping
    /// this object (process death) loses the token for good — the host must pair
    /// again. The remote already knows this device either way.
    SaveFailed {
        detail: String,
    },
    Committed {
        generation: u64,
    },
    Abandoned,
}

impl StagedState {
    pub fn name(&self) -> &'static str {
        match self {
            Self::AwaitingRemoteAuth => "awaitingRemoteAuth",
            Self::ReadyToCommit => "readyToCommit",
            Self::SaveFailed { .. } => "saveFailed",
            Self::Committed { .. } => "committed",
            Self::Abandoned => "abandoned",
        }
    }
}

impl StagedPairing {
    /// Start a staged change. `expected_generation` is the saved host's
    /// generation (or `None` when the host must not exist yet).
    pub fn begin(profile: HostProfile, expected_generation: Option<u64>) -> Self {
        Self {
            profile,
            expected_generation,
            token: None,
            state: StagedState::AwaitingRemoteAuth,
        }
    }

    pub fn profile(&self) -> &HostProfile {
        &self.profile
    }

    pub fn expected_generation(&self) -> Option<u64> {
        self.expected_generation
    }

    pub fn state(&self) -> &StagedState {
        &self.state
    }

    pub fn token(&self) -> Option<&DeviceToken> {
        self.token.as_ref()
    }

    /// The remote accepted the pairing and returned this device's token. The
    /// saved profile is still untouched — that is the whole point of staging.
    pub fn remote_authenticated(&mut self, token: DeviceToken) -> Result<(), StoreError> {
        match self.state {
            StagedState::AwaitingRemoteAuth | StagedState::SaveFailed { .. } => {
                self.token = Some(token);
                self.state = StagedState::ReadyToCommit;
                Ok(())
            }
            _ => Err(self.invalid_state()),
        }
    }

    /// Publish the candidate. Only from [`StagedState::ReadyToCommit`] (and
    /// [`StagedState::SaveFailed`], where it retries the interrupted save).
    pub fn commit(&mut self, store: &HostStore) -> Result<StoredHost, StoreError> {
        match self.state {
            StagedState::ReadyToCommit | StagedState::SaveFailed { .. } => {}
            _ => return Err(self.invalid_state()),
        }
        let Some(token) = self.token.clone() else {
            return Err(self.invalid_state());
        };
        match store.commit_profile(&self.profile, self.expected_generation, token) {
            Ok(stored) => {
                self.state = StagedState::Committed {
                    generation: stored.generation,
                };
                Ok(stored)
            }
            Err(error @ StoreError::Io { .. }) => {
                self.state = StagedState::SaveFailed {
                    detail: error.to_string(),
                };
                Err(error)
            }
            // A generation mismatch is somebody else's committed change: this
            // candidate is simply stale and stays staged until the caller drops it.
            Err(error) => Err(error),
        }
    }

    /// Drop the candidate (cancel, failed remote authentication, stale answer).
    /// A no-op once committed: the store already holds that change.
    pub fn cancel(&mut self) {
        if matches!(self.state, StagedState::Committed { .. }) {
            return;
        }
        self.token = None;
        self.state = StagedState::Abandoned;
    }

    fn invalid_state(&self) -> StoreError {
        StoreError::InvalidStagedState {
            server_id: self.profile.server_id.clone(),
            state: self.state.name(),
        }
    }
}
