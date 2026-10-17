// ABOUTME: The app-surface seam: one method per AppHandle dependency of the control dispatcher.
// ABOUTME: Step-0 artefact of plan Task 2 — the exhaustive B3 inventory lives in the module doc.

//! # App-surface inventory — plan Task 2 Step 0 (the B3 gate)
//!
//! `install_control_handler` in `src-tauri/src/main.rs` (lines **2881–4496** on
//! branch `daemon-phase1`, commit `997ba5a`) is the only place in the host stack
//! that touched the Tauri `AppHandle`. This table is the exhaustive list of
//! those touch points and is **authoritative** for this trait (plan D5): the
//! trait grows only from a row below, and a `DaemonState` field is added only
//! for a row classified *daemon-side*.
//!
//! Note on line anchors: the plan's Verified Context numbers predate the T1
//! workspace split (they were ~30 lines higher). The numbers here are re-anchored
//! to this branch and are the ones a reviewer should open.
//!
//! ## Reproduction
//!
//! ```text
//! awk 'NR>=2881 && NR<=4496' src-tauri/src/main.rs | grep -nE '\bapp\b|try_state|tauri[:]{2}'
//! ```
//!
//! (The framework path is spelled `tauri[:]{2}` on purpose: a literal one
//! would trip this crate's own guard, `scripts/check-daemon-no-tauri.js`, even
//! inside a doc comment.)
//!
//! 33 matching lines. Two are false positives — `registry_state` contains the
//! substring `try_state` (main.rs:3498, 3527) and neither line touches the app.
//! The remaining 31 lines cover three `tauri` async-runtime calls
//! (`3169`, `3442`, `3514`, classified *runtime swap* below), the `AppHandle`
//! parameter itself (`2891`) and the closure capture (`2947`), five multi-line
//! `try_state` chains, and the 21 logical touch-point sites tabulated here.
//!
//! ## Legend
//!
//! - **forwarded** — the daemon cannot do this: it needs the app process
//!   (windows, native dialogs, updater, devtools, relaunch, menus, child
//!   webviews). Becomes an [`AppSurface`] method, executed app-side; the
//!   daemon's realization forwards over the admin channel (plan D5, T3/T5).
//! - **daemon-side** — the object already lives in the host stack; the
//!   `app.try_state::<T>()` lookup is replaced by an explicit `DaemonState`
//!   field, so a daemon-only process has it without any Tauri state container.
//! - **runtime swap** — neither app state nor an app-surface op: a call into
//!   the Tauri async runtime that the move replaces with plain `tokio`
//!   (`tokio::task::spawn_blocking`), because the daemon owns its own runtime.
//!
//! ## Inventory
//!
//! | # | site | today's code | class | target |
//! | --- | --- | --- | --- | --- |
//! | A1 | 2901 | `app.manage(EnvironmentInstaller(installer.clone()))` — registers the resolver built at 2896–2900 from `pi_launch::resolve_bundled_pi` | daemon-side | `DaemonState.environment_installer: Arc<environment_install::Installer>` (the dispatcher already uses the captured `installer` Arc at 2949/3176/3188/3193/3195/3197 — only the *managed-state* registration is app-specific; its sole reader is out-of-dispatcher, see below) |
//! | A2 | 3113–3117 | `open_native_workspace_window(&app, &host_origin, &target, owner_registry.clone(), owner, &capability)` | forwarded | [`AppSurface::open_workspace_window`] (host_origin, target, owner, capability) |
//! | A3 | 3336, 3353 | `pick_folder_core(&app)` — 3336 is skill-source add, 3353 the plain `pick_folder` op; both are the same dialog | forwarded | [`AppSurface::pick_folder`] |
//! | A4 | 3359 | `pick_image_files_core(&app, initial_dir)` | forwarded | [`AppSurface::pick_image_files`] |
//! | A5 | 3367–3374 | `save_session_export_core(&app, &host_origin, &export_url, &file_name, arg_str("initialDir"))` | forwarded | [`AppSurface::save_session_export`] — **the `initial_dir` argument is preserved** (the plan's sketch omits it; dropping it would regress the save dialog's start directory) |
//! | A6 | 3403–3406 | `app.get_webview_window(&label).open_devtools()` | forwarded | [`AppSurface::open_devtools`] (label) |
//! | A7 | 4034 | `check_for_update_core(&app)` | forwarded | [`AppSurface::check_for_update`] |
//! | A8 | 4036 | `download_and_install_update_core(&app, progress)` | forwarded | [`AppSurface::download_and_install_update`] (progress) |
//! | A9 | 4396 | `update_new_session_menu_state(&app)` — landing binding flipped during a transition commit | forwarded | [`AppSurface::menu_state_changed`] |
//! | A10 | 4477–4479 | `window_close_approve` → `app.get_webview_window(&label).close()` | forwarded | [`AppSurface::close_window`] (label) |
//! | A11 | 4447–4460 | `window_close_cancel` — **no `app.`/`try_state` line today**: it mutates the in-process `close_approvals()` static (main.rs:4550) shared with the app-side close delegate | forwarded | [`AppSurface::cancel_window_close`] (label, request_id) — under D4 close approvals live app-side, so the daemon-side cancel must clear the app's pending close; the pending map itself is never daemon state |
//! | A12 | 4489 | `app.restart()` | forwarded | [`AppSurface::relaunch_app`] |
//! | A13 | — | pane ops (`browser_pane_create/set_rect/set_visible/eval/navigate/url/destroy`) — **zero `app.`/`try_state` lines in the dispatcher**: they are served on the data plane by forwarding (host_server.rs:64, 3589+) | forwarded | [`AppSurface::pane`] — resolved by review B1: the daemon installs a `host_server::PaneForwarder` beside its control handler and forwards every op here, where the app executes it against its own `BrowserPaneRuntime`; the in-process `PaneSurface` slot was deleted (follow-up #6: production never installed it), so forwarding is the only pane path |
//! | D1 | 3340–3341, 3423–3424, 3495–3496, 4397 | `app.try_state::<SkillSourceRegistryState>()` (skill-source issue / scan / install / `revoke_workspace`) | daemon-side | `DaemonState.skill_sources: Arc<SkillSourceRegistry>` |
//! | D2 | 3436–3437, 3508–3509 | `app.try_state::<HostInstallSecret>()` | daemon-side | `DaemonState.install_secret: Arc<HostInstallSecret>` (the type moves into `picot-host` with the dispatcher; the app keeps no copy) |
//! | D3 | 4369–4371 | `app.try_state::<host_server::HostServer>()` → `revoke_session_exports(owner)` | daemon-side | `DaemonState.host: Arc<HostServer>` — `host` is already a local in the construction path, so the lookup disappears |
//! | D4 | 4400–4402, 4442–4444 | `app.try_state::<Arc<GitService>>()` → `clear_workspace_state(owner)` (transition commit and cancel) | daemon-side | `DaemonState.git: Arc<GitService>` |
//! | T1 | 3169, 3442, 3514 | Tauri async-runtime `spawn_blocking` (environment probe, skill scan, skill install) | runtime swap | `tokio::task::spawn_blocking` — not an `AppSurface` method |
//!
//! Plan-expected sites all resolve: window open (A2), dialogs (A3/A4/A5),
//! updater (A7/A8), export revocation (D3), menu recompute + skill/git
//! (A9/D1/D4), git clear on cancel (D4), window close (A10/A11), devtools (A6),
//! relaunch (A12), and "the install-block state lookup" (A1 — the plan expected
//! a `try_state`-style lookup inside the dispatcher; in this revision the
//! installer is a captured `Arc` and the only managed-state reader is outside
//! the dispatcher, see the boundary list below).
//!
//! ## App-side readers outside the dispatcher (NOT rows of this inventory)
//!
//! Listed so the Step-2/Step-3 construction move does not silently strand them;
//! each one reads state that becomes `DaemonState`-owned, or is called by a row
//! above:
//!
//! - `main.rs:2102` `close_approvals()` / `:2133` `handle_close_requested` /
//!   `:2297` `handle_window_destroyed` — the app-side close-pending map and the
//!   teardown path that today shares the dispatcher's static (rows A10/A11).
//!   Re-anchored again by R5: `handle_close_requested` no longer reaches for an
//!   in-process host (there is none) — it asks the daemon with
//!   `owner.close_request`, and the daemon pushes `window_close_request` into
//!   that owner's WebView on the data plane. The pending map and the native
//!   fallback beside it stay app-side.
//! - `main.rs:4714` `focused_owner_has_workspace` and `:4755`
//!   `update_new_session_menu_state` — read `OwnerRegistryState` +
//!   `NativePiManagerState` from the app handle; called by row A9.
//! - `main.rs:4979–4994` `RunEvent::Exit` — reads `EnvironmentInstaller`
//!   (A1), `NativePiManagerState`, `TerminalManagerState`,
//!   `OfficecliWatchState` and calls `child_supervision::clear_registry()`;
//!   `main.rs:5032/5035` repeats the manager teardown + registry clear on the
//!   signal path. These become daemon shutdown (plan D12/T4), not surface ops.
//! - `main.rs:4901` and `:4719` — window-focus hooks reading
//!   `OwnerRegistryState` before calling `update_new_session_menu_state`.
//!
//! ## Step-2 trait corrections (signature-level; recorded, not silently absorbed)
//!
//! Two Step-0 signatures could not carry today's behavior, so Step 2 sharpened
//! them instead of writing a silent regression. No row was added or removed.
//!
//! 1. **A10 `close_window(label, request_id)`** (Step 0: `close_window(label)`).
//!    The pending-close map is app state under D4, so the approve settlement —
//!    including the `no pending window close` and `request id mismatch`
//!    results the control response already returns — has to happen app-side.
//!    Without the request id the app-side impl could not tell an approval from
//!    a replay, which is exactly what those results exist for.
//! 2. **A4 `pick_image_files(...) -> Result<Value, String>`** (Step 0:
//!    `Option<Vec<String>>`). The picker returns image objects carrying the
//!    base64 bytes (`PickedImageFile { name, mimeType, data }`, main.rs:281);
//!    a `Vec<String>` would have dropped the payload. The JSON pass-through
//!    shape matches row A5's `save_session_export`.
//!
//! A third finding, not a signature change: row T1's grep was scoped to the
//! dispatcher function, so it missed the three framework async-runtime
//! `spawn_blocking` calls inside `dispatch_git_host_operation` (pre-move
//! main.rs:2528, 2581, 2639) — same op, same replacement, six sites in total
//! now on plain `tokio`.
//!
//! ## Deviations from the plan text (recorded, not silently absorbed)
//!
//! 1. `save_session_export` keeps `initial_dir` (see A5) — the sketch's three
//!    arguments would drop a live argument at the call site.
//! 2. `open_workspace_window`'s `target` parameter is the existing
//!    [`crate::runtime_coordinator::RuntimeTarget`], not a new
//!    `WorkspaceWindowTarget` type: the call site already passes that type and
//!    it is `Serialize + Deserialize`, so no new wire type is invented here.
//! 3. A11 has no `AppHandle` site today; it is an inventory row because D4
//!    moves the close-pending map app-side, which makes a daemon-side cancel
//!    observable to the app for the first time.
//! 4. A13 likewise has no dispatcher site: browser panes were already a trait
//!    slot after T1, so the pane row exists only to keep the plan sketch's
//!    forwarding point.
//!
//! Everything above arrives as a *skeleton* in Task 2 Step 0. The WS-forwarding
//! realization arrives with the admin channel (T3/T5, `DaemonState`'s surface)
//! and the app-side realization (T2 Step 2, `tauri_app_surface.rs`) — both
//! consume this trait, neither is defined here.

