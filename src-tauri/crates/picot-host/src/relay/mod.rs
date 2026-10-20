// ABOUTME: Relay Phase 2 module root (spec 2026-10-17-relay-phase2 §2.1).
// ABOUTME: Owned by DaemonState; dial is the daemon's only outbound network face.

//! Relay transport for the picot-daemon: an outbound TLS WebSocket family to
//! the company relay carrying per-connection end-to-end-encrypted host
//! sessions, so paired remote devices operate the owner data plane without the
//! app process staying alive (spec `2026-10-17-relay-phase2-pair-device-design`
//! §2.1).
//!
//! Submodules, mirroring the parent spec §4.1 split:
//!
//! - [`e2ee`] — daemon identity keypair, NaCl box crypto, E2EE handshake.
//! - [`client_role`] — the desktop client's outbound role=client socket:
//!   dial, E2EE client handshake, bounded frames, redial, half-open.
//! - [`controller`] — enablement lifecycle, status view, pairing-offer ops.
//! - [`host_store`] — the desktop client's host profiles and device tokens:
//!   0600 JSON, fail-closed corruption, staged generation-guarded commits.
//! - [`host_tofu`] — trust-on-first-use decisions and the key fingerprint.
//! - [`pairing`] — pairing offer payload and `#offer=` fragment URL codec.
//! - [`transport`] — control/data sockets, reconnect backoff, sync reconcile.

pub mod client_role;
pub mod controller;
pub mod e2ee;
pub mod host_store;
pub mod host_tofu;
pub mod pairing;
pub mod smoke_client;
pub mod transport;
