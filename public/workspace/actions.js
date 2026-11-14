// ABOUTME: Coordinates workspace/session navigation and native transition permits.
// ABOUTME: Keeps same-window activation separate from cross-workspace navigation.

import { t } from "../i18n.js";

// Multi-task model
// ──────────────────
// A `pi --mode rpc` process can only drive ONE active session at a time.
// `new_session` / `switch_session` / fork inside an existing process just
// *replace* the active session — the previous session's .jsonl stays on
// disk and can be reloaded later, but it stops being the live, running
// session in that process. So any concurrently-running session structurally
// needs its own pi process.
//
// "Start a new session" entry points (header "+ New Session" and sidebar
// project tile "start new chat") both use the same pattern: spawn a fresh
// HEADLESS pi for the target cwd, then hand the prepared target to the
// caller's tail. The default tail navigates THIS window's WebView (full
// reload); callers that pass `adoptPrepared` adopt the runtime in place
// instead — no page reload, no interruption of a previously-running session.
//
// "Open project" / "Open folder" entry points still attach to an existing
// pi instance for the same cwd when one exists — those actions are about
// *finding* the project, not starting a new task.
//
// Swap overlay
// ──────────────
// All entry points that end in `navigate(url)` (a full-page WebView
// reload) optionally take an `onBeforeSwap` callback. The host (app.js)
// uses it to raise a full-screen overlay so the user sees a continuous
// spinner instead of a 1–2 second freeze (while pi spawns) followed by a
// white flash (while the WebView reloads). The overlay is persisted
// across the navigation boundary via sessionStorage; the new page boots
// straight into it (see index.html bootstrap script).

function runOnBeforeSwap(onBeforeSwap, label) {
  if (typeof onBeforeSwap !== "function") return () => {};
  try {
    return onBeforeSwap(label) || (() => {});
  } catch {
    return () => {};
  }
}

// "Attach to workspace" flow used by Open Project / Open Folder. Reuses an
// existing pi instance for the same cwd when present, otherwise spawns a
// windowless pi and navigates the *current* window to it.
async function attachToWorkspace({
  targetCwd,
  transport,
  navigate,
  adoptPrepared,
  onBeforeSwap,
  beforeWorkspaceCommit,
  beforeWorkspaceTransition,
  onWorkspaceTransitionCancelled,
  renderError,
}) {
  let prepared = null;
  let dismissOverlay = () => {};
  try {
    if (typeof transport.prepareWorkspaceTarget === "function") {
      // The overlay goes up BEFORE preparation, not after. Preparation spawns
      // the Pi child and waits for its first `get_state` reply, which is the
      // slowest step in this flow; raising the overlay afterwards would leave
      // that entire wait with no on-screen feedback.
      dismissOverlay = runOnBeforeSwap(onBeforeSwap, "Opening workspace…");
      prepared = await transport.prepareWorkspaceTarget(targetCwd, {
        reuseExisting: true,
      });
      const crossWorkspace = prepared?.classification === "cross";
      if (crossWorkspace && typeof beforeWorkspaceTransition === "function") {
        const settled = await beforeWorkspaceTransition(prepared);
        if (!settled) {
          dismissOverlay();
          onWorkspaceTransitionCancelled?.();
          await transport.cancelWorkspaceTransition(prepared.transitionGeneration).catch(() => {});
          return null;
        }
      }
      if (typeof prepared?.transitionGeneration !== "number") {
        throw new Error("Workspace transition was not prepared");
      }
      // The caller invalidates anything bound to the workspace being left
      // *before* the host commits (plan 2026-11-03 §4): the freeze alone still
      // allows the controlled checkpoint, not a lasting binding.
      beforeWorkspaceCommit?.();
      const committed = await transport.commitWorkspaceTransition(prepared.transitionGeneration);
      if (typeof adoptPrepared === "function") {
        await adoptPrepared(prepared, {
          targetCwd,
          workspaceGeneration: committed?.workspaceGeneration,
        });
      } else {
        navigate(prepared.targetOrigin, { targetCwd });
      }
      return { samePort: false, targetOrigin: prepared.targetOrigin };
    }

    throw new Error("Native workspace transition is unavailable");
  } catch (e) {
    dismissOverlay();
    if (prepared?.transitionGeneration != null) {
      onWorkspaceTransitionCancelled?.();
      await transport.cancelWorkspaceTransition(prepared.transitionGeneration).catch(() => {});
    }
    if (renderError) renderError(`${t("errors.attachWorkspaceFailed")}: ${String(e)}`);
    return null;
  }
}

