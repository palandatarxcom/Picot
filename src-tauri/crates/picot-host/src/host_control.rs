// ABOUTME: Defines canonical HostServer control callback types and owner event delivery.
// ABOUTME: Keeps host transport contracts independent from retired broker WebSocket code.

use crate::window_owner::OwnerId;
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientClass {
    Native,
    Remote,
}

#[derive(Clone, Debug)]
pub struct VerifiedClientContext {
    pub client_id: u64,
    pub class: ClientClass,
    pub owner_id: Option<OwnerId>,
    /// The paired device behind a `Remote` context (contract 1): its
    /// persisted view is the navigation authority, so the dispatcher branches
    /// on it instead of the window-owner registry. `None` for Native.
    pub device_id: Option<String>,
}

pub type ProgressSink = Arc<dyn Fn(Value) + Send + Sync>;
pub type ControlHandler = Arc<
    dyn Fn(VerifiedClientContext, Value, ProgressSink) -> BoxFuture<'static, Result<Value, String>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct HostEventSink {
    sender: broadcast::Sender<OwnerEvent>,
}

#[derive(Clone, Debug)]
pub struct OwnerEvent {
    pub owner: Option<OwnerId>,
    pub value: Value,
}

impl HostEventSink {
    pub fn new(sender: broadcast::Sender<OwnerEvent>) -> Self {
        Self { sender }
    }

    pub fn send_owner_event(&self, owner: &OwnerId, value: Value) -> bool {
        self.sender
            .send(OwnerEvent {
                owner: Some(owner.clone()),
                value,
            })
            .is_ok()
    }

    /// Test seam (R5): a subscriber that observes exactly what a WebView's
    /// data-plane connection would receive, so an owner push is assertable
    /// without a real WebSocket. Production subscribers live in the `/v2/ws`
    /// handler, which owns its own receiver.
    pub fn subscribe_for_tests(&self) -> broadcast::Receiver<OwnerEvent> {
        self.sender.subscribe()
    }

    pub fn broadcast_native_event(&self, value: Value) -> usize {
        self.sender
            .send(OwnerEvent { owner: None, value })
            .map(|_| 1)
            .unwrap_or(0)
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<OwnerEvent> {
        self.sender.subscribe()
    }
}