use crate::host_control::ProgressSink;
use crate::host_server::PaneRect;
use crate::runtime_coordinator::RuntimeTarget;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Updater progress pushed back to the requester of
/// [`AppSurface::download_and_install_update`].
///
/// Same shape as [`crate::host_control::ProgressSink`] on purpose: the
/// dispatcher already holds a `ProgressSink` for this call, so the alias keeps
/// Step 2 a mechanical substitution. Named separately because the fan-in
/// differs per mode — the app-side impl calls it straight from the updater
/// callback, the forwarding impl feeds it from `surface_progress` push frames.
pub type SurfaceProgress = ProgressSink;

/// One browser-pane operation, mirroring the seven `browser_pane_*` data-plane
/// ops one-for-one (`host_server.rs:3589+`).
///
/// Wire encoding (Task 5's `pane` op): a tagged value (`kind`) with camelCase
/// field names, matching the rest of the admin protocol. The tag values are the
/// data-plane op names minus their `browser_pane_` prefix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneOp {
    Create {
        #[serde(rename = "paneId")]
        pane_id: String,
        #[serde(rename = "windowLabel")]
        window_label: String,
        url: String,
        rect: PaneRect,
    },
    SetRect {
        #[serde(rename = "paneId")]
        pane_id: String,
        rect: PaneRect,
    },
    SetVisible {
        #[serde(rename = "paneId")]
        pane_id: String,
        visible: bool,
    },
    EvalJson {
        #[serde(rename = "paneId")]
        pane_id: String,
        expression: String,
    },
    Navigate {
        #[serde(rename = "paneId")]
        pane_id: String,
        url: String,
    },
    Url {
        #[serde(rename = "paneId")]
        pane_id: String,
    },
    Destroy {
        #[serde(rename = "paneId")]
        pane_id: String,
    },
}

