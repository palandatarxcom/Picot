// ABOUTME: Picot host stack shared by the Tauri app package and the picot-daemon binary.
// ABOUTME: Zero Tauri coupling at the Cargo level, enforced by scripts/check-daemon-no-tauri.js.

// This crate is a library only so the app and the daemon share one host stack;
// its `pub` surface is intra-project, not a published API. Widening the moved
// modules' visibility across the crate boundary is what makes clippy's public
// API style lints applicable, so they are allowed here rather than sprinkled
// over the moved code (which stays a pure move).
#![allow(
    clippy::should_implement_trait,
    clippy::new_without_default,
    clippy::len_without_is_empty
)]

// Host runtime modules are compiled into one native transport path.
// Retired compatibility handlers remain explicit and fail closed where needed.
//
// The `#[allow(dead_code)]` attributes mirror the `mod` declarations the Tauri
// binary carried before the 2026-10-15 workspace split, so the lint posture of
// these modules is unchanged by the move.
#[allow(dead_code)]
pub mod anydoc_preview;
pub mod cache_optimizer_config;
pub mod caveman_config;
pub mod child_supervision;
pub mod environment_install;
pub mod environment_probe;
pub mod environment_prompt;
pub mod fff_config;
pub mod goal_config;
pub mod host_capability;
#[allow(dead_code)]
pub mod host_config;
#[allow(dead_code)]
pub mod host_control;
pub mod host_data;
pub mod host_files;
pub mod host_router;
pub mod host_server;
pub mod mcp_login_runner;
pub mod metadata_store;
pub mod mutation_types;
pub mod native_pi_manager;
#[allow(dead_code)]
pub mod oauth_manager;
pub mod officecli_watch;
pub mod operation_registry;
pub mod package_manager;
pub mod paste_offload;
pub mod search_env_config;
// Public API staged for the broker (Task 5) and host lifecycle (Task 7a).
#[allow(dead_code)]
pub mod command_policy;
pub mod cost_compat;
#[allow(dead_code)]
pub mod ephemeral_registry;
pub mod git_pi_runner;
pub mod git_service;
pub mod host_credentials;
pub mod host_ephemeral;
pub mod host_models;
pub mod host_skills;
pub mod lens_config;
pub mod pi_launch;
pub mod pi_path;
pub mod pi_rpc_bridge;
pub mod ponytail_config;
#[allow(dead_code)]
pub mod process_tree;
pub mod project_trust;
pub mod remote_auth;
pub mod rpiv_config;
pub mod runtime;
pub mod runtime_coordinator;
pub mod session_ui_profile_store;
pub mod skill_install;
pub mod skill_source_registry;
pub mod subagents_inventory;
pub mod subagents_settings;
pub mod telemetry;
#[allow(dead_code)]
pub mod temp_resources;
pub mod terminal_manager;
pub mod terminal_output;
pub mod terminal_profiles;
pub mod terminal_registry;
pub mod terminal_state_store;
pub mod transport_limits;
pub mod vcc_config;
pub mod window_owner;
pub mod windows_child;
pub mod workspace_controls;

/// Build identity of the running host stack (`git describe --always --dirty`).
/// Emitted by `build.rs`; one value per cargo invocation for every crate member.
pub const BUILD_ID: &str = env!("PICOT_BUILD_ID");

/// Source-tree `src-tauri` directory, baked in by `build.rs`.
///
/// Dev-mode fallbacks (`resources/`, repo-relative `extensions/`) and test
/// fixtures were written against the app package's manifest dir; this is the
/// same path from the host crate's new location, so those relative sub-paths
/// stay unchanged.
pub const SRC_TAURI_DIR: &str = env!("PICOT_SRC_TAURI_DIR");