// Registering a workspace must end in a fresh primary session owned by this
// window. The same primitive is also used by the header "+ New Session" path;
// it spawns a fresh headless pi and navigates THIS window to it.
export async function startRegisteredWorkspaceSession({
  targetCwd,
  transport,
  navigate,
  adoptPrepared,
  onBeforeSwap,
  beforeWorkspaceCommit,
  beforeWorkspaceTransition,
  onWorkspaceTransitionCancelled,
  renderError,
}) {
  if (!targetCwd) {
    renderError(t("errors.newSessionPathUnavailable"));
    return false;
  }
  if (typeof navigate !== "function") {
    renderError(t("errors.newSessionNavUnavailable"));
    return false;
  }
  return spawnFreshSession({
    targetCwd,
    transport,
    navigate,
    adoptPrepared,
    onBeforeSwap,
    beforeWorkspaceCommit,
    beforeWorkspaceTransition,
    onWorkspaceTransitionCancelled,
    renderError,
    label: t("sidebar.startingSession"),
    debugTag: "workspaceRegister",
  });
}

export async function startInWindowNewSession({
  transport,
  getCurrentCwd,
  navigate,
  adoptPrepared,
  onBeforeSwap,
  beforeWorkspaceCommit,
  beforeWorkspaceTransition,
  onWorkspaceTransitionCancelled,
  renderError,
}) {
  if (!transport) {
    renderError(t("errors.newSessionOnlyNative"));
    return false;
  }

  let targetCwd = null;
  if (typeof getCurrentCwd === "function") {
    try {
      targetCwd = getCurrentCwd();
    } catch {
      targetCwd = null;
    }
  }

  if (!targetCwd) {
    renderError(t("errors.newSessionPathUnavailable"));
    return false;
  }

  if (typeof navigate !== "function") {
    renderError(t("errors.newSessionNavUnavailable"));
    return false;
  }

  console.debug("[Session route] newSession:decision", {
    targetCwd,
    mode: "host-runtime-spawn",
  });
  return spawnFreshSession({
    targetCwd,
    transport,
    navigate,
    adoptPrepared,
    onBeforeSwap,
    beforeWorkspaceCommit,
    beforeWorkspaceTransition,
    onWorkspaceTransitionCancelled,
    renderError,
    label: t("sidebar.startingSession"),
    debugTag: "newSession",
  });
}

// Spawn a brand-new headless pi for `targetCwd` and navigate the current
// window to it. Shared by workspace registration and the other new-session
// entry points.
async function spawnFreshSession({
  targetCwd,
  transport,
  navigate,
  adoptPrepared,
  onBeforeSwap,
  beforeWorkspaceCommit,
  beforeWorkspaceTransition,
  onWorkspaceTransitionCancelled,
  renderError,
  label,
  debugTag,
  errorLabel = t("errors.newSessionFailed"),
}) {
  let dismissOverlay = () => {};
  let prepared = null;
  try {
    if (typeof transport.prepareWorkspaceTarget !== "function") {
      throw new Error("Native workspace transition is unavailable");
    }
    // Raised before preparation: the spawn plus first `get_state` is the long
    // pole, so it must happen under the overlay rather than before it.
    dismissOverlay = runOnBeforeSwap(onBeforeSwap, label);
    prepared = await transport.prepareWorkspaceTarget(targetCwd, {
      forceNewSession: true,
      reuseExisting: false,
    });
    if (prepared?.classification === "cross" && typeof beforeWorkspaceTransition === "function") {
      const settled = await beforeWorkspaceTransition(prepared);
      if (!settled) {
        dismissOverlay();
        onWorkspaceTransitionCancelled?.();
        await transport.cancelWorkspaceTransition(prepared.transitionGeneration).catch(() => {});
        return false;
      }
    }
    if (typeof prepared?.transitionGeneration !== "number") {
      throw new Error("Native workspace transition was not prepared");
    }
    beforeWorkspaceCommit?.();
    const committed = await transport.commitWorkspaceTransition(prepared.transitionGeneration);
    console.debug(`[Session route] ${debugTag}:created`, { targetCwd });
    if (typeof adoptPrepared === "function") {
      // In-place adoption: the document survives, so the overlay raised before
      // prepare must come down here — no new page boots to clear it.
      await adoptPrepared(prepared, {
        targetCwd,
        workspaceGeneration: committed?.workspaceGeneration,
      });
      dismissOverlay();
    } else {
      navigate(prepared.targetOrigin, { targetCwd });
    }
    return true;
  } catch (e) {
    dismissOverlay();
    if (prepared?.transitionGeneration != null) {
      onWorkspaceTransitionCancelled?.();
      await transport.cancelWorkspaceTransition(prepared.transitionGeneration).catch(() => {});
    }
    renderError(`${errorLabel}: ${e}`);
    return false;
  }
}

function resolveProjectCwd(project) {
  return project?.sessions?.find((session) => session?.cwd)?.cwd || project?.path;
}

