// ABOUTME: The app-side pump for forwarded surface ops: `app_surface_request` frames decoded onto
// ABOUTME: the app's AppSurface implementation (plan Task 5, rows A2-A13 over the admin channel).

//! # Forwarded surface ops (plan D5, Task 5)
//!
//! The daemon's `app_surface_request` frames are decoded here and executed
//! against the app's [`AppSurface`] — the same seam the in-process dispatcher
//! used before the split, so nothing about a dialog, a window, an updater call
//! or a pane op changes with the process boundary.
//!
//! The op names and argument keys come from
//! [`picot_host::daemon::surface_forward::wire`], the very table the daemon
//! encodes with: one source of truth for both ends.
//!
//! The executor holds `Arc<dyn AppSurface>`, not `TauriAppSurface`, so the
//! decoder is unit-testable with a recording surface (no app process, no
//! `AppHandle`).

use std::sync::Arc;

use picot_host::daemon::app_surface::{AppSurface, PaneOp};
use picot_host::daemon::surface_forward::wire;
use picot_host::host_control::ProgressSink;
use picot_host::runtime_coordinator::RuntimeTarget;
use serde_json::Value;

use super::admin_client::SurfaceExecutor;

/// Decodes forwarded ops and runs them on the app surface.
pub struct ForwardedSurfaceOps {
    surface: Arc<dyn AppSurface>,
}

impl ForwardedSurfaceOps {
    pub fn new(surface: Arc<dyn AppSurface>) -> Self {
        Self { surface }
    }
}

fn arg(args: &Value, key: &str) -> Result<Value, String> {
    args.get(key)
        .cloned()
        .ok_or_else(|| format!("the forwarded op is missing its {key} argument"))
}

fn arg_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("the forwarded op requires a string {key} argument"))
}

fn arg_opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_owned)
}

