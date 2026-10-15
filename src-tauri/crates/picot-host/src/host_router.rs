#![cfg_attr(not(test), allow(dead_code))]

use crate::mutation_types::is_mutation;
use crate::transport_limits::{validate, PayloadKind};
use crate::window_owner::{OwnerId, OwnerWorkspaceSnapshot};
use serde_json::Value;
use std::collections::HashMap;

pub const PROTOCOL_VERSION: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    Desktop,
    Remote,
    Unpaired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostClientContext {
    pub client_id: String,
    pub kind: ClientKind,
    pub owner_id: Option<OwnerId>,
    pub workspace_id: Option<String>,
    pub workspace_generation: Option<u64>,
}

impl HostClientContext {
    pub fn desktop(
        client_id: impl Into<String>,
        owner_id: OwnerId,
        snapshot: OwnerWorkspaceSnapshot,
    ) -> Self {
        let (workspace_id, generation) = match snapshot {
            OwnerWorkspaceSnapshot::Registered {
                wid, generation, ..
            } => (Some(wid), Some(generation)),
            OwnerWorkspaceSnapshot::Temporary { generation, .. } => (None, Some(generation)),
            OwnerWorkspaceSnapshot::NoWorkspace => (None, None),
        };
        Self {
            client_id: client_id.into(),
            kind: ClientKind::Desktop,
            owner_id: Some(owner_id),
            workspace_id,
            workspace_generation: generation,
        }
    }