// Sidebar "start new chat" entry point (project tile in the open
// workspace window). Spawns a fresh headless pi for the project's cwd
// and navigates THIS window to it. The previously-attached Pi process stays
// alive in the background and remains reachable via the running-instances list.
// No new OS window is opened, and no running session is interrupted — same model
// as in-window "+ New Session", just sourced from a project tile instead of the
// header.
export async function startNewProjectChat({
  project,
  transport,
  getCurrentCwd,
  shouldSpawnParallel,
  navigate,
  adoptPrepared,
  onBeforeSwap,
  beforeWorkspaceCommit,
  beforeWorkspaceTransition,
  onWorkspaceTransitionCancelled,
  renderError,
}) {
  if (!transport) {
    renderError(t("errors.newChatOnlyNative"));
    return false;
  }

  const targetCwd = resolveProjectCwd(project);
  if (!targetCwd) {
    renderError(t("errors.newChatPathUnavailable"));
    return false;
  }

  if (typeof navigate !== "function") {
    renderError(t("errors.newChatNavUnavailable"));
    return false;
  }

  const currentCwd = typeof getCurrentCwd === "function" ? getCurrentCwd() : null;
  const sameWorkspace = Boolean(currentCwd && targetCwd && currentCwd === targetCwd);
  const wantsParallel =
    typeof shouldSpawnParallel === "function" ? Boolean(shouldSpawnParallel()) : false;
  console.debug("[Session route] projectNewChat:decision", {
    targetCwd,
    currentCwd,
    sameWorkspace,
    wantsParallel,
  });

  if (!wantsParallel && sameWorkspace) {
    return spawnFreshSession({
      targetCwd,
      transport,
      navigate,
      adoptPrepared,
      onBeforeSwap,
      beforeWorkspaceCommit,
      beforeWorkspaceTransition,
      onWorkspaceTransitionCancelled,
      renderError,
      label: t("sidebar.startingSession"),
      debugTag: "projectNewChat",
      errorLabel: t("errors.newChatFailed"),
    });
  }

  if (!wantsParallel) {
    const result = await attachToWorkspace({
      targetCwd,
      transport,
      navigate,
      adoptPrepared,
      onBeforeSwap,
      beforeWorkspaceCommit,
      beforeWorkspaceTransition,
      onWorkspaceTransitionCancelled,
      renderError,
    });
    return result !== null;
  }

  return spawnFreshSession({
    targetCwd,
    transport,
    navigate,
    adoptPrepared,
    onBeforeSwap,
    beforeWorkspaceCommit,
    beforeWorkspaceTransition,
    onWorkspaceTransitionCancelled,
    renderError,
    label: t("sidebar.startingSession"),
    debugTag: "projectNewChat",
    errorLabel: t("errors.newChatFailed"),
  });
}

// Launcher bubble / "Open Folder" entry point. Does NOT spawn a parallel
// pi if one is already running for that cwd — opening a project is about
// *finding* it, not starting a new task. The user can still hit "+ New
// Session" inside the workspace window to fork a parallel agent.
export async function openProjectWorkspace({
  project,
  transport,
  navigate,
  adoptPrepared,
  onBeforeSwap,
  beforeWorkspaceCommit,
  beforeWorkspaceTransition,
  onWorkspaceTransitionCancelled,
  renderError,
}) {
  if (!transport) {
    renderError(t("errors.openProjectOnlyNative"));
    return false;
  }

  const targetCwd = resolveProjectCwd(project);
  if (!targetCwd) {
    renderError(t("errors.openProjectPathUnavailable"));
    return false;
  }

  try {
    const result = await attachToWorkspace({
      targetCwd,
      transport,
      onBeforeSwap,
      navigate,
      adoptPrepared,
      beforeWorkspaceCommit,
      beforeWorkspaceTransition,
      onWorkspaceTransitionCancelled,
      renderError,
    });
    return result !== null;
  } catch (e) {
    renderError(`${t("errors.openProjectFailed")}: ${String(e)}`);
    return false;
  }
}

export async function openFolderAsWorkspace({
  transport,
  navigate,
  adoptPrepared,
  onBeforeSwap,
  beforeWorkspaceCommit,
  beforeWorkspaceTransition,
  onWorkspaceTransitionCancelled,
  renderError,
}) {
  if (!transport) {
    renderError(t("errors.openFolderOnlyNative"));
    return false;
  }

  try {
    const selectedPath = await transport.pickFolder();
    if (!selectedPath) return false;

    // Registration must precede the owner-bound runtime admission. An
    // existing registry row is deliberately not a no-op: adding a workspace
    // always opens a fresh primary session in the current window.
    if (typeof transport.addWorkspace !== "function") {
      throw new Error("Workspace registration is unavailable");
    }
    const registration = await transport.addWorkspace(selectedPath);
    const targetCwd = registration?.workspace?.canonicalPath || selectedPath;
    return startRegisteredWorkspaceSession({
      targetCwd,
      transport,
      navigate,
      adoptPrepared,
      onBeforeSwap,
      beforeWorkspaceCommit,
      beforeWorkspaceTransition,
      onWorkspaceTransitionCancelled,
      renderError,
    });
  } catch (e) {
    renderError(`${t("errors.openFolderFailed")}: ${String(e)}`);
    return false;
  }
}