#[async_trait::async_trait]
impl SurfaceExecutor for ForwardedSurfaceOps {
    async fn execute(
        &self,
        op: &str,
        args: Value,
        progress: ProgressSink,
    ) -> Result<Value, String> {
        match op {
            wire::OP_OPEN_WORKSPACE_WINDOW => {
                let target: RuntimeTarget = serde_json::from_value(arg(&args, wire::ARG_TARGET)?)
                    .map_err(|error| {
                    format!("the forwarded workspace target is malformed: {error}")
                })?;
                self.surface
                    .open_workspace_window(
                        &arg_str(&args, wire::ARG_HOST_ORIGIN)?,
                        &target,
                        &arg_str(&args, wire::ARG_LABEL)?,
                        &arg_str(&args, wire::ARG_OWNER)?,
                        &arg_str(&args, wire::ARG_CAPABILITY)?,
                    )
                    .await?;
                Ok(Value::Null)
            }
            wire::OP_OPEN_DEVTOOLS => {
                self.surface
                    .open_devtools(&arg_str(&args, wire::ARG_LABEL)?)
                    .await?;
                Ok(Value::Null)
            }
            wire::OP_CLOSE_WINDOW => {
                self.surface
                    .close_window(
                        &arg_str(&args, wire::ARG_LABEL)?,
                        &arg_str(&args, wire::ARG_REQUEST_ID)?,
                    )
                    .await?;
                Ok(Value::Null)
            }
            wire::OP_CANCEL_WINDOW_CLOSE => {
                self.surface
                    .cancel_window_close(
                        &arg_str(&args, wire::ARG_LABEL)?,
                        &arg_str(&args, wire::ARG_REQUEST_ID)?,
                    )
                    .await?;
                Ok(Value::Null)
            }
            wire::OP_MENU_STATE_CHANGED => {
                self.surface.menu_state_changed().await?;
                Ok(Value::Null)
            }
            wire::OP_RELAUNCH_APP => {
                self.surface.relaunch_app().await?;
                Ok(Value::Null)
            }
            wire::OP_PANE => {
                let pane_op: PaneOp = serde_json::from_value(arg(&args, wire::ARG_OP)?)
                    .map_err(|error| format!("the forwarded pane op is malformed: {error}"))?;
                self.surface.pane(pane_op).await
            }
            wire::OP_PICK_FOLDER => Ok(match self.surface.pick_folder().await? {
                Some(path) => Value::from(path),
                None => Value::Null,
            }),
            wire::OP_PICK_IMAGE_FILES => {
                self.surface
                    .pick_image_files(arg_opt_str(&args, wire::ARG_INITIAL_DIR))
                    .await
            }
            wire::OP_SAVE_SESSION_EXPORT => {
                self.surface
                    .save_session_export(
                        &arg_str(&args, wire::ARG_HOST_ORIGIN)?,
                        &arg_str(&args, wire::ARG_EXPORT_URL)?,
                        &arg_str(&args, wire::ARG_FILE_NAME)?,
                        arg_opt_str(&args, wire::ARG_INITIAL_DIR),
                    )
                    .await
            }
            wire::OP_CHECK_FOR_UPDATE => self.surface.check_for_update().await,
            wire::OP_DOWNLOAD_AND_INSTALL_UPDATE => {
                self.surface.download_and_install_update(progress).await
            }
            other => Err(format!("unknown forwarded app-surface op {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use picot_host::host_server::PaneRect;
    use serde_json::json;
    use std::sync::Mutex;

    /// Records what the decoder handed to the surface. Every method answers a
    /// recognizable value so a wrong decode cannot pass as the right one.
    #[derive(Default)]
    struct RecordingSurface {
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl RecordingSurface {
        fn record(&self, op: &str, detail: Value) {
            self.calls.lock().unwrap().push((op.to_string(), detail));
        }

        fn calls(&self) -> Vec<(String, Value)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl AppSurface for RecordingSurface {
        async fn open_workspace_window(
            &self,
            host_origin: &str,
            target: &RuntimeTarget,
            label: &str,
            owner: &str,
            capability: &str,
        ) -> Result<(), String> {
            self.record(
                "open_workspace_window",
                json!({
                    "hostOrigin": host_origin,
                    "target": target,
                    "label": label,
                    "owner": owner,
                    "capability": capability,
                }),
            );
            Ok(())
        }

        async fn open_devtools(&self, label: &str) -> Result<(), String> {
            self.record("open_devtools", json!(label));
            Ok(())
        }

        async fn close_window(&self, label: &str, request_id: &str) -> Result<(), String> {
            self.record("close_window", json!([label, request_id]));
            Ok(())
        }

        async fn cancel_window_close(&self, label: &str, request_id: &str) -> Result<(), String> {
            self.record("cancel_window_close", json!([label, request_id]));
            Ok(())
        }

        async fn menu_state_changed(&self) -> Result<(), String> {
            self.record("menu_state_changed", Value::Null);
            Ok(())
        }

        async fn relaunch_app(&self) -> Result<(), String> {
            self.record("relaunch_app", Value::Null);
            Ok(())
        }

        async fn pane(&self, op: PaneOp) -> Result<Value, String> {
            self.record("pane", serde_json::to_value(&op).unwrap());
            Ok(Value::from("pane-answer"))
        }

        async fn pick_folder(&self) -> Result<Option<String>, String> {
            self.record("pick_folder", Value::Null);
            Ok(Some("/tmp/picked".to_string()))
        }

        async fn pick_image_files(&self, initial_dir: Option<String>) -> Result<Value, String> {
            self.record("pick_image_files", json!(initial_dir));
            Ok(json!([{"name": "a.png"}]))
        }

        async fn save_session_export(
            &self,
            host_origin: &str,
            export_url: &str,
            file_name: &str,
            initial_dir: Option<String>,
        ) -> Result<Value, String> {
            self.record(
                "save_session_export",
                json!([host_origin, export_url, file_name, initial_dir]),
            );
            Ok(Value::from("/tmp/session.md"))
        }

        async fn check_for_update(&self) -> Result<Value, String> {
            self.record("check_for_update", Value::Null);
            Ok(json!({"available": true}))
        }

        async fn download_and_install_update(
            &self,
            progress: ProgressSink,
        ) -> Result<Value, String> {
            progress(json!({"phase": "download"}));
            self.record("download_and_install_update", Value::Null);
            Ok(json!({"installed": true}))
        }
    }

    fn no_progress() -> ProgressSink {
        Arc::new(|_event: Value| {})
    }

    async fn run(surface: Arc<RecordingSurface>, op: &str, args: Value) -> Result<Value, String> {
        ForwardedSurfaceOps::new(surface)
            .execute(op, args, no_progress())
            .await
    }

    #[tokio::test]
    async fn every_forwarded_row_decodes_onto_the_app_surface() {
        let surface = Arc::new(RecordingSurface::default());

        run(
            Arc::clone(&surface),
            wire::OP_OPEN_WORKSPACE_WINDOW,
            json!({
                "hostOrigin": "http://127.0.0.1:51000",
                "target": {
                    "workspaceId": "w1",
                    "sessionId": "s1",
                    "instanceId": "i1",
                    "ownerId": "o1",
                    "workspaceGeneration": 4,
                },
                "label": "native-workspace-w1",
                "owner": "o1",
                "capability": "cap-1",
            }),
        )
        .await
        .expect("open_workspace_window");

        run(
            Arc::clone(&surface),
            wire::OP_OPEN_DEVTOOLS,
            json!({"label": "native-landing"}),
        )
        .await
        .expect("open_devtools");
        run(
            Arc::clone(&surface),
            wire::OP_CLOSE_WINDOW,
            json!({"label": "native-landing", "requestId": "close-1"}),
        )
        .await
        .expect("close_window");
        run(
            Arc::clone(&surface),
            wire::OP_CANCEL_WINDOW_CLOSE,
            json!({"label": "native-landing", "requestId": "close-1"}),
        )
        .await
        .expect("cancel_window_close");
        run(Arc::clone(&surface), wire::OP_MENU_STATE_CHANGED, json!({}))
            .await
            .expect("menu_state_changed");
        run(Arc::clone(&surface), wire::OP_RELAUNCH_APP, json!({}))
            .await
            .expect("relaunch_app");

        let pane = run(
            Arc::clone(&surface),
            wire::OP_PANE,
            json!({"op": {"kind": "create", "paneId": "p1", "windowLabel": "native-landing", "url": "https://example.com", "rect": {"x": 1.0, "y": 2.0, "width": 3.0, "height": 4.0}}}),
        )
        .await
        .expect("pane");
        assert_eq!(pane, Value::from("pane-answer"));

        let picked = run(Arc::clone(&surface), wire::OP_PICK_FOLDER, json!({}))
            .await
            .expect("pick_folder");
        assert_eq!(picked, Value::from("/tmp/picked"));
        run(
            Arc::clone(&surface),
            wire::OP_PICK_IMAGE_FILES,
            json!({"initialDir": "/tmp/images"}),
        )
        .await
        .expect("pick_image_files");
        run(
            Arc::clone(&surface),
            wire::OP_SAVE_SESSION_EXPORT,
            json!({
                "hostOrigin": "http://127.0.0.1:51000",
                "exportUrl": "/v2/session-export/0123456789abcdef0123456789abcdef",
                "fileName": "session.md",
                "initialDir": null,
            }),
        )
        .await
        .expect("save_session_export");
        run(Arc::clone(&surface), wire::OP_CHECK_FOR_UPDATE, json!({}))
            .await
            .expect("check_for_update");
        run(
            Arc::clone(&surface),
            wire::OP_DOWNLOAD_AND_INSTALL_UPDATE,
            json!({}),
        )
        .await
        .expect("download_and_install_update");

        let calls = surface.calls();
        assert_eq!(calls.len(), 12, "one call per forwarded row: {calls:?}");
        assert_eq!(calls[0].0, "open_workspace_window");
        assert_eq!(
            calls[0].1,
            json!({
                "hostOrigin": "http://127.0.0.1:51000",
                "target": {
                    "workspaceId": "w1",
                    "sessionId": "s1",
                    "instanceId": "i1",
                    "ownerId": "o1",
                    "workspaceGeneration": 4,
                },
                "label": "native-workspace-w1",
                "owner": "o1",
                "capability": "cap-1",
            })
        );
        assert_eq!(calls[2].1, json!(["native-landing", "close-1"]));
        assert_eq!(
            calls[6].1,
            json!({
                "kind": "create",
                "paneId": "p1",
                "windowLabel": "native-landing",
                "url": "https://example.com",
                "rect": {"x": 1.0, "y": 2.0, "width": 3.0, "height": 4.0},
            }),
            "the pane op survives the round trip with its exact shape"
        );
        assert_eq!(calls[7].0, "pick_folder");
        assert_eq!(calls[8].1, json!("/tmp/images"));
        assert_eq!(
            calls[9].1,
            json!([
                "http://127.0.0.1:51000",
                "/v2/session-export/0123456789abcdef0123456789abcdef",
                "session.md",
                null
            ])
        );
        assert_eq!(calls[11].0, "download_and_install_update");
    }

    #[tokio::test]
    async fn progress_frames_reach_the_sink_the_pump_handed_over() {
        // The pump's sink is what turns an updater callback into a
        // `surface_progress` frame; here it is a recording closure.
        let surface = Arc::new(RecordingSurface::default());
        let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
        let sink: ProgressSink = {
            let seen = Arc::clone(&seen);
            Arc::new(move |event: Value| seen.lock().unwrap().push(event))
        };
        ForwardedSurfaceOps::new(surface)
            .execute(wire::OP_DOWNLOAD_AND_INSTALL_UPDATE, json!({}), sink)
            .await
            .expect("the streaming op completes");
        assert_eq!(
            *seen.lock().unwrap(),
            vec![json!({"phase": "download"})],
            "the updater's progress must reach the pump's sink"
        );
    }

    #[tokio::test]
    async fn a_missing_or_unknown_argument_never_reaches_the_app_surface() {
        let surface = Arc::new(RecordingSurface::default());
        let unknown = run(Arc::clone(&surface), "not_an_op", json!({}))
            .await
            .expect_err("an unknown op is refused");
        assert!(unknown.contains("not_an_op"), "{unknown}");

        let missing = run(
            Arc::clone(&surface),
            wire::OP_CLOSE_WINDOW,
            json!({"label": "x"}),
        )
        .await
        .expect_err("a missing requestId is refused");
        assert!(missing.contains("requestId"), "{missing}");

        let malformed = run(
            Arc::clone(&surface),
            wire::OP_PANE,
            json!({"op": {"kind": "create"}}),
        )
        .await
        .expect_err("a malformed pane op is refused");
        assert!(malformed.contains("pane op"), "{malformed}");

        assert!(
            surface.calls().is_empty(),
            "a refused decode must not touch the app: {:?}",
            surface.calls()
        );
    }

    #[tokio::test]
    async fn a_cancelled_picker_answers_null() {
        struct NoPick;
        #[async_trait::async_trait]
        impl AppSurface for NoPick {
            async fn open_workspace_window(
                &self,
                _host_origin: &str,
                _target: &RuntimeTarget,
                _label: &str,
                _owner: &str,
                _capability: &str,
            ) -> Result<(), String> {
                Err("unused".into())
            }
            async fn open_devtools(&self, _label: &str) -> Result<(), String> {
                Err("unused".into())
            }
            async fn close_window(&self, _label: &str, _request_id: &str) -> Result<(), String> {
                Err("unused".into())
            }
            async fn cancel_window_close(
                &self,
                _label: &str,
                _request_id: &str,
            ) -> Result<(), String> {
                Err("unused".into())
            }
            async fn menu_state_changed(&self) -> Result<(), String> {
                Err("unused".into())
            }
            async fn relaunch_app(&self) -> Result<(), String> {
                Err("unused".into())
            }
            async fn pane(&self, _op: PaneOp) -> Result<Value, String> {
                Err("unused".into())
            }
            async fn pick_folder(&self) -> Result<Option<String>, String> {
                Ok(None)
            }
            async fn pick_image_files(
                &self,
                _initial_dir: Option<String>,
            ) -> Result<Value, String> {
                Err("unused".into())
            }
            async fn save_session_export(
                &self,
                _host_origin: &str,
                _export_url: &str,
                _file_name: &str,
                _initial_dir: Option<String>,
            ) -> Result<Value, String> {
                Err("unused".into())
            }
            async fn check_for_update(&self) -> Result<Value, String> {
                Err("unused".into())
            }
            async fn download_and_install_update(
                &self,
                _progress: ProgressSink,
            ) -> Result<Value, String> {
                Err("unused".into())
            }
        }
        let ops = ForwardedSurfaceOps::new(Arc::new(NoPick));
        let cancelled = ops
            .execute(wire::OP_PICK_FOLDER, json!({}), no_progress())
            .await
            .expect("a cancelled picker is not an error");
        assert_eq!(cancelled, Value::Null);
    }

    #[test]
    fn the_pane_wire_shape_round_trips_every_variant() {
        let variants = vec![
            PaneOp::Create {
                pane_id: "p".into(),
                window_label: "w".into(),
                url: "https://example.com".into(),
                rect: PaneRect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 20.0,
                },
            },
            PaneOp::SetRect {
                pane_id: "p".into(),
                rect: PaneRect {
                    x: 1.0,
                    y: 2.0,
                    width: 3.0,
                    height: 4.0,
                },
            },
            PaneOp::SetVisible {
                pane_id: "p".into(),
                visible: true,
            },
            PaneOp::EvalJson {
                pane_id: "p".into(),
                expression: "1+1".into(),
            },
            PaneOp::Navigate {
                pane_id: "p".into(),
                url: "https://example.com".into(),
            },
            PaneOp::Url {
                pane_id: "p".into(),
            },
            PaneOp::Destroy {
                pane_id: "p".into(),
            },
        ];
        for op in variants {
            let wire = serde_json::to_value(&op).expect("serializes");
            let decoded: PaneOp = serde_json::from_value(wire.clone()).expect("deserializes");
            assert_eq!(decoded, op, "round trip changed the op: {wire}");
        }
    }
}
