// ABOUTME: The owner close-request bridge: the app's admin ask, the daemon's data-plane push,
// ABOUTME: and the approve/cancel/timeout settlement the app's window close waits on.

//! # Owner close requests (R5)
//!
//! The WebView owns the window-close *confirmation* flow (its coordinator runs
//! the per-participant risk/settlement dialogs — unsaved drafts, live
//! terminals, Side Chat discard). The app owns the native window, so it
//! intercepts `CloseRequested` and must hand the question to the WebView. That
//! hand-off cannot be in-process any more: the WebView is loaded from the
//! **daemon's** host origin and holds a data-plane WS to the daemon (plan D1),
//! so only the daemon can push a frame into it.
//!
//! The chain this module implements the daemon half of:
//!
//! ```text
//! app (CloseRequested, prevent_close)
//!   → admin `owner.close_request {owner, requestId}`
//!       → data-plane push `{"type":"window_close_request","requestId":"close-N"}`
//!           → WebView close coordinator (risk/settlement dialogs)
//!               → data-plane `window_close_approve` | `window_close_cancel`
//!                   → app-surface `close_window` | `cancel_window_close` (app)
//!   ← admin result `{requestId, delivered, outcome}`
//! ```
//!
//! `owner.close_request` is a **bounded long-poll**: it answers as soon as the
//! WebView settles (approve/cancel), and on the deadline it answers `timeout`.
//! The app's close delegate then falls back to its native dialog — the
//! degradation path, which also covers `undeliverable` (no WebView attached to
//! that owner at all, e.g. a crashed renderer).
//!
//! The request id is the app's (`close-N`, minted by the app crate's close
//! delegate): the app's pending-close map is app state (plan D4), so the id the
//! coordinator echoes back settles that map — the daemon only relays it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::oneshot;

/// The data-plane frame the daemon pushes to one owner's WebView.
pub const WINDOW_CLOSE_REQUEST: &str = "window_close_request";

/// How long `owner.close_request` waits for the WebView's settlement before it
/// answers `timeout` (the app then falls back to the native dialog).
///
/// The deadline has to cover a human deciding in the WebView's risk dialog —
/// so it is generous, and it is **not** the app's request deadline: the app
/// awaits this op with its own, slightly longer one
/// (`CLOSE_REQUEST_CLIENT_TIMEOUT` in the app crate). A live WebView that
/// never answers is the only case this deadline exists for; a WebView that is
/// *gone* is answered immediately as `undeliverable`.
pub const CLOSE_REQUEST_SETTLE_TIMEOUT: Duration = Duration::from_secs(30);

/// The `window_close_request` frame, spelled once (the protocol table's
/// literal, pinned by `tests/owner_close_request.rs`).
pub fn window_close_request_frame(request_id: &str) -> Value {
    json!({ "type": WINDOW_CLOSE_REQUEST, "requestId": request_id })
}

/// How one owner close request ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloseOutcome {
    /// The coordinator approved; the app-surface settlement closed the window.
    Approved,
    /// The coordinator cancelled; the app dropped its pending close.
    Cancelled,
    /// The deadline passed with no settlement (a live but silent WebView).
    Timeout,
    /// No WebView holds this owner, so there was nothing to ask.
    Undeliverable,
}

impl CloseOutcome {
    /// True when the app's close delegate must run its native dialog: the
    /// WebView never owned the question (`undeliverable`) or never answered
    /// (`timeout`).
    pub fn requires_native_fallback(self) -> bool {
        matches!(self, CloseOutcome::Timeout | CloseOutcome::Undeliverable)
    }
}

/// The `owner.close_request` reply (protocol table:
/// `admin_result.result = {requestId, delivered, outcome}`).
///
/// `delivered` reports whether the push reached a WebView; `outcome` reports
/// how the request ended. The two are not redundant: a delivered request that
/// times out is `{delivered:true, outcome:"timeout"}` — the app asked, waited,
/// and must now degrade.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OwnerCloseDelivery {
    #[serde(rename = "requestId")]
    pub request_id: String,
    pub delivered: bool,
    pub outcome: CloseOutcome,
}

impl OwnerCloseDelivery {
    /// Decode one `owner.close_request` reply. A malformed reply is an error
    /// the caller treats exactly like a failed op (native dialog), never a
    /// silently plausible "approved".
    pub fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone())
            .map_err(|error| format!("malformed owner.close_request reply: {error}"))
    }

    /// The one decision the app's close delegate makes from this reply: does
    /// it need to run the native dialog?
    pub fn requires_native_fallback(&self) -> bool {
        self.outcome.requires_native_fallback()
    }
}

/// The daemon's pending owner close requests: one waiter per request id,
/// resolved by the data-plane approve/cancel ops or by the op's own deadline.
///
/// The waiter is registered **before** the push (see
/// [`DaemonState::push_owner_close_request`]'s callers) so a coordinator that
/// answers instantly cannot race its own registration; `forget` drops a waiter
/// whose push failed.
#[derive(Default)]
pub struct OwnerCloseRequests {
    waiters: Mutex<HashMap<String, oneshot::Sender<CloseOutcome>>>,
}

impl OwnerCloseRequests {
    /// Register the waiter for one request id and hand back its receiver.
    pub fn register(&self, request_id: &str) -> oneshot::Receiver<CloseOutcome> {
        let (sender, receiver) = oneshot::channel();
        self.waiters
            .lock()
            .expect("owner close waiter lock")
            .insert(request_id.to_string(), sender);
        receiver
    }

    /// Settle one pending request. False when no request by that id is pending
    /// (a duplicate approve, or one that already timed out) — the caller must
    /// not treat that as "cancelled".
    pub fn resolve(&self, request_id: &str, outcome: CloseOutcome) -> bool {
        let sender = self
            .waiters
            .lock()
            .expect("owner close waiter lock")
            .remove(request_id);
        match sender {
            Some(sender) => sender.send(outcome).is_ok(),
            None => false,
        }
    }

    /// Drop a waiter without settling it (its push never landed).
    pub fn forget(&self, request_id: &str) {
        self.waiters
            .lock()
            .expect("owner close waiter lock")
            .remove(request_id);
    }

    /// True while a request id is pending (test read-back).
    pub fn is_pending(&self, request_id: &str) -> bool {
        self.waiters
            .lock()
            .expect("owner close waiter lock")
            .contains_key(request_id)
    }

    /// Wait for the settlement of one request. The deadline (and a dropped
    /// sender) both answer [`CloseOutcome::Timeout`]: "no coordinator told us
    /// how this ended" is one outcome, not two.
    pub async fn await_settlement(
        &self,
        request_id: &str,
        receiver: oneshot::Receiver<CloseOutcome>,
        timeout: Duration,
    ) -> CloseOutcome {
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) | Err(_) => {
                // Drop the slot: a settlement arriving after the deadline must
                // not resurrect a request the app already degraded.
                self.forget(request_id);
                CloseOutcome::Timeout
            }
        }
    }
}