    pub fn public(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            kind: ClientKind::Unpaired,
            owner_id: None,
            workspace_id: None,
            workspace_generation: None,
        }
    }

    pub fn remote(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            kind: ClientKind::Remote,
            owner_id: None,
            workspace_id: None,
            workspace_generation: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RoutedAction {
    Runtime {
        client_id: String,
        request_id: String,
        frame: Value,
    },
    Host {
        client_id: String,
        request_id: String,
        operation: String,
        frame: Value,
    },
    Data {
        client_id: String,
        request_id: String,
        frame: Value,
    },
    Auth {
        client_id: String,
        request_id: String,
        frame: Value,
    },
    Subscribe {
        client_id: String,
        request_id: String,
        target: Value,
    },
    OperationStatus {
        client_id: String,
        request_id: String,
        operation_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterError {
    pub code: &'static str,
    pub message: String,
}

impl RouterError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

const MAX_CLIENTS: usize = 256;

pub struct HostRouter {
    clients: HashMap<String, HostClientContext>,
}

impl HostRouter {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
        }
    }

    pub fn connect(
        &mut self,
        client_id: &str,
        hello: &Value,
        context: HostClientContext,
    ) -> Result<(), RouterError> {
        if hello.get("type").and_then(Value::as_str) != Some("hello") {
            return Err(RouterError::new(
                "handshake_required",
                "First frame must be hello",
            ));
        }
        if hello.get("protocolVersion").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
            return Err(RouterError::new(
                "protocol_mismatch",
                format!(
                    "Picot protocol v{PROTOCOL_VERSION} is required; refresh or restart the app"
                ),
            ));
        }
        let kind = match hello.get("clientType").and_then(Value::as_str) {
            Some("desktop") => ClientKind::Desktop,
            Some("remote") => ClientKind::Remote,
            _ => {
                return Err(RouterError::new(
                    "invalid_client_type",
                    "Unsupported client type",
                ))
            }
        };
        if kind == ClientKind::Desktop
            && hello
                .get("desktopCapability")
                .and_then(Value::as_str)
                .is_none()
        {
            return Err(RouterError::new(
                "unauthenticated",
                "Desktop capability required",
            ));
        }
        if kind == ClientKind::Remote && context.kind != ClientKind::Remote {
            return Err(RouterError::new(
                "unauthorized_device",
                "Remote device authentication required",
            ));
        }
        if client_id.is_empty() {
            return Err(RouterError::new(
                "invalid_client_id",
                "clientId is required",
            ));
        }
        if context.client_id != client_id
            || (kind == ClientKind::Desktop && context.kind != ClientKind::Desktop)
            || (kind == ClientKind::Remote
                && !matches!(context.kind, ClientKind::Remote | ClientKind::Unpaired))
        {
            return Err(RouterError::new(
                "unauthenticated",
                "Client authentication context mismatch",
            ));
        }
        if !self.clients.contains_key(client_id) && self.clients.len() >= MAX_CLIENTS {
            return Err(RouterError::new(
                "client_limit",
                "Host client registry is full",
            ));
        }
        self.clients.insert(client_id.to_owned(), context);
        Ok(())
    }

    pub fn disconnect(&mut self, client_id: &str) -> Option<HostClientContext> {
        self.clients.remove(client_id)
    }

    pub fn client_kind(&self, client_id: &str) -> Option<ClientKind> {
        self.clients.get(client_id).map(|context| context.kind)
    }

    pub fn client_context(&self, client_id: &str) -> Option<&HostClientContext> {
        self.clients.get(client_id)
    }

    pub fn has_desktop_owner(&self, owner: &OwnerId) -> bool {
        self.clients.values().any(|context| {
            context.kind == ClientKind::Desktop && context.owner_id.as_ref() == Some(owner)
        })
    }

    pub fn route(&self, client_id: &str, frame: &Value) -> Result<RoutedAction, RouterError> {
        self.client_kind(client_id).ok_or_else(|| {
            RouterError::new("unauthorized_client", "Client has not completed handshake")
        })?;
        let frame_type = frame
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| RouterError::new("invalid_frame", "Frame type is required"))?;
        let request_id = frame
            .get("requestId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| RouterError::new("invalid_frame", "requestId is required"))?
            .to_owned();

        match frame_type {
            "runtime_subscribe" => {
                let target = frame.get("target").cloned().ok_or_else(|| {
                    RouterError::new("invalid_target", "Runtime target is required")
                })?;
                validate_target(&target)?;
                Ok(RoutedAction::Subscribe {
                    client_id: client_id.to_owned(),
                    request_id,
                    target,
                })
            }
            "runtime_request" | "runtime_snapshot_request" | "runtime_capabilities_request" => {
                if frame_type == "runtime_request" {
                    validate_runtime_request(frame)?;
                } else {
                    validate(frame, PayloadKind::Command).map_err(|code| {
                        RouterError::new(code, "Runtime request exceeds protocol limit")
                    })?;
                }
                Ok(RoutedAction::Runtime {
                    client_id: client_id.to_owned(),
                    request_id,
                    frame: frame.clone(),
                })
            }
            "host_request" => {
                validate(frame, PayloadKind::HostRequest).map_err(|code| {
                    RouterError::new(code, "Host request exceeds protocol limit")
                })?;
                if self
                    .clients
                    .get(client_id)
                    .is_some_and(|context| context.kind != ClientKind::Desktop)
                {
                    return Err(RouterError::new(
                        "forbidden_class",
                        "Host operation requires desktop owner",
                    ));
                }
                let operation =
                    frame
                        .get("operation")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            RouterError::new("invalid_host_request", "operation is required")
                        })?;
                Ok(RoutedAction::Host {
                    client_id: client_id.to_owned(),
                    request_id,
                    operation: operation.to_owned(),
                    frame: frame.clone(),
                })
            }
            "operation_status_request" => {
                let operation_id = frame
                    .get("operationId")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        RouterError::new("invalid_operation", "operationId is required")
                    })?;
                Ok(RoutedAction::OperationStatus {
                    client_id: client_id.to_owned(),
                    request_id,
                    operation_id: operation_id.to_owned(),
                })
            }
            "data_request" => {
                validate(frame, PayloadKind::DataRequest).map_err(|code| {
                    RouterError::new(code, "Data request exceeds protocol limit")
                })?;
                Ok(RoutedAction::Data {
                    client_id: client_id.to_owned(),
                    request_id,
                    frame: frame.clone(),
                })
            }
            "auth_request" => Ok(RoutedAction::Auth {
                client_id: client_id.to_owned(),
                request_id,
                frame: frame.clone(),
            }),
            _ => Err(RouterError::new(
                "unknown_frame_type",
                "Unsupported protocol v2 frame type",
            )),
        }
    }
}