/// The app surface a `DaemonState` needs to be a full control handler.
///
/// Object-safe by construction (`Arc<dyn AppSurface>` is the field type in
/// `DaemonState`): every method is `async` behind the `async_trait` macro, and
/// no method is generic. Implementations must be `Send + Sync` — the daemon
/// calls them from its own tasks and the app calls them from the Tauri threads.
///
/// The inventory at the top of this file is the contract: an implementation
/// that needs an operation not listed there means the inventory is wrong, and
/// the inventory is fixed first (plan D5/B3).
#[async_trait::async_trait]
pub trait AppSurface: Send + Sync {
    /// Row A2 — open the workspace window for a committed workspace/session
    /// target at `host_origin`, injecting `capability` as the owner's init
    /// script.
    ///
    /// `label` is the daemon-allocated window label (multi-window §6.1): the
    /// daemon is the single allocation authority for workspace labels and
    /// reserved it at the owner mint; the app threads it through verbatim
    /// (window, ledger, mirror) and never recomputes it.
    async fn open_workspace_window(
        &self,
        host_origin: &str,
        target: &RuntimeTarget,
        label: &str,
        owner: &str,
        capability: &str,
    ) -> Result<(), String>;

    /// Row A6 — `open_devtools` op for the owner's window label.
    async fn open_devtools(&self, label: &str) -> Result<(), String>;

