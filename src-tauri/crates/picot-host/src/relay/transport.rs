// ABOUTME: Relay control/data transport (spec 2026-10-17-relay-phase2 §2.5/§2.7, plan T4/T5).
// ABOUTME: Paseo relay-transport parameters copied verbatim; overflow is local.

//! Responsibilities (spec §2.1, §2.5, §2.7):
//!
//! - Control socket: `wss://<endpoint>/ws?serverId=&role=server&v=2` with
//!   Paseo's exact parameters — reconnect backoff `min(30s, 1s × attempt)`,
//!   ping every 10s, 30s stale force-close, 8s control-ready timeout, 10s WS
//!   handshake timeout, `permessage-deflate` off.
//! - Data sockets: one per relay `connectionId` (`&connectionId=`), 15s open
//!   timeout, each running its own E2EE channel; `sync` frames reconcile the
//!   daemon's socket set against the relay's list of live connectionIds.
//! - Capacity bounds (spec §2.7): control frames ≤ 64 KiB, inbound data
//!   ciphertext sized so plaintext ≤ `WS_PHYSICAL_FRAME_BYTES`, connections
//!   counted against `MAX_CLIENTS`, ≤ 4 concurrent E2EE handshakes, bounded
//!   per-connection outbound queue — any overflow closes only the offending
//!   connection.
//! - TLS: `native-tls` connector (system roots, chain + SNI validation) with
//!   an injectable seam for fixture-CA tests.
//! - Session bridging: decrypted frames feed the generalized host session
//!   entry under host-assigned internal ids (`relay:<uuid>`); the hello-declared
//!   `clientId` is metadata only (spec §2.2 three-ID model).
//!
//! Landing in plan tasks T4/T5; until then this module is a documented placeholder.