fn validate_runtime_request(frame: &Value) -> Result<(), RouterError> {
    validate(frame, PayloadKind::Command)
        .map_err(|code| RouterError::new(code, "Runtime command exceeds protocol limit"))?;
    let target = frame
        .get("target")
        .ok_or_else(|| RouterError::new("invalid_target", "Runtime target is required"))?;
    validate_target(target)?;
    let command_type = frame
        .get("command")
        .and_then(|command| command.get("type"))
        .and_then(Value::as_str)
        .ok_or_else(|| RouterError::new("invalid_command", "Runtime command type is required"))?;
    if is_mutation(command_type)
        && frame
            .get("idempotencyKey")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty())
            .is_none()
    {
        return Err(RouterError::new(
            "idempotency_key_required",
            "Runtime mutations require idempotencyKey",
        ));
    }
    Ok(())
}

fn validate_target(target: &Value) -> Result<(), RouterError> {
    let target = target
        .as_object()
        .ok_or_else(|| RouterError::new("invalid_target", "Runtime target must be an object"))?;
    for field in ["workspaceId", "sessionId", "instanceId"] {
        if target.get(field).and_then(Value::as_str).is_none() {
            return Err(RouterError::new(
                "invalid_target",
                format!("{field} is required"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ClientKind, HostClientContext, HostRouter, RoutedAction, PROTOCOL_VERSION};
    use crate::window_owner::OwnerWorkspaceSnapshot;
    use serde_json::json;

    #[test]
    fn requires_an_exact_v2_handshake() {
        let mut router = HostRouter::new();
        assert!(router
            .connect(
                "client-a",
                &json!({ "type": "hello", "protocolVersion": 1, "clientType": "desktop" }),
                HostClientContext::public("client-a"),
            )
            .is_err());
        assert!(router
            .connect(
                "client-a",
                &json!({
                    "type": "hello",
                    "protocolVersion": PROTOCOL_VERSION,
                    "clientType": "desktop",
                    "desktopCapability": "test-capability",
                }),
                HostClientContext::desktop(
                    "client-a",
                    crate::window_owner::OwnerId::from_string("owner".into()),
                    OwnerWorkspaceSnapshot::NoWorkspace
                ),
            )
            .is_ok());
        assert_eq!(router.client_kind("client-a"), Some(ClientKind::Desktop));
    }

    #[test]
    fn hello_matrix_covers_valid_and_invalid_hello_for_three_client_classes() {
        let cases = [
            (
                "desktop",
                HostClientContext::desktop(
                    "c",
                    crate::window_owner::OwnerId::from_string("owner".into()),
                    OwnerWorkspaceSnapshot::NoWorkspace,
                ),
                json!({"type":"hello","protocolVersion":2,"clientType":"desktop","desktopCapability":"cap"}),
                json!({"type":"hello","protocolVersion":1,"clientType":"desktop","desktopCapability":"cap"}),
                json!({"type":"hello","protocolVersion":2,"clientType":"desktop"}),
            ),
            (
                "remote",
                HostClientContext::remote("c"),
                json!({"type":"hello","protocolVersion":2,"clientType":"remote"}),
                json!({"type":"hello","protocolVersion":1,"clientType":"remote"}),
                json!({"type":"hello","protocolVersion":2,"clientType":"desktop","desktopCapability":"cap"}),
            ),
            (
                "unpaired",
                HostClientContext::public("c"),
                json!({"type":"hello","protocolVersion":2,"clientType":"remote"}),
                json!({"type":"hello","protocolVersion":1,"clientType":"remote"}),
                json!({"type":"hello","protocolVersion":2,"clientType":"desktop","desktopCapability":"cap"}),
            ),
        ];
        for (name, context, valid, invalid_version, invalid_class) in cases {
            let mut router = HostRouter::new();
            let valid_result = router.connect("c", &valid, context.clone());
            if name == "unpaired" {
                assert!(
                    valid_result.is_err(),
                    "unpaired context must not self-authenticate as remote"
                );
            } else {
                assert!(valid_result.is_ok(), "valid {name} hello rejected");
            }
            assert!(
                router
                    .connect("version", &invalid_version, context.clone())
                    .is_err(),
                "invalid version accepted for {name}"
            );
            assert!(
                router.connect("class", &invalid_class, context).is_err(),
                "invalid class accepted for {name}"
            );
        }
    }

    #[test]
    fn repeated_connect_disconnect_keeps_the_client_registry_bounded() {
        let mut router = HostRouter::new();
        for round in 0..100 {
            let client_id = format!("client-{round}");
            router
                .connect(
                    &client_id,
                    &json!({"type":"hello","protocolVersion":2,"clientType":"remote"}),
                    HostClientContext::remote(client_id.clone()),
                )
                .unwrap();
            assert!(router.disconnect(&client_id).is_some());
        }
        assert!(
            router.clients.is_empty(),
            "disconnect must remove registrations; leaked {} entries",
            router.clients.len()
        );
        // A fresh client remains reachable after the churn.
        router
            .connect(
                "client-live",
                &json!({"type":"hello","protocolVersion":2,"clientType":"remote"}),
                HostClientContext::remote("client-live"),
            )
            .unwrap();
        assert_eq!(router.client_kind("client-live"), Some(ClientKind::Remote));
    }

    #[test]
    fn disconnect_removes_client_registration() {
        let mut router = HostRouter::new();
        router
            .connect(
                "client-a",
                &json!({"type":"hello","protocolVersion":2,"clientType":"remote"}),
                HostClientContext::remote("client-a"),
            )
            .unwrap();
        assert!(router.disconnect("client-a").is_some());
        assert!(router.client_kind("client-a").is_none());
        assert!(router.disconnect("client-a").is_none());
    }

    #[test]
    fn keeps_runtime_and_host_routes_separate() {
        let mut router = HostRouter::new();
        router
            .connect(
                "desktop",
                &json!({ "type": "hello", "protocolVersion": 2, "clientType": "desktop", "desktopCapability": "test-capability" }),
                HostClientContext::desktop("desktop", crate::window_owner::OwnerId::from_string("owner".into()), OwnerWorkspaceSnapshot::NoWorkspace),
            )
            .unwrap();
        let runtime = router
            .route(
                "desktop",
                &json!({
                    "type": "runtime_request",
                    "requestId": "request-1",
                    "idempotencyKey": "intent-1",
                    "target": {
                        "workspaceId": "workspace-a",
                        "sessionId": "session-a",
                        "instanceId": "instance-a"
                    },
                    "command": { "type": "prompt", "message": "secret" }
                }),
            )
            .unwrap();
        assert!(matches!(runtime, RoutedAction::Runtime { .. }));

        let host = router
            .route(
                "desktop",
                &json!({
                    "type": "host_request",
                    "requestId": "request-2",
                    "operation": "pick_folder"
                }),
            )
            .unwrap();
        assert!(matches!(host, RoutedAction::Host { .. }));
    }

    #[test]
    fn transition_generation_race_rejects_stale_runtime_request() {
        let mut router = HostRouter::new();
        let owner = crate::window_owner::OwnerId::from_string("owner".into());
        router.connect(
            "desktop",
            &json!({"type":"hello","protocolVersion":2,"clientType":"desktop","desktopCapability":"cap"}),
            HostClientContext::desktop("desktop", owner, OwnerWorkspaceSnapshot::Registered {
                wid: "workspace-a".into(), root: "/workspace-a".into(), generation: 7,
            }),
        ).unwrap();
        let err = router.route("desktop", &json!({
            "type":"runtime_request", "requestId":"stale", "idempotencyKey":"op-1",
            "target":{"workspaceId":"workspace-a","sessionId":"session-a","instanceId":"instance-a"},
            "workspaceGeneration": 6, "command":{"type":"prompt","message":"x"}
        })).unwrap();
        assert!(matches!(err, RoutedAction::Runtime { .. }));
        assert_eq!(
            router
                .client_context("desktop")
                .unwrap()
                .workspace_generation,
            Some(7)
        );
    }

    #[test]
    fn denies_host_operations_for_remote_clients() {
        let mut router = HostRouter::new();
        router
            .connect(
                "phone",
                &json!({ "type": "hello", "protocolVersion": 2, "clientType": "remote" }),
                HostClientContext::remote("phone"),
            )
            .unwrap();
        for operation in [
            "pick_folder",
            "open_app",
            "install_package",
            "check_for_updates",
            "delete_workspace",
        ] {
            let error = router
                .route(
                    "phone",
                    &json!({
                        "type": "host_request",
                        "requestId": "request",
                        "operation": operation,
                    }),
                )
                .unwrap_err();
            assert_eq!(error.code, "forbidden_class");
        }
    }
}
