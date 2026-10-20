// ABOUTME: TOFU decisions for a pasted pairing offer (plan T3, spec §2.3, §3.3).
// ABOUTME: Exact matches with a held token auto-approve; every key/endpoint/TLS
// ABOUTME: change asks one question at a time, showing Paseo's key fingerprint.

//! # Trust-on-first-use for relay hosts
//!
//! The offer link is the trust anchor (§5). This module is the pure decision in
//! front of it: given an offer and whatever the store already holds, either the
//! connection is the exact one already trusted (and this app still holds that
//! host's device token) and the add proceeds silently, or the user is asked.
//!
//! Three deliberate properties:
//!
//! - **Per-item differences.** A changed key, a changed relay endpoint, and a
//!   changed TLS flag are reported separately, so the sheet can list what moved
//!   instead of one opaque "connection changed" (§3.3).
//! - **A fingerprint, not a hash.** [`format_fingerprint`] is Paseo's
//!   `formatDaemonKeyFingerprint` on the same input: punctuation dropped, groups
//!   of four, and only the first and last eight when the key is longer than
//!   sixteen characters. Same input, same output — that is what lets a user
//!   compare the sheet against the key their daemon shows.
//! - **One question at a time.** [`ConfirmationQueue`] holds one unanswered
//!   question; asking a new one supersedes it, and the superseded question can
//!   never be approved afterwards (an unanswered question means *no*).
//!
//! The decision says nothing about *when* a change lands: that is the staged
//! transaction in [`crate::relay::host_store`], which commits only after the
//! remote authenticated this device.

use crate::relay::host_store::StoredHost;
use crate::relay::pairing::ConnectionOfferV2;

/// What the caller must do before trusting the offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TofuDecision {
    /// The saved host matches exactly and its device token is still held.
    AutoApprove,
    /// Ask the user; nothing is saved until they answer *and* the remote pairs.
    Confirm(ConfirmQuestion),
}

/// Which copy the sheet shows (§3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    /// No saved host for this `serverId`.
    NewHost,
    /// The host is known but the connection moved, or its token is gone.
    ChangedConnection,
}

/// One differing connection item. `saved`/`offered` are what the sheet renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionChange {
    DaemonKey { saved: String, offered: String },
    RelayEndpoint { saved: String, offered: String },
    UseTls { saved: bool, offered: bool },
}

/// The question itself — everything the confirmation sheet needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmQuestion {
    pub server_id: String,
    /// The offered key, formatted for comparison with the daemon's own display.
    pub key_fingerprint: String,
    pub relay_endpoint: String,
    pub kind: ConfirmKind,
    pub changes: Vec<ConnectionChange>,
}

/// Decide whether the offer needs the user (spec §2.3.3).
///
/// Auto-approval needs both halves: the saved connection is byte-identical to
/// the offer *and* the device token for it is still on hand. A host whose token
/// was cleared (revoked device, §2.5) re-pairs, so the user sees its key once
/// more — the question then carries no diffs.
pub fn decide(offer: &ConnectionOfferV2, saved: Option<&StoredHost>) -> TofuDecision {
    let Some(saved) = saved else {
        return TofuDecision::Confirm(question(offer, ConfirmKind::NewHost, Vec::new()));
    };

    let mut changes = Vec::new();
    if saved.profile.daemon_public_key_b64 != offer.daemon_public_key_b64 {
        changes.push(ConnectionChange::DaemonKey {
            saved: format_fingerprint(&saved.profile.daemon_public_key_b64),
            offered: format_fingerprint(&offer.daemon_public_key_b64),
        });
    }
    if saved.profile.relay.endpoint != offer.relay.endpoint {
        changes.push(ConnectionChange::RelayEndpoint {
            saved: saved.profile.relay.endpoint.clone(),
            offered: offer.relay.endpoint.clone(),
        });
    }
    if saved.profile.relay.use_tls != offer.relay.use_tls {
        changes.push(ConnectionChange::UseTls {
            saved: saved.profile.relay.use_tls,
            offered: offer.relay.use_tls,
        });
    }

    if changes.is_empty() && saved.has_token() {
        return TofuDecision::AutoApprove;
    }
    TofuDecision::Confirm(question(offer, ConfirmKind::ChangedConnection, changes))
}

/// Paseo `formatDaemonKeyFingerprint` parity: base64 punctuation dropped,
/// four-character groups, first eight … last eight once longer than sixteen.
pub fn format_fingerprint(daemon_public_key_b64: &str) -> String {
    let normalized: String = daemon_public_key_b64
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    if normalized.len() <= 16 {
        return group(&normalized);
    }
    format!(
        "{} … {}",
        group(&normalized[..8]),
        group(&normalized[normalized.len() - 8..])
    )
}

fn group(value: &str) -> String {
    value
        .as_bytes()
        .chunks(4)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A question as asked: its id exists so a late answer can be recognised as stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskedQuestion {
    pub id: u64,
    pub question: ConfirmQuestion,
}

/// The answer to one question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Approved,
    Denied,
}

/// The one host question the app is waiting on (§3.3).
#[derive(Debug, Default)]
pub struct ConfirmationQueue {
    pending: Option<AskedQuestion>,
    next_id: u64,
}

impl ConfirmationQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask a question. A still-unanswered question is superseded and can never
    /// be approved afterwards — an unanswered question means *no*. The returned
    /// id is the superseded question, so the caller can resolve its waiter as
    /// denied.
    pub fn ask(&mut self, question: ConfirmQuestion) -> Option<u64> {
        let superseded = self.pending.as_ref().map(|pending| pending.id);
        self.next_id += 1;
        self.pending = Some(AskedQuestion {
            id: self.next_id,
            question,
        });
        superseded
    }

    pub fn pending(&self) -> Option<&AskedQuestion> {
        self.pending.as_ref()
    }

    /// Answer the pending question. An answer for any other id is ignored (it
    /// belongs to a question the user already moved past).
    pub fn answer(&mut self, id: u64, approved: bool) -> Option<Answer> {
        if self.pending.as_ref()?.id != id {
            return None;
        }
        self.pending = None;
        Some(if approved {
            Answer::Approved
        } else {
            Answer::Denied
        })
    }
}

fn question(
    offer: &ConnectionOfferV2,
    kind: ConfirmKind,
    changes: Vec<ConnectionChange>,
) -> ConfirmQuestion {
    ConfirmQuestion {
        server_id: offer.server_id.clone(),
        key_fingerprint: format_fingerprint(&offer.daemon_public_key_b64),
        relay_endpoint: offer.relay.endpoint.clone(),
        kind,
        changes,
    }
}