    /// Multi-window §6.3 — focus the window of the live holder that blocked a
    /// session reopen.
    ///
    /// `label` is the holder's window label, resolved daemon-side from the
    /// owner registry (the same label map the close flow mirrors — no new
    /// channel). Mirrors Row A2's error contract: a failure travels to the
    /// caller, which re-checks the holder — it never falls back to opening a
    /// window past a live holder.
    async fn focus_window(&self, label: &str) -> Result<(), String>;

    /// Row A10 — settle the pending close that `window_close_approve`
    /// approves (the pending map lives app-side, D4) and close the window.
    ///
    /// `request_id` is part of the settlement, not decoration: the approve
    /// path validates it against the pending request (`no pending window
    /// close` / `request id mismatch`) exactly as the in-process dispatcher
    /// did before the split. Row A11 carries the same pair for the cancel
    /// direction.
    async fn close_window(&self, label: &str, request_id: &str) -> Result<(), String>;

    /// Row A11 — clear the app's pending close for `label`/`request_id` when a
    /// `window_close_cancel` arrives.
    async fn cancel_window_close(&self, label: &str, request_id: &str) -> Result<(), String>;

    /// Row A9 — recompute the New Session menu state after a workspace
    /// transition changed an owner's binding.
    async fn menu_state_changed(&self) -> Result<(), String>;

    /// Row A12 — relaunch the app process.
    async fn relaunch_app(&self) -> Result<(), String>;

    /// Row A13 — run one child-webview operation (see [`PaneOp`]).
    async fn pane(&self, op: PaneOp) -> Result<Value, String>;

    /// Row A3 — native folder picker; `Ok(None)` on cancel.
    async fn pick_folder(&self) -> Result<Option<String>, String>;

    /// Row A4 — native image picker; `Ok(Null)` on cancel, otherwise the
    /// selected image objects (`name` / `mimeType` / base64 `data`).
    ///
    /// The payload is opaque JSON on purpose: the picker reads and
    /// base64-encodes the files app-side, and the dispatcher forwards the
    /// array to the client unchanged. (Step 0 sketched `Option<Vec<String>>`,
    /// which cannot carry the encoded bytes — the images would be silently
    /// dropped, so Step 2 corrected the type to the pass-through shape row A5
    /// already uses.)
    async fn pick_image_files(&self, initial_dir: Option<String>) -> Result<Value, String>;

    /// Row A5 — fetch the granted export over `host_origin`, then show the
    /// native save dialog. `Ok(Null)` when the user cancels, otherwise the
    /// written path.
    async fn save_session_export(
        &self,
        host_origin: &str,
        export_url: &str,
        file_name: &str,
        initial_dir: Option<String>,
    ) -> Result<Value, String>;

    /// Row A7 — updater check; `Ok(Null)` when up to date.
    async fn check_for_update(&self) -> Result<Value, String>;

    /// Row A8 — download + install, streaming progress through `progress`.
    async fn download_and_install_update(&self, progress: SurfaceProgress)
        -> Result<Value, String>;
}
