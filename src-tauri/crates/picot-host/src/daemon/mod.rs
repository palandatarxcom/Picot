// ABOUTME: Daemon-side state ownership: DaemonState, the app-surface seam, and (T3+) admin/instance.
// ABOUTME: Step 0 of plan Task 2 lands only the AppSurface inventory and trait; DaemonState follows in Steps 2–3.

//! Phase 1 (2026-10-15) moves the whole host stack out of the Tauri main
//! process (plan D1/D2). This module owns what `setup_native_runtime`
//! (main.rs:2049–2264) built in the app process and what the control
//! dispatcher needs to reach.
//!
//! Landed so far (Task 2 Step 0):
//!
//! - [`app_surface`] — the exhaustive `AppHandle`/`try_state` inventory of the
//!   control dispatcher and the object-safe [`app_surface::AppSurface`] seam
//!   every row maps onto.
//!
//! Not landed yet (tracked by the plan, not placeholders):
//!
//! - `DaemonState` + `DaemonConfig` (Task 2 Steps 2–3): the Arc set that
//!   `setup_native_runtime` builds, the dispatcher extraction into
//!   `picot_host::host_control_dispatch`, and the app-surface field.
//! - `admin` (Task 3) and `instance` (Task 4).

pub mod app_surface;
