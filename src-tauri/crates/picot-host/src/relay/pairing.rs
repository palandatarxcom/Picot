// ABOUTME: Pairing offer codec (spec 2026-10-17-relay-phase2 §2.6/§3.3, plan T2).
// ABOUTME: Paseo v2 payload shape plus the optional one-scan pairingToken.

//! Responsibilities (spec §2.1, §2.6, §3.3):
//!
//! - Encode/parse the connection offer payload: `v: 2`, `serverId`,
//!   `daemonPublicKeyB64`, `relay { endpoint, useTls }`, and the optional
//!   `pairingToken` (Picot's one-scan addition on Paseo's v2 schema; Paseo
//!   clients ignore unknown fields, so interop is preserved).
//! - Render the QR/copy URL: `https://picot.021521.xyz/#offer=<base64url>`
//!   (unpadded base64url, `A-Za-z0-9-_`), and parse a pasted URL back into an
//!   offer by locating the `#offer=` fragment anywhere in the string.
//! - Reject `v != 2`, empty `serverId`/`daemonPublicKeyB64`, missing relay
//!   endpoint; ignore unknown fields (forward-compatible).
//!
//! Landing in plan task T2; until then this module is a documented placeholder.
