// ABOUTME: Orchestrates Picot's main chat, workspace, file, and ephemeral-chat modules.
// ABOUTME: Keeps feature state in focused modules and wires their lifecycle events.

/**
 * Main App - Ties everything together
 */

import { createComposerDraftStore } from "./app/composer-draft-store.js";
import { resolveComposerSessionIdentity } from "./app/composer-session-identity.js";
import { installHostOriginFetch } from "./app/host-origin.js";
import { createPromptDelivery } from "./app/prompt-delivery.js";
import { StateManager } from "./app/state.js";
import { initTransport } from "./app/transport.js";
import { createAppUpdater } from "./app/updater.js";
import { setupVoiceInput } from "./app/voice-input.js";
import { resolveWebSocketUrl, WebSocketClient } from "./app/websocket-client.js";
import {
  applyAppearanceToDom,
  defaultWebglRenderer,
  FONT_SIZE_LEVELS,
  loadAppearanceCookie,
  migrateLegacyTerminalPreferences,
  normalizeFontLevel,
  normalizePreviewThemeMode,
  normalizeScrollbackLimit,
  normalizeSmoothScrollDuration,
  normalizeThemeMode,
  saveAppearanceCookie,
  TERMINAL_FONT_SIZE_PX,
  TERMINAL_THEME_MODES,
} from "./appearance-preferences.js";
import { createBackgroundSessionFiles } from "./background-session-files.js";
import { setEditorHighlightTheme } from "./code-editor.js";
import { createCompactCoordinator } from "./compact-coordinator.js";
import { setupComposerCommandMenu } from "./composer-command-menu.js";
import { setupComposerFit } from "./composer-fit.js";
import { planFollowUpSend, planSteeringSend } from "./composer-follow-up.js";
import { setupComposerImageAttachments } from "./composer-image-attachments.js";
import { setupComposerPasteOffload } from "./composer-paste-offload.js";
import { EphemeralChatView } from "./ephemeral-chat-view.js";
import { setupExtensionUpdateIndicator } from "./extension-update-indicator.js";
import { createFilePreviewFollow, isWriteTool, pathFromToolArgs } from "./file-preview-follow.js";
import { FilePreviewPanel } from "./file-preview-panel.js";
import { GitClient } from "./git-client.js";
import { GitPanel } from "./git-panel.js";
import {
  getLanguagePreference,
  initI18n,
  LANGUAGES,
  onLocaleChange,
  setLocale,
  t,
} from "./i18n.js";
import { createIcon, replaceButtonGlyph, setButtonIcon } from "./icons.js";
import { processImageFile, processImagePayload } from "./image-attachments.js";
import { InfoPanel } from "./info-panel.js";
import { setupLanQr } from "./lan-qr.js";
import { createLiveRuntimeSubscriptions } from "./live-runtime-subscriptions.js";
import { getLastModel, setLastModel } from "./models/last-model-store.js";
import { openModelDropdownMenu } from "./models/model-dropdown.js";
import {
  filterModelsByCatalogVisibility,
  isSelectedModel,
  selectModel,
} from "./models/selection.js";
import { renderPackageInstallFailure } from "./packages/install-status.js";
import {
  createPreferencesClient,
  PREFERENCE_KEYS,
  reconcileRenderPreferences,
  saveUserRenderPreference,
} from "./preferences-client.js";
import { renderPreviewThemeOptions } from "./preview-themes.js";
import { QuickChatDialog } from "./quick-chat-dialog.js";
import { resolvePreparedRuntimeTarget } from "./session/in-place-runtime-target.js";
import {
  shouldRefreshSidebarForNewSession,
  shouldShowProvisionalSession,
} from "./session/new-session-refresh.js";
import { getOnboardingState } from "./session/onboarding.js";
import {
  confirmDeferredFileBrowserWorkspace,
  deferFileBrowserWorkspace,
  shouldSuppressFileBrowserLoad,
  shouldSuppressFileBrowserRefresh,
} from "./session/routing.js";
import { anchorHistoryToBottom } from "./session/scroll-anchor.js";
import { createScrollOwner } from "./session/scroll-ownership.js";
import { SessionUiStateStore } from "./session-ui-state.js";
import { ConfigGateway, consumeConfigResponseFrame } from "./settings/config-gateway.js";
import { createConfigReadiness } from "./settings/config-readiness.js";
import { setupEnvironmentPage } from "./settings/environment-page.js";
import { setupExtensionsTabShell } from "./settings/extensions-tab-shell.js";
import { createMcpHostOps, setupMcpPage } from "./settings/mcp-page.js";
import { setupMobileAccess } from "./settings/mobile-access.js";
import { setupModelsPage } from "./settings/models-page.js";
import { createOauthGateway } from "./settings/oauth-gateway.js";
import { setupPackageBrowse } from "./settings/package-browse.js";
import { setupPackageManager } from "./settings/package-manager.js";
import { setupPackageSkillsTab } from "./settings/package-skills-tab.js";
import { setupPiPathToggle } from "./settings/pi-path-toggle.js";
import {
  clearSettingsSaveMessage,
  setSettingsSaveButtonSaving,
  showSettingsSaveError,
  showSettingsSaveSuccess,
} from "./settings/save-status.js";
import { setupSettingsConfig } from "./settings/settings-config.js";
import { setupSkillsInstallTab } from "./settings/skills-install-tab.js";
import { setupSkillsPage } from "./settings/skills-page.js";
import { setupSkillsTabShell } from "./settings/skills-tab-shell.js";
import { setupSubagentsTab } from "./settings/subagents-tab.js";
import {
  applyShowThinking,
  renderThinkingEffort,
  setupSettingsToggles,
  THINKING_LEVELS,
} from "./settings/toggles.js";
import { setupUsageTabs } from "./settings/usage-tabs.js";
import { SideChatManager } from "./side-chat-manager.js";
import { buildSessionItem } from "./sidebar/build-session-item.js";
import {
  clearFocusParam,
  FOCUS_WORKSPACE_PARAM,
  resolveFocusState,
  withFocusParam,
} from "./sidebar/focus-state.js";
import { SessionSidebar } from "./sidebar/index.js";
import { setupSidebarSearchControl } from "./sidebar/search-control.js";
import { WorkspaceFocusSidebar } from "./sidebar/workspace-focus-sidebar.js";
import { createMainSidebarResizer, createSidebarResizer } from "./sidebar-resizer.js";
import { createSessionTaskAnalysis } from "./task-debugger/session-task-analysis.js";
import { buildTurnsFromEntries } from "./task-debugger/turn-history.js";
import { createTurnTraceRecorder } from "./task-debugger/turn-trace.js";
import { createTaskNotifications } from "./task-notifications.js";
import { TerminalClient } from "./terminal-client.js";
import { loadTerminalFont, TERMINAL_FONT_FAMILY, TERMINAL_FONT_STACK } from "./terminal-font.js";
import { formatTerminalStartError, TerminalPanel } from "./terminal-panel.js";
import { TerminalSearch } from "./terminal-search.js";
import {
  encodeBase64 as encodeTerminalBase64,
  resolveTerminalTheme,
  TerminalTab,
} from "./terminal-tab.js";
import { applyTheme, getCurrentTheme, themes } from "./themes.js";
import { createHostFileMentionSearch, setupAtFileMention } from "./ui/at-file-mention.js";
import { BackgroundQuestionnaireStore } from "./ui/background-questionnaire-store.js";
import { guardComposerArrowInsertion } from "./ui/composer-caret-guard.js";
import { createTriggerRouter } from "./ui/composer-triggers.js";
import { repaintContextViz, setupContextViz } from "./ui/context-viz.js";
import { createConversationNav } from "./ui/conversation-nav.js";
import { DialogHandler } from "./ui/dialogs.js";
import { createFollowUpQueue } from "./ui/follow-up-queue.js";
import { createHeaderStatusBar } from "./ui/header-status-bar.js";
import { disconnectGateAutoReveal, observeGateAutoReveal } from "./ui/history-gate-auto-reveal.js";
import { initImageLightbox } from "./ui/image-lightbox.js";
import { setupMessagesInsets } from "./ui/layout-insets.js";
import { MessageRenderer } from "./ui/message-renderer.js";
import { findPendingQuestionnaire } from "./ui/pending-questionnaire.js";
import { createPiQueuePark } from "./ui/pi-queue-park.js";
import { summarizeProcessGroup } from "./ui/process-group.js";
import { QuestionnaireCard } from "./ui/questionnaire-card.js";
import { isRpivTodoCommandNotify, RpivTodoMirrorPanel } from "./ui/rpiv-todo-mirror.js";
import { SafetyGuardDialog } from "./ui/safety-guard-dialog.js";
import { setupScrollbarAutoHide } from "./ui/scrollbar-auto-hide.js";
import { setupSessionSearchDialog } from "./ui/session-search-dialog.js";
import { setupSkillSlashCommand } from "./ui/skill-slash-command.js";
import { SubagentAsyncMirrorPanel } from "./ui/subagent-async-mirror.js";
import { ToolCardRenderer } from "./ui/tool-card.js";
import { createTurnSection } from "./ui/turn.js";
import { mountTurnFilesCard, renderTurnFilesCard } from "./ui/turn-files-card.js";
import {
  assistantHasText,
  HISTORY_FULL_MOUNT_TURNS,
  HISTORY_REVEAL_BATCH_TURNS,
  resolveTurnDurationMs,
  splitFinalAssistantBlocks,
  summarizeTurnRail,
} from "./ui/turn-model.js";
import { createWidgetMirrorRegistry, runtimeIdForTarget } from "./ui/widget-mirror-registry.js";
import { WindowCloseCoordinator } from "./window-close-coordinator.js";
import {
  openFolderAsWorkspace,
  startInWindowNewSession,
  startNewProjectChat,
  startRegisteredWorkspaceSession,
} from "./workspace/actions.js";
import { FileBrowser } from "./workspace/file-browser.js";
import {
  cacheSidebarProjects,
  consumeNavState,
  readCachedSidebarProjects,
  snapshotNavState,
} from "./workspace/nav-state-cache.js";
import { normalizeLocalPath } from "./workspace/path-utils.js";
import {
  createWorkspaceActionsController,
  populateAppLogo as sharedPopulateAppLogo,
} from "./workspace-actions.js";
import { registryPinsFromProjects } from "./workspace-projects.js";

// Initialize locale messages before constructing components that call t().
const savedTheme = getCurrentTheme();
applyTheme(savedTheme);
await initI18n();

const mobileClientMode = new URLSearchParams(window.location.search).get("mobile") === "1";
// Workspace Focus mode orchestration state.
// `currentFocusProject` is the project whose task-workspace sidebar is
// currently rendered; null while the normal sidebar is shown. It is the
// single in-memory mirror of the URL `focusWorkspaceId` param: navigation
// uses it to decide whether to carry focus across a same-workspace runtime route.
let currentFocusProject = null;
// Snapshot of the normal sidebar (scroll/search/expansion) captured on
// focus entry and restored on exit. Null outside an active focus session.
let normalSidebarSnapshot = null;
let currentFocusSidebar = null;
const navigateInWindow = (url, metadata = {}) => {
  let targetUrl;
  try {
    // Validate origin first so a cross-origin target is rejected before any
    // focus-param mutation runs. withFocusParam then keeps focusWorkspaceId
    // only when the target cwd equals the focused project's canonical path;
    // any stale param is stripped, so cross-workspace/unknown navigation
    // clears focus rather than carrying a dangling id onto the next page.
    const parsed = new URL(url, window.location.href);
    const currentUrl = new URL(window.location.href);
    if (
      parsed.protocol !== currentUrl.protocol ||
      parsed.hostname !== currentUrl.hostname ||
      parsed.username ||
      parsed.password
    ) {
      console.error("[navigation] rejected cross-origin target");
      return;
    }
    const requestedFocusId = currentUrl.searchParams.get(FOCUS_WORKSPACE_PARAM);
    // During navigation to a freshly spawned native runtime, the current
    // focus project has not been re-resolved yet on page load. Fall back to
    // focusWorkspaceId from the current URL; withFocusParam re-encodes it onto
    // the target
    // URL only when targetCwd matches that project's path, and strips it
    // otherwise. Once the new page boots and resolveFocusState runs,
    // currentFocusProject is repopulated from the sidebar and this fallback is
    // no longer exercised.
    const focusProject =
      currentFocusProject ||
      (requestedFocusId?.startsWith("workspace:")
        ? { path: requestedFocusId.slice("workspace:".length) }
        : null);
    targetUrl = withFocusParam(metadata.targetCwd, focusProject, parsed);
    if (mobileClientMode) targetUrl.searchParams.set("mobile", "1");
  } catch {
    console.error("[navigation] rejected invalid target");
    return;
  }
  window.location.assign(targetUrl.toString());
};

// ──────────────────────────────────────────────────────────────────────
// Workspace Focus mode orchestration
// ──────────────────────────────────────────────────────────────────────
// Focus replaces the normal sidebar with a task-workbench view of a single
// workspace (back / new task / read-only session list). Its state lives only
// in the URL `focusWorkspaceId` param: it survives a same-workspace runtime route
// (New Task / parallel session) and is dropped on any cross-workspace nav.
// `currentFocusProject` is the in-memory mirror used by navigateInWindow.
function focusSidebarEl() {
  return document.getElementById("sidebar");
}

async function enterFocus(project) {
  if (!project) return;
  // Switching focus directly between two workspaces is rare (the focus button
  // is only shown for the active workspace), but guard against it so the prior
  // snapshot is not silently overwritten by the new workspace's focus view.
  if (currentFocusProject && currentFocusProject.path !== project.path) {
    currentFocusProject = null;
    normalSidebarSnapshot = null;
  }
  if (currentFocusProject?.path === project.path) return;
  // Close any open transient UI from the normal sidebar before snapshotting.
  sidebar.closeContextMenu?.();
  sidebar.quickInfo?.close?.();

  const sessionList = document.getElementById("session-list");
  // Make sure the snapshot reflects real DOM, not a loading skeleton, when
  // focus is entered during boot before the first render completed.
  if (!normalSidebarSnapshot) sidebar.render();
  normalSidebarSnapshot = {
    scrollTop: sessionList.scrollTop,
    searchQuery: sidebar.searchQuery,
    expandedWorkspaces: new Set(sidebar.expandedWorkspaces),
  };

  focusSidebarEl()?.classList.add("focus-mode");
  // Show a "Workspace" label after the logo while focus is active; removed on exit.
  const modeToggle = document.querySelector(".mode-toggle");
  if (modeToggle && !modeToggle.parentElement.querySelector(".sidebar-focus-brand")) {
    const brand = document.createElement("span");
    brand.className = "sidebar-focus-brand";
    brand.textContent = t("workspace.focusTitle");
    modeToggle.after(brand);
  }
  currentFocusProject = project;

  const focusCardInfo = {
    path: project.path,
    count:
      typeof project.sessionCount === "number"
        ? project.sessionCount
        : (project.sessions?.length ?? 0),
  };
  const focusSidebar = new WorkspaceFocusSidebar(sessionList, {
    project,
    cardInfo: focusCardInfo,
    activeSessionFile: sidebar.activeSessionFile,
    unread: sidebar.unread,
    streaming: sidebar.streamingFiles,
    buildSessionItem,
    deletionBlockedReason: (filePath) => sidebar.deletionBlockedReason(filePath),
    renameBlockedReason: (filePath) => sidebar.renameBlockedReason(filePath),
    createIcon,
    registerStatusItem: (filePath, item) => {
      const items = sidebar.statusItemsByPath.get(filePath) || new Set();
      items.add(item);
      sidebar.statusItemsByPath.set(filePath, items);
    },
    onBack: () => exitFocus(),
    onNewTask: (p) => {
      handleNewProjectChat(p);
    },
    onSessionSelect: handleSessionSelect,
    onDelete: (filePath) => sidebar.deleteSession(filePath),
    onRename: (filePath, session, item) => sidebar.renameSession(filePath, session, item),
  });
  currentFocusSidebar = focusSidebar;
  focusSidebar.render();
  sidebar.rebuildStatusIndex();

  // Focus owns a complete session list, even when its normal workspace row was
  // collapsed and therefore had no history loaded yet.
  if (project.source === "registry") {
    await sidebar.ensureWorkspaceSessions(project);
    if (currentFocusProject?.path === project.path) refreshFocusView();
  }

  // Persist focus in the URL so a same-workspace port navigation carries it.
  try {
    const next = withFocusParam(project.path, currentFocusProject, window.location.href);
    if (next.toString() !== window.location.href) {
      history.replaceState(history.state, "", next.toString());
    }
  } catch {
    /* URL mutation is best-effort; focus still works in-memory. */
  }

  // Asynchronously enrich the focus info card with the git repository name.
  if (project.workspaceId) {
    // Registry rows carry DB uuids the DB-blind server cannot resolve;
    // translate to the canonical-path identity it accepts.
    fetchWorkspaceRepository(project.path ? `path:${project.path}` : project.workspaceId).then(
      (repository) => {
        if (!repository || !currentFocusSidebar) return;
        if (currentFocusProject?.path !== project.path) return;
        currentFocusSidebar.setProjectState({
          cardInfo: {
            ...(currentFocusSidebar.cardInfo || {}),
            path: project.path,
            count:
              typeof project.sessionCount === "number"
                ? project.sessionCount
                : (project.sessions?.length ?? 0),
            repository,
          },
        });
        currentFocusSidebar.render();
        sidebar.rebuildStatusIndex();
      },
    );
  }
}

function exitFocus() {
  const snapshot = normalSidebarSnapshot;
  normalSidebarSnapshot = null;
  currentFocusSidebar = null;
  currentFocusProject = null;

  try {
    const cleaned = clearFocusParam(window.location.href);
    if (cleaned.toString() !== window.location.href) {
      history.replaceState(history.state, "", cleaned.toString());
    }
  } catch {
    /* best-effort */
  }

  document.querySelector(".sidebar-focus-brand")?.remove();
  focusSidebarEl()?.classList.remove("focus-mode");
  if (snapshot) {
    sidebar.expandedWorkspaces = snapshot.expandedWorkspaces;
    // Restore the search filter BEFORE render so the rebuilt list reflects it.
    sidebar.searchQuery = snapshot.searchQuery;
  }
  sidebar.render();
  if (snapshot) {
    const sessionList = document.getElementById("session-list");
    if (sessionList) sessionList.scrollTop = snapshot.scrollTop;
    const searchInput = document.getElementById("session-search-input");
    if (searchInput) searchInput.value = snapshot.searchQuery;
  }
}

// Refreshes the live focus view with the latest sidebar data. Called by
// SessionSidebar.render (via the isFocusActive/onFocusRefresh delegation) and
// by resolveAndApplyFocus when focus is already active on the matched
// workspace, so loadSessions()/setActive() updates reach the focus view
// instead of being dropped by the matched-state early return.
function refreshFocusView() {
  if (!currentFocusSidebar || !currentFocusProject) return;
  const project =
    sidebar.projects.find((p) => p?.path === currentFocusProject.path) || currentFocusProject;
  currentFocusSidebar.setProjectState({
    project,
    cardInfo: {
      ...(currentFocusSidebar.cardInfo || {}),
      path: project.path,
      count:
        typeof project.sessionCount === "number"
          ? project.sessionCount
          : (project.sessions?.length ?? 0),
    },
    activeSessionFile: sidebar.activeSessionFile,
    unread: sidebar.unread,
    streaming: sidebar.streamingFiles,
  });
  currentFocusSidebar.render();
  sidebar.rebuildStatusIndex();
}

// Fetches the git repository name for the Focus workspace info card; returns
// null when the workspace is not a
// git repo or the request fails so the card simply omits the repo row.
async function fetchWorkspaceRepository(workspaceId) {
  if (!workspaceId) return null;
  try {
    const data = await transport.workspaceInfo();
    if (data?.isGit === true && data.repository) return data.repository;
    return null;
  } catch {
    return null;
  }
}

// Idempotent focus resolver called at boot and after every session/project
// data update. pending (active session or its project still unknown) never
// touches the UI; matched re-enters focus only when the resolved workspace
// differs from the one already focused; mismatched exits focus if active,
// otherwise just scrubs a residual URL param.
function resolveAndApplyFocus() {
  const requestedId = new URLSearchParams(window.location.search).get(FOCUS_WORKSPACE_PARAM);
  const { state, project } = resolveFocusState({
    requestedId,
    projects: sidebar.projects,
    activeSessionFile: sidebar.activeSessionFile,
    runtimeWorkspaceId: wsClient.getRuntimeTarget()?.workspaceId,
  });
  if (state === "pending") return;
  if (state === "matched") {
    if (currentFocusProject?.path === project.path) {
      refreshFocusView();
      return;
    }
    enterFocus(project);
    return;
  }
  if (currentFocusProject) {
    exitFocus();
    return;
  }
  try {
    const cleaned = clearFocusParam(window.location.href);
    if (cleaned.toString() !== window.location.href) {
      history.replaceState(history.state, "", cleaned.toString());
    }
  } catch {
    /* best-effort */
  }
}

// ──────────────────────────────────────────────────────────────────────
// Instance-swap overlay
// ──────────────────────────────────────────────────────────────────────
// `+ New Session`, `start new chat`, `Open Project`, and `Open Folder`
// all end with a host-origin workspace route,
// which is a full-page navigation and would otherwise show a 1–2s
// freeze (while pi spawns) and then a white flash (while the WebView
// reloads). To make this look like a single smooth transition we:
//
//   1. Open a fullscreen spinner overlay BEFORE awaiting openWorkspace.
//   2. Persist a sessionStorage flag so the new page boots into the
//      same overlay (see <head> bootstrap script in index.html).
//   3. After the new page's WebSocket first connects, fade out.
//
// Returns a `dismiss` function that rolls back the overlay if the
// swap fails before navigation (e.g. openWorkspace rejects).
// Snapshot ephemeral UI state before ANY in-window navigation. The new page
// boots fresh; consumeNavState() restores scroll, sidebar expansion, search,
// and the input draft so session/workspace switches feel continuous.
function snapshotUiStateForNavigation() {
  flushDraftNow(); // a pending debounce timer is not a saved draft (C4)
  try {
    snapshotNavState({
      messageScroll: messagesContainer ? messagesContainer.scrollTop : null,
      sidebarScroll: sidebarEl ? sidebarEl.scrollTop : null,
      inputDraft: messageInput ? messageInput.value : "",
      expandedWorkspaces: sidebar ? sidebar.expandedWorkspaces : [],
      searchQuery: sidebar ? sidebar.searchQuery : "",
    });
  } catch {
    /* snapshot is best-effort */
  }
}

function showSwapOverlay(label) {
  snapshotUiStateForNavigation();
  try {
    sessionStorage.setItem("pi-studio:swapping-instance", "1");
  } catch {}
  document.body.classList.add("swapping-instance");
  const overlay = document.getElementById("instance-swap-overlay");
  if (overlay) overlay.setAttribute("data-visible", "true");
  const labelEl = document.getElementById("instance-swap-overlay-label");
  if (labelEl && typeof label === "string" && label) labelEl.textContent = label;
  return hideSwapOverlay;
}

function hideSwapOverlay() {
  try {
    sessionStorage.removeItem("pi-studio:swapping-instance");
  } catch {}
  document.body.classList.remove("swapping-instance");
  const overlay = document.getElementById("instance-swap-overlay");
  if (overlay) overlay.setAttribute("data-visible", "false");
}

// Returned to workspace/actions.js — they call this BEFORE openWorkspace
// (so the overlay covers spawn latency) and the returned dismiss is only
// invoked on error (success path lets the overlay persist across the
// navigation boundary).
const onBeforeInstanceSwap = (label) => showSwapOverlay(label);

// If the page booted into the overlay (because we just navigated from
// a previous instance), fade it out as soon as the WebSocket reaches
// the new pi. The post-connect wait avoids a brief flash of empty
// chat UI before the first workspace/session data finishes populating things.
function dismissBootSwapOverlayWhenReady() {
  if (!document.body.classList.contains("swapping-instance")) return;
  const fade = () => {
    requestAnimationFrame(() => {
      const overlay = document.getElementById("instance-swap-overlay");
      if (overlay) overlay.setAttribute("data-visible", "false");
      document.body.classList.remove("swapping-instance");
      try {
        sessionStorage.removeItem("pi-studio:swapping-instance");
      } catch {}
    });
  };
  const alreadyOpen = wsClient.ws && wsClient.ws.readyState === WebSocket.OPEN;
  if (alreadyOpen) {
    fade();
  } else {
    const onConnect = () => {
      wsClient.removeEventListener("connected", onConnect);
      fade();
    };
    wsClient.addEventListener("connected", onConnect);
  }
  setTimeout(() => {
    if (document.body.classList.contains("swapping-instance")) hideSwapOverlay();
  }, 5000);
}

// Initialize components
installHostOriginFetch(window);
const wsUrl = resolveWebSocketUrl(window);
const wsClient = new WebSocketClient(wsUrl);
// Unified control transport: every process/window lifecycle and native op goes
// through HostServer v2 WebSocket. Pi runtime commands use runtime_request;
// host lifecycle uses host_request.
const transport = initTransport({ wsClient, env: window });

// One shared client serves startup reconciliation, user-initiated persistence,
// and the composer draft store; created early because the draft store (C4)
// reads it during session-restore code that can run before late init.
const preferencesClient = createPreferencesClient({ transport });
// Settings data-plane runtime adapter: /picot-config prompts ride the same v2
// runtime_request channel as chat; sendRuntime resolves with Pi's reply data.
const settingsRuntime = {
  request: (command, target, options) => wsClient.sendRuntime(command, target, options),
};
const getSettingsRuntimeTarget = () => wsClient.getRuntimeTarget();
// Startup configuration reads (the skills page activates eagerly) must not
// fire into a runtime that is still spawning. The gate opens with the first
// foreground snapshot of the CURRENT routing triple and re-arms whenever an
// in-page session adoption swaps the triple — readiness tracks the target,
// it never stays open for a runtime that has not proven itself live.
const configReadiness = createConfigReadiness({
  targetKeyOf: () => {
    const target = getSettingsRuntimeTarget();
    if (!target?.workspaceId || !target?.sessionId) return null;
    return [target.workspaceId, target.sessionId, target.instanceId ?? ""].join("\u0000");
  },
});
const configGateway = new ConfigGateway({
  runtime: settingsRuntime,
  getTarget: getSettingsRuntimeTarget,
  waitUntilReady: configReadiness.waitUntilReady,
});
// OAuth login flows share the config transport; their __picotOauth frames
// must be consumed before the config gateway sees them (design §5 M3).
const oauthGateway = createOauthGateway({
  runtime: settingsRuntime,
  getTarget: getSettingsRuntimeTarget,
});
// The settings usage tab renders in THIS document (no iframe): the dashboard
// shares the main window's authenticated transport. It is imported
// dynamically AFTER initI18n so the custom-element upgrade (whose shell
// renders t() strings immediately) never runs against empty dictionaries —
// the old iframe loaded its own copy post-init for the same reason.
let settingsQuotaPanel = null;
{
  const { setCostDashboardTransport } = await import("./cost/dashboard.js");
  setCostDashboardTransport(transport);
  // Provider quota (spec 2026-09-22) is its own Settings tab: the gateway
  // lives in this document, so the section mounts here rather than inside the
  // cost dashboard's shadow root.
  const { createProviderQuotaPanel } = await import("./cost/provider-quota-panel.js");
  const { quotaLocaleBundle } = await import("./cost/quota-locale.js");
  settingsQuotaPanel = createProviderQuotaPanel(
    {
      container: () => document.getElementById("settings-provider-quota"),
      gateway: configGateway,
      dataTransport: transport,
    },
    { locale: quotaLocaleBundle() },
  );
  window.addEventListener("localechange", () => settingsQuotaPanel?.render());
  void settingsQuotaPanel.loadReports();
}

// Usage page sub-tabs: the cost dashboard and the provider quota section are
// two views of one Settings page, each lazy-loading on first selection.
// Usage page sub-tabs (cost / provider quota). The switching itself lives in
// its own module so it is unit-testable and independent of this file's size.
setupUsageTabs({
  onSelect: (view) => {
    if (view === "quota") void settingsQuotaPanel?.loadReports();
    else void document.getElementById("settings-cost-dashboard")?.ensureLoaded?.();
  },
});
// Mobile LAN QR: header button + modal over the native pairing controls
// (token minted on open; visibility follows the running host's LAN bind).
setupLanQr({
  transport,
  setButtonIcon,
  replaceButtonGlyph,
  openExternalLink: (url) => void transport.openExternal(url),
});
// Canonical workspace pages obtain host-derived runtime identity before opening
// the socket. Browser URL state never supplies instance or owner identity.
if (wsClient.canonicalRoute) {
  await wsClient.loadCanonicalTarget();
  // Upstream initial load: fetch the disk history in parallel with the
  // runtime — the file read does not wait for Pi to start.
  const canonicalTarget = wsClient.getRuntimeTarget();
  if (canonicalTarget?.workspaceId && canonicalTarget?.sessionId) {
    void fetchDiskHistory(canonicalTarget);
  }
}
// `?mobile=1` is a browser client even if it reaches the desktop broker, so it
// must not use native workspace/window controls.
const nativeAvailable = () => !mobileClientMode && transport.capabilities.native;
const canUseSessionControl = () => transport.capabilities.native;
const state = new StateManager();
const messagesElement = document.getElementById("messages");
// sessionTreeActions: the main chat is the only persisted-session surface;
// its Fork/Edit buttons dispatch to app.js's transport handlers below.
// Scroll ownership (spec P3): one owner for the messages viewport, shared by
// both renderers so a user reading history is never fought by auto-follow.
const messagesScrollOwner = createScrollOwner({ container: messagesElement });
// Turn rendering is opted in via TURNS_RENDERING (see the live-turn block);
// the renderer itself stays surface-agnostic.
const messageRenderer = new MessageRenderer(messagesElement, {
  sessionTreeActions: true,
  scrollOwner: messagesScrollOwner,
});
const toolCardRenderer = new ToolCardRenderer(messagesElement, {
  enableFileRefs: true,
  scrollOwner: messagesScrollOwner,
});

// The scroll-to-bottom control re-arms following explicitly (rule 5). This
// button previously had no handler at all — the badge showed but did nothing.
const scrollBottomBtn = document.getElementById("scroll-bottom-btn");
scrollBottomBtn?.addEventListener("click", () => {
  messagesScrollOwner.followBottom();
  scrollBottomBtn.classList.add("hidden");
  scrollBottomBadge.classList.add("hidden");
});
initImageLightbox(messagesElement);
const dialogHandler = new DialogHandler({
  container: document.getElementById("dialog-container"),
  notificationContainer: document.getElementById("messages"),
  send: (message) => wsClient.send(message),
});
// Rich bash-approval card (datarx-safety-guard-pi): first-shot interception
// before the generic dialog — long approval prompts must scroll, not stretch.
const safetyGuardDialog = new SafetyGuardDialog({
  container: document.getElementById("dialog-container"),
  // The approval belongs to the turn whose bash call triggered it: render it in
  // that turn's inline card slot so it scrolls with the conversation. With no
  // live turn to host it (replayed background request, transcript re-render,
  // abort) the card falls back to the modal container.
  resolveHost: () => activeTurn?.card?.host ?? null,
  send: (message) => wsClient.send(message),
});
const questionnaireCard = new QuestionnaireCard({
  // The question belongs to the live turn whose ask_user_question call
  // opened it: render in that turn's inline card slot (unified blocker slot,
  // same host as the safety-guard approval). Rebuilt cards (parked restore,
  // replayed request, transcript re-render) have no live turn; they anchor
  // at the transcript tail instead — never a modal popup.
  container: document.getElementById("dialog-container"),
  resolveHost: () => activeTurn?.card?.host ?? null,
  resolveFallbackHost: () => mountQuestionnaireStreamSlot(),
  send: (message) => wsClient.send(message),
  wsClient,
  confirmAbandon: ({ title, message }) => dialogHandler.showLocalConfirm({ title, message }),
  // A walker frame lost in transit would strand the agent forever (submit
  // drains nothing, pi never times out). One idempotent re-subscribe makes
  // the host replay its pending — i.e. still unanswered — dialogs.
  resubscribe: () => wsClient.requestPendingDialogReplay(),
});

// Stable inline slot for rebuilt questionnaires, re-appended to the transcript
// tail on every mount so the card scrolls with the conversation instead of
// popping a modal. `appendChild` on an already-attached node just moves it to
// the end, which is exactly the anchoring we want after new stream content.
const questionnaireStreamSlot = document.createElement("div");
questionnaireStreamSlot.className = "questionnaire-stream-slot hidden";
questionnaireStreamSlot.id = "questionnaire-stream-slot";
function mountQuestionnaireStreamSlot() {
  messagesElement.appendChild(questionnaireStreamSlot);
  return questionnaireStreamSlot;
}
// Backgrounded runtimes wait forever on extension_ui_response; park their
// questionnaire state here so the wait stays answerable after the user returns.
const backgroundQuestionnaires = new BackgroundQuestionnaireStore();
// Log-derived unanswered ask_user_question per session file: the durable
// counterpart to the park, so an arriving blocking request can rebuild the
// card even after a reload or a cross-workspace return. Recomputed by every
// transcript render (a tool result removes it) and read only for its own
// session.
const pendingQuestionsBySession = new Map();

function clearConversationRenderers() {
  // P5.1: the registry is session-scoped state; a view reset drops it so the
  // rail never lists another session's turns. Refresh immediately or the
  // rail keeps rendering the cleared registry's stale ticks until the next
  // registration happens to fire.
  turnRegistry.clear();
  convNav?.refresh();
  // P1.3: a session switch clears the pending optimistic bubble reference and
  // settles any still-open turn — a stale bubble can never cross sessions.
  closeLiveTurn();
  pendingUserEl = null;
  pendingUserKey = null;
  pendingPromptPreview = "";
  messageRenderer.clear();
  toolCardRenderer.clear();
}

// Session sidebar
const sidebar = new SessionSidebar(
  document.getElementById("session-list"),
  handleSessionSelect,
  handleNewProjectChat,
  {
    // Registry data source + workspace.* controls ride the broker transport.
    transport,
    onOpenProject: (project) => {
      if (!project?.path) return handleOpenFolder();
      return transport.openInApp(project.path);
    },
    onRegisterWorkspace: (targetCwd) => handleRegisteredWorkspace(targetCwd),
    // Focus mode is disabled for feedback (2026-09): entry button hidden by
    // omitting onWorkspaceFocus — _workspaceFocusEnabled() then yields false.
    // Focus machinery (enterFocus/WorkspaceFocusSidebar/URL param) stays.
    // To re-enable, restore: onWorkspaceFocus: (project) => enterFocus(project),
    isCurrentWorkspace: (project) => project?.path === getCurrentWorkspacePath(),
    onSessionNotice: (message) => {
      if (typeof messageRenderer?.renderSystemMessage === "function") {
        messageRenderer.renderSystemMessage(message);
      } else {
        console.warn("[Sidebar] session notice:", message);
      }
    },
    // C4: a deleted session's composer draft goes with it.
    onSessionDeleted: (filePath) => {
      if (filePath) void composerDraftStore.clearForSessionFile(filePath);
    },
    isFocusActive: () => currentFocusProject !== null,
    getFocusWorkspacePath: () => currentFocusProject?.path || null,
    onFocusRefresh: () => refreshFocusView(),
  },
);

// ── Super Agent wiring ──────────────────────────────────────────────────────
// Compatibility surface for Super Agent add-on Web Components.
window.__saNav = {
  get transport() {
    return transport;
  },
  navigateInWindow,
  startInWindowNewSession,
};

// ── Settings and dialogs ───────────────────────────────────────────────────

// UI elements
const messageInput = document.getElementById("message-input");
const chatForm = document.getElementById("chat-form");
const sessionUiState = new SessionUiStateStore({
  profileClient: {
    load: () => transport.loadSessionUiProfile(wsClient.sessionId),
    save: (profile) => transport.saveSessionUiProfile(wsClient.sessionId, profile),
  },
});
let activeUiSessionFile = null;
// One-shot gate-reveal seed consumed by the next renderSessionHistory.
const pendingRevealRestore = null;
// Monotonic counter bumped whenever restoreSessionUiState binds a different
// session. applySessionUiProfile captures it before each await and bails when
// the token changes, so a profile restore that resumes after the user has
// switched sessions cannot clobber the new session's model/thinking display.
let uiSessionGeneration = 0;
const sendBtn = document.getElementById("send-btn");
setButtonIcon(sendBtn, "send", { size: 16 });
const abortBtn = document.getElementById("abort-btn");
setButtonIcon(abortBtn, "square", { size: 16, filled: true });
const statusIndicator = document.getElementById("status-indicator");
const statusText = document.getElementById("status-text");
const skillSlashMenu = document.getElementById("skill-slash-menu");
const atFileMentionMenu = document.getElementById("at-file-mention-menu");

const skillSlashPicker = setupSkillSlashCommand({
  input: messageInput,
  container: skillSlashMenu,
  loadSkills: async () => {
    const response = await rpcCommand({ type: "list_slash_commands" }, null, true);
    if (!response?.success) {
      throw new Error(response?.error || "Failed to load slash commands");
    }
    return response.data?.commands || [];
  },
});

// @-file mention completion runs router-driven on the main composer: the
// trigger router owns the only keydown listener (wired before the main
// Enter-to-send listener) so at most one picker can consume any key.
const atFileMentionPicker = setupAtFileMention({
  input: messageInput,
  container: atFileMentionMenu,
  getWorkspaceRoot: () => getCurrentWorkspacePath(),
  searchFiles: createHostFileMentionSearch(() => transport),
  router: true,
});
createTriggerRouter({
  input: messageInput,
  pickers: [
    { kind: "slash", ...skillSlashPicker },
    { kind: "mention", ...atFileMentionPicker },
  ],
});

const addProjectBtn = document.getElementById("add-project-btn");
if (addProjectBtn) {
  setButtonIcon(addProjectBtn, "folder-plus", { size: 16 });
  addProjectBtn.addEventListener("click", () => {
    if (workspaceLaunchInProgress) return;
    setWorkspaceLaunchInProgress(true);
    void sidebar.addProjectViaPicker().finally(() => setWorkspaceLaunchInProgress(false));
  });
}
const sidebarEl = document.getElementById("sidebar");
const sidebarToggle = document.getElementById("sidebar-toggle");
setButtonIcon(sidebarToggle, "menu", { size: 16 });
const sidebarOverlay = document.getElementById("sidebar-overlay");

const refreshSessionsBtn = document.getElementById("refresh-sessions-btn");
const sessionSearchInput = document.getElementById("session-search-input");
const sessionSearchClearBtn = document.getElementById("session-search-clear");
const sessionSearchOverlay = document.getElementById("session-search-overlay");
const sessionSearchDialog = document.getElementById("session-search-dialog");
const sessionSearchDialogInput = document.getElementById("session-search-dialog-input");
const sessionSearchResults = document.getElementById("session-search-results");
setButtonIcon(sessionSearchClearBtn, "x", { size: 12 });
const typingIndicator = document.getElementById("typing-indicator");

const sessionUsageEl = document.getElementById("session-usage");
const tokenUsageEl = document.getElementById("token-usage");
const contextDonutArc = document.getElementById("context-donut-arc");
const contextDonutLabel = document.getElementById("context-donut-label");
// Donut arc length for r=9 (matches the static SVG in index.html).
const CONTEXT_DONUT_CIRCUMFERENCE = 2 * Math.PI * 9;
setButtonIcon(refreshSessionsBtn, "refresh-cw", { size: 16 });
setButtonIcon(document.getElementById("quick-chat-btn"), "message-circle", { size: 16 });
const scrollBottomBadge = document.getElementById("scroll-bottom-badge");
// One arrow, on the button itself: the badge is a plain "New" label.
const scrollBottomIcon = document.querySelector("#scroll-bottom-btn .scroll-bottom-icon");
const scrollBottomIconNode = createIcon("arrow-down", { size: 16 });
if (scrollBottomIcon && scrollBottomIconNode)
  scrollBottomIcon.replaceChildren(scrollBottomIconNode);
const convNavEl = document.getElementById("conv-nav");
const convNavTrack = document.getElementById("conv-nav-track");

const convNavTooltip = document.getElementById("conv-nav-tooltip");
const convNavTooltipQ = document.getElementById("conv-nav-tooltip-q");
const convNavTooltipA = document.getElementById("conv-nav-tooltip-a");
const convNavTooltipSep = document.getElementById("conv-nav-tooltip-sep");
const messagesContainer = document.getElementById("messages");

// Reparent the nav tooltip to <body> so it escapes the main panel's stacking
// context — otherwise the side-chat and file-browser panels render above it.
if (convNavTooltip.parentElement && convNavTooltip.parentElement !== document.body) {
  document.body.appendChild(convNavTooltip);
}
const mainContainer = document.querySelector(".main");
const headerEl = document.querySelector(".header");
const inputAreaEl = document.querySelector(".input-area");
const settingsDragRegion = document.getElementById("settings-drag-region");

function startWindowDrag(e) {
  if (e.button !== 0) return;
  if (e.target.closest("button, a, input, select, textarea, [role=button]")) return;
  window.__TAURI__?.window?.getCurrentWindow().startDragging();
}

headerEl?.addEventListener("mousedown", startWindowDrag);
settingsDragRegion?.addEventListener("mousedown", startWindowDrag);

setupMessagesInsets({
  main: mainContainer,
  messages: messagesContainer,
  header: headerEl,
  inputArea: inputAreaEl,
});

// Pane scrollbars draw their thumb only while scrolling (transcript, file
// list, git panel); the capture listener covers panels built after this runs.
setupScrollbarAutoHide();

// Ambient extension widgets are mirrored into panels owned by their Pi runtime.
const widgetMirrorRegistry = createWidgetMirrorRegistry({ container: inputAreaEl });
const backgroundSessionFiles = createBackgroundSessionFiles();
const liveRuntimeSubscriptions = createLiveRuntimeSubscriptions({
  transport,
  wsClient,
  sidebar,
  backgroundSessionFiles,
});
// Runtime spawns reach this page as owner events (same window owns every
// runtime it navigates to); each one may need a fresh subscription.
wsClient.addEventListener("runtimeStarted", (event) => {
  void liveRuntimeSubscriptions.refresh().then(() => {
    // A spawn is a new process, so its queue is empty by construction: a park
    // left behind by the runtime it replaced (or by a stop this page missed)
    // must not repaint. The refreshed instance summaries resolve the session
    // file the park is keyed by.
    const sessionFile = backgroundSessionFiles.resolve(event.detail);
    piQueuePark.forget(sessionFile);
    if (sessionFile && sessionFile === activeUiSessionFile) renderPiQueue(null);
  });
});
widgetMirrorRegistry.registerRenderer({
  widgetKey: "rpiv-todos",
  toolNames: ["todo"],
  matchesNotify: (message) => {
    const text = String(message ?? "");
    return isRpivTodoCommandNotify(text);
  },
  replay: (panel, messages) => panel.hydrateFromMessages(messages),
  createPanel: ({ container, widgetPlacement }) =>
    new RpivTodoMirrorPanel({ container, widgetPlacement, onClear: requestTodoClear }),
});

// pi-subagents background runs arrive as `subagent-async` snapshots; the panel
// owns its own state, so no replay and no notify suppression are wired.
widgetMirrorRegistry.registerRenderer({
  widgetKey: "subagent-async",
  toolNames: [],
  createPanel: ({ container, widgetPlacement }) =>
    new SubagentAsyncMirrorPanel({ container, widgetPlacement }),
});

async function requestTodoClear() {
  const confirmed = await dialogHandler.showLocalConfirm({
    title: t("todoMirror.clearConfirmTitle"),
    message: t("todoMirror.clearConfirmBody"),
  });
  if (!confirmed) return;
  const command = {
    type: "prompt",
    message: t("todoMirror.clearPrompt"),
  };
  if (state?.isStreaming) command.streamingBehavior = "followUp";
  wsClient.send(command);
}

// State tracking
let currentStreamingElement = null;
// When the live assistant element was created; the footer shows the
// elapsed time at finalize (pi's events carry no duration of their own).
let currentStreamingStartedAt = null;
let currentStreamingText = "";
// True while pi's auto-retry is re-hitting the same model after a transient
// error (429/overload/5xx). During this window the session stays bound to the
// failing model, so switching models won't take effect until the stuck run is
// aborted. Tracked from `auto_retry_start` / `auto_retry_end` events.
let isAutoRetrying = false;
// True when the most recent assistant turn ended with stopReason "error"
// (e.g. a rate-limit 429). Cleared once a fresh run starts or succeeds.
let lastTurnErrored = false;
let lastInputTokens = 0;
let contextWindowSize = 0; // fetched from model info
const originalTitle = document.title;
let hasFocus = true;
let unreadCount = 0;
let isScrolledUp = false;
let lastSentMessage = null; // Track to avoid duplicate rendering in mirror mode
let lastUsage = null; // Full usage object for context visualiser
let contextVizController = null;
const compactCoordinator = createCompactCoordinator({
  // An RPC acknowledgement only confirms dispatch. Keep status ownership with
  // the lifecycle coordinator rather than briefly reporting it as completed.
  send: () => wsClient.send({ type: "compact" }),
  onState: () => syncCompactControls(),
});
let mirrorActiveSessionFile = null; // The live session file path from the TUI
let viewingActiveSession = true; // Whether we're viewing the live session or a historical one
let isMirrorMode = false; // Set when mirror_sync received
let workspaceLaunchInProgress = false;
// When true, the next foreground message lifecycle events should reload the
// sidebar until the newly persisted session file appears in the list.
let pendingNewSessionRefresh = false;
let pendingNewSessionPreviousFile = null;
let newSessionRefreshPromise = null;
// A cross-workspace session switch changes host runtime scope asynchronously.
// Keep requested root until post-switch mirror snapshot confirms new session;
// loading earlier could read path against stale workspace and return 403.
let pendingFileBrowserWorkspace = null;
let sessionsLoaded = false;
// Serializes handleSessionSelect: the function is a long async sequence that
// mutates shared routing state (mirrorActiveSessionFile, viewingActiveSession).
// Two overlapping invocations (fast double-click on different sessions) would
// interleave their awaits and corrupt that state, so a second call queues behind the first.
let sessionSelectChain = Promise.resolve();
let deferredMirrorSync = null;
// A selection may render its saved JSONL before its pi process has completed a
// switch. Ignore the old process's same-port snapshot until it confirms the
// selected session, otherwise it clobbers the restored history (and its
// multi-turn navigator) with stale entries.
let pendingMirrorSessionFile = null;
// A fork's composer prefill, deferred until the post-fork mirror_sync has
// rebound activeUiSessionFile to the forked session file. Applying it right
// after the fork ack would be wiped by that snapshot's restoreSessionUiState
// (the new file's saved draft is empty), and persisting then would write the
// text into the pre-fork session's draft. previousSessionFile guards against
// consuming the prefill on an unrelated same-session snapshot.
let pendingPostSyncComposer = null;
// Set when a live Info-tree append failed (no cache / unknown parent /
// recalibration due); agent_end then falls back to a full get_session_tree
// sync instead of one per turn unconditionally.
// Sequence token for in-flight tree refreshes: stale responses are dropped
// (upstream's infoTreeSeq pattern).
let infoTreeSeq = 0;
let lastRenderedWelcomeWorkspacePath = null;
let foregroundWorkspacePath = "";
function logSessionRoute(label, details = {}) {
  console.debug(`[Session route] ${label}`, {
    activeSessionFile: sidebar?.activeSessionFile || null,
    mirrorActiveSessionFile,
    viewingActiveSession,
    isStreaming: state?.isStreaming,
    wsSessionId: wsClient?.sessionId || null,
    ...details,
  });
}
const workspaceIndicatorEl = document.createElement("div");
workspaceIndicatorEl.id = "workspace-indicator";
workspaceIndicatorEl.className = "pill workspace-indicator hidden";
workspaceIndicatorEl.title = "";
document
  .querySelector(".header-right")
  ?.insertBefore(workspaceIndicatorEl, document.querySelector("#context-viz"));

const gitBranchEl = document.createElement("div");
gitBranchEl.id = "git-branch-indicator";
gitBranchEl.className = "pill git-branch-indicator hidden";
gitBranchEl.title = t("git.currentBranch");
const gitBranchIcon = createIcon("git-info", { size: 14 });
if (gitBranchIcon) gitBranchEl.append(gitBranchIcon);
const gitBranchLabel = document.createElement("span");
gitBranchEl.append(gitBranchLabel);
document
  .querySelector(".header-right")
  ?.insertBefore(gitBranchEl, document.querySelector("#context-viz"));

function updateGitBranchIndicator(branch = "") {
  const name = typeof branch === "string" ? branch.trim() : "";
  if (!name) {
    gitBranchEl.classList.add("hidden");
    gitBranchLabel.textContent = "";
    return;
  }
  gitBranchEl.classList.remove("hidden");
  gitBranchLabel.textContent = name;
  gitBranchEl.title = t("git.branchName", { name });
}

function refreshGitBranch() {
  updateGitBranchIndicator(gitPanel.snapshot?.branch || "");
}

function updateWorkspaceIndicator(path = "") {
  const normalizedPath = typeof path === "string" ? path.trim() : "";
  if (!normalizedPath) {
    workspaceIndicatorEl.classList.add("hidden");
    workspaceIndicatorEl.textContent = "";
    workspaceIndicatorEl.title = "";
    if (typeof refreshHeaderOpenAppButton === "function") refreshHeaderOpenAppButton();
    return;
  }
  workspaceIndicatorEl.classList.remove("hidden");
  workspaceIndicatorEl.textContent = normalizedPath;
  workspaceIndicatorEl.title = normalizedPath;
  if (typeof refreshHeaderOpenAppButton === "function") refreshHeaderOpenAppButton();
}

function getCurrentWorkspacePath() {
  return foregroundWorkspacePath;
}

function workspacePathFromId(workspaceId) {
  if (typeof workspaceId !== "string") return "";
  return workspaceId.startsWith("workspace:") ? workspaceId.slice("workspace:".length) : "";
}

function renderWorkspaceWelcome({ force = false } = {}) {
  const workspacePath = getCurrentWorkspacePath();
  const welcomeVisible = Boolean(document.querySelector(".welcome"));
  if (!force && welcomeVisible && lastRenderedWelcomeWorkspacePath === workspacePath) {
    return;
  }
  messageRenderer.renderWelcome({ workspacePath });
  lastRenderedWelcomeWorkspacePath = workspacePath;
}

function hasAnySessionsLoaded() {
  return (
    Array.isArray(sidebar.projects) &&
    sidebar.projects.some(
      (project) => Array.isArray(project.sessions) && project.sessions.length > 0,
    )
  );
}

function setWorkspaceLaunchInProgress(inProgress) {
  workspaceLaunchInProgress = inProgress;
  if (addProjectBtn) {
    addProjectBtn.disabled = inProgress;
    addProjectBtn.setAttribute("aria-busy", inProgress ? "true" : "false");
  }
}

// File browser
const fileSidebar = document.getElementById("file-sidebar");
const fileSidebarToggle = document.getElementById("file-sidebar-toggle");
setButtonIcon(fileSidebarToggle, "panel-right", { size: 16 });
const fileSidebarClose = document.getElementById("file-sidebar-close");
const fileSidebarNewFile = document.getElementById("file-sidebar-new-file");
const fileSidebarNewFolder = document.getElementById("file-sidebar-new-folder");
const fileSidebarRefresh = document.getElementById("file-sidebar-refresh");
const fileSidebarPathRow = document.getElementById("file-sidebar-path-row");
const fileSidebarCopy = document.getElementById("file-sidebar-copy");
const fileSidebarToggleHidden = document.getElementById("file-sidebar-toggle-hidden");
const infoPanelRefresh = document.getElementById("info-panel-refresh");
const gitPanelRefresh = document.getElementById("git-panel-refresh");
const fileSidebarFinder = document.getElementById("file-sidebar-finder");
for (const [button, iconName, size] of [
  [fileSidebarClose, "x", 16],
  [fileSidebarNewFile, "file-plus", 16],
  [fileSidebarNewFolder, "folder-plus", 16],
  [fileSidebarRefresh, "refresh-cw", 16],
  [fileSidebarCopy, "copy", 16],
  [fileSidebarToggleHidden, "eye", 16],
  [infoPanelRefresh, "refresh-cw", 16],
  [gitPanelRefresh, "refresh-cw", 16],
  [fileSidebarFinder, "folder-open", 16],
  [document.getElementById("config-editor-close"), "x", 14],
]) {
  if (button) setButtonIcon(button, iconName, { size });
}
const fileSidebarInfoTab = document.getElementById("file-sidebar-info-tab");
const fileSidebarFilesTab = document.getElementById("file-sidebar-files-tab");
const fileSidebarGitTab = document.getElementById("file-sidebar-git-tab");
for (const [tab, iconName] of [
  [fileSidebarInfoTab, "circle-info"],
  [fileSidebarFilesTab, "folder"],
  [fileSidebarGitTab, "git-info"],
]) {
  const iconHost = tab?.querySelector(".file-sidebar-tab-icon");
  const icon = createIcon(iconName, { size: 14 });
  if (iconHost && icon) iconHost.replaceChildren(icon);
}
const fileList = document.getElementById("file-list");
const fileSidebarPath = document.getElementById("file-sidebar-path");
const gitPanelElement = document.getElementById("git-panel");
const gitClient = new GitClient({
  send: (message) => {
    // 1 === WebSocket.OPEN; the numeric literal avoids a live global lookup
    // that breaks in test environments where WebSocket is unstubbed.
    if (wsClient.ws && wsClient.ws.readyState === 1) {
      wsClient.ws.send(JSON.stringify(message));
    }
  },
});
let latestGitDiffRequest = null;
let latestGitDiffDescriptor = null;
let latestGitCommitDiffRequest = null;
let latestGitCommitDiffDescriptor = null;
const gitPanel = new GitPanel({
  container: gitPanelElement,
  fileList,
  client: gitClient,
  // The toolbar branch pill renders from the latest status snapshot; the
  // panel's onStatus hook fires on every setSnapshot.
  onStatus: () => refreshGitBranch(),
  openDiff: (entry) => filePreviewPanel.openDiff?.(entry),
  onDiffRequest: (requestId, descriptor) => {
    latestGitDiffRequest = requestId;
    latestGitDiffDescriptor = descriptor || null;
  },
  onHistoryDiffRequest: (requestId, descriptor) => {
    latestGitCommitDiffRequest = requestId;
    latestGitCommitDiffDescriptor = descriptor || null;
  },
});
const syncFileSidebarHiddenToggle = (showHidden) => {
  fileSidebarToggleHidden?.setAttribute("aria-pressed", String(showHidden));
};
// New file / new folder are honest about why they are unavailable: no project
// open, a mutation already in flight, or a host without the write data plane.
const fileBrowserActionButtons = [
  [fileSidebarNewFile, "files.newFile"],
  [fileSidebarNewFolder, "files.newFolder"],
];
const syncFileBrowserMutability = (canMutate) => {
  for (const [button, labelKey] of fileBrowserActionButtons) {
    if (!button) continue;
    button.disabled = !canMutate;
    // The tooltip carries the reason while the button is dead, and goes back to
    // naming the action when it is live.
    button.title = t(canMutate ? labelKey : "files.mutationsUnavailable");
  }
};

function absoluteWorkspacePath(relativePath) {
  const root = normalizeLocalPath(getCurrentWorkspacePath());
  if (!root) return relativePath;
  return relativePath === "." ? root : `${root.replace(/\/$/, "")}/${relativePath}`;
}

// Every workspace-file mutation crosses one boundary: the host does the
// filesystem work, the preview panel retargets its own tabs, and the tree only
// invalidates its own listings.
const workspaceFileMutations = {
  createEntry: (parentPath, name, kind) => transport.fileCreate(parentPath, name, kind),
  renameEntry: async (path, name) => {
    const result = await transport.fileRename(path, name);
    if (typeof result?.path === "string") {
      filePreviewPanel.renameFilePathPrefix(
        absoluteWorkspacePath(path),
        absoluteWorkspacePath(result.path),
      );
    }
    return result;
  },
  deleteEntry: async (path) => {
    const result = await transport.fileDelete(path);
    await filePreviewPanel.closeDeletedPathPrefix(absoluteWorkspacePath(path));
    return result;
  },
};
const fileBrowser = new FileBrowser(fileList, fileSidebarPath, messageInput, {
  onFileSelect: (filePath, metadata) => {
    void filePreviewPanel.openFile(filePath, metadata);
  },
  onShowHiddenChange: syncFileSidebarHiddenToggle,
  openPath: (filePath) => transport.openInApp(filePath),
  // Paths cross this boundary workspace-relative: the tree's keys, the host's
  // `list_files` argument and `file_create`'s parentPath are all the same
  // spelling, so no side re-derives the other's paths.
  listFiles: (path) => transport.listFiles(path),
  ...workspaceFileMutations,
  writesAvailable: () => nativeAvailable(),
  onMutabilityChange: syncFileBrowserMutability,
});
// i18n re-applies `data-i18n-title` on a locale change, which would overwrite
// the disabled reason; re-assert it.
onLocaleChange(() => syncFileBrowserMutability(fileBrowser.canMutate()));
const sideChatButton = document.getElementById("side-chat-btn");
const syncSideChatButton = ({ panelOpen, activeContent } = {}) => {
  const active = panelOpen === true && activeContent?.kind === "transient";
  sideChatButton?.setAttribute("aria-pressed", String(active));
};
const CHAT_MIN_FALLBACK_PX = 400;

/** The chat column's floor, read from the CSS var that owns the number. */
function chatMinPx() {
  const raw = getComputedStyle(document.documentElement).getPropertyValue("--chat-min");
  const px = Number.parseFloat(raw);
  return Number.isFinite(px) && px > 0 ? px : CHAT_MIN_FALLBACK_PX;
}

/**
 * How wide one side panel may become without squeezing the chat column below
 * `--chat-min`. Every panel's maximum is derived from this single authority, so
 * no combination of preview + sidebars can starve the conversation.
 */
function sidePanelMaxWidth(excludeEl) {
  const row = [sidebarEl, fileSidebar, document.getElementById("file-preview-panel")];
  const used = row
    .filter((el) => el && el !== excludeEl && !el.classList.contains("collapsed"))
    .reduce((sum, el) => sum + el.offsetWidth, 0);
  // 4px covers the row's two 2px resizer rails.
  return Math.max(200, window.innerWidth - chatMinPx() - used - 4);
}

const filePreviewPanel = new FilePreviewPanel({
  panel: document.getElementById("file-preview-panel"),
  resizer: document.getElementById("file-preview-resizer"),
  tabBar: document.getElementById("file-preview-tabs"),
  content: document.getElementById("file-preview-content"),
  mainContainer: document.querySelector(".main"),
  transport,
  maxWidthPx: () => sidePanelMaxWidth(document.getElementById("file-preview-panel")),
  onStateChange: syncSideChatButton,
  onOpenDesktop: (filePath) => {
    transport.openInApp(filePath).catch((error) => {
      console.error("[App] open in desktop failed:", error);
    });
  },
});
// Follow the agent's write-tool calls so the preview panel hot-reloads the
// file on disk (HTML iframes pick up edits without manual reopening).
// A burst of write tools (multi-file apply_patch, several writes in one
// turn) coalesces into one sidebar listing reload; only refresh a listing
// that is already rendered so a never-opened browser is not force-loaded.
let fileBrowserWorkspacePath = null;
let fileBrowserRefreshTimer = null;

function cancelFileBrowserRefresh() {
  if (fileBrowserRefreshTimer) {
    clearTimeout(fileBrowserRefreshTimer);
    fileBrowserRefreshTimer = null;
  }
}

function scheduleFileBrowserRefresh() {
  if (!fileBrowser.hasListing()) return;
  if (
    shouldSuppressFileBrowserRefresh({
      pendingWorkspace: pendingFileBrowserWorkspace,
      currentWorkspacePath: getCurrentWorkspacePath(),
      fileBrowserWorkspacePath,
    })
  ) {
    return;
  }
  cancelFileBrowserRefresh();
  fileBrowserRefreshTimer = setTimeout(() => {
    fileBrowserRefreshTimer = null;
    if (!fileBrowser.hasListing()) return;
    if (
      shouldSuppressFileBrowserRefresh({
        pendingWorkspace: pendingFileBrowserWorkspace,
        currentWorkspacePath: getCurrentWorkspacePath(),
        fileBrowserWorkspacePath,
      })
    ) {
      return;
    }
    fileBrowser.refresh().catch(() => {});
  }, 500);
}
const filePreviewFollow = createFilePreviewFollow({
  panel: filePreviewPanel,
  getWorkspacePath: async () => getCurrentWorkspacePath() || "",
  onWriteApplied: (rawPath) => {
    scheduleFileBrowserRefresh();
    state.addTurnWrite(rawPath);
  },
});

// ── Ephemeral chats (Side Chat + Quick Chat) + window close coordination ─────
function confirmEphemeralDiscard(_risks, _reason) {
  // Minimal confirmation; the full localized summary dialog lives in the close
  // coordinator for window close. Per-chat close uses this lightweight gate.
  return Promise.resolve(window.confirm(t("ephemeral.confirmDiscard")) ? "discard" : "cancel");
}

function showCloseSummaryDialog(_risk) {
  return Promise.resolve(window.confirm(t("ephemeral.confirmCloseSummary")) ? "discard" : "cancel");
}

const createEphemeralView = (runtime) =>
  new EphemeralChatView({
    runtime,
    kind: runtime.kind,
    toolsEnabled: runtime.kind === "side-chat",
    // Ephemeral chats share the window's owner workspace (served by the
    // main-session Pi), never the Quick Chat temporary cwd.
    getWorkspaceRoot: () => getCurrentWorkspacePath(),
    loadModelCatalog: () => configGateway.call("list_model_catalog"),
    configGateway,
    onOpenModelsSettings: () => openModelsSettings().catch(() => {}),
  });

// Side Chat defaults come from Pi's own settings — no main-session model
// inheritance (2026-09-30 decision).
const sideChatManager = new SideChatManager({
  transport,
  filePreviewPanel,
  confirmDiscard: confirmEphemeralDiscard,
  createView: createEphemeralView,
});
const quickChatDialog = new QuickChatDialog({
  transport,
  dialogRoot: document.getElementById("quick-chat-dialog-root"),
  chipRoot: document.getElementById("quick-chat-chip-root"),
  boundsElement: document.querySelector(".main"),
  confirmDiscard: confirmEphemeralDiscard,
  createView: createEphemeralView,
});
const windowCloseCoordinator = new WindowCloseCoordinator({
  transport,
  showSummaryDialog: showCloseSummaryDialog,
});
windowCloseCoordinator.registerParticipant("file", filePreviewPanel);
windowCloseCoordinator.registerParticipant("side", sideChatManager);
windowCloseCoordinator.registerParticipant("quick", quickChatDialog);

// ── Terminal panel (native owner only) ──────────────────────────────────────
// The host owns every PTY; the WebView owns xterm rendering. The panel is gated
// on native capability, so LAN/mobile clients render no terminal surface.

// ── Appearance preferences (Settings → Appearance) ─────────────────────
// The cookie cache is read synchronously at bootstrap for first paint; here
// we lift any legacy per-origin terminal preferences once, then keep every
// appearance setting on the cookie + DB dual-track like ui.theme.
migrateLegacyTerminalPreferences(typeof localStorage !== "undefined" ? localStorage : null);
const savedAppearance = loadAppearanceCookie();
let chatFontSizeLevel = savedAppearance.chatFontSize;
let previewFontSizeLevel = savedAppearance.previewFontSize;
let previewThemeMode = savedAppearance.previewTheme;
let terminalFontSizeLevel = savedAppearance.terminalFontSize;
// The inline bootstrap already set the CSS variables pre-paint; re-apply here
// so the CodeMirror highlight palette matches before any editor is created.
applyAppearanceDom();
// WebGL renderer is opt-in: absent preference falls back to the platform
// default (ON on macOS/Linux, OFF on Windows — see defaultWebglRenderer).
// Mutable: the Appearance page toggle updates these live.
let webglRendererEnabled =
  typeof savedAppearance.terminalWebglRenderer === "boolean"
    ? savedAppearance.terminalWebglRenderer
    : defaultWebglRenderer();
// Terminal color scheme: "system" follows the Picot theme; "light"/"dark"
// force canonical palettes. Default dark.
let terminalThemeMode = normalizeThemeMode(savedAppearance.terminalThemeMode);
let terminalFontSize = TERMINAL_FONT_SIZE_PX[terminalFontSizeLevel];
let terminalScrollbackLimit = normalizeScrollbackLimit(savedAppearance.terminalScrollbackLimit);
let terminalSmoothScrollDuration = normalizeSmoothScrollDuration(
  savedAppearance.terminalSmoothScrollDuration,
);
const terminalClient = new TerminalClient({
  send: (envelope) => {
    if (wsClient.ws && wsClient.ws.readyState === WebSocket.OPEN) {
      try {
        wsClient.ws.send(JSON.stringify(envelope));
      } catch (err) {
        console.warn("[terminal] send failed:", err);
      }
    }
  },
  createTab: (terminalId, generation) =>
    new TerminalTab({
      terminalId,
      generation,
      container: terminalPanel.getTabContainer(terminalId),
      terminalFactory: () =>
        new globalThis.PicotXterm.Terminal({
          fontFamily: TERMINAL_FONT_STACK,
          fontSize: terminalFontSize,
          fontWeight: 400,
          scrollback: terminalScrollbackLimit,
          smoothScrollDuration: terminalSmoothScrollDuration,
        }),
      fontFamily: TERMINAL_FONT_FAMILY,
      fontSize: terminalFontSize,
      loadFont: () =>
        loadTerminalFont({
          family: TERMINAL_FONT_FAMILY,
          fontSize: terminalFontSize,
        }),
      fitAddonFactory: () => new globalThis.PicotXterm.FitAddon(),
      serializeAddonFactory: () => new globalThis.PicotXterm.SerializeAddon(),
      searchAddonFactory: () => new globalThis.PicotXterm.SearchAddon(),
      unicode11AddonFactory: () => new globalThis.PicotXterm.Unicode11Addon(),
      webglAddonFactory: webglRendererEnabled ? () => new globalThis.PicotXterm.WebglAddon() : null,
      initialTheme: resolveTerminalTheme(terminalThemeMode),
      sendInput: (id, gen, b64) =>
        terminalClient.command({
          type: "terminal_input",
          terminalId: id,
          generation: gen,
          dataBase64: b64,
        }),
      sendResize: (id, gen, cols, rows) =>
        terminalClient.command({
          type: "terminal_resize",
          terminalId: id,
          generation: gen,
          cols,
          rows,
        }),
    }),
});
const terminalPanel = new TerminalPanel({
  native: nativeAvailable(),
  subscribeLocale: onLocaleChange,
  getAvailableHeight: () =>
    document.querySelector(".workspace")?.clientHeight ||
    document.querySelector(".workspace-content")?.clientHeight ||
    600,
  client: {
    create: (profileId) => terminalClient.command({ type: "terminal_create", profileId }),
    close: (id, gen) =>
      terminalClient.command({ type: "terminal_close", terminalId: id, generation: gen }),
    restart: (id, gen, profileId) =>
      terminalClient.command({
        type: "terminal_restart",
        terminalId: id,
        generation: gen,
        profileId,
      }),
    focusTab: (id) => terminalClient.tabs.get(id)?.tab?.focus?.(),
    refitTab: (id) => terminalClient.tabs.get(id)?.tab?.refit?.(),
    refitAll: () => {
      for (const entry of terminalClient.tabs.values()) entry.tab?.refit?.();
    },
    setPanelHeight: (height) =>
      terminalClient.command({ type: "terminal_set_panel_height", heightPx: height }),
    checkpointAll: async () => {
      const pending = [];
      for (const [id, entry] of terminalClient.tabs) {
        const serialized = entry.tab.serializeForCheckpoint?.(2000);
        if (!serialized) continue;
        pending.push(
          terminalClient.sendAndAwait(
            {
              type: "terminal_checkpoint",
              terminalId: id,
              generation: entry.generation,
              watermark: entry.lastAppliedSequence,
              snapshotBase64: encodeTerminalBase64(new TextEncoder().encode(serialized)),
            },
            (msg) => msg.type === "terminal_checkpoint_acked" && msg.terminalId === id,
          ),
        );
      }
      const results = await Promise.all(pending);
      if (results.some((result) => result === null)) {
        throw new Error("terminal checkpoint timed out");
      }
    },
    closeAll: async () => {
      const ids = [...terminalClient.tabs.keys()];
      // Await each host-acknowledged close (terminal_closed) instead of polling
      // the local cache; a timeout resolves null so Rust kill_owner still wins.
      const results = await Promise.all(
        ids.map(async (id) => {
          const entry = terminalClient.tabs.get(id);
          if (!entry) return null;
          return terminalClient.sendAndAwait(
            { type: "terminal_close", terminalId: id, generation: entry.generation },
            (msg) => msg.type === "terminal_closed" && msg.terminalId === id,
          );
        }),
      );
      if (results.some((result) => result === null)) {
        throw new Error("terminal close timed out");
      }
      terminalClient.reset();
      terminalPanel.setTabs([]);
    },
  },
});
// Find bar for the terminal panel. Mounted lazily together with the panel
// (mountTerminalPanelIfNative) and opened via Cmd/Ctrl+F from the global
// keydown handler when the terminal has focus.
const terminalSearch = new TerminalSearch({
  getActiveTab: () => terminalClient.tabs.get(terminalPanel.activeTerminalId)?.tab ?? null,
});
// The panel is mounted only after the broker confirms native capability:
// `transport.capabilities.native` is false before the WS handshake completes,
// so mounting at module top would skip it on the desktop. The event listeners
// are registered once; only the DOM mount is gated on the capability.
function mountTerminalPanelIfNative() {
  if (!nativeAvailable() || terminalPanel.toggleEl) {
    return;
  }
  const workspaceEl = document.querySelector(".workspace") || document.body;
  const headerEl = document.querySelector(".workspace .header") || workspaceEl;
  const toolbarEl = headerEl.querySelector(".header-right") || headerEl;
  const sideChatToggle = toolbarEl.querySelector("#side-chat-btn");
  const fileSidebarToggle = toolbarEl.querySelector("#file-sidebar-toggle");
  if (sideChatToggle && fileSidebarToggle) {
    toolbarEl.insertBefore(sideChatToggle, fileSidebarToggle);
  }
  terminalPanel.native = true;
  terminalPanel.mount({ toggleContainer: toolbarEl, panelContainer: workspaceEl });
  terminalSearch.mount(terminalPanel.root);
  if (fileSidebarToggle && terminalPanel.toggleEl) {
    toolbarEl.insertBefore(terminalPanel.toggleEl, fileSidebarToggle);
  }
  windowCloseCoordinator.registerParticipant("terminal", terminalPanel);
}
wsClient.addEventListener("terminalEvent", (event) => {
  const msg = event.detail || {};
  // Async PTY events arrive wrapped in a `terminal_event` envelope.
  if (msg.type === "terminal_event") {
    const payload = msg.payload || {};
    if (payload.type === "terminal_output") {
      terminalClient.applyOutput(payload);
      terminalPanel.markActivity(payload.terminalId);
    } else if (payload.type === "terminal_exited" || payload.type === "terminal_failed") {
      terminalClient.removeTab(payload.terminalId, payload.generation);
      terminalClient.command({ type: "terminal_list" });
    }
    return;
  }
  // Synchronous command responses (terminal_created/listed/closed/...).
  terminalClient.resolveResponse(msg);
  if (msg.type === "terminal_listed") {
    terminalClient.applyListed(msg);
    terminalPanel.setTabs(
      (msg.tabs || []).map((tab) => ({
        terminalId: tab.terminalId,
        generation: tab.generation,
        label: tab.label,
        profileId: tab.profileId,
        status: tab.status,
        historyGap: tab.historyGap,
        failReason: tab.failReason,
      })),
    );
    if (Number.isFinite(msg.panelHeightPx)) {
      terminalPanel.setHeight(msg.panelHeightPx);
    }
  } else if (
    msg.type === "terminal_created" ||
    msg.type === "terminal_restarted" ||
    msg.type === "terminal_closed"
  ) {
    terminalClient.command({ type: "terminal_list" });
  }
});
wsClient.addEventListener("terminalCommandFailed", (event) => {
  const message = event.detail?.error;
  const commandType = terminalClient.consumeCommandType(event.detail?.requestId);
  console.warn("[terminal] command failed:", event.detail);
  if (
    (commandType === "terminal_create" || commandType === "terminal_restart") &&
    typeof message === "string" &&
    message
  ) {
    terminalPanel.showStartError?.(formatTerminalStartError(message));
  }
  terminalClient.requestList();
});
mountTerminalPanelIfNative();

async function prepareEphemeralWorkspaceTransition() {
  quickChatDialog.setInteractionLocked(true);
  filePreviewPanel.setInteractionLocked(true);
  try {
    const accepted = await sideChatManager.prepareWorkspaceTransition();
    if (accepted) {
      sideChatManager.setInteractionLocked(true);
      await terminalPanel.beforeWorkspaceTransition?.();
    } else {
      quickChatDialog.setInteractionLocked(false);
      filePreviewPanel.setInteractionLocked(false);
    }
    return accepted;
  } catch (error) {
    sideChatManager.setInteractionLocked(false);
    quickChatDialog.setInteractionLocked(false);
    filePreviewPanel.setInteractionLocked(false);
    throw error;
  }
}

function cancelEphemeralWorkspaceTransition() {
  sideChatManager.setInteractionLocked(false);
  quickChatDialog.setInteractionLocked(false);
  filePreviewPanel.setInteractionLocked(false);
  terminalPanel.cancelWorkspaceTransition?.();
}

wsClient.addEventListener("ownerBootstrap", async (event) => {
  if (!nativeAvailable()) return;
  // The host advertises the current workspace generation; terminal commands
  // carry it as a compare-only attachment token (broker rejects a mismatch).
  if (typeof event.detail?.workspaceGeneration === "number") {
    terminalClient.setWorkspaceGeneration(event.detail.workspaceGeneration);
    gitClient.setWorkspaceGeneration(event.detail.workspaceGeneration);
    // The generation is now known: (re)probe status unconditionally. Covers
    // both a panel-open click whose probe was swallowed pre-bootstrap and a
    // mirror sync that outran owner_bootstrap on a hidden panel.
    // Clear stale diff/AI/commit state so late responses from the previous
    // workspace cannot leak into the new one.
    latestGitDiffRequest = null;
    latestGitDiffDescriptor = null;
    latestGitCommitDiffRequest = null;
    latestGitCommitDiffDescriptor = null;
    gitPanel.aiSnapshot = null;
    gitPanel.commitMessage = "";
    gitPanel.pendingConfirmationToken = null;
    gitPanel.historyPanel?.clearSession();
    gitPanel.notGitRepo = false;
    gitPanel.gitUnavailable = false;
    // Push state belongs to the workspace that started it: a failure from the
    // previous one must not render as this one's.
    gitPanel.pushError = null;
    gitPanel.pushResult = null;
    gitPanel.pushInProgress = false;
    gitPanel.pendingPushRequestId = null;
    void gitPanel.refresh();
    if (terminalPanel.toggleEl) terminalClient.requestList();
  }
  try {
    const advertised = event.detail?.instances;
    const instances = Array.isArray(advertised)
      ? advertised
      : (await transport.getEphemeralBootstrap()) || [];
    sideChatManager.rebind(instances.filter((d) => d.kind === "side-chat"));
    const quick = instances.find((d) => d.kind === "quick-chat");
    if (quick) quickChatDialog.rebind(quick);
  } catch (err) {
    console.warn("[ephemeral] bootstrap fetch failed:", err);
  }
});
// Unified generation guard: every async Git response (status, AI, commit, diff,
// confirmation, failure, and write acknowledgement) must match the current
// workspace generation. A late response from a previous workspace is silently
// dropped so it can never overwrite the current UI state.
const gitResponseMatchesGeneration = (detail) =>
  detail?.workspaceGeneration != null && gitClient.generation === detail.workspaceGeneration;
wsClient.addEventListener("gitStatus", (event) => {
  if (!gitResponseMatchesGeneration(event.detail)) return;
  gitPanel.setSnapshot(event.detail.snapshot);
  // A successful status probe proves this workspace is a Git repository, so
  // the Git entry is usable even if the workspace-info probe was inconclusive.
  syncGitTabVisibility(true);
});
wsClient.addEventListener("gitAiCommitMessage", (event) => {
  if (!gitResponseMatchesGeneration(event.detail)) return;
  // Out-of-order guard: a user can request AI twice in quick succession. The
  // older request may resolve after the newer one and would otherwise
  // overwrite the current dialog message and bound snapshot with stale data.
  if (event.detail?.requestId !== gitPanel.pendingAiRequestId) return;
  gitPanel.applyAiResult(event.detail?.snapshot, event.detail?.message);
});
wsClient.addEventListener("gitCommitConfirmationRequired", (event) => {
  if (!gitResponseMatchesGeneration(event.detail)) return;
  // A stale confirmation frame from commit A must not pollute the pending
  // token of the currently-active commit B.
  if (event.detail?.requestId !== gitPanel.pendingCommitRequestId) return;
  gitPanel.applyConfirmationToken(event.detail?.confirmationToken);
});
wsClient.addEventListener("gitCommitStarted", (event) => {
  if (!gitResponseMatchesGeneration(event.detail)) return;
  // A delayed started frame from a superseded commit must not flip the current
  // dialog back into the in-progress state.
  if (event.detail?.requestId !== gitPanel.pendingCommitRequestId) return;
  gitPanel.setCommitInProgress(true);
});
wsClient.addEventListener("gitDiff", (event) => {
  const requestId = event.detail?.requestId;
  const diff = event.detail?.diff;
  if (!diff || !requestId || requestId !== latestGitDiffRequest) return;
  if (!gitResponseMatchesGeneration(event.detail)) return;
  filePreviewPanel.openDiff({ ...latestGitDiffDescriptor, ...diff });
});
wsClient.addEventListener("gitLog", (event) => {
  if (!gitResponseMatchesGeneration(event.detail)) return;
  gitPanel.historyPanel?.applyLog(event.detail);
});
wsClient.addEventListener("gitLogDetail", (event) => {
  if (!gitResponseMatchesGeneration(event.detail)) return;
  gitPanel.historyPanel?.applyLogDetail(event.detail);
});
wsClient.addEventListener("gitCommitDiff", (event) => {
  const requestId = event.detail?.requestId;
  const diff = event.detail?.diff;
  if (!diff || requestId !== latestGitCommitDiffRequest) return;
  if (!gitResponseMatchesGeneration(event.detail)) return;
  filePreviewPanel.openDiff({ ...latestGitCommitDiffDescriptor, ...diff });
});
wsClient.addEventListener("gitCommitResult", (event) => {
  if (!gitResponseMatchesGeneration(event.detail)) return;
  // A result frame from commit A must never close or clear the dialog state
  // of a later commit B. Bind every result to the currently-pending request;
  // applyCommitResult clears pendingCommitRequestId on acceptance.
  if (event.detail?.requestId !== gitPanel.pendingCommitRequestId) return;
  gitPanel.applyCommitResult(event.detail);
  if (event.detail?.status === "succeeded") void gitPanel.refresh();
});
wsClient.addEventListener("gitCommandAck", (event) => {
  if (!gitResponseMatchesGeneration(event.detail)) return;
  if (!gitClient.consumeWriteAck(event.detail)) return;
  void gitPanel.refresh();
});
wsClient.addEventListener("gitPush", (event) => {
  if (!gitResponseMatchesGeneration(event.detail)) return;
  if (!gitClient.consumePushOutcome(event.detail)) return;
  gitPanel.applyPushResult({ status: "succeeded", ...event.detail?.outcome });
  // A push moves the branch's ahead/behind counts; refresh so the panel does
  // not keep showing the pre-push divergence.
  void gitPanel.refresh();
});
wsClient.addEventListener("gitPushFailed", (event) => {
  if (!gitResponseMatchesGeneration(event.detail)) return;
  if (!gitClient.consumePushOutcome(event.detail)) return;
  gitPanel.applyPushResult({ status: "failed", error: event.detail?.error });
});
wsClient.addEventListener("gitCommandFailed", (event) => {
  console.warn("[git] command failed", event.detail);
  if (!gitResponseMatchesGeneration(event.detail)) return;
  gitClient.consumeWriteFailure(event.detail);
  gitPanel.historyPanel?.handleFailure(event.detail?.requestId);
  // Only AI generation failure opens the commit dialog with an empty message so
  // the user can still write a commit message by hand.
  if (event.detail?.type === "git_ai_commit_message_failed") {
    // Out-of-order guard: ignore a stale AI failure that belongs to a request
    // the user has already superseded with a newer one.
    if (event.detail?.requestId !== gitPanel.pendingAiRequestId) return;
    gitPanel.applyAiFailure(event.detail?.error || t("git.aiFailed"));
    return;
  }
  // A commit failure (stale snapshot, hook rejection, invalid token) must end
  // the in-progress state so the dialog is retryable, with the error surfaced.
  if (event.detail?.requestId && event.detail.requestId === gitPanel.pendingCommitRequestId) {
    gitPanel.applyCommitFailure(event.detail?.error);
  }
  // A status probe against a non-repository workspace fails with git's
  // "not a git repository" error and never produces a git_status frame, so the
  // panel would otherwise sit on the generic "no status loaded" message.
  // Surface the real reason instead — but only for the current status probe,
  // never for stale or concurrent non-status failures.
  if (gitPanel.isStatusFailure(event.detail?.requestId)) {
    // Either failure proves the workspace has no readable branch: the stale
    // pill from a previous workspace must not survive the probe.
    updateGitBranchIndicator("");
    const error = typeof event.detail?.error === "string" ? event.detail.error : "";
    if (error === "git_not_found") {
      gitPanel.setGitUnavailable(true);
      syncGitTabVisibility(false);
    } else if (error.includes("not a git repository")) {
      gitPanel.setNotGitRepo(true);
      // The workspace is not a Git repository — remove the Git entry entirely
      // (and bounce out of the Git tab when it was active) instead of keeping a
      // dead panel that only renders the not-a-repository notice.
      syncGitTabVisibility(false);
    }
  }
});
wsClient.addEventListener("ephemeralEvent", (event) => {
  const { instanceId } = event.detail || {};
  const side = sideChatManager.chats.get(instanceId)?.runtime;
  if (side) {
    side.applySequencedEvent(event.detail);
    return;
  }
  if (quickChatDialog.runtime?.instanceId === instanceId) {
    quickChatDialog.runtime.applySequencedEvent(event.detail);
  }
});
wsClient.addEventListener("ephemeralCommandFailed", (event) => {
  const requestId = event.detail?.requestId;
  for (const chat of sideChatManager.chats.values()) {
    chat.runtime.handleCommandFailure(requestId);
  }
  quickChatDialog.runtime?.handleCommandFailure(requestId);
});
wsClient.addEventListener("windowCloseRequest", (event) => {
  windowCloseCoordinator.handleHostCloseRequest(event.detail?.requestId);
});

sideChatButton?.addEventListener("click", () => {
  if (nativeAvailable()) void sideChatManager.openMostRecent();
});
setButtonIcon(sideChatButton, "message-square", { size: 16 });
syncSideChatButton(filePreviewPanel);
document.getElementById("quick-chat-btn")?.addEventListener("click", () => {
  if (nativeAvailable()) void quickChatDialog.open();
});

async function refreshFileBrowserForWorkspace(
  path = getCurrentWorkspacePath(),
  { force = false } = {},
) {
  // During a cross-workspace session switch the host remains scoped to the
  // previous workspace until mirror snapshot confirms the new session. Defer
  // workspace-scoped loads until `handleMirrorSync` confirms the new scope.
  if (shouldSuppressFileBrowserLoad(pendingFileBrowserWorkspace)) return true;
  const normalized = typeof path === "string" ? path.trim() : "";
  if (!force && normalized === fileBrowserWorkspacePath) {
    return true;
  }
  const switched = await filePreviewPanel.setWorkspaceRoot(normalized);
  if (!switched) return false;

  const workspaceChanged = normalized !== fileBrowserWorkspacePath;
  if (workspaceChanged) fileBrowser.setWorkspaceRoot(normalized);
  const isCollapsed = fileSidebar.classList.contains("collapsed");
  if (isCollapsed && !force) {
    fileBrowserWorkspacePath = normalized;
    return true;
  }
  await fileBrowser.load();
  fileBrowserWorkspacePath = normalized;
  return true;
}

fileSidebarRefresh?.addEventListener("click", () => void fileBrowser.refresh());
fileSidebarToggleHidden?.addEventListener("click", () => {
  void fileBrowser.setShowHidden(!fileBrowser.showHidden);
});
infoPanelRefresh?.addEventListener("click", () => void refreshInfoTree());
gitPanelRefresh?.addEventListener("click", () => void gitPanel.refresh());

// The open/close of either side panel moves the preview panel without
// resizing it, so the native browser pane it hosts would keep a stale rect:
// ResizeObserver reports size, never position. Announce the settled layout so
// the preview panel can re-push the rect. One class observer covers every
// entry point (header toggle, tab pill, close button, restore-at-boot).
{
  const cancelSettle = new WeakMap();
  const announceLayoutSettled = (element) => {
    cancelSettle.get(element)?.();
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      element.removeEventListener("transitionend", onTransitionEnd);
      cancelSettle.delete(element);
      window.dispatchEvent(new Event("picot-layout-settled"));
    };
    const onTransitionEnd = (event) => {
      if (event.target === element && event.propertyName === "margin-right") finish();
    };
    const timer = setTimeout(finish, 350);
    element.addEventListener("transitionend", onTransitionEnd);
    cancelSettle.set(element, () => {
      settled = true;
      clearTimeout(timer);
      element.removeEventListener("transitionend", onTransitionEnd);
    });
  };
  for (const element of [fileSidebar, sidebarEl]) {
    new MutationObserver(() => announceLayoutSettled(element)).observe(element, {
      attributes: true,
      attributeFilter: ["class"],
    });
  }
}

fileSidebarToggle.addEventListener("click", () => {
  const isCollapsed = fileSidebar.classList.toggle("collapsed");
  fileSidebarToggle.setAttribute("aria-pressed", String(!isCollapsed));
  if (!isCollapsed) {
    refreshFileBrowserForWorkspace(getCurrentWorkspacePath(), { force: true });
  }
  localStorage.setItem("pi-studio-file-sidebar", isCollapsed ? "closed" : "open");
});

const FILE_SIDEBAR_TAB_STORAGE_KEY = "pi-studio-file-sidebar-tab";

function setFileSidebarTab(tab) {
  const showInfo = tab === "info";
  const showGit = tab === "git";
  fileSidebarInfoTab.classList.toggle("active", showInfo);
  fileSidebarInfoTab.setAttribute("aria-selected", String(showInfo));
  fileSidebarFilesTab.classList.toggle("active", !showInfo && !showGit);
  fileSidebarFilesTab.setAttribute("aria-selected", String(!showInfo && !showGit));
  fileSidebarGitTab.classList.toggle("active", showGit);
  fileSidebarGitTab.setAttribute("aria-selected", String(showGit));
  infoPanelEl.classList.toggle("hidden", !showInfo);
  fileSidebarPathRow.classList.toggle("hidden", showInfo || showGit);
  fileList.classList.toggle("hidden", showInfo || showGit);
  gitPanelElement.classList.toggle("hidden", !showGit);
  fileSidebarNewFile.classList.toggle("hidden", showInfo || showGit);
  fileSidebarNewFolder.classList.toggle("hidden", showInfo || showGit);
  fileSidebarRefresh.classList.toggle("hidden", showInfo || showGit);
  fileSidebarToggleHidden.classList.toggle("hidden", showInfo || showGit);
  infoPanelRefresh.classList.toggle("hidden", !showInfo);
  gitPanelRefresh.classList.toggle("hidden", !showGit);
  document.getElementById("file-sidebar-finder").classList.toggle("hidden", showInfo || showGit);
  if (showInfo) {
    infoPanel.updateSessionFile(activeSessionFilePath());
    taskAnalysis?.refresh();
    void refreshInfoTree();
    infoPanel.scrollToSelectedEntry();
  }
  localStorage.setItem(FILE_SIDEBAR_TAB_STORAGE_KEY, tab);
  if (showGit) {
    // Clear any stale snapshot from a previous workspace so the panel
    // never shows old data while waiting for a fresh status response.
    gitPanel.snapshot = null;
    gitPanel.notGitRepo = false;
    gitPanel.gitUnavailable = false;
    gitPanel.render();
    void gitPanel.refresh();
  }
}

// ── Git entry visibility ────────────────────────────────────────────
// Hide the file-sidebar Git tab for workspaces that are not Git
// repositories, so a non-Git project never shows a dead Git panel. The tab is
// driven by the authoritative, generation-guarded git status probe
// (gitStatus / git_command_failed on workspace entry and on open). The
// /api/workspace-info probe is only a seq-guarded fast-path hint: it hides the
// tab on an explicit non-Git answer and never forces the tab visible.
function syncGitTabVisibility(show) {
  fileSidebarGitTab?.classList.toggle("hidden", !show);
  // Bounce out of the Git tab when hiding it, so a non-Git workspace never
  // leaves the user staring at the empty panel.
  if (!show && fileSidebarGitTab?.getAttribute("aria-selected") === "true") {
    setFileSidebarTab("files");
  }
}

// Fast-path hint for the Git entry: when the workspace-info probe answers, it
// nudges the tab toward the right state, but it is never the source of truth.
// A non-OK / inconclusive probe leaves the tab as-is so the authoritative git
// status probe (gitPanel.refresh on workspace entry) decides. A stale response
// from a previous workspace (seq guard) must not re-show a just-hidden tab.
let gitEntryProbeSeq = 0;
// Last workspace path whose git status was probed on this page. A fresh
// page (session-click navigation) starts this at "", so the first foreground
// snapshot always re-probes even for the same workspace path.
let lastGitProbePath = "";
async function syncGitEntryForWorkspace(workspaceId) {
  if (!workspaceId) return;
  const seq = ++gitEntryProbeSeq;
  try {
    const data = await transport.workspaceInfo();
    if (seq !== gitEntryProbeSeq) return; // superseded by a newer workspace
    if (seq !== gitEntryProbeSeq) return;
    // Only an explicit non-Git answer hides the entry; anything inconclusive
    // (missing flag) defers to the git status probe.
    if (data && data.isGit === false) syncGitTabVisibility(false);
  } catch {
    // Failed fast-path: leave the tab alone; the git status probe drives it.
  }
}

/**
 * Expand the file sidebar and switch to the requested tab. Used by the
 * workspace-path and git-branch header pills so clicking them directly opens
 * the matching panel.
 *
 * The click is always an "open and focus" action — never a toggle. Even when
 * the sidebar was already open on the target tab we re-trigger the refresh
 * and flash the tab so the click is acknowledged rather than feeling inert.
 */
function openFileSidebarTab(tab) {
  fileSidebar.classList.remove("collapsed");
  fileSidebarToggle.setAttribute("aria-pressed", "true");
  localStorage.setItem("pi-studio-file-sidebar", "open");
  setFileSidebarTab(tab);
  if (tab === "files") {
    void refreshFileBrowserForWorkspace(getCurrentWorkspacePath(), { force: true });
  }
  flashSidebarTab(tab);
}

function flashSidebarTab(tab) {
  const el =
    tab === "info" ? fileSidebarInfoTab : tab === "git" ? fileSidebarGitTab : fileSidebarFilesTab;
  if (!el) return;
  el.classList.remove("flash-highlight");
  // Reflow restarts the animation so repeated clicks re-flash.
  void el.offsetWidth;
  el.classList.add("flash-highlight");
  el.addEventListener("animationend", () => el.classList.remove("flash-highlight"), { once: true });
}

workspaceIndicatorEl.addEventListener("click", () => openFileSidebarTab("files"));
gitBranchEl.addEventListener("click", () => openFileSidebarTab("git"));

fileSidebarInfoTab.addEventListener("click", () => setFileSidebarTab("info"));
fileSidebarFilesTab.addEventListener("click", () => setFileSidebarTab("files"));
fileSidebarGitTab.addEventListener("click", () => setFileSidebarTab("git"));

fileSidebarClose.addEventListener("click", () => {
  fileSidebar.classList.add("collapsed");
  fileSidebarToggle.setAttribute("aria-pressed", "false");
  localStorage.setItem("pi-studio-file-sidebar", "closed");
});

fileSidebarNewFile?.addEventListener("click", () => fileBrowser.beginCreate("file"));
fileSidebarNewFolder?.addEventListener("click", () => fileBrowser.beginCreate("directory"));

document.getElementById("file-sidebar-finder").addEventListener("click", () => {
  const target = fileBrowser.getRevealTarget();
  if (target) {
    transport.openInApp(target).catch((error) => {
      console.error("[App] reveal in file manager failed:", error);
    });
  }
});

// ═══════════════════════════════════════
// "Open workspace in app" header control (VS Code / Cursor / Terminal / …)
// Mirrors the Codex-style split button in the chat header.
// ═══════════════════════════════════════
// Shared controller for every "open workspace in app" surface: the header
// split-button AND the Info panel's Workspace rows. Per the Info panel design
// invariant, app list loading, icon/monogram fallback, selection persistence,
// and the launch call live in ONE place (workspace-actions.js). Header keeps
// only its own chrome: button refresh + the dropdown menu.
let infoPanel = null; // assigned below; callbacks here may fire first
const workspaceActions = createWorkspaceActionsController({
  transport,
  isNativeAvailable: () => nativeAvailable(),
  getWorkspacePath: getCurrentWorkspacePath,
  storageKey: "pi-studio-open-app",
  onSelectionChange: () => refreshHeaderOpenAppButton(),
  onAppsLoaded: () => refreshHeaderOpenAppButton(),
});
// The file sidebar's copy button rides the same controller as the header — one
// implementation of "copy this workspace's path" across surfaces.
let fileSidebarCopyTimer = null;
fileSidebarCopy?.addEventListener("click", async () => {
  const copied = await workspaceActions.copyWorkspacePath();
  const label = copied ? t("files.copiedPath") : t("files.copyFailed");
  fileSidebarCopy.title = label;
  fileSidebarCopy.setAttribute("aria-label", label);
  clearTimeout(fileSidebarCopyTimer);
  fileSidebarCopyTimer = setTimeout(() => {
    const reset = t("files.copyAbsolutePath");
    fileSidebarCopy.title = reset;
    fileSidebarCopy.setAttribute("aria-label", reset);
  }, 1200);
});
const headerOpenApp = {
  el: document.getElementById("header-open-app"),
  btn: document.getElementById("header-open-app-btn"),
  logo: document.getElementById("header-open-app-logo"),
  toggle: document.getElementById("header-open-app-toggle"),
  menu: document.getElementById("header-open-app-menu"),
};
setButtonIcon(headerOpenApp.toggle, "chevron-down", { size: 10 });

function getSelectedOpenApp() {
  return workspaceActions.getSelectedApp();
}

function refreshHeaderOpenAppButton() {
  if (!headerOpenApp.el) return;
  const hasNative = nativeAvailable();
  const path = getCurrentWorkspacePath();
  const selected = getSelectedOpenApp();
  if (!hasNative || !selected || !path || workspaceActions.apps.length === 0) {
    headerOpenApp.el.classList.add("hidden");
    return;
  }
  headerOpenApp.el.classList.remove("hidden");
  sharedPopulateAppLogo(headerOpenApp.logo, selected);
  headerOpenApp.btn.title = t("nav.openWorkspaceInNamedApp", { path, app: selected.label });
  headerOpenApp.btn.setAttribute(
    "aria-label",
    t("nav.openWorkspaceInAppAria", { app: selected.label }),
  );
}

async function openWorkspaceInApp(app) {
  // The shared controller owns selection persistence and the transport call.
  await workspaceActions.openWorkspaceInApp(app);
}

function closeHeaderOpenAppMenu() {
  if (headerOpenApp.menu) headerOpenApp.menu.classList.add("hidden");
}

function toggleHeaderOpenAppMenu() {
  if (!headerOpenApp.menu) return;
  if (!headerOpenApp.menu.classList.contains("hidden")) {
    closeHeaderOpenAppMenu();
    return;
  }
  headerOpenApp.menu.replaceChildren();
  for (const app of workspaceActions.apps) {
    const row = document.createElement("button");
    row.type = "button";
    row.className = "header-open-app-menu-item";
    if (app.id === workspaceActions.selectedId) row.classList.add("active");
    row.title = t("nav.openInApp", { app: app.label });
    row.setAttribute("aria-label", t("nav.openInApp", { app: app.label }));
    const logo = document.createElement("span");
    logo.className = "header-open-app-logo";
    logo.setAttribute("aria-hidden", "true");
    sharedPopulateAppLogo(logo, app);
    const label = document.createElement("span");
    label.textContent = app.label;
    row.append(logo, label);
    row.addEventListener("click", (ev) => {
      ev.stopPropagation();
      closeHeaderOpenAppMenu();
      void openWorkspaceInApp(app);
    });
    headerOpenApp.menu.appendChild(row);
  }
  headerOpenApp.menu.classList.remove("hidden");
}

async function loadHeaderOpenApps() {
  // The shared controller loads, validates selection, and notifies listeners
  // (header refresh + Info panel row refresh).
  await workspaceActions.loadApps();
}

if (headerOpenApp.btn) {
  headerOpenApp.btn.addEventListener("click", (e) => {
    e.stopPropagation();
    void openWorkspaceInApp();
  });
}
if (headerOpenApp.toggle) {
  headerOpenApp.toggle.addEventListener("click", (e) => {
    e.stopPropagation();
    toggleHeaderOpenAppMenu();
  });
}
document.addEventListener("click", () => closeHeaderOpenAppMenu());
void loadHeaderOpenApps();

// ═════════════════════════════════════
// Info panel (right column): workspace actions + Pi session tree
// ═════════════════════════════════════

const infoPanelEl = document.getElementById("info-panel");

/** Correlated request/response over the shared WS command channel. */
function wsRequest(command, timeoutMs = 15000) {
  // All runtime requests use v2 correlation. No legacy broker fallback exists.
  return wsClient.sendRuntime(command, { timeoutMs });
}

/** Authoritative light tree refresh (entries + leafId only, no chat re-render).
 * Upstream contract: prefer pi's live get_entries — the full tree WITH pi's
 * authoritative active leaf, so Resume/Edit flip the panel's active branch
 * immediately (the file's tip-derived leaf still points at the abandoned
 * branch until a new message lands). The host data plane reading the session
 * file is the fallback when the runtime cannot serve. Both sources return
 * the complete tree (verified against pi 0.84.2: get_entries ≈ full file). */
async function refreshInfoTree() {
  if (!infoPanel || !infoPanelEl || infoPanelEl.classList.contains("hidden")) return;
  if (wsClient.ws?.readyState !== 1) return;
  const target = wsClient.getRuntimeTarget();
  if (!target?.workspaceId || !target?.sessionId) return;
  const seq = ++infoTreeSeq;
  try {
    const data = await wsRequest({ type: "get_entries" });
    if (seq !== infoTreeSeq) return;
    // An empty result is not a tree (fresh spawn, wrong-instance routing):
    // fall through to the file instead of wiping the panel.
    if (Array.isArray(data?.entries) && data.entries.length > 0) {
      sessionDebug.tree = {
        source: "runtime",
        entries: data.entries.length,
        session: target.sessionId,
        at: Date.now(),
      };
      infoPanel.updateTree({ entries: data.entries, leafId: data.leafId ?? null });
      return;
    }
    throw new Error("Runtime get_entries returned no entries");
  } catch (runtimeError) {
    try {
      const response = await wsClient.sendData("read_session_tree", {
        workspaceId: target.workspaceId,
        sessionId: target.sessionId,
      });
      if (seq !== infoTreeSeq) return;
      const entries = response?.tree?.entries;
      if (Array.isArray(entries) && entries.length > 0) {
        sessionDebug.tree = {
          source: "session-file",
          entries: entries.length,
          session: target.sessionId,
          at: Date.now(),
        };
        infoPanel.updateTree({
          entries,
          leafId: response?.tree?.leafId ?? null,
        });
        return;
      }
      throw new Error("read_session_tree returned no entries");
    } catch (fileError) {
      // A session with neither a live tree nor a file (brand-new chat) has
      // no tree anywhere: an empty panel, not a warning per sync.
      if (
        String(runtimeError?.message || runtimeError).includes("not found") ||
        String(fileError?.message || fileError).includes("not found")
      ) {
        infoPanel.updateTree({ entries: [], leafId: null });
        return;
      }
      console.warn("[InfoPanel] tree refresh failed:", runtimeError, fileError);
    }
  }
}

/** After an explicit tree navigation (Resume / Edit), pi's active branch is
 * the authority — but the session file's last-message tip chain still points
 * at the OLD branch until a new message lands, so the disk fallback would
 * resurrect it. Re-anchor from pi's live flat entries: walk the new leaf's
 * ancestor path, rebuild the transcript source WITH entry ids (locate keeps
 * working), and let the snapshot render's tie-break keep this render. */
async function reanchorAfterTreeNavigation() {
  const target = wsClient.getRuntimeTarget();
  if (!target?.workspaceId || !target?.sessionId) return;
  diskHistory = null;
  try {
    const live = await wsRequest({ type: "get_entries" });
    const entries = Array.isArray(live?.entries) ? live.entries : [];
    const leafId = typeof live?.leafId === "string" ? live.leafId : null;
    if (entries.length > 0 && leafId) {
      const byId = new Map(entries.filter((e) => typeof e?.id === "string").map((e) => [e.id, e]));
      const activeIds = new Set();
      let cursor = byId.get(leafId);
      while (cursor && !activeIds.has(cursor.id)) {
        activeIds.add(cursor.id);
        cursor = typeof cursor.parentId === "string" ? byId.get(cursor.parentId) : undefined;
      }
      const pathEntries = entries.filter((e) => activeIds.has(e?.id));
      diskHistory = {
        sessionId: target.sessionId,
        messages: pathEntries
          .filter((e) => e.type === "message" && e.message)
          .map((e) => ({ ...e.message, entryId: e.id })),
      };
      renderTranscriptEntries(pathEntries, { leafId });
      // The panel's active path must follow pi's live leaf: read_session_tree
      // derives its leaf from the file tip, which still points at the
      // abandoned branch until a new message lands on the resumed one.
      infoPanel?.updateTree({ entries, leafId });
    }
  } catch (_error) {
    // Live entries unavailable (e.g. temporary runtime): the plain snapshot
    // below still renders the correct branch, just without anchors.
  }
  wsClient.requestSnapshot();
}

/** Resume an inactive branch: explicit Pi-native leaf switch, never a scroll. */
async function handleResumeBranch(entryId) {
  if (state.isStreaming) {
    messageRenderer.renderError(t("infoPanel.actionWhileStreaming"));
    return;
  }
  if (!canUseSessionControl()) {
    messageRenderer.renderError(t("infoPanel.actionDesktopOnly"));
    return;
  }
  try {
    // Upstream contract: tree navigation rides the /picot-config bridge
    // (extension → pi ctx.navigateTree). A native `navigate_tree` RPC does
    // not exist in pi 0.84.2.
    await bridgeData("navigate_tree", { targetId: entryId, summarize: false });
    // Per the design, resuming clears the composer because the old branch's
    // unsent input does not belong to the resumed branch. C4: the branch's
    // stored draft must go too, or C3's restore paths could resurrect it.
    messageInput.value = "";
    messageInput.style.height = "auto";
    void composerDraftStore.clear(composerIdentity());
    await reanchorAfterTreeNavigation();
  } catch (err) {
    messageRenderer.renderError(t("errors.treeNavigateFailed", { error: err }));
  }
}

// Passive per-turn trace: reads runtime frames the app already receives, so
// nothing is re-run and no model is asked anything. Spans are keyed per
// runtime target; the analysis panel merges this live trace with turns
// rebuilt from the saved session file (see loadHistoryTurnsForTarget).
const turnTrace = createTurnTraceRecorder();

/**
 * Rebuild this session's earlier turns for the task debugger. Same two
 * sources as the Info panel tree, for the same reason: pi owns the live
 * entry list, and the saved file answers when the runtime cannot. A session
 * that never persisted a file has no history turns - only live ones.
 */
async function loadHistoryTurnsForTaskDebugger() {
  const target = wsClient.getRuntimeTarget();
  const sessionId = target?.sessionId;
  if (!target?.workspaceId || !sessionId) return [];
  try {
    const data = await wsRequest({ type: "get_entries" });
    if (Array.isArray(data?.entries) && data.entries.length > 0) {
      if (wsClient.getRuntimeTarget()?.sessionId !== sessionId) return [];
      return buildTurnsFromEntries(data.entries, { target, leafId: data.leafId ?? null });
    }
    throw new Error("Runtime get_entries returned no entries");
  } catch {
    if (!mirrorActiveSessionFile) return [];
  }
  const response = await wsClient
    .sendData("read_session_tree", { workspaceId: target.workspaceId, sessionId })
    .catch(() => null);
  if (wsClient.getRuntimeTarget()?.sessionId !== sessionId) return [];
  return buildTurnsFromEntries(response?.tree?.entries ?? [], {
    target,
    leafId: response?.tree?.leafId ?? null,
  });
}

// analyzeWithAi stays null: v3 has no throwaway-session spawn surface, and
// the mechanical timing/failure report needs no model round-trip.
// Task-completion OS notifications. The enabled flag is DB-backed
// (Settings → General, preferences table); the sync cache avoids an async
// round-trip per runtime frame.
let taskNotificationsEnabled = true;
const taskNotifications = createTaskNotifications(wsClient, () => taskNotificationsEnabled);

const taskAnalysis = infoPanelEl
  ? createSessionTaskAnalysis({
      getTurns: () => turnTrace.getTurns(wsClient.getRuntimeTarget()),
      loadHistoryTurns: loadHistoryTurnsForTaskDebugger,
      t,
    })
  : null;

if (infoPanelEl) {
  // Created before the InfoPanel that mounts its section element.
  infoPanel = new InfoPanel({
    panel: infoPanelEl,
    t,
    onNavigateLeaf: (entryId) => void handleResumeBranch(entryId),
    onSelectEntry: (entryId) => selectInfoEntry(entryId),
    isStreaming: () => state.isStreaming,
    // P2: a gate-folded turn reveals before the Info row locates its anchor.
    ensureEntryMounted: (entryId) => ensureEntryMounted(entryId),
    taskAnalysis,
  });
}

// Restore file sidebar state
const fileSidebarIsOpen = localStorage.getItem("pi-studio-file-sidebar") === "open";
fileSidebarToggle.setAttribute("aria-pressed", String(fileSidebarIsOpen));
if (fileSidebarIsOpen) {
  fileSidebar.classList.remove("collapsed");
  // Restore the previously selected sidebar tab (Files or Git) so a reopen
  // after close returns to the view the user left on. Defaults to Files.
  setFileSidebarTab(
    localStorage.getItem(FILE_SIDEBAR_TAB_STORAGE_KEY) === "git"
      ? "git"
      : localStorage.getItem(FILE_SIDEBAR_TAB_STORAGE_KEY) === "info"
        ? "info"
        : "files",
  );
  if (!gitPanelElement.classList.contains("hidden")) {
    // setFileSidebarTab already requests Git status when the Git tab is
    // restored; for Files, refresh the browser as before.
  } else {
    refreshFileBrowserForWorkspace(getCurrentWorkspacePath(), { force: true });
  }
}

// Resizable sidebars — drag handle on inner edge, persisted to localStorage.
// maxWidth is a function: the cap moves with the window and with the sibling
// panels, so the chat column keeps --chat-min no matter how the panels are set.
createMainSidebarResizer(sidebarEl, {
  maxWidth: () => Math.min(500, sidePanelMaxWidth(sidebarEl)),
});
createSidebarResizer({
  sidebarEl: fileSidebar,
  side: "right",
  storageKey: "picot-file-sidebar-width",
  minWidth: 200,
  maxWidth: () => Math.min(500, sidePanelMaxWidth(fileSidebar)),
});

// ═══════════════════════════════════════
// Focus tracking for tab title notifications
// ═══════════════════════════════════════

window.addEventListener("focus", () => {
  hasFocus = true;
  unreadCount = 0;
  document.title = originalTitle;
});

window.addEventListener("blur", () => {
  hasFocus = false;
});

// Reconnect WebSocket when returning to the app (iOS suspends WS connections)
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible" && wsClient.ws?.readyState !== WebSocket.OPEN) {
    console.log("[App] Returning to app, reconnecting...");
    wsClient.forceReconnect();
  }
});

// ═══════════════════════════════════════
// Conversation navigator rail (Codex-style)
// ═══════════════════════════════════════

// Build the list of conversations: each entry is the user message el + its
// immediately following assistant message el (may be null mid-stream).
// ── Conversation navigator rail (spec P5) ─────────────────────────────────
// Registry-sourced turns replace the DOM walk: the rail lists every turn of
// the session regardless of what is mounted, settled turns carry their answer
// preview again (the DOM pairing was dead), and one keyboard-reachable
// listbox owns the hit surface with windowed, constant-pitch ticks.
const turnRegistry = new Map();

function registerTurn(entry) {
  const existing = turnRegistry.get(entry.id) || {};
  turnRegistry.set(entry.id, {
    ...existing,
    ...entry,
    promptPreview: entry.promptPreview ?? existing.promptPreview ?? "",
    answerPreview: entry.answerPreview ?? existing.answerPreview ?? "",
    entryId: entry.entryId ?? existing.entryId ?? null,
    mountedElement: entry.mountedElement ?? existing.mountedElement ?? null,
  });
  convNav?.refresh();
}

function registryTurns() {
  return [...turnRegistry.values()];
}

/** Reveal turns so that turn index `turnIdx` is mounted; shared by the rail
 *  jump seam and the Info-panel entry seam. No-op when the gate is open. */
function revealTurnsUpTo(turnIdx) {
  const gate = historyGate;
  if (!gate?.control?.isConnected || !gate.renderTurn) return;
  if (turnIdx < 0 || turnIdx >= gate.turnCount) return;
  const needed = gate.turnCount - turnIdx; // turns from turnIdx to the end
  const previous = gate.revealedCount;
  gate.revealedCount = Math.max(gate.revealedCount, needed);
  const from = Math.max(0, gate.turnCount - gate.revealedCount);
  const to = gate.turnCount - previous;
  if (to > from) {
    const fragment = document.createDocumentFragment();
    for (let i = from; i < to; i++) gate.renderTurn(gate.turns[i], fragment);
    insertTurnFragmentBelowControl(fragment);
    updateHistoryGateControl();
  }
}

// P2/P5 seam: a folded turn reveals through the gate, then its mounted
// element resolves for the rail's jump. Registry-mounted turns short-circuit.
function ensureTurnMounted(turnId) {
  const entry = turnRegistry.get(turnId);
  if (entry?.mountedElement?.isConnected) return Promise.resolve(entry.mountedElement);
  const index = registryTurns().findIndex((turn) => turn.id === turnId);
  if (index >= 0) revealTurnsUpTo(index);
  return Promise.resolve(turnRegistry.get(turnId)?.mountedElement ?? null);
}

/** Entry-id seam: any entry (user, answer, folded retry) reveals its turn. */
function ensureEntryMounted(entryId) {
  const gate = historyGate;
  if (!gate?.control?.isConnected || !gate.renderTurn) return Promise.resolve();
  if (typeof entryId !== "string" || !entryId) return Promise.resolve();
  const idx = (gate.messageEntryIds ?? []).indexOf(entryId);
  if (idx < 0) return Promise.resolve();
  const turnIdx = gate.turns.findIndex(([s, e]) => idx >= s && idx < e);
  if (turnIdx >= 0) revealTurnsUpTo(turnIdx);
  return Promise.resolve();
}

const convNav = createConversationNav({
  navEl: convNavEl,
  trackEl: convNavTrack,
  tooltipEl: convNavTooltip,
  tooltipQEl: convNavTooltipQ,
  tooltipAEl: convNavTooltipA,
  tooltipSepEl: convNavTooltipSep,
  container: messagesContainer,
  headerEl,
  getTurns: registryTurns,
  ensureTurnMounted,
  onSelectTurn: (turn) => {
    if (turn.mountedElement?.isConnected) flashJumpHighlight(turn.mountedElement);
    if (turn.entryId) selectInfoEntry(turn.entryId, { scroll: true });
  },
  scrollOwner: messagesScrollOwner,
  t,
});

function jumpToPreviousUserMessage() {
  const idx = convNav.getActiveIndex();
  if (idx > 0) void convNav.jumpTo(idx - 1);
}

// Jumps to the next conversation's user message. At (or past) the last turn
// it scrolls all the way to the bottom instead — so the full final reply is
// visible without an extra click.
function jumpToNextConversationOrBottom() {
  const turns = registryTurns();
  const nextIdx = convNav.getActiveIndex() + 1;
  if (nextIdx <= turns.length - 1) {
    void convNav.jumpTo(nextIdx);
  } else {
    messagesScrollOwner.followBottom();
    scrollBottomBadge.classList.add("hidden");
  }
}

// ── Scroll wiring: the new-message badge stays distance-based; the rail
// owns its own coalesced spy (module) and refreshes via mutation/resize.
messagesContainer.addEventListener("scroll", () => {
  const threshold = 150;
  const atBottom =
    messagesContainer.scrollHeight - messagesContainer.scrollTop - messagesContainer.clientHeight <
    threshold;
  isScrolledUp = !atBottom;
  if (atBottom) scrollBottomBadge.classList.add("hidden");
  // The jump-to-bottom button is position-driven only (Paseo semantics);
  // new messages stay the badge's job.
  scrollBottomBtn?.classList.toggle("hidden", atBottom);
});
window.addEventListener("resize", () => convNav.refresh());

function flashJumpHighlight(target) {
  target.classList.remove("message-jump-highlight");
  void target.offsetWidth; // force reflow so re-triggering the animation replays
  target.classList.add("message-jump-highlight");
  target.addEventListener("animationend", () => target.classList.remove("message-jump-highlight"), {
    once: true,
  });
}

function selectInfoEntry(entryId, { scroll = false } = {}) {
  infoPanel?.selectEntry(entryId);
  if (scroll) infoPanel?.scrollToSelectedEntry();
}

// ── Session fork via "Fork from here" button on user messages ──────────────
// Tool-card file references dispatch "previewfile" bubbles from the message
// container; open (or refresh) the file preview for that path.
messagesContainer.addEventListener("previewfile", (event) => {
  const path = event.detail?.path;
  if (path) void filePreviewFollow.openPath(path).catch(() => {});
});
// Turn file chips live in the transcript too; clicking one opens the file.
messagesContainer.addEventListener("click", (event) => {
  const row = event.target.closest(".turn-files-row");
  if (!row?.dataset.path) return;
  event.stopPropagation();
  void filePreviewFollow.openPath(row.dataset.path).catch(() => {});
});
messagesContainer.addEventListener("messagefork", async (e) => {
  const { entryId, text } = e.detail || {};
  if (!entryId) return;
  if (state.isStreaming) {
    messageRenderer.renderError(t("infoPanel.actionWhileStreaming"));
    return;
  }
  if (!canUseSessionControl()) {
    messageRenderer.renderError(t("infoPanel.actionDesktopOnly"));
    return;
  }
  const btn = e.target.closest(".message-fork-btn");
  if (btn) {
    btn.disabled = true;
    btn.classList.add("forking");
  }
  try {
    // pi forks natively in-place (same process/port) and emits
    // `session_start { reason: "fork" }`; the resulting mirror_sync snapshot
    // re-renders the forked history and updates routing. We only nudge the
    // sidebar so the new forked session file appears in the list.
    await wsRequest({ type: "fork", entryId });
    refreshSidebarAfterUserPrompt();
    // Design: after a fork the composer is prefilled with the forked user
    // message's text, awaiting edits — never auto-sent. Deferred to the
    // post-fork mirror_sync because the snapshot restores the new file's
    // (empty) draft after this ack, which would wipe an immediate prefill.
    if (typeof text === "string" && text) {
      pendingPostSyncComposer = { text, previousSessionFile: activeUiSessionFile };
      messageInput.focus();
    }
  } catch (err) {
    messageRenderer.renderError(t("errors.forkFailed", { error: err }));
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.classList.remove("forking");
    }
  }
});

// Edit-and-branch: pi's native /tree select on a user message — the leaf
// moves to that entry's parent and the original prompt lands in the composer;
// only the user's submit creates the sibling branch.
messagesContainer.addEventListener("messageedit", async (e) => {
  const { entryId, text } = e.detail || {};
  if (!entryId) return;
  if (state.isStreaming) {
    messageRenderer.renderError(t("infoPanel.actionWhileStreaming"));
    return;
  }
  if (!canUseSessionControl()) {
    messageRenderer.renderError(t("infoPanel.actionDesktopOnly"));
    return;
  }
  const btn = e.target.closest(".message-edit-btn");
  if (btn) btn.disabled = true;
  try {
    // Dispatches /picot-config navigate_tree → pi ctx.navigateTree(userEntry):
    // leaf = the user entry's parent, editorText = the original prompt. The
    // old branch's descendants become the Info tree's inactive branch.
    await bridgeData("navigate_tree", { targetId: entryId, summarize: false });
    if (typeof text === "string") {
      messageInput.value = text;
      messageInput.style.height = "auto";
      messageInput.focus();
    }
    await reanchorAfterTreeNavigation();
  } catch (err) {
    messageRenderer.renderError(t("errors.treeNavigateFailed", { error: err }));
    if (btn) btn.disabled = false;
  }
});

function showNewMessageBadge() {
  if (isScrolledUp) {
    scrollBottomBadge.classList.remove("hidden");
  }
}

// ═══════════════════════════════════════
// WebSocket event handlers
// ═══════════════════════════════════════

wsClient.addEventListener("connected", () => {
  updateConnectionStatus("connected");
  // Fetch model context window size for token % display
  setTimeout(fetchContextWindow, 1000);
  // Terminal reattachment is triggered after owner_bootstrap supplies the
  // current workspace generation. Sending before that token is known would
  // make a cross-workspace reload use generation zero.
});

wsClient.addEventListener("disconnected", () => {
  updateConnectionStatus("disconnected");
  sidebar.clearStreaming();

  // If the streaming state is still true 3 s after disconnect (pi likely
  // crashed — agent_end won't re-fire after reconnect), unlock the UI.
  // Brief intentional reconnects (Case 1 session switch) complete in < 100 ms
  // so they are unaffected by the 3-second gate.
  setTimeout(() => {
    if (wsClient.connectionState !== "open" && state.isStreaming) {
      state.setStreaming(false);
      showTypingIndicator(false);
      updateUI();
      // The run's agent_end will never re-fire; settle the live turn so no
      // spinner or 1s status timer survives the disconnect.
      if (activeTurn) settleLiveTurn();
    }
  }, 3000);
});

wsClient.addEventListener("reconnectFailed", () => {
  updateConnectionStatus("disconnected");
  messageRenderer.renderError(t("errors.connectionLost"));
});

wsClient.addEventListener("runtimeError", (e) => {
  const { requestId, code, message } = e.detail || {};
  if (!requestId || !promptDelivery.get(requestId)) return;
  promptDelivery.rejectByRequestId(requestId, { code, message });
});

// A task-completion OS notification was clicked: route this window to the
// finished session (cross-workspace switches ride handleSessionSelect's own
// transition logic).
wsClient.addEventListener("notificationActivated", (e) => {
  const { sessionFile, cwd } = e.detail || {};
  if (!sessionFile) return;
  // Re-selecting the already-active session would bump restore tokens and
  // can disturb a live turn; the click's only job is bringing it to view.
  if (wsClient.getRuntimeTarget()?.sessionId === sessionFile) return;
  if (wsClient.connectionState !== "open") return;
  try {
    void handleSessionSelect({ filePath: sessionFile, cwd: cwd || undefined });
  } catch (error) {
    console.error("[Notifications] failed to route to the finished session:", error);
  }
});

wsClient.addEventListener("runtimeEvent", (e) => {
  const frame = e.detail;
  if (!frame?.event) return;
  // M3 mutual exclusion: OAuth envelopes are consumed first and never reach
  // the config gateway or chat rendering; config responses follow.
  if (oauthGateway.consumeFrame(frame)) return;
  if (consumeConfigResponseFrame(configGateway, frame)) return;
  // Fired for foreground AND background runtimes: a task that finishes while
  // its window is not focused is exactly the case an OS notification covers.
  // Runs after the consume gates so OAuth/config envelopes never pair here.
  taskNotifications.handleRuntimeFrame(frame);
  handleRPCEvent({
    ...frame.event,
    __target: frame.target,
    __sequence: frame.sequence,
  });
});

// Any successful runtime round-trip to the CURRENT triple is the same
// liveness proof a foreground snapshot provides. Snapshots remain the
// primary opener, but rare orderings can strand a live runtime unproven —
// a snapshot classified background after a rapid switch, a restarted
// runtime whose flow never re-snapshots — and every config call then dies
// as "timed out waiting for runtime". Strict full-triple match: a reply
// bound to another runtime never opens this gate.
wsClient.addEventListener("controlResponse", (e) => {
  const detail = e.detail || {};
  if (detail.type !== "runtime_response" || detail.ok === false || !detail.target) return;
  const current = wsClient.getRuntimeTarget();
  if (
    current &&
    detail.target.workspaceId === current.workspaceId &&
    detail.target.sessionId === current.sessionId &&
    (detail.target.instanceId ?? "") === (current.instanceId ?? "")
  ) {
    configReadiness.noteForegroundSnapshot("runtime-response");
  }
});

wsClient.addEventListener("runtimeSnapshot", (e) => {
  const frame = e.detail;
  const snapshotState = frame?.state || {};
  const pi = snapshotState.pi && typeof snapshotState.pi === "object" ? snapshotState.pi : {};
  const snapshotTarget = frame?.target;
  const currentTarget = wsClient.getRuntimeTarget();
  const isBackgroundSnapshot =
    snapshotTarget &&
    currentTarget &&
    (snapshotTarget.workspaceId !== currentTarget.workspaceId ||
      snapshotTarget.sessionId !== currentTarget.sessionId ||
      snapshotTarget.instanceId !== currentTarget.instanceId);
  if (isBackgroundSnapshot) {
    const sessionFile = pi.sessionFile || snapshotTarget.sessionId;
    if (sessionFile) sidebar.setStreaming(sessionFile, Boolean(pi.isStreaming));
    return;
  }
  // First foreground snapshot proves the current runtime target is live:
  // release its deferred configuration reads and fail-fast stale waiters.
  configReadiness.noteForegroundSnapshot();
  handleMirrorSync({
    ...pi,
    workspaceId: frame.target?.workspaceId,
    // The host state machine is the authority on "this run is going": it flips
    // to Working on agent_start and back to Idle on agent_end/agent_settled.
    // Pi's own `isStreaming` is an instantaneous `!isIdle()` sample that can read
    // false in the gaps between messages and tool calls of a live run.
    lifecycle: snapshotState.lifecycle,
    // Preserve Pi's persisted session identity from `get_state`; route
    // runtime identity is separate and must never overwrite `sessionId`.
    instanceId: frame.target?.instanceId,
    sequence: frame.sequence,
    // Keep route runtime identity separate from Pi's persisted session file.
    // Native targets use `session-*`; Pi mirror state uses an absolute .jsonl
    // path. Mixing them makes a foreground snapshot look like background data.
    runtimeWorkspaceId: frame.target?.workspaceId,
    runtimeSessionId: frame.target?.sessionId,
    runtimeInstanceId: frame.target?.instanceId,
    // Pi's get_messages returns bare AgentMessage objects; the renderer
    // consumes session-file entries ({type:"message", message}). Without
    // this bridge every snapshot entry is dropped and the transcript stays
    // empty with no error.
    entries: (snapshotState.messages || []).map((message) => ({
      id: typeof message?.id === "string" ? message.id : null,
      parentId: typeof message?.parentId === "string" ? message.parentId : null,
      type: "message",
      message,
    })),
    messages: snapshotState.messages || [],
    stats: snapshotState.stats || {},
  });
});

// ═══════════════════════════════════════
// RPC event handlers
// ═══════════════════════════════════════

function handleRPCEvent(event) {
  const eventTarget = event?.__target || null;
  const currentTarget = wsClient.getRuntimeTarget();
  const eventBelongsToCurrentRuntime =
    !eventTarget ||
    !currentTarget ||
    (eventTarget.workspaceId === currentTarget.workspaceId &&
      eventTarget.sessionId === currentTarget.sessionId &&
      (!eventTarget.instanceId || eventTarget.instanceId === currentTarget.instanceId));
  const eventSessionFile =
    event?.sessionFile || (eventBelongsToCurrentRuntime ? mirrorActiveSessionFile : null) || null;
  const eventRuntimeId = runtimeIdForTarget(eventTarget || currentTarget);
  if (!eventBelongsToCurrentRuntime) {
    // Sidebar rows are keyed by the jsonl sessionFile, which pi's native
    // events never carry — resolve it from the remembered instance/mirror
    // mapping or the green dot keyed in the foreground can never be cleared.
    handleBackgroundRPCEvent(
      event?.sessionFile || backgroundSessionFiles.resolve(eventTarget),
      event,
      eventRuntimeId,
    );
    return;
  }
  if (eventTarget?.sessionId && mirrorActiveSessionFile) {
    // Learn the mapping while this runtime is foreground so a later
    // background agent_end can find the same row the foreground
    // agent_start lit up.
    backgroundSessionFiles.remember(eventTarget, mirrorActiveSessionFile);
  }

  // While user previews a different session, suppress live rendering so
  // history view is not overwritten by another runtime's output.
  widgetMirrorRegistry.handleRuntimeChange(eventRuntimeId);
  turnTrace.handleRuntimeFrame({ target: eventTarget || currentTarget, event });

  switch (event.type) {
    case "agent_start":
      handleAgentStart(event);
      taskAnalysis?.setStreaming(true);
      break;
    case "agent_end":
      handleAgentEnd(event);
      taskAnalysis?.setStreaming(false);
      if (pendingNewSessionRefresh) {
        scheduleNewSessionSidebarRefresh(event);
      }
      break;
    case "agent_settled":
      handleAgentSettled();
      taskAnalysis?.setStreaming(false);
      break;
    case "message_start":
      handleMessageStart(event.message);
      // Refresh the sidebar as soon as the new session is persisted. Pi writes
      // the brand-new session's .jsonl on the first user message round-trip, so
      // refreshing on the user message (not just the assistant turn) makes the
      // session — with its first message as the title — show up immediately.
      if (pendingNewSessionRefresh) {
        scheduleNewSessionSidebarRefresh(event);
      }
      break;
    case "message_update":
      handleMessageUpdate(event);
      break;
    case "message_end":
      handleMessageEnd(event.message, eventSessionFile, event.entryId);
      if (pendingNewSessionRefresh) {
        scheduleNewSessionSidebarRefresh(event);
      }
      break;
    case "tool_execution_start":
      handleToolExecutionStart(event);
      break;
    case "tool_execution_update":
      handleToolExecutionUpdate(event);
      break;
    case "tool_execution_end":
      handleToolExecutionEnd(event);
      break;
    case "compaction_start":
      handleCompactionStart();
      break;
    case "compaction_end":
      handleCompactionEnd(event);
      break;
    case "queue_update":
      piQueuePark.set(mirrorActiveSessionFile, event);
      renderPiQueue(event);
      break;
    case "runtime_stopped":
    case "runtime_crashed":
      // The process that held the queued messages is gone, so its pills must
      // not outlive it: returning to that session spawns a fresh runtime.
      piQueuePark.forget(eventSessionFile);
      renderPiQueue(piQueuePark.get(activeUiSessionFile));
      break;
    case "auto_retry_start":
      handleAutoRetryStart(event);
      break;
    case "auto_retry_end":
      handleAutoRetryEnd(event);
      break;
    case "extension_ui_request":
      handleExtensionUIRequest(event, eventRuntimeId);
      break;
    case "extension_error":
      messageRenderer.renderError(t("errors.extensionError", { error: event.error }));
      break;
    case "session_name":
      handleSessionNameEvent(event);
      break;
    case "session_tree":
      // Pi moved the active leaf natively (Info panel Resume / message Edit).
      // Re-sync from the authoritative snapshot: chat re-renders the new
      // active path and the Info tree flips active/inactive state.
      wsClient.requestSnapshot();
      break;
  }
}

function handleSessionNameEvent(event) {
  if (!event.name) return;
  const activeItem = document.querySelector(".session-item.active .session-title");
  if (activeItem) activeItem.textContent = event.name;
}

function handleBackgroundRPCEvent(sessionFile, event, runtimeId) {
  switch (event.type) {
    case "agent_start":
      sidebar.setStreaming(sessionFile, true);
      break;
    case "agent_end": {
      sidebar.setStreaming(sessionFile, false);
      sidebar.markUnread(sessionFile);
      break;
    }
    case "message_end":
      sidebar.markUnread(sessionFile);
      break;
    case "queue_update":
      // A backgrounded runtime can still dequeue a steer into its running turn:
      // park the report so the pills are truthful when the user returns.
      piQueuePark.set(sessionFile, event);
      break;
    case "runtime_stopped":
    case "runtime_crashed":
      // The host stops the runtime (window destroy, owner revoke, restart):
      // the green dot must not outlive its process, and neither may its queue.
      sidebar.setStreaming(sessionFile, false);
      piQueuePark.forget(sessionFile);
      break;
    case "tool_execution_start":
      // ask_user_question in a background session parks its card state; the
      // walker's requests queue until the user returns to that session.
      backgroundQuestionnaires.parkToolStart(sessionFile, runtimeId, event);
      break;
    case "tool_execution_end":
      // The parked questionnaire's tool finished elsewhere (answered, aborted,
      // or errored): its parked entry must not replay against a dead call.
      backgroundQuestionnaires.handleToolEnd(sessionFile, runtimeId, event.toolCallId);
      if (!event.isError) {
        widgetMirrorRegistry.handleToolResult(event.toolName, event.result, runtimeId);
      }
      break;
    case "extension_ui_request":
      if (event.method === "setWidget") {
        widgetMirrorRegistry.handleWidgetRequest(event, runtimeId);
      } else if (event.method === "notify") {
        widgetMirrorRegistry.handleCommandNotify(event.message, runtimeId);
      } else if (backgroundQuestionnaires.queueRequest(sessionFile, runtimeId, event)) {
        // Blocking walker request from a background runtime: queue it and
        // badge the session instead of popping a modal for the wrong session.
        if (sessionFile) sidebar.markUnread(sessionFile);
      }
      break;
  }
}

function handleCompactionStart() {
  compactCoordinator.started();
  const el = document.createElement("div");
  el.className = "system-message compaction-message";
  el.id = "compaction-indicator";
  const spinner = document.createElement("span");
  spinner.className = "compaction-spinner";
  spinner.textContent = "⟳";
  el.replaceChildren(spinner, document.createTextNode(` ${t("status.compacting")}`));
  messagesContainer.appendChild(el);
  scrollToBottom();
}

function handleCompactionEnd(event) {
  const succeeded = event.success !== false && !event.error;
  compactCoordinator.ended({ success: succeeded, error: event.error });
  const indicator = document.getElementById("compaction-indicator");
  if (succeeded) {
    if (indicator) {
      indicator.textContent = event.summary
        ? t("status.compactedWithSummary", { summary: event.summary })
        : t("status.compacted");
      indicator.classList.add("compaction-done");
    }
    // Pi has replaced its context; until it supplies usage again, the old
    // number is provably stale and must not remain visible.
    lastInputTokens = 0;
    lastUsage = null;
    headerStatusBar?.sync({ currentUsage: null });
    updateTokenUsage();
    return;
  }

  const error = event.error || event.summary || t("errors.compactionFailed");
  if (indicator) {
    indicator.textContent = t("errors.compactionFailedDetail", { error });
    indicator.classList.add("compaction-done");
  } else {
    messageRenderer.renderError(t("errors.compactionFailedDetail", { error }));
  }
}

/**
 * Refresh the sidebar after a brand-new session's first message round-trips.
 *
 * Pi only persists a new session's .jsonl on the first message round-trip, and
 * The session surface can briefly answer *successfully* without the new file yet
 * (loadSessions' built-in retry only covers fetch failures, not "fetched but
 * the row isn't there"). So we reload, and if the freshly created session still
 * isn't in the list, retry a few times with a short backoff before giving up.
 */
function scheduleNewSessionSidebarRefresh(event) {
  if (newSessionRefreshPromise) return newSessionRefreshPromise;
  newSessionRefreshPromise = refreshSidebarForNewSession(event).finally(() => {
    newSessionRefreshPromise = null;
  });
  return newSessionRefreshPromise;
}

async function refreshSidebarForNewSession(event = null, attempt = 0) {
  // Cached registry rows would mask the brand-new session file: refresh the
  // current workspace's cache first so the reload below can observe it.
  const workspacePath = getCurrentWorkspacePath();
  const projects = await sidebar.refresh({ workspacePath }).catch(() => null);

  const liveFile = getCurrentLiveSessionFile(event);
  if (liveFile) {
    // The native route session ID identifies the runtime, while a scanned row
    // identifies its JSONL by absolute filePath. Resolve either identity from
    // the response this refresh fetched; otherwise fresh rows never replace
    // the provisional row and their streaming indicator stays orphaned.
    const found = (projects || sidebar.projects)
      .flatMap((project) => project.sessions || [])
      .find((session) => session.filePath === liveFile || session.id === liveFile);
    if (found?.filePath) {
      if (sidebar.isStreaming(liveFile)) {
        sidebar.setStreaming(liveFile, false);
        sidebar.setStreaming(found.filePath, true);
      }
      // Rebind, not just null the state: the placeholder object was already
      // re-inserted into project.sessions by the refresh above and would
      // otherwise keep rendering next to the persisted row until the next
      // full list rebuild.
      sidebar.rebindProvisionalSession(found.filePath);
      sidebar.setActive(found.filePath);
      restoreSessionUiState(found.filePath);
      resolveAndApplyFocus();
      // The new session may not have had a stable identity when its first
      // assistant event arrived. Once the persisted file is known, hydrate
      // the authoritative aggregate for the new session.
      void hydrateHeaderSessionStats();
      pendingNewSessionRefresh = false;
      pendingNewSessionPreviousFile = null;
      return;
    }
  }

  if (attempt < 4) {
    await new Promise((resolve) => setTimeout(resolve, 250 * (attempt + 1)));
    return refreshSidebarForNewSession(event, attempt + 1);
  }
}

function getCurrentLiveSessionFile(event = null) {
  // Once mirror_sync has supplied Pi's persisted JSONL path, it is the UI
  // identity. The route target remains a runtime ID for fresh, unpersisted
  // sessions and is only a fallback for the provisional green indicator.
  return [mirrorActiveSessionFile, event?.__target?.sessionId].find(
    (file) => file && file !== pendingNewSessionPreviousFile,
  );
}

// ── Live turn orchestration (spec P1) ──────────────────────────────────────
// The main window renders each agent run into one <section class="turn">:
// optimistic user bubble + status header + rail (thinking/tools/demoted text)
// + final answer. This replaces collapseCompletedTurn()'s post-hoc surgery —
// content lands in the right slot as it arrives.
const TURNS_RENDERING = true; // opt-in; reverting restores flat rendering

let activeTurn = null;
let activeTurnStartedAt = null;
let pendingUserEl = null; // optimistic user bubble awaiting agent_start claim
let pendingUserKey = null; // runtime key the bubble was rendered under
let pendingPromptPreview = ""; // prompt text for the turn registry at open
let turnRailSteps = 0;
let turnRailToolCalls = 0;

function runtimeKeyOf(target) {
  if (!target?.workspaceId || !target?.sessionId) return "route";
  return `${target.workspaceId}/${target.sessionId}/${target.instanceId ?? "primary"}`;
}

function closeLiveTurn({ settled = false } = {}) {
  // A card hosted by this turn cannot outlive it: re-home a still-pending
  // blocker (approval or questionnaire) in the modal container before the
  // transcript drops the turn, or its runtime would wait on
  // extension_ui_response forever.
  safetyGuardDialog.rehost();
  questionnaireCard.rehost();
  if (!activeTurn) return;
  // A turn adopted mid-run can end before it produced anything (the run ended
  // while its session was coming back): an empty rail label, an empty answer
  // and a status row with no clock would all just leave gaps, so drop it.
  if (turnProducedNothing(activeTurn)) {
    activeTurn.status.destroy();
    activeTurn.element.remove();
    activeTurn = null;
    activeTurnStartedAt = null;
    pendingUserEl = null;
    pendingUserKey = null;
    return;
  }
  if (!settled) {
    // A turn that never saw agent_end (reconnect, session switch): settle it
    // with no duration rather than leaving a live spinner behind.
    activeTurn.status.setSettled(null);
  }
  activeTurn.rail.setLabel(summarizeTurnRail(turnRailSteps, turnRailToolCalls));
  activeTurn.rail.setDisclosure(false);
  activeTurn.status.destroy();
  activeTurn = null;
  activeTurnStartedAt = null;
  pendingUserEl = null;
  pendingUserKey = null;
}

function turnProducedNothing(turn) {
  return (
    turn.element.querySelector(".message.user") === null &&
    turn.rail.host.childElementCount === 0 &&
    turn.answer.host.childElementCount === 0 &&
    !turn.card.host?.childElementCount
  );
}

function openLiveTurn(event = null, { deferElapsed = false } = {}) {
  if (!TURNS_RENDERING) return;
  closeLiveTurn();
  // Adoption (a session returning to the front mid-run) has no known start:
  // Pi never re-emits agent_start, so the clock waits for the first live output
  // instead of counting from the moment we mounted the turn.
  activeTurnStartedAt = deferElapsed ? null : Date.now();
  activeTurn = createTurnSection({
    turnId: null,
    modelLabel: currentModelId || "",
    startedAt: activeTurnStartedAt,
  });
  // Claim the optimistic user bubble only when it was rendered under the
  // runtime this event belongs to; otherwise this is an assistant-origin turn
  // and the stale bubble stays flat (cleared on session switch/agent end).
  if (pendingUserEl?.isConnected) {
    const eventKey = runtimeKeyOf(event?.__target || wsClient.getRuntimeTarget());
    if (!pendingUserKey || !eventKey || pendingUserKey === eventKey) {
      activeTurn.claimUserElement(pendingUserEl);
    }
  }
  pendingUserEl = null;
  pendingUserKey = null;
  messagesElement.appendChild(activeTurn.element);
  activeTurn.status.setLive();
  turnRailSteps = 0;
  turnRailToolCalls = 0;
  registerTurn({
    id: activeTurn.id,
    promptPreview: pendingPromptPreview,
    mountedElement: activeTurn.element,
  });
  pendingPromptPreview = "";
}

/**
 * The first live output of a run whose turn was adopted counts from here: the
 * reader was elsewhere for the earlier part of the run, and Pi reports no run
 * start, so a clock that only counts what was watched never lies.
 */
function startAdoptedTurnClock() {
  if (!activeTurn || activeTurnStartedAt !== null) return;
  activeTurnStartedAt = Date.now();
  activeTurn.status.beginElapsed(activeTurnStartedAt);
}

/**
 * Adopting a session whose Pi run is still going must not read as a finished
 * transcript: history rendering has no status row and folds every rail
 * ("history rails render folded"), and Pi never re-emits `agent_start`, so
 * without a live turn the run's remaining output appends flat with no working
 * indicator — a run blocked on a question then looks dead. Its rail is empty
 * (this run's earlier rows are already persisted history above it); its clock
 * starts at the first live output.
 */
function maybeOpenAdoptedLiveTurn() {
  if (!state.isStreaming || activeTurn) return;
  openLiveTurn(null, { deferElapsed: true });
}

function settleLiveTurn() {
  if (!activeTurn) return;
  const duration = resolveTurnDurationMs({
    startedAt: activeTurnStartedAt,
    completedAt: Date.now(),
  });
  activeTurn.status.setSettled(duration);
  registerTurn({
    id: activeTurn.id,
    answerPreview: activeTurn.answer.host.textContent ?? "",
  });
  closeLiveTurn({ settled: true });
}

/**
 * Live expression of classifyTurnSegments: when a tool call starts, the
 * answer slot's open text segment is demoted into the rail (at most once per
 * element), preserving arrival order exactly like history rendering.
 */
function demoteAnswerSegmentIntoRail() {
  if (!activeTurn) return;
  const host = activeTurn.answer.host;
  const el = host.lastElementChild;
  if (!el?.classList?.contains("assistant") || el.dataset.turnDemoted) return;
  el.dataset.turnDemoted = "true";
  // Rail rows carry no toolbar and use the settled (history) styling — the
  // same contract history rail rows render under.
  el.querySelector(".message-actions")?.remove();
  el.classList.add("history");
  activeTurn.rail.host.appendChild(el);
  turnRailSteps += 1;
}

function handleAgentSettled() {
  // 兜底：agent_end 正常已处理；此处仅在其未到达（如重连后不再重发）时
  // 确保 streaming/typing 状态归位。幂等，无副作用（不重复通知/markUnread）。
  state.setStreaming(false);
  showTypingIndicator(false);
  const live = getCurrentLiveSessionFile();
  if (live) sidebar.setStreaming(live, false);
  // The turn's fold now happens structurally at settle time (P1): if
  // agent_end was missed, settle whatever turn is still open.
  if (activeTurn) settleLiveTurn();
  // Files-card fallback for a missed agent_end. settleTurnWrites() drains, so
  // whichever path runs first renders the card and the second sees an empty
  // list and returns: the two paths are idempotent by construction.
  void appendTurnFilesCard();
  // agent_settled is the authoritative "run fully done" signal (no retry,
  // compaction retry, or queued continuation remains): the local follow-up
  // queue drains here — never on the optimistic idle that abort applies.
  maybeDrainFollowUpQueue();
}

function handleAgentStart(event = null) {
  state.setStreaming(true);
  showTypingIndicator(true);
  openLiveTurn(event);
  // A dispatched follow-up's run began: the drain guard releases so the next
  // settled signal may drain the following item.
  followUpDrainInFlight = false;
  // A fresh run is under way — clear any prior error latch so a normal turn
  // isn't treated as "stuck on a failed model" by the model switcher.
  lastTurnErrored = false;
  updateUI();
  const live = getCurrentLiveSessionFile(event);
  if (live) sidebar.setStreaming(live, true);
}

// pi's auto-retry is re-hitting the SAME model after a transient error. The
// session is busy on the failing model during the backoff; surface that so the
// UI doesn't look idle and the model switcher knows to abort before switching.
function handleAutoRetryStart(event = null) {
  isAutoRetrying = true;
  lastTurnErrored = true;
  state.setStreaming(true);
  showTypingIndicator(true);
  const attempt = event?.attempt;
  const maxAttempts = event?.maxAttempts;
  if (attempt && maxAttempts) {
    statusText.textContent = `Retrying (${attempt}/${maxAttempts})...`;
  } else {
    statusText.textContent = "Retrying…";
  }
  updateUI();
}

function handleAutoRetryEnd(event = null) {
  isAutoRetrying = false;
  // Success clears the error latch; a final failure keeps it so the next model
  // switch aborts the dead run.
  if (event?.success) lastTurnErrored = false;
  updateUI();
}

function handleAgentEnd(event = null) {
  state.setStreaming(false);
  showTypingIndicator(false);
  currentStreamingElement = null;
  currentStreamingText = "";
  updateUI();

  const live = getCurrentLiveSessionFile(event);
  if (live) {
    sidebar.setStreaming(live, false);
    // If user is not currently viewing this session in the sidebar,
    // mark it as unread so they see a blue dot when they look back.
    if (live !== sidebar.activeSessionFile) {
      sidebar.markUnread(live);
    }
  }

  // Notify via tab title if unfocused
  if (!hasFocus) {
    unreadCount++;
    document.title = `(${unreadCount}) ● ${originalTitle}`;
  }

  // P1: the finished turn settles structurally — status header gets its
  // duration, the rail folds to its summary label. agent_end is the normal
  // completion path; handleAgentSettled covers a missed agent_end.
  settleLiveTurn();
  void appendTurnFilesCard();
}

let lastTurnAssistantElement = null;
/**
 * Turn files card (2026-09-19 spec): the turn's written files render as a
 * collapsed card after the answer, one row per file with frozen-at-turn-end
 * git stats. Stats come from ONE host query (working-tree cumulative); any
 * failure — non-git workspace, git error, transport down — degrades to a
 * plain file list, never blocking the transcript.
 */
async function appendTurnFilesCard() {
  const writes = state.settleTurnWrites();
  if (!writes.length) {
    lastTurnAssistantElement = null;
    return;
  }
  const host = lastTurnAssistantElement?.isConnected ? lastTurnAssistantElement : null;
  lastTurnAssistantElement = null;
  if (!host) return;
  let statsByPath = null;
  let statsUnavailable = false;
  try {
    const result = await transport.gitTurnStats(writes.map((entry) => entry.filePath));
    // The op echoes one entry per requested path, so a short or absent array is
    // a contract violation: a failed read, not "nothing changed".
    if (Array.isArray(result?.files) && result.files.length === writes.length) {
      statsByPath = new Map(result.files.map((file) => [file.path, file]));
    } else {
      statsUnavailable = true;
    }
  } catch {
    // Non-git workspace, git failure, or timeout: the card says the stats are
    // unavailable rather than looking like "nothing changed" (2026-09-19 spec).
    statsUnavailable = true;
  }
  const card = renderTurnFilesCard({ writes, statsByPath, statsUnavailable });
  if (card && host.isConnected) mountTurnFilesCard(host, card);
}

let currentStreamingThinking = "";

function handleMessageStart(message) {
  startAdoptedTurnClock();
  if (message.role === "assistant") {
    currentStreamingText = "";
    currentStreamingThinking = "";
    // P1: a live turn's new assistant segment opens in the answer slot.
    currentStreamingElement = messageRenderer.renderAssistantMessage(
      { content: "" },
      true,
      false,
      activeTurn?.answer?.host ?? null,
    );
  } else if (message.role === "user") {
    if (!lastSentMessage || getMessageText(message) !== lastSentMessage) {
      const content = getMessageText(message);
      const images = getMessageImages(message);
      if (content || images.length > 0) {
        const echoEl = renderNavigableUserMessage({ content, images, timestamp: Date.now() });
        // A steer renders no optimistic bubble, and pi emits agent_start BEFORE
        // the user echo (probe: 0.4s agent_start, 0.4s message_start(user)). So
        // this echo is the open turn's only user row: claim it into the turn,
        // above status/rail/answer, or the prompt would render below the very
        // answer it steered.
        if (echoEl && activeTurn) {
          if (!activeTurn.element.querySelector(".message.user")) {
            activeTurn.claimUserElement(echoEl);
          } else {
            // Delivered INSIDE the same run (a follow-up or steer drained
            // mid-turn): pi sends no agent_start for it, so this user message is
            // the turn boundary. Without a fresh turn the next task's answer
            // keeps appending to the previous turn's answer slot and this prompt
            // strands below it.
            // The previous task did finish here, so settle it with its real
            // duration rather than leaving its status row blank.
            settleLiveTurn();
            openLiveTurn(null);
            activeTurn?.claimUserElement(echoEl);
          }
        }
      }
    }
    lastSentMessage = null;
  }
}

function getMessageText(message) {
  if (typeof message.content === "string") return message.content;
  if (Array.isArray(message.content)) {
    return message.content
      .filter((b) => b.type === "text")
      .map((b) => b.text)
      .join("\n");
  }
  return "";
}

function getMessageImages(message) {
  if (!Array.isArray(message?.content)) return [];
  return message.content
    .filter((block) => block?.type === "image")
    .map((block) => ({
      data: block.source?.data || block.data || "",
      mimeType: block.source?.media_type || block.media_type || "image/png",
    }));
}

function renderNavigableUserMessage({ content, images, isHistory = false, timestamp }) {
  const element = messageRenderer.renderUserMessage(
    { content: content || "", images, timestamp },
    isHistory,
  );
  return element;
}

function getAssistantText(message) {
  if (typeof message?.content === "string") return message.content;
  if (!Array.isArray(message?.content)) return "";
  return message.content
    .filter((block) => block.type === "text")
    .map((block) => block.text || "")
    .join("\n");
}

function getAssistantThinking(message) {
  if (!Array.isArray(message?.content)) return "";
  return message.content
    .filter((block) => block.type === "thinking")
    .map((block) => block.thinking || "")
    .join("\n");
}

function ensureStreamingAssistantElement(message = null) {
  if (currentStreamingElement) return currentStreamingElement;
  currentStreamingText = getAssistantText(message);
  currentStreamingThinking = getAssistantThinking(message);
  // P1: with a live turn open, the assistant segment renders into the turn's
  // answer slot; demotion moves it into the rail when a tool call follows.
  const turnHost = activeTurn?.answer?.host ?? null;
  currentStreamingElement = messageRenderer.renderAssistantMessage(
    { content: "" },
    true,
    false,
    turnHost,
  );
  currentStreamingStartedAt = Date.now();
  if (currentStreamingThinking) {
    if (activeTurn) {
      messageRenderer.renderStreamingThinkingInto(activeTurn.rail.host, currentStreamingThinking);
    } else {
      messageRenderer.updateStreamingThinking(currentStreamingElement, currentStreamingThinking);
    }
  }
  if (currentStreamingText) {
    messageRenderer.updateStreamingMessage(currentStreamingElement, currentStreamingText);
  }
  return currentStreamingElement;
}

function handleMessageUpdate(event) {
  startAdoptedTurnClock();
  const { assistantMessageEvent, message } = event;
  if (message?.role === "assistant") {
    ensureStreamingAssistantElement(message);
  }

  if (assistantMessageEvent.type === "thinking_delta") {
    currentStreamingThinking =
      getAssistantThinking(message) || currentStreamingThinking + assistantMessageEvent.delta;
    if (activeTurn) {
      // P1: thinking renders into the rail, never inside .message-content.
      messageRenderer.renderStreamingThinkingInto(activeTurn.rail.host, currentStreamingThinking);
    } else if (currentStreamingElement) {
      messageRenderer.updateStreamingThinking(currentStreamingElement, currentStreamingThinking);
    }
  } else if (assistantMessageEvent.type === "text_delta") {
    currentStreamingText =
      getAssistantText(message) || currentStreamingText + assistantMessageEvent.delta;
    if (currentStreamingElement) {
      messageRenderer.updateStreamingMessage(currentStreamingElement, currentStreamingText);
    }
  }
}

function handleMessageEnd(message, eventSessionFile = null, entryId = null) {
  if (message?.role === "user" && entryId) {
    // Pi persisted user entry and included leaf id. Tag live-rendered element
    // so Info-tree scroll and
    // Fork/Edit work immediately, before the next full snapshot.
    const untagged = messagesContainer.querySelector(".message.user:not([data-entry-id])");
    if (untagged) untagged.dataset.entryId = entryId;
  }
  // Live-turn Info-tree update (upstream contract): a persisted user or final
  // assistant message refreshes the OPEN panel from pi's live tree;
  // toolResult activity does not trigger a refresh.
  if (message?.role === "user" || message?.role === "assistant") {
    void refreshInfoTree();
  }
  if (message?.role === "assistant" && message?.stopReason === "error") {
    const provider = message?.provider ? String(message.provider) : "unknown";
    const model = message?.model ? String(message.model) : "unknown";
    const errorMessage = message?.errorMessage
      ? String(message.errorMessage)
      : t("errors.modelRequestFailed");
    messageRenderer.renderError(
      t("errors.modelRequestFailedDetail", { provider, model, message: errorMessage }),
    );
    // Latch the error so a subsequent model switch aborts the stuck run
    // (pi may still be auto-retrying this same failing model).
    lastTurnErrored = true;
  } else if (message?.role === "assistant") {
    lastTurnErrored = false;
  }
  if (!currentStreamingElement && message?.role === "assistant") {
    ensureStreamingAssistantElement(message);
  }
  if (currentStreamingElement) {
    // Pass usage info for cost display
    const usage = message?.usage || null;
    // P1: with a live turn open the rail already owns the thinking block —
    // finalize must not rebuild it inside .message-content (duplicate render
    // of the same thinking in rail AND answer). Flat rendering keeps it.
    messageRenderer.finalizeStreamingMessage(
      currentStreamingElement,
      usage,
      activeTurn ? "" : currentStreamingThinking,
      currentStreamingStartedAt === null ? null : Date.now() - currentStreamingStartedAt,
    );
    // Remember the turn's latest assistant element; agent_end settles the
    // turn's writes and renders the chips row under it (a turn may contain
    // several assistant messages — only the last one gets the row).
    lastTurnAssistantElement = currentStreamingElement;
    if (entryId) lastTurnAssistantElement.dataset.entryId = entryId;
    currentStreamingElement = null;
    currentStreamingThinking = "";
    currentStreamingStartedAt = null;

    // Track current-context usage only. Session aggregate tokens have a
    // single owner in headerStatusBar and are never derived from rendering.
    if (usage?.input) {
      lastInputTokens = usage.input + (usage.cacheRead || 0);
      lastUsage = usage;
    }
    // Extend the authoritative session aggregate with this live completion
    // only. If the session identity has not hydrated yet, request the
    // authoritative snapshot instead of risking an unscoped increment.
    const sessionFile = eventSessionFile || activeSessionFileForStatusBar();
    const applied = headerStatusBar?.applyLiveUsage(
      {
        input: usage?.input || 0,
        output: usage?.output || 0,
        cacheRead: usage?.cacheRead || 0,
        cacheWrite: usage?.cacheWrite || 0,
      },
      { sessionFile },
    );
    if (applied === false) void hydrateHeaderSessionStats();
    headerStatusBar?.sync({ currentUsage: lastUsage });
    updateTokenUsage();
    showNewMessageBadge();
  }
}

function handleToolExecutionStart(event) {
  startAdoptedTurnClock();
  const { toolCallId, toolName, args } = event;
  if (questionnaireCard.handleToolExecutionStart(event)) {
    // The inline card lands at the stream tail: bring it into view like any
    // other new stream content.
    messagesScrollOwner.scrollToBottom();
  }

  state.addToolExecution(toolCallId, {
    toolName,
    args,
    status: "pending",
  });

  if (activeTurn) {
    demoteAnswerSegmentIntoRail();
    turnRailSteps += 1;
    turnRailToolCalls += 1;
    toolCardRenderer.createToolCard(state.getToolExecution(toolCallId), activeTurn.rail.host);
  } else {
    toolCardRenderer.createToolCard(state.getToolExecution(toolCallId));
  }
  filePreviewFollow.onToolStart(event);
}

function handleToolExecutionUpdate(event) {
  const { toolCallId, partialResult } = event;
  const output = formatToolOutput(partialResult);

  state.updateToolExecution(toolCallId, {
    status: "streaming",
    output,
  });

  toolCardRenderer.updateToolCard(state.getToolExecution(toolCallId));
}

function handleToolExecutionEnd(event) {
  // The question is answered (or errored): its log-derived entry must not
  // rebuild a card for the next blocking request.
  questionnaireCard.handleToolExecutionEnd(event);
  const { toolCallId, result, isError } = event;
  const pendingKey = sessionKeyForDialogs();
  if (pendingQuestionsBySession.get(pendingKey)?.toolCallId === toolCallId) {
    pendingQuestionsBySession.delete(pendingKey);
  }
  const output = formatToolOutput(result);

  state.updateToolExecution(toolCallId, {
    status: isError ? "error" : "complete",
    output,
    isError,
  });

  toolCardRenderer.finalizeToolCard(toolCallId, result, isError);
  // Follow the agent's writes so the live preview (e.g. HTML iframes)
  // reflects disk changes without manual reopening. The callback records
  // successful in-workspace writes for the turn-end file chips row (it
  // already gates on success + workspace containment).
  void filePreviewFollow.onToolEnd(event).catch(() => {});
  if (!isError) {
    widgetMirrorRegistry.handleToolResult(
      event.toolName,
      result,
      runtimeIdForTarget(event.__target || wsClient.getRuntimeTarget()),
    );
  }
}

/**
 * A blocking request whose questionnaire card nothing rebuilt — the in-memory
 * park is gone (page reload, cross-workspace return) or its delivery never
 * reached this page — still has its questions in the session log. Rebuild the
 * card from there and hand it the request, so the walker's wait stays
 * answerable instead of degrading to one plain dialog per question.
 *
 * Only this session's log entry is eligible, and the card's own matcher decides
 * whether the request belongs to it: a mismatch rolls the card back and the
 * plain dialog path takes over, so a stale entry can never swallow another
 * tool's dialog.
 */
function adoptLoggedPendingQuestionnaire(request) {
  if (questionnaireCard.isActive()) return false;
  // Only the walker's answerable methods can belong to a questionnaire; the
  // non-blocking frames (notify/setWidget/setStatus) never do.
  if (request?.method !== "select" && request?.method !== "input") return false;
  const pending = pendingQuestionsBySession.get(sessionKeyForDialogs());
  if (!pending) return false;
  const started = questionnaireCard.start({
    toolCallId: pending.toolCallId,
    toolName: "ask_user_question",
    args: { questions: pending.questions },
  });
  if (!started) return false;
  if (questionnaireCard.handleExtensionUIRequest(request)) {
    // The rebuilt card is anchored at the stream tail: bring it into view.
    messagesScrollOwner.scrollToBottom();
    return true;
  }
  questionnaireCard.teardown({ cancelPending: false });
  return false;
}

/** The identity dialogs and the parked/looked-up questionnaire share. */
function sessionKeyForDialogs() {
  return activeUiSessionFile ?? mirrorActiveSessionFile ?? null;
}

function handleExtensionUIRequest(
  event,
  runtimeId = runtimeIdForTarget(event?.__target || wsClient.getRuntimeTarget()),
) {
  if (questionnaireCard.handleExtensionUIRequest(event)) return;
  if (safetyGuardDialog.handleExtensionUIRequest(event)) return;
  if (adoptLoggedPendingQuestionnaire(event)) return;
  switch (event.method) {
    case "select":
      dialogHandler.showSelect(event);
      break;
    case "confirm":
      dialogHandler.showConfirm(event);
      break;
    case "input":
      dialogHandler.showInput(event);
      break;
    case "editor":
      dialogHandler.showEditor(event);
      break;
    case "notify":
      // Suppress /todos only when the matching runtime has visible mirrored state.
      if (widgetMirrorRegistry.handleCommandNotify(event.message, runtimeId)) break;
      dialogHandler.showNotification(event);
      break;
    case "setStatus":
      break;
    case "setWidget":
      widgetMirrorRegistry.handleWidgetRequest(event, runtimeId);
      break;
    default:
      console.warn("[App] Unknown extension UI method:", event.method);
  }
}

function formatToolOutput(result) {
  if (!result) return "";

  if (result.content && Array.isArray(result.content)) {
    return result.content
      .map((block) => {
        if (block.type === "text") return block.text;
        return JSON.stringify(block);
      })
      .join("\n");
  }

  return JSON.stringify(result, null, 2);
}

// ═══════════════════════════════════════
// Input handling — textarea with auto-resize
// ═══════════════════════════════════════

chatForm.addEventListener("submit", (e) => {
  e.preventDefault();
  sendMessage();
});

messageInput.addEventListener("keydown", (e) => {
  // IME composition uses Enter to confirm candidates; never send during composition.
  // Some WebKit/IME combinations report Enter candidate confirmation with
  // `isComposing === false` but `keyCode === 229`, so keep the legacy fallback.
  const isImeComposing = e.isComposing || e.keyCode === 229;
  if (isImeComposing) return;

  // Option/Alt+Enter queues a follow-up while a run is streaming (C5). The
  // interception must precede the plain-Enter branch, which would otherwise
  // treat Alt+Enter as a normal send.
  if (e.key === "Enter" && e.altKey && !e.shiftKey) {
    e.preventDefault();
    void sendFollowUp();
    return;
  }

  // Enter sends, Shift+Enter inserts newline
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    sendMessage();
  }
});

// WKWebView (Tauri 2) can turn an unconsumed arrow keyDown into insertText
// with the legacy C0 encoding (U+001D = right …): strip it at the composer.
guardComposerArrowInsertion(messageInput);

// Auto-resize textarea; unsent text persists per session as a C4 draft.
messageInput.addEventListener("input", () => {
  messageInput.style.height = "auto";
  messageInput.style.height = `${Math.min(messageInput.scrollHeight, 160)}px`;
  scheduleDraftSave();
});

// ═══════════════════════════════════════
// Image attachment
// ═══════════════════════════════════════

const attachBtn = document.getElementById("attach-btn");
setButtonIcon(attachBtn, "plus", { size: 16 });
const imageInput = document.getElementById("image-input");
const imagePreviews = document.getElementById("image-previews");
const composerCard = document.getElementById("composer-card");

// Image attachments: attach button, native picker, paste/drop, previews.
// Uses the shared helper so the main chat and ephemeral chats stay in lockstep.
// The file-tree drag handler (text/plain path mention) stays inline because it
// is main-chat-only behavior; only the image portion delegates to the helper.
const mainPasteOffload = setupComposerPasteOffload({
  textarea: messageInput,
  container: composerCard,
  offload: async (content) => {
    const response = await fetch("/v2/paste-offload", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ content }),
    });
    const result = await response.json().catch(() => null);
    if (!response.ok)
      throw new Error(
        result?.error?.code || result?.error || `Paste offload failed: ${response.status}`,
      );
    return result?.path;
  },
  t,
});
const mainImageAttachments = setupComposerImageAttachments({
  document,
  composerCard,
  textarea: messageInput,
  attachBtn,
  imageInput,
  imagePreviews,
  processImageFile,
  processImagePayload,
  pickImageFiles: (cwd) => transport.pickImageFiles(cwd),
  getWorkspacePath: getCurrentWorkspacePath,
  isNativeAvailable: nativeAvailable,
  onError: (message) => messageRenderer.renderError(message),
  t,
});
composerCard.addEventListener(
  "drop",
  (e) => {
    // File Tree drag: text/plain carries an absolute path. Image drops are
    // handled by mainImageAttachments already; this listener only intercepts
    // text/plain path mentions before the helper runs.
    const rawPath = e.dataTransfer.getData("text/plain");
    if (rawPath?.startsWith("/")) {
      if (fileBrowser.insertFileMention(rawPath)) {
        e.stopPropagation();
      }
    }
  },
  true,
);

// ═══════════════════════════════════════
// Send message (with images)
// ═══════════════════════════════════════

// Direct sends are delivery-recorded (C3): the composer's text and attachments
// are released only on Pi's correlated acceptance, restored on rejection, and
// surfaced as an unconfirmed pill when no reply arrives within 8s.
const attachmentCommandImage = (img) => ({
  type: "image",
  data: img.data,
  mimeType: img.mimeType || "image/png",
});

function composerIdentity() {
  return resolveComposerSessionIdentity({
    activeUiSessionFile,
    runtimeTarget: wsClient.getRuntimeTarget(),
  });
}

const promptDelivery = createPromptDelivery({
  send: (cmd) => wsClient.sendRuntimeWithId(cmd),
  onAccept: (record, { late }) => {
    updateUI();
    // A pull-back already restored the text on purpose; never fight it.
    if (late && record.pulledBack) return;
    // The reply can land after a session switch, where the composer shows the
    // incoming identity's own draft: mutating it (and its stored draft below)
    // would delete work that has nothing to do with this send. Same guard as
    // onReject.
    if (record.sessionIdentity && record.sessionIdentity !== composerIdentity()) return;
    if (messageInput.value === record.textAtSend) {
      messageInput.value = "";
      messageInput.style.height = "auto";
    }
    if (record.imageSources.length > 0) {
      mainImageAttachments.removePendingImages(record.imageSources);
      // A session switch stashed the same image objects for the sending
      // identity; consume them there too or switching back would resurrect
      // already-accepted attachments.
      if (record.sessionIdentity && attachmentStash.has(record.sessionIdentity)) {
        const remaining = attachmentStash
          .get(record.sessionIdentity)
          .filter((img) => !record.imageSources.includes(img));
        if (remaining.length > 0) attachmentStash.set(record.sessionIdentity, remaining);
        else attachmentStash.delete(record.sessionIdentity);
      }
    }
    if (record.sessionIdentity) {
      const stored = composerDraftStore.get(record.sessionIdentity);
      if (stored === record.textAtSend || stored === record.text) {
        void composerDraftStore.clear(record.sessionIdentity);
      }
    }
    renderQueuedMessages();
  },
  onReject: (record, reason) => {
    updateUI();
    // Only a rejection of a send dispatched while idle may unlock the
    // streaming UI: that run never started. A dispatch made mid-run
    // (follow_up, or a direct send racing a run) must not end another
    // run's streaming state.
    if (record.streamingAtDispatch === false) {
      state.setStreaming(false);
      showTypingIndicator(false);
    }
    messageRenderer.renderError(
      t("errors.messageNotDelivered", { detail: reason?.message || reason?.code || "" }),
    );
    // A dispatched follow-up item returns to the HEAD of its queue (D6,
    // 2026-09-26 spec): its text belongs to the queue, not the composer, and
    // in-order retry must not jump behind items that never left.
    // Pulled back first? The user already holds the text in the composer —
    // re-queueing would duplicate it in both places.
    if (record.meta?.followUpQueue && record.pulledBack) {
      followUpDrainInFlight = false;
      renderQueuedMessages();
      return;
    }
    const requeued = record.meta?.followUpQueue;
    if (requeued) {
      followUpQueue.unshift(requeued.key, requeued.item);
      // Images went with the item; put them back in the previews, but only
      // when this identity still owns the composer — a switched session
      // keeps them on the queued item instead of leaking into another
      // session's composer.
      if (
        Array.isArray(requeued.item.images) &&
        requeued.item.images.length > 0 &&
        record.sessionIdentity === composerIdentity()
      ) {
        mainImageAttachments.replacePendingImages([
          ...mainImageAttachments.getPendingImages(),
          ...requeued.item.images,
        ]);
      }
      followUpDrainInFlight = false;
      renderQueuedMessages();
      return;
    }
    if (record.pulledBack) return; // text already back in the composer
    if (record.sessionIdentity && record.sessionIdentity !== composerIdentity()) {
      // The visible composer now belongs to another session: never touch its
      // DOM. The failed text goes back to the sending identity's draft store,
      // and only where it cannot overwrite a newer draft (an empty store, or
      // one still holding exactly the failed send's own text).
      const stored = composerDraftStore.get(record.sessionIdentity);
      if (!stored || stored === record.textAtSend) {
        void composerDraftStore.save(record.sessionIdentity, record.text);
      }
      renderQueuedMessages();
      return;
    }
    const current = messageInput.value;
    if (!current.trim()) {
      messageInput.value = record.text;
    } else if (current !== record.textAtSend) {
      // Newer draft wins position; the failed text is appended after it.
      messageInput.value = `${current}\n${record.text}`;
    }
    // A rejected drain-dispatch never sees agent_start, which is what
    // releases the drain guard — release it here or the queue stalls until
    // the next unrelated run.
    followUpDrainInFlight = false;
    renderQueuedMessages();
  },
  onUnconfirmed: () => {
    updateUI();
    renderQueuedMessages();
  },
});

// ── Per-session composer drafts (C4) ──────────────────────────────────────
// Drafts are keyed by the composer session identity, debounced while typing,
// and force-saved at every edge where the page may stop running. Attachments
// are stashed in memory per identity (persisting image bytes is out of scope).
const composerDraftStore = createComposerDraftStore({ preferences: preferencesClient });
void composerDraftStore.loadAll();
const attachmentStash = new Map(); // identity -> pending image objects
let draftSaveTimer = null;

function scheduleDraftSave() {
  if (draftSaveTimer) clearTimeout(draftSaveTimer);
  draftSaveTimer = setTimeout(() => {
    draftSaveTimer = null;
    const text = messageInput.value;
    if (text.trim()) void composerDraftStore.save(composerIdentity(), text);
    else void composerDraftStore.clear(composerIdentity());
  }, 300);
}

function flushDraftNow() {
  if (draftSaveTimer) {
    clearTimeout(draftSaveTimer);
    draftSaveTimer = null;
  }
  const text = messageInput.value;
  if (text.trim()) void composerDraftStore.save(composerIdentity(), text);
  else void composerDraftStore.clear(composerIdentity());
}

window.addEventListener("pagehide", flushDraftNow);
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") flushDraftNow();
});

// Session switch: the outgoing identity's draft/attachments are captured
// synchronously (a pending timer is not a saved draft), then the incoming
// identity's state restores. The composer text is REPLACED unconditionally:
// whatever it held belongs to the outgoing identity, so keeping it would leak
// (and let the user send) one session's draft while authoring for another.
// A cross-page swap-navigation draft was already force-saved for its own
// identity before navigation; the incoming session's own draft is what shows.
function switchComposerIdentityState(previousIdentity) {
  if (draftSaveTimer) {
    clearTimeout(draftSaveTimer);
    draftSaveTimer = null;
  }
  const outgoing = messageInput.value;
  if (outgoing.trim()) void composerDraftStore.save(previousIdentity, outgoing);
  else void composerDraftStore.clear(previousIdentity);
  const outgoingImages = mainImageAttachments.getPendingImages();
  if (outgoingImages.length > 0) attachmentStash.set(previousIdentity, outgoingImages);
  else attachmentStash.delete(previousIdentity);

  // The pi queue pills (and their Clear-queue button) belong to the runtime
  // that produced them. This seam is where every session-identity change lands
  // — including the in-page same-workspace switch, which does not reload the
  // page — so the previous session's queue must not stay on screen as if it
  // were the new session's. The incoming session's own parked queue paints in
  // its place: pi re-emits queue_update only on mutation, so the park (not pi)
  // restores a queue that is still live in the runtime we are returning to.
  renderPiQueue(piQueuePark.get(activeUiSessionFile));
  // The local follow-up queue is keyed by session file: repaint it for the
  // incoming identity at the same seam.
  renderQueuedMessages();

  const nextIdentity = composerIdentity();
  mainImageAttachments.replacePendingImages(attachmentStash.get(nextIdentity) || []);
  setComposerDraft(composerDraftStore.get(nextIdentity) || "");
}

function refreshSidebarAfterUserPrompt() {
  sidebar.refresh().catch(() => {});
}

function markPendingNewSessionRefresh() {
  pendingNewSessionRefresh = shouldRefreshSidebarForNewSession({ mirrorActiveSessionFile });
}

function subscribeToLiveRuntimeTargets() {
  void liveRuntimeSubscriptions.refresh();
}

function sendMessage() {
  if (mainPasteOffload.isBusy()) return;
  if (!currentOnboardingState().canQuery) return;
  // One correlated send at a time: the delivery record owns the input until
  // acceptance/rejection settles, so a second Enter cannot double-send.
  if (promptDelivery.hasAwaiting(composerIdentity())) return;

  const message = messageInput.value.trim();
  if (!message) return;

  if (state.isStreaming) {
    // Streaming-Enter sends REAL steering (2026-09-19 steering spec): the
    // local client-side queue is gone — the message goes to pi now and is
    // injected between tool calls; the "Steer" pill comes from queue_update.
    // sendSteering builds its own command and attachment snapshot, so this
    // branch returns before the direct-send work below.
    void sendSteering();
    return;
  }

  const cmd = {
    type: "prompt",
    message,
  };

  // Snapshot pending attachments without consuming them: previews stay up,
  // acceptance removes exactly these, and a rejection leaves them pending.
  const imageSources = [...mainImageAttachments.getPendingImages()];
  if (imageSources.length > 0) {
    cmd.images = imageSources.map(attachmentCommandImage);
  }

  lastSentMessage = message;
  // Direct sends are immediate. Mark a fresh runtime here too, or its
  // first persisted JSONL never triggers refreshSidebarForNewSession() and
  // appears only after the user manually refreshes the sidebar.
  markPendingNewSessionRefresh();
  const optimisticEl = renderNavigableUserMessage({
    content: message,
    images: cmd.images,
    timestamp: Date.now(),
  });
  // P1: the bubble awaits agent_start's claim into the open turn; the key
  // stops a stale bubble from crossing sessions/runtimes.
  pendingUserEl = optimisticEl;
  pendingUserKey = runtimeKeyOf(wsClient.getRuntimeTarget());
  pendingPromptPreview = message;
  promptDelivery.dispatch(cmd, {
    kind: "prompt",
    text: message,
    textAtSend: messageInput.value,
    images: cmd.images || [],
    imageSources,
    sessionIdentity: composerIdentity(),
    streamingAtDispatch: state.isStreaming,
  });
}

// ── Follow-up queue send (C5) ──────────────────────────────────────────────
// `follow_up` only queues inside pi; extension commands cannot be queued
// (they execute immediately even mid-run), so the send intent consults the
// same get_commands registry the composer menu reads.
let extensionCommandNames = null; // Set<string> | null (null = registry not loaded)

// The probe rides the send path (steer/follow_up intent): a busy pi must not
// hold Enter hostage behind the 15s default — 2s caps the wait. The composer
// MENU's catalog load keeps its own long timeout (it has a loading state).
const EXTENSION_COMMAND_PROBE_TIMEOUT_MS = 2000;

async function loadExtensionCommandNames() {
  if (extensionCommandNames) return extensionCommandNames;
  try {
    const data = await wsRequest({ type: "get_commands" }, EXTENSION_COMMAND_PROBE_TIMEOUT_MS);
    extensionCommandNames = new Set(
      (Array.isArray(data?.commands) ? data.commands : [])
        .filter((command) => command?.source === "extension" && typeof command?.name === "string")
        .map((command) => command.name.replace(/^\//, "").toLowerCase()),
    );
  } catch {
    // Transient timeout: stay unloaded so the next send retries — caching an
    // empty registry permanently would misclassify extension commands as
    // steerable text. This call degrades to "no extension commands"; a wrong
    // steer is rejected by pi and restored by the C3 record.
    return new Set();
  }
  return extensionCommandNames;
}

function firstCommandToken(message) {
  const match = String(message).match(/^\/([^\s]+)/);
  return match ? match[1].toLowerCase() : null;
}

/**
 * Streaming-Enter send (2026-09-19 steering spec): plain text goes as
 * `prompt + streamingBehavior:"steer"` (delivered between tool calls, before
 * the next LLM call — genuine mid-run course correction); an extension
 * command goes as a bare prompt (the protocol executes those immediately
 * even mid-run). No optimistic bubble — the "Steer" pill renders from
 * queue_update; delivery rides the C3 record like every other send.
 */
async function sendSteering() {
  if (mainPasteOffload.isBusy()) return;
  if (!currentOnboardingState().canQuery) return;
  if (promptDelivery.hasAwaiting(composerIdentity())) return;
  const message = messageInput.value.trim();
  if (!message) return;

  const token = firstCommandToken(message);
  const extensionCommand = token ? (await loadExtensionCommandNames()).has(token) : false;
  // The probe above is a round trip, so another Enter can have taken the
  // delivery record while it was in flight — dispatching here would deliver the
  // same steer twice.
  if (promptDelivery.hasAwaiting(composerIdentity())) return;
  const intent = planSteeringSend({ streaming: state.isStreaming, extensionCommand });
  if (intent === "direct") {
    sendMessage(); // not streaming (or fell idle between keypress and here)
    return;
  }

  const cmd = { type: "prompt", message };
  if (intent === "steer") cmd.streamingBehavior = "steer";
  // intent === "prompt-now": extension commands execute immediately, even
  // during streaming — a bare prompt is the protocol-sanctioned form.
  const imageSources = [...mainImageAttachments.getPendingImages()];
  if (imageSources.length > 0) {
    cmd.images = imageSources.map(attachmentCommandImage);
  }
  promptDelivery.dispatch(cmd, {
    kind: intent === "steer" ? "steer" : "prompt",
    text: message,
    textAtSend: messageInput.value,
    images: cmd.images || [],
    imageSources,
    sessionIdentity: composerIdentity(),
    // A steer (or a mid-run extension command) is dispatched while a run is
    // active: a rejection must not unlock that run's streaming state.
    streamingAtDispatch: state.isStreaming,
  });
}

async function sendFollowUp() {
  if (mainPasteOffload.isBusy()) return;
  if (!currentOnboardingState().canQuery) return;
  if (promptDelivery.hasAwaiting(composerIdentity())) return;
  const message = messageInput.value.trim();
  if (!message) return;

  const token = firstCommandToken(message);
  const extensionCommand = token ? (await loadExtensionCommandNames()).has(token) : false;
  // Same in-flight window as sendSteering: re-check before dispatching.
  if (promptDelivery.hasAwaiting(composerIdentity())) return;
  if (planFollowUpSend({ streaming: state.isStreaming, extensionCommand }) === "direct") {
    // Idle (user intent is delivery) or an extension command (executes
    // immediately): the plain Enter path is the correct behavior.
    sendMessage();
    return;
  }

  // Streaming: the follow-up parks in Picot's own queue (2026-09-26 spec) —
  // pi's follow_up bucket has no per-item protocol (clear_queue is all-or-
  // nothing), so the queue this UI edits and drains lives client-side. No
  // wire frame, no C3 record: the item is local until drained or sent now.
  const imageSources = [...mainImageAttachments.getPendingImages()];
  followUpQueue.append(sessionKeyForDialogs(), { text: message, images: imageSources });
  if (imageSources.length > 0) mainImageAttachments.replacePendingImages([]);
  messageInput.value = "";
  messageInput.style.height = "auto";
  messageInput.dispatchEvent(new Event("input", { bubbles: true }));
  renderQueuedMessages();
}

/**
 * Send one queued follow-up outside the composer. Idle sends take the direct
 * prompt path (optimistic bubble included); a run in progress gets the item as
 * a steer — "send now" never aborts the current task (spec D4). Rejection is
 * C3's: the item's text returns to the composer, it is not re-queued.
 */
function dispatchFollowUpItem(item, queueKey = sessionKeyForDialogs() ?? "") {
  const images = Array.isArray(item.images) ? item.images : [];
  const cmd = { type: "prompt", message: item.text };
  if (images.length > 0) cmd.images = images.map(attachmentCommandImage);
  if (state.isStreaming) cmd.streamingBehavior = "steer";
  if (!state.isStreaming) {
    markPendingNewSessionRefresh();
    // Match direct sends: the optimistic user bubble already represents Pi's
    // upcoming message_start(user), so do not render its echo a second time.
    lastSentMessage = item.text;
    // Same claim-into-turn contract as sendMessage: the bubble waits for
    // agent_start to fold into the open turn's user row.
    pendingUserEl = renderNavigableUserMessage({
      content: item.text,
      images: cmd.images,
      timestamp: Date.now(),
    });
    pendingUserKey = runtimeKeyOf(wsClient.getRuntimeTarget());
    pendingPromptPreview = item.text;
  }
  promptDelivery.dispatch(cmd, {
    kind: state.isStreaming ? "steer" : "prompt",
    text: item.text,
    textAtSend: "",
    images: cmd.images || [],
    imageSources: images,
    sessionIdentity: composerIdentity(),
    streamingAtDispatch: state.isStreaming,
    // The rejection path re-queues the item (D6, 2026-09-26 spec) instead of
    // dumping its text into the composer.
    meta: { followUpQueue: { key: queueKey, item } },
  });
}

/** Drain one queued follow-up once the run is authoritatively done. */
function maybeDrainFollowUpQueue() {
  if (followUpDrainInFlight || state.isStreaming) return;
  // A delivery still in flight owns the send slot; the item keeps its place
  // and the next settled signal retries the drain.
  if (promptDelivery.hasAwaiting(composerIdentity())) return;
  // No known session file yet (pre-snapshot): an anonymous bucket still
  // holds the items so they render and drain instead of vanishing.
  const key = sessionKeyForDialogs() ?? "";
  const next = followUpQueue.shift(key);
  if (!next) return;
  followUpDrainInFlight = true;
  dispatchFollowUpItem(next);
  renderQueuedMessages();
}

// The park mirrors pi's live queue for a session whose runtime is backgrounded;
// see ARCHITECTURE on runtimes surviving session switches.
const piQueuePark = createPiQueuePark();

// Follow-ups queue Picot-side (2026-09-26 spec): pi's queue protocol is
// all-or-nothing, so per-item edit/delete needs a client-owned truth keyed by
// session file. Enter-while-streaming still goes to pi as a steer directly.
const followUpQueue = createFollowUpQueue();
// One drain in flight at a time: mirror-sync and the disk render both funnel
// into the same idle check, and a second shift before agent_start would send
// two prompts racing each other.
let followUpDrainInFlight = false;

const queuedMessagesEl = document.getElementById("queued-messages");

function followUpQueueRow(item) {
  const row = document.createElement("div");
  row.className = "queued-msg followup-msg";
  const label = document.createElement("span");
  label.className = "queued-msg-label";
  label.textContent = t("queue.followUp");
  const text = document.createElement("span");
  text.className = "queued-msg-text";
  text.textContent = item.text;
  row.append(label, text);
  const actions = document.createElement("span");
  actions.className = "queued-msg-actions";
  const action = (iconName, labelKey, handler) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "queued-msg-action";
    button.setAttribute("aria-label", t(labelKey));
    button.title = t(labelKey);
    const icon = createIcon(iconName, { size: 12 });
    if (icon) button.appendChild(icon);
    button.addEventListener("click", handler);
    actions.appendChild(button);
  };
  action("pencil", "queue.edit", () => {
    followUpQueue.remove(sessionKeyForDialogs(), item.id);
    // Restore convention shared with clear_queue's refill: empty composer
    // takes the text as-is, a draft keeps its position and the item appends.
    if (!messageInput.value.trim()) messageInput.value = item.text;
    else if (!messageInput.value.includes(item.text))
      messageInput.value = `${messageInput.value}\n${item.text}`;
    if (item.images?.length) {
      // A rejected dispatch may have restored these same objects to the
      // previews already; append only what is genuinely missing.
      const pending = mainImageAttachments.getPendingImages();
      const missing = item.images.filter((img) => !pending.includes(img));
      if (missing.length) {
        mainImageAttachments.replacePendingImages([...pending, ...missing]);
      }
    }
    messageInput.dispatchEvent(new Event("input", { bubbles: true }));
    renderQueuedMessages();
    messageInput.focus();
  });
  action("trash-2", "queue.delete", () => {
    followUpQueue.remove(sessionKeyForDialogs(), item.id);
    renderQueuedMessages();
  });
  action("arrow-up", "queue.sendNow", () => {
    // One correlated send at a time (same contract as Enter): a delivery
    // still awaiting Pi's reply owns the composer's send slot.
    if (promptDelivery.hasAwaiting(composerIdentity())) return;
    followUpQueue.remove(sessionKeyForDialogs(), item.id);
    dispatchFollowUpItem(item);
    renderQueuedMessages();
  });
  row.appendChild(actions);
  return row;
}

function renderQueuedMessages() {
  // This area renders two sources: Picot's own follow-up queue (editable,
  // per-item) and C3's unconfirmed-delivery pills (click to pull back).
  queuedMessagesEl.replaceChildren();
  const queueKey = sessionKeyForDialogs() ?? "";
  const queuedItems = followUpQueue.items(queueKey);
  const unconfirmedRecords = promptDelivery.unconfirmed();
  if (unconfirmedRecords.length === 0 && queuedItems.length === 0) {
    queuedMessagesEl.classList.add("hidden");
    return;
  }
  queuedMessagesEl.classList.remove("hidden");
  for (const item of queuedItems) queuedMessagesEl.appendChild(followUpQueueRow(item));
  for (const record of unconfirmedRecords) {
    const item = document.createElement("div");
    item.className = "queued-msg unconfirmed-msg";
    item.title = t("queue.unconfirmedSend");
    const label = document.createElement("span");
    label.className = "queued-msg-label";
    label.textContent = t("queue.unconfirmedSend");
    const message = document.createElement("span");
    message.className = "queued-msg-text";
    message.textContent = record.text;
    item.append(label, message);
    // Clicking the pill restores its text without sending.
    item.addEventListener("click", () => {
      if (!promptDelivery.pullBack(record.requestId)) return;
      if (!messageInput.value.trim()) messageInput.value = record.text;
      else if (!messageInput.value.includes(record.text))
        messageInput.value = `${messageInput.value}\n${record.text}`;
      messageInput.dispatchEvent(new Event("input", { bubbles: true }));
      // The record is gone, so the pill must not survive this render pass.
      renderQueuedMessages();
      messageInput.focus();
    });
    queuedMessagesEl.appendChild(item);
  }
}

function renderPiQueue(queue) {
  // pi 侧 steer/followUp 队列（queue_update 事件）。本地客户端队列已删除
  // （2026-09-19 steering spec）：Enter=steer、Alt+Enter=follow_up 都排 pi 队列，
  // 这里是唯一的队列展示面，pill 只读；清空走 clear_queue（Q2-A）。
  // Accepts a live queue_update event or the session's parked report.
  const steering = Array.isArray(queue?.steering) ? queue.steering : [];
  const followUp = Array.isArray(queue?.followUp) ? queue.followUp : [];
  const el = getOrCreatePiQueueEl();
  el.replaceChildren();
  const items = [
    ...steering.map((msg) => ({ kind: t("queue.steering"), msg })),
    ...followUp.map((msg) => ({ kind: t("queue.followUp"), msg })),
  ];
  if (items.length === 0) {
    el.classList.add("hidden");
    return;
  }
  el.classList.remove("hidden");
  // Header with the one protocol-supported cancel: clear_queue removes ALL
  // queued steering+followUp and returns their text, which lands back in the
  // composer (Q2-A — per-item removal does not exist).
  const header = document.createElement("div");
  header.className = "pi-queue-header";
  const clear = document.createElement("button");
  clear.type = "button";
  clear.className = "pi-queue-clear";
  clear.textContent = t("queue.clearQueue");
  clear.setAttribute("aria-label", t("queue.clearQueue"));
  clear.addEventListener("click", () => void clearPiQueueAndRestore());
  header.appendChild(clear);
  el.appendChild(header);
  for (const { kind, msg } of items) {
    const item = document.createElement("div");
    item.className = "queued-msg pi-queued-msg";
    const label = document.createElement("span");
    label.className = "queued-msg-label";
    label.textContent = kind;
    const text = document.createElement("span");
    text.className = "queued-msg-text";
    text.textContent = msg;
    item.append(label, text);
    el.appendChild(item);
  }
}

function getOrCreatePiQueueEl() {
  let el = document.getElementById("pi-queue");
  if (!el) {
    el = document.createElement("div");
    el.id = "pi-queue";
    el.className = "hidden";
    queuedMessagesEl.insertAdjacentElement("afterend", el);
  }
  return el;
}

abortBtn.addEventListener("click", () => {
  abortCurrentRun();
});

// ═══════════════════════════════════════
// Command Palette
// ═══════════════════════════════════════

const commandBtn = document.getElementById("command-btn");
setButtonIcon(commandBtn, "bot", { size: 16 });
const commandPalette = document.getElementById("command-palette");
const commandList = document.getElementById("command-list");

const commands = [
  {
    icon: "text-collapse",
    label: t("input.compact"),
    desc: t("input.compactDesc"),
    action: () => requestCompact(),
  },
  {
    icon: "clipboard",
    label: t("input.exportHtml"),
    desc: t("input.exportHtmlDesc"),
    action: () => rpcExportHtml(),
  },
  {
    icon: "bar-chart",
    label: t("input.sessionStats"),
    desc: t("input.sessionStatsDesc"),
    action: () => showSessionStats(),
  },
  {
    icon: "chevrons-down",
    label: t("input.expandAllTools"),
    desc: t("input.expandAllToolsDesc"),
    action: () => {
      setAllProcessRailsExpanded(true);
      toolCardRenderer.expandAll();
    },
  },
  {
    icon: "chevrons-up",
    label: t("input.collapseAllTools"),
    desc: t("input.collapseAllToolsDesc"),
    action: () => {
      setAllProcessRailsExpanded(false);
      toolCardRenderer.collapseAll();
    },
  },
];

/**
 * Expand/collapse every process rail on screen. A collapsed rail group hides
 * all of its cards, so card-level expansion alone has no visible effect —
 * the group wrappers must toggle too (covers live .turn-rail and history
 * .process-details-group alike; both share the process-details base class).
 */
function setAllProcessRailsExpanded(expanded) {
  messagesElement.querySelectorAll(".process-details-group").forEach((el) => {
    el.classList.toggle("expanded", expanded);
    el.querySelector(".process-details-toggle")?.setAttribute("aria-expanded", String(expanded));
  });
}
const mainCommandMenu = setupComposerCommandMenu({
  button: commandBtn,
  menu: commandPalette,
  list: commandList,
  getCommands: () => commands,
  document,
  createIcon,
});

// Split send button (2026-09-19 manual review): the caret beside Send is a
// DIRECT delayed-send control — no dropdown, no floating widget. The click
// shares sendFollowUp with Option/Alt+Enter, so idle degrades to a direct
// send and extension commands take the normal path automatically.
const sendCaretBtn = document.getElementById("send-caret-btn");
setButtonIcon(sendCaretBtn, "chevron-up", { size: 12 });
sendCaretBtn.addEventListener("click", () => void sendFollowUp());

// Commands the embedded Pi RPC protocol answers directly (pi `docs/rpc.md`).
// The native runtime forwards these as a v2 `runtime_request`; there is no
// longer an in-Pi HTTP handler to POST them to.
const RUNTIME_RPC_COMMANDS = new Set([
  "set_model",
  "cycle_model",
  "get_available_models",
  "get_available_thinking_levels",
  "set_thinking_level",
  "cycle_thinking_level",
  "set_auto_compaction",
  "set_session_name",
  "get_session_stats",
  "get_state",
  "new_session",
  // pi 0.84.4+: removes ALL queued steering/followUp and returns their text
  // (queue clearing, 2026-09-19 steering spec). The host forwards runtime
  // commands verbatim — this table is the only gate.
  "clear_queue",
]);

// Commands owned by the Rust host control plane rather than the runtime. A Map
// keeps the lookup prototype-free without depending on `Object.hasOwn`, which is
// newer than the WebKit baseline this WebView targets.
const HOST_CONTROL_COMMANDS = new Map([
  ["get_pi_version", () => transport.getPiVersion()],
  ["get_app_version", () => transport.getAppVersion()],
  ["is_dev", () => transport.isDev()],
]);

// Skills surfaces were retired with /api/rpc and are now native host
// controls; the slash-command list reads the runtime's own command registry
// (get_commands). Skills and prompt templates are the expansion-type entries
// (both expand on send; extension commands execute code and stay excluded),
// so a `/skill:` or `/template` typed in the composer always matches Pi.
const RUNTIME_GET_COMMANDS_TIMEOUT_MS = 15000;
async function listSlashCommandsViaRuntime() {
  const data = await wsRequest({ type: "get_commands" }, RUNTIME_GET_COMMANDS_TIMEOUT_MS);
  const commands = Array.isArray(data?.commands) ? data.commands : [];
  return {
    commands: commands
      .filter(
        (command) =>
          (command?.source === "skill" || command?.source === "prompt") &&
          typeof command?.name === "string" &&
          command.name.length > 0,
      )
      .map((command) => ({
        command: `/${command.name}`,
        name: command.name.replace(/^skill:/, ""),
        description: typeof command?.description === "string" ? command.description.trim() : "",
        scope: command?.location === "project" ? "project" : "personal",
        kind: command.source === "prompt" ? "prompt" : "skill",
      })),
  };
}
// Skill/settings inventory ops ride the /picot-config bridge: the live Pi
// process owns skill discovery and settings mutation, so scopes resolve
// against the active workspace exactly as chat does. The bridge resolves
// { ok, data }; rpcCommand callers expect the bare data payload.
const bridgeData = (op, params) =>
  configGateway.call(op, params).then((result) => {
    if (!result?.ok) throw new Error(result?.error || `${op} failed`);
    return result.data ?? {};
  });
const SKILL_HOST_COMMANDS = new Map([
  ["list_slash_commands", () => listSlashCommandsViaRuntime()],
  // Discovered-skills inventory: host control ops (no Pi runtime needed,
  // works at landing). The package-skills entries below stay on the bridge —
  // their inventory/mutation carries project-delta semantics that only the
  // bridge implementation has (the host port drifted).
  ["list_skill_inventory", (cmd) => transport.listSkillInventory(cmd.scope)],
  [
    "list_package_skill_inventory",
    (cmd) => bridgeData("list_package_skill_inventory", { scope: cmd.scope }),
  ],
  ["set_skill_enabled", (cmd) => transport.setSkillEnabled(cmd.scope, cmd.target, cmd.enabled)],
  [
    "set_package_skill_enabled",
    (cmd) =>
      bridgeData("set_package_skill_enabled", {
        scope: cmd.scope,
        target: cmd.target,
        enabled: cmd.enabled,
      }),
  ],
  [
    "set_default_thinking_level",
    (cmd) => bridgeData("set_default_thinking_level", { level: cmd.level }),
  ],
  ["get_default_auto_compaction", () => bridgeData("get_default_auto_compaction", {})],
  [
    "set_default_auto_compaction",
    (cmd) => bridgeData("set_default_auto_compaction", { enabled: cmd.enabled }),
  ],
  ["get_default_codemode", () => bridgeData("get_default_codemode", {})],
  ["set_default_codemode", (cmd) => bridgeData("set_default_codemode", { enabled: cmd.enabled })],
]);

function nativeRpcCommand(cmd) {
  const type = cmd?.type;
  if (RUNTIME_RPC_COMMANDS.has(type)) {
    return wsRequest(cmd).then(
      (data) => ({ success: true, data: data ?? {} }),
      (error) => ({ success: false, error: error?.message || String(error) }),
    );
  }
  const control = HOST_CONTROL_COMMANDS.get(type);
  if (control) {
    return Promise.resolve(control()).then(
      (data) => ({ success: true, data: data ?? {} }),
      (error) => ({ success: false, error: error?.message || String(error) }),
    );
  }
  if (SKILL_HOST_COMMANDS.has(type) || typeof type !== "string") {
    const handler = SKILL_HOST_COMMANDS.get(type);
    if (!handler) {
      return Promise.resolve({
        success: false,
        error: `${type} has no native runtime implementation`,
      });
    }
    return Promise.resolve()
      .then(() => handler(cmd))
      .then(
        (data) => ({ success: true, data: data ?? {} }),
        (error) => ({ success: false, error: error?.message || String(error) }),
      );
  }
  if (typeof type !== "string") {
    return Promise.resolve({
      success: false,
      error: `${type || "command"} has no native runtime implementation`,
    });
  }
  return Promise.resolve({ success: false, error: `Unknown command: ${type}` });
}

/**
 * Adopt a just-selected model in the composer: labels, thinking level, and the
 * context-window bookkeeping. Shared by the model dropdown and the
 * fresh-session inheritance path so both leave identical state behind.
 */
function applySelectedComposerModel(selectedModel) {
  currentModelProvider = selectedModel.provider || "";
  currentModelId = selectedModel.id;
  if (selectedModel.thinkingLevel) {
    currentThinkingLevel = selectedModel.thinkingLevel;
  }
  saveCurrentSessionProfile();
  updateThinkingBtn();
  updateModelLabel();
  if (selectedModel.contextWindow) {
    contextWindowSize = selectedModel.contextWindow;
    updateTokenUsage();
  }
}

/**
 * Give a brand-new session the model the user picked last, instead of pi's
 * built-in default. Only called for sessions with no history of their own:
 * replaying set_model into a session that has one would rewrite the harness
 * state pi just restored from that session's record.
 */
async function inheritLastModel() {
  if (!isMirrorMode || !wsClient.getRuntimeTarget()) return;
  const last = getLastModel();
  if (!last) return;
  if (last.provider === currentModelProvider && last.modelId === currentModelId) return;
  // The enabled list is only filled by fetchModelInfo(). Without this wait a
  // cold start looks the model up in an empty list and silently skips the very
  // inheritance this exists for; the generation guard drops the work if the
  // session moved on while the list was loading.
  const generation = uiSessionGeneration;
  if (!hasLoadedAvailableModels) {
    await fetchModelInfo();
    if (generation !== uiSessionGeneration) return;
  }
  // A model the user has not enabled (or one that is gone with its provider)
  // must not wedge startup: skip it and leave pi's default in place. Curation
  // applies to NEW sessions, which is exactly this path.
  const model = visibleModels.find((entry) =>
    isSelectedModel(entry, { provider: last.provider, modelId: last.modelId }),
  );
  if (!model) {
    console.warn(
      "[Models] last selected model is not in the enabled list; keeping pi's default:",
      `${last.provider}/${last.modelId}`,
    );
    return;
  }
  // Reuse the dropdown's switch path: it retries while a cold session is still
  // starting up ("No context available") and reports failures the same way.
  await selectModel({
    model,
    rpcCommand,
    refreshModelInfo: fetchModelInfo,
    applySelectedModel: applySelectedComposerModel,
  });
}

async function rpcCommand(cmd, statusMsg, silent = false) {
  try {
    if (statusMsg && !silent) statusText.textContent = statusMsg;
    const data = await nativeRpcCommand(cmd);
    if (data.success && !silent) {
      statusText.textContent = t("status.done");
      setTimeout(() => {
        statusText.textContent = t("status.connected");
      }, 2000);
    } else if (!data.success) {
      console.error("rpcCommand failed:", cmd.type, data.error);
      if (!silent) {
        statusText.textContent = data.error || t("status.failed");
        setTimeout(() => {
          statusText.textContent = t("status.connected");
        }, 3000);
      }
    }
    return data;
  } catch (e) {
    console.error("rpcCommand error:", cmd.type, e);
    if (!silent) {
      statusText.textContent = t("status.error");
      setTimeout(() => {
        statusText.textContent = t("status.connected");
      }, 3000);
    }
  }
}

async function rpcExportHtml() {
  try {
    statusText.textContent = t("status.exporting");
    // pi's export_html RPC writes the file itself (rpc.md) — by default into
    // the workspace cwd — and returns its path. Opening happens in Picot's
    // own HTML preview: the system opener on a file:// URL would use the
    // .html default HANDLER (an editor on dev machines), not the browser.
    const result = await rpcCommand({ type: "export_html" }, null, true);
    const exportedPath = result?.success ? result.data?.path : null;
    if (!exportedPath) throw new Error(result?.error || "Session export returned no path");
    statusText.textContent = t("status.done");
    const opened = await filePreviewFollow.openPath(exportedPath).catch(() => null);
    if (!opened) {
      // Preview unavailable: surface where the file landed instead.
      messageRenderer.renderSystemMessage(exportedPath);
    }
  } catch (error) {
    console.error("session export failed:", error);
    statusText.textContent = t("status.failed");
  }
  setTimeout(() => {
    statusText.textContent = t("status.connected");
  }, 4000);
}

async function showSessionStats() {
  let data;
  try {
    statusText.textContent = t("status.loadingStats");
    // rpcCommand re-wraps the runtime reply into { success, data } — the
    // bare wsRequest shape (already-unwrapped data) never matched the old
    // envelope check, so stats silently never rendered.
    data = await rpcCommand({ type: "get_session_stats" }, null, true);
  } catch (error) {
    console.error("session stats failed:", error);
    statusText.textContent = t("status.failed");
    return;
  }
  if (data?.success && data.data) {
    const s = data.data;
    const lines = [
      t("status.sessionStatsTitle"),
      t("status.sessionStatsMessages", {
        total: s.totalMessages,
        user: s.userMessages,
        assistant: s.assistantMessages,
      }),
      t("status.sessionStatsToolCalls", { count: s.toolCalls }),
    ];
    if (s.tokens) {
      lines.push(t("status.sessionStatsContext", { tokens: (s.tokens.input / 1000).toFixed(1) }));
    }
    messageRenderer.renderSystemMessage(lines.join("\n"));
  }
}

// ═══════════════════════════════════════
// Model Picker
// ═══════════════════════════════════════

const modelDropdown = document.getElementById("model-dropdown");
const modelDropdownBtn = document.getElementById("model-dropdown-btn");
const modelDropdownLabel = document.getElementById("model-dropdown-label");
const modelDropdownChevron = modelDropdownBtn?.querySelector(".model-dropdown-chevron");
if (modelDropdownChevron) {
  const icon = createIcon("chevron-down", { size: 10 });
  if (icon) {
    icon.classList.add("model-dropdown-chevron");
    modelDropdownChevron.replaceWith(icon);
  }
}
const modelDropdownMenu = document.getElementById("model-dropdown-menu");
const thinkingBtn = document.getElementById("thinking-btn");
function updateThinkingBtn() {
  // The button shows only the level word (upstream composer shape); the
  // template exists so a locale may still prepend a word if it ever needs one.
  thinkingBtn.textContent = t("settings.thinkingCompact", {
    level: currentThinkingLevel || t("settings.off"),
  });
  thinkingBtn.title = t("settings.thinkingTitle");
  thinkingBtn.setAttribute(
    "aria-label",
    t("settings.thinkingAriaLabel", { level: currentThinkingLevel || t("settings.off") }),
  );
  thinkingBtn.classList.toggle("off", currentThinkingLevel === "off");
}
let currentModelProvider = "";
let currentModelId = "";
// Every model the registry reports as runnable (credentials present). Used to
// resolve a session's own model: pinning, restore and profile snapshots must
// still find a model the user has not enabled, or a pinned session silently
// falls back to pi's default.
let availableModels = [];
// The curated subset the composer picker offers. Visibility is opt-in, so this
// is what the dropdown renders and what a NEW session may inherit.
let visibleModels = [];
// True while the last catalog read failed: an empty picker then means "your
// list could not be read", not "you enabled nothing".
let modelsCatalogUnavailable = false;
let hasLoadedAvailableModels = false;
let didAutoOpenEmptyModelsDropdown = false;
let currentThinkingLevel = "off";
let currentDefaultThinkingLevel = "medium";

function currentOnboardingState() {
  return getOnboardingState({
    hasSessions: hasAnySessionsLoaded(),
    workspacePath: getCurrentWorkspacePath(),
    availableModels,
  });
}

function openModelsSettings() {
  return openSettings("models").then(() => {
    selectSettingsTab("models");
  });
}

function updateOnboardingUI() {
  const onboarding = currentOnboardingState();
  const needsSetup = !onboarding.canQuery;
  composerCard.classList.toggle("onboarding-disabled", needsSetup);
  if (needsSetup) {
    messageInput.placeholder = onboarding.message;
  }
  return onboarding;
}

async function filterConfiguredModels(models) {
  try {
    const visible = filterModelsByCatalogVisibility(
      models,
      await configGateway.call("list_model_catalog"),
    );
    modelsCatalogUnavailable = false;
    return visible;
  } catch (error) {
    console.warn("[Models] Failed to load configured model visibility:", error);
    // Fail closed: an unreadable catalog must not re-expose every available
    // model after the user curated the list. The next refresh repopulates it.
    modelsCatalogUnavailable = true;
    return [];
  }
}

async function fetchModelInfo() {
  try {
    const [modelsResult, stateResult] = await Promise.all([
      rpcCommand({ type: "get_available_models" }, null, true),
      rpcCommand({ type: "get_state" }, null, true),
    ]);
    const modelsData = modelsResult || {};
    const stateData = stateResult || {};

    if (modelsData.success && Array.isArray(modelsData.data?.models)) {
      availableModels = modelsData.data.models;
      visibleModels = await filterConfiguredModels(availableModels);
      hasLoadedAvailableModels = true;
      if (visibleModels.length > 0) {
        didAutoOpenEmptyModelsDropdown = false;
      }
    }
    if (stateData.success && stateData.data?.model) {
      currentModelProvider = stateData.data.model.provider || "";
      currentModelId = stateData.data.model.id || "";

      const model = availableModels.find((entry) =>
        isSelectedModel(entry, {
          provider: currentModelProvider,
          modelId: currentModelId,
        }),
      );
      // pi reports a model the registry does not know (config change, removed
      // provider). Fall back only within the curated list: picking an
      // un-enabled model here would move the session off the user's choice.
      if (!model && visibleModels.length > 0) {
        const fallbackModel = visibleModels[0];
        const resp = await rpcCommand({
          type: "set_model",
          provider: fallbackModel.provider,
          modelId: fallbackModel.id,
        });
        if (resp?.success) {
          currentModelProvider = fallbackModel.provider || "";
          currentModelId = fallbackModel.id;
          if (fallbackModel.contextWindow) {
            contextWindowSize = fallbackModel.contextWindow;
            updateTokenUsage();
          }
        }
      } else {
        updateModelLabel();
        if (model?.contextWindow) {
          contextWindowSize = model.contextWindow;
          updateTokenUsage();
        }
      }
    }
    if (stateData.success && stateData.data) {
      if (stateData.data.thinkingLevel) {
        currentThinkingLevel = stateData.data.thinkingLevel;
        updateThinkingBtn();
      }
      if (stateData.data.defaultThinkingLevel) {
        currentDefaultThinkingLevel = stateData.data.defaultThinkingLevel;
      }
    }
  } catch (_e) {
    // ignore
  } finally {
    updateModelLabel();
    updateUI();
    maybeAutoOpenEmptyModelsDropdown();
  }
}

function maybeAutoOpenEmptyModelsDropdown() {
  if (
    hasLoadedAvailableModels &&
    visibleModels.length === 0 &&
    !didAutoOpenEmptyModelsDropdown &&
    modelDropdownMenu.classList.contains("hidden") &&
    settingsPanel.classList.contains("hidden")
  ) {
    didAutoOpenEmptyModelsDropdown = true;
    openModelDropdown();
  }
}

function updateModelLabel() {
  const shortName = currentModelId.replace(/^claude-/, "").replace(/-\d{8}$/, "");
  modelDropdownLabel.textContent = shortName || t("misc.model");
}

function toggleModelDropdown() {
  const isOpen = !modelDropdownMenu.classList.contains("hidden");
  if (isOpen) {
    closeModelDropdown();
  } else {
    openModelDropdown();
  }
}

function openModelDropdown() {
  openModelDropdownMenu({
    doc: document,
    dropdown: modelDropdown,
    menu: modelDropdownMenu,
    loadModels: () => visibleModels,
    isSelected: (m) =>
      isSelectedModel(m, { provider: currentModelProvider, modelId: currentModelId }),
    onPick: async (m) => {
      closeModelDropdown();
      // If the session is stuck auto-retrying the current (failing) model, or
      // the last turn errored out, the in-flight run stays bound to the old
      // model and the switch would have no visible effect. Abort the dead run
      // first so the new model applies to the next prompt immediately. A
      // healthy stream is left untouched — we only interrupt retry/error runs.
      if (isAutoRetrying || lastTurnErrored) {
        // Route through the confirmed stop: an optimistic unlock over a
        // dropped abort strands a blue composer on a run pi never
        // stopped (the 2026-09-26 fix's very bug class).
        await abortCurrentRun();
        isAutoRetrying = false;
        lastTurnErrored = false;
      }
      const result = await selectModel({
        model: m,
        rpcCommand,
        refreshModelInfo: fetchModelInfo,
        applySelectedModel: (selectedModel) => {
          // Remember the manual pick so future new sessions inherit it.
          setLastModel(selectedModel);
          applySelectedComposerModel(selectedModel);
        },
      });
      if (!result?.success) {
        messageRenderer.renderError(`Model switch failed: ${result?.error || "unknown error"}`);
      }
    },
    configGateway,
    onOpenSettingsClick: () => {
      closeModelDropdown();
      openModelsSettings().catch(() => {});
    },
    modelsUnavailable: modelsCatalogUnavailable,
    close: closeModelDropdown,
    t,
  });
}

function closeModelDropdown() {
  modelDropdownMenu.classList.add("hidden");
  modelDropdown.classList.remove("open");
}

modelDropdownBtn.addEventListener("click", toggleModelDropdown);

// Close dropdown on outside click
document.addEventListener("click", (e) => {
  if (!modelDropdown.contains(e.target)) {
    closeModelDropdown();
  }
});

// Thinking level button — cycles through levels
thinkingBtn.addEventListener("click", async () => {
  const data = await rpcCommand({ type: "cycle_thinking_level" }, "Cycling thinking…");
  if (data?.success && data.data?.level) {
    currentThinkingLevel = data.data.level;
    saveCurrentSessionProfile();
    updateThinkingBtn();
  }
});

// ═══════════════════════════════════════
// Keyboard shortcuts
// ═══════════════════════════════════════

document.addEventListener("keydown", (e) => {
  // Escape — Abort streaming, or close sidebar on mobile
  if (e.key === "Escape") {
    // Close palettes/panels first
    if (!settingsPanel.classList.contains("hidden")) {
      closeSettings();
      return;
    }
    if (!commandPalette.classList.contains("hidden")) {
      mainCommandMenu.close();
      return;
    }
    if (!modelDropdownMenu.classList.contains("hidden")) {
      closeModelDropdown();
      return;
    }

    if (state.isStreaming) {
      abortCurrentRun();
    } else if (!sidebarEl.classList.contains("collapsed") && window.innerWidth <= 768) {
      toggleSidebar();
    }
  }

  // / — Focus message input (when not already in an input)
  if (e.key === "/" && !isInInput()) {
    e.preventDefault();
    messageInput.focus();
  }

  // Cmd/Ctrl+F — Terminal find bar, only when the terminal panel is expanded
  // and focus is already inside it; otherwise the browser's native find stays.
  if (
    (e.key === "f" || e.key === "F") &&
    (e.metaKey || e.ctrlKey) &&
    !e.shiftKey &&
    !e.altKey &&
    terminalPanel.isExpanded() &&
    terminalPanel.root?.contains(document.activeElement)
  ) {
    e.preventDefault();
    terminalSearch.open();
  }

  // Cmd+N (macOS) / Ctrl+N (Windows/Linux) — Start a new chat session in
  // the current workspace. Mirrors the header "+ New Session" button.
  // We intentionally do NOT gate on isInInput() so the shortcut works
  // even while the user is typing in the composer. Shift/Alt are excluded
  // so we don't shadow Cmd+Shift+N (reserved for future "new window").
  if ((e.key === "n" || e.key === "N") && (e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey) {
    e.preventDefault();
    newSession().catch((_err) => {
      messageRenderer.renderError(t("errors.newSessionFailed"));
    });
  }

  // Cmd+Option+I (macOS) / Ctrl+Alt+I (Windows/Linux) — Open webview inspector.
  if ((e.key === "i" || e.key === "I") && (e.metaKey || e.ctrlKey) && e.altKey && !e.shiftKey) {
    e.preventDefault();
    if (nativeAvailable()) {
      transport.openDevtools().catch((err) => {
        messageRenderer.renderError(t("errors.failedToOpenInspector", { error: err }));
      });
    }
  }

  // Cmd/Ctrl+Up — Jump to the previous conversation (skip typing in inputs).
  if (e.key === "ArrowUp" && (e.metaKey || e.ctrlKey) && !isInInput()) {
    e.preventDefault();
    jumpToPreviousUserMessage();
  }

  // Cmd/Ctrl+Down — Jump to the next conversation, or the bottom if this is
  // already the last one.
  if (e.key === "ArrowDown" && (e.metaKey || e.ctrlKey) && !isInInput()) {
    e.preventDefault();
    jumpToNextConversationOrBottom();
  }
});

function isInInput() {
  const tag = document.activeElement?.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || document.activeElement?.isContentEditable;
}

// ═══════════════════════════════════════
// Sidebar
// ═══════════════════════════════════════

function isMobile() {
  return window.innerWidth <= 768;
}

function updateSidebarToggleIcon() {
  // Icon is a static inline SVG in index.html; keep it as-is regardless of
  // sidebar open/closed state. (Previously this overwrote the SVG with the
  // "\u2630" text glyph on first toggle, changing the icon's appearance.)
}

function toggleSidebar() {
  sidebarEl.classList.toggle("collapsed");
  sidebarOverlay.classList.toggle(
    "visible",
    !sidebarEl.classList.contains("collapsed") && isMobile(),
  );
  updateSidebarToggleIcon();
}

sidebarToggle.addEventListener("click", toggleSidebar);

sidebarOverlay.addEventListener("click", () => {
  sidebarEl.classList.add("collapsed");
  sidebarOverlay.classList.remove("visible");
  updateSidebarToggleIcon();
});

refreshSessionsBtn.addEventListener("click", () => {
  if (isMobile()) {
    location.reload();
    return;
  }
  // Static glyph policy: refresh never spins. A pending refresh disables the
  // button and flips aria-busy so assistive tech announces the busy state.
  // Rebind any provisional row to the mirror-known file first: the toolbar
  // refresh path itself cannot map a runtime id to a scanned file.
  if (mirrorActiveSessionFile && sidebar.provisionalSession) {
    sidebar.rebindProvisionalSession(mirrorActiveSessionFile);
  }
  refreshSessionsBtn.disabled = true;
  refreshSessionsBtn.setAttribute("aria-busy", "true");
  sidebar
    .refresh()
    .then(() => {
      if (isMirrorMode) updateMirrorLiveIndicator();
    })
    .catch((error) => {
      console.error("[Sessions] refresh failed:", error);
    })
    .finally(() => {
      refreshSessionsBtn.disabled = false;
      refreshSessionsBtn.removeAttribute("aria-busy");
    });
});

// Swipe from left edge to open sidebar on mobile
(function initSwipeGesture() {
  let touchStartX = 0;
  let touchStartY = 0;
  let tracking = false;

  document.addEventListener(
    "touchstart",
    (e) => {
      const touch = e.touches[0];
      // Only track swipes starting within 20px of left edge
      if (touch.clientX < 20 && isMobile() && sidebarEl.classList.contains("collapsed")) {
        touchStartX = touch.clientX;
        touchStartY = touch.clientY;
        tracking = true;
      }
    },
    { passive: true },
  );

  document.addEventListener(
    "touchmove",
    (e) => {
      if (!tracking) return;
      const touch = e.touches[0];
      const dx = touch.clientX - touchStartX;
      const dy = Math.abs(touch.clientY - touchStartY);
      // If vertical movement dominates, cancel
      if (dy > dx) {
        tracking = false;
      }
    },
    { passive: true },
  );

  document.addEventListener(
    "touchend",
    (e) => {
      if (!tracking) return;
      tracking = false;
      const touch = e.changedTouches[0];
      const dx = touch.clientX - touchStartX;
      if (dx > 60) {
        sidebarEl.classList.remove("collapsed");
        sidebarOverlay.classList.add("visible");
      }
    },
    { passive: true },
  );
})();

// Session search
setupSidebarSearchControl({
  input: sessionSearchInput,
  clearButton: sessionSearchClearBtn,
  onChange: (value) => sidebar.setSearchQuery(value),
});

// Cmd/Ctrl-K spotlight dialog overlay. Reuses the sidebar search input as
// the trigger so focus, click, and the global shortcut all open the same
// dialog. The sidebar in-line filter keeps running in parallel — the dialog
// only changes the visible affordance.
function flattenSidebarSessionsToSearchRows() {
  const currentPath = getCurrentWorkspacePath();
  const rows = [];
  for (const project of sidebar?.projects ?? []) {
    const projectName = project.folderName || project.dirName || "";
    const projectPath = project.path || project.dirName || "";
    const isCurrentWorkspace = !!currentPath && projectPath === currentPath;
    for (const session of project.sessions ?? []) {
      if (!session?.filePath) continue;
      rows.push({
        id: session.filePath,
        name: session.name || "",
        firstMessage: session.firstMessage || "",
        projectName,
        projectPath,
        isCurrentWorkspace,
      });
    }
  }
  return rows;
}

setupSessionSearchDialog({
  triggerInput: sessionSearchInput,
  triggerClear: sessionSearchClearBtn,
  overlay: sessionSearchOverlay,
  dialog: sessionSearchDialog,
  input: sessionSearchDialogInput,
  list: sessionSearchResults,
  // v3 has no message-level search RPC; the dialog tolerates `data` being
  // absent and falls back to title-only matching.
  data: undefined,
  getWorkspaceId: () => undefined,
  getSessions: flattenSidebarSessionsToSearchRows,
  onSelect: (session) => {
    if (!session?.id) return;
    for (const project of sidebar?.projects ?? []) {
      const target = (project.sessions ?? []).find((item) => item.filePath === session.id);
      if (target) {
        // A collapsed workspace group hides the row the search jumps to:
        // expand the group and center the row before the select lands.
        sidebar?.revealSessionInWorkspace?.(project, target);
        void handleSessionSelect(target, project);
        return;
      }
    }
  },
});

/**
 * Reset the chat surface to a fresh "new session" view inside the current window.
 * Clears renderers/state, unmarks the active sidebar item and refreshes the list
 * so the newly created session shows up once pi writes its first message to disk.
 */
function setComposerDraft(value) {
  messageInput.value = value || "";
  messageInput.style.height = "auto";
  messageInput.style.height = `${Math.min(messageInput.scrollHeight, 160)}px`;
}

/** The session file the Info panel reports: the active mirror first, then the
 * sidebar's selected row (the pair the removed header popover read). */
function activeSessionFilePath() {
  return mirrorActiveSessionFile || sidebar.activeSessionFile || "";
}

function restoreSessionUiState(sessionFile) {
  const next = sessionFile || null;
  // Bump the guard token only when the bound session actually changes, so
  // repeated restores of the same session don't needlessly invalidate an
  // in-flight profile restore.
  if (next !== activeUiSessionFile) {
    const previousIdentity = composerIdentity();
    activeUiSessionFile = next;
    taskAnalysis?.resetHistory();
    uiSessionGeneration += 1;
    switchComposerIdentityState(previousIdentity);
    infoPanel?.updateSessionFile(next || "");
  }
}

function saveCurrentSessionProfile() {
  if (!activeUiSessionFile) return;
  const model = availableModels.find((entry) =>
    isSelectedModel(entry, { provider: currentModelProvider, modelId: currentModelId }),
  );
  if (!model?.provider || !model.id) return;
  void sessionUiState.saveProfile({
    provider: model.provider,
    modelId: model.id,
    thinkingLevel: currentThinkingLevel || "off",
  });
}

async function applySessionUiProfile(sessionFile) {
  // pi's set_model / set_thinking_level reconfigure the harness bound to the
  // CURRENT active session (writes are deferred to that session's pending
  // writes when mid-turn), not a global defaults file. Because this runs in
  // the foreground mirror_sync path, the active pi session is the one we just
  // restored, so the profile sticks to its session and never leaks into Pi's
  // global config or another session.
  restoreSessionUiState(sessionFile);
  const generation = uiSessionGeneration;
  const reported = {
    provider: currentModelProvider,
    modelId: currentModelId,
    thinkingLevel: currentThinkingLevel || "off",
  };
  const profile = await sessionUiState.loadProfile();
  if (generation !== uiSessionGeneration) return;
  if (!profile) {
    // Snapshot-on-first-see: pi restores sessions that never had an explicit
    // model/thinking change from the GLOBAL defaults, and pi's setModel /
    // setThinkingLevel also rewrite those defaults. So changing model/thinking
    // in session A silently drifts the defaults that a never-customized
    // session B restores from. Snapshotting every viewed session's reported
    // state pins it: the next restore replays this profile instead of
    // inheriting whatever default session A last wrote.
    snapshotReportedProfile(reported);
    return;
  }
  if (!hasLoadedAvailableModels) {
    await fetchModelInfo();
    if (generation !== uiSessionGeneration) return;
  }
  const model = availableModels.find(
    (entry) => entry.provider === profile.provider && entry.id === profile.modelId,
  );
  if (!model) {
    // availableModels may not have loaded yet during cold mirror_sync. Log
    // instead of silently dropping context window.
    console.warn(
      "[session-ui] profile model not yet in registry:",
      `${profile.provider}/${profile.modelId}`,
    );
  }
  // Short-circuit when the restored state already matches the profile: pi
  // restored this session's own settings (or the snapshot equals them), so
  // replaying set_model/set_thinking_level would only rewrite pi's GLOBAL
  // defaults with this session's values for no user-visible gain.
  // A historical profile may refer to a model removed from current config.
  // Keep transcript restore independent: do not send an invalid set_model;
  // still restore thinking level when runtime accepts it.
  const modelAvailable = Boolean(model);
  const modelAlreadyMatches =
    (reported.provider === profile.provider && reported.modelId === profile.modelId) ||
    !modelAvailable;
  const thinkingAlreadyMatches = reported.thinkingLevel === profile.thinkingLevel;
  if (modelAlreadyMatches && thinkingAlreadyMatches) return;
  const modelResult = modelAlreadyMatches
    ? { success: true, data: null }
    : await rpcCommand(
        { type: "set_model", provider: profile.provider, modelId: profile.modelId },
        null,
        true,
      );
  // The user may have switched sessions while set_model was in flight. If so,
  // abandon this restore so it cannot overwrite the now-active session's UI.
  if (generation !== uiSessionGeneration) return;
  if (modelResult?.success) {
    currentModelProvider = profile.provider;
    currentModelId = profile.modelId;
    if (modelResult.data?.thinkingLevel) {
      currentThinkingLevel = modelResult.data.thinkingLevel;
    }
    updateThinkingBtn();
    updateModelLabel();
    const contextWindow = model?.contextWindow;
    if (contextWindow) {
      contextWindowSize = contextWindow;
      updateTokenUsage();
    }
  }
  if (profile.thinkingLevel && !thinkingAlreadyMatches) {
    const thinkingResult = await rpcCommand(
      { type: "set_thinking_level", level: profile.thinkingLevel },
      null,
      true,
    );
    if (generation !== uiSessionGeneration) return;
    if (thinkingResult?.success) {
      currentThinkingLevel = thinkingResult.data?.level || profile.thinkingLevel;
      updateThinkingBtn();
    }
    // Re-pinning after a successful replay keeps the profile's updated_at
    // fresh and captures any clamping the runtime applied, so the next
    // restore replays exactly what this session last showed.
    if (generation === uiSessionGeneration) saveCurrentSessionProfile();
  }
}

function snapshotReportedProfile(reported) {
  // Only pin when the model registry can resolve a provider for the reported
  // id; a cold-start race (mirror_sync before /api/models) simply skips this
  // snapshot and retries on the next sync.
  const model = availableModels.find((entry) =>
    isSelectedModel(entry, { provider: reported.provider, modelId: reported.modelId }),
  );
  if (!model?.provider || !model.id) return;
  void sessionUiState.saveProfile({
    provider: model.provider,
    modelId: model.id,
    thinkingLevel: reported.thinkingLevel,
  });
}

/**
 * Park the visible questionnaire before a session switch clears it. The
 * previous runtime keeps waiting on extension_ui_response; parkActive hands
 * its card state to the per-session store so restore can rebuild it when the
 * user returns. Keyed by the leaving session's file, falling back to its
 * runtime id when no mirror sync has landed yet.
 */
function parkActiveQuestionnaire() {
  if (!questionnaireCard.isActive()) return;
  const previousFile = mirrorActiveSessionFile || sidebar.activeSessionFile || null;
  const previousRuntimeId = runtimeIdForTarget(wsClient.getRuntimeTarget());
  backgroundQuestionnaires.parkActive(
    previousFile,
    previousRuntimeId,
    questionnaireCard.captureAndClear(),
  );
}

async function resetUiForNewSession() {
  parkActiveQuestionnaire();
  questionnaireCard.handleSessionSwitch();
  pendingNewSessionPreviousFile = mirrorActiveSessionFile || sidebar.activeSessionFile || null;
  state.reset();
  clearConversationRenderers();
  filePreviewFollow.clear();
  cancelFileBrowserRefresh();
  renderWorkspaceWelcome();
  sidebar.clearActive();
  resolveAndApplyFocus();
  mirrorActiveSessionFile = null;
  viewingActiveSession = true;
  restoreSessionUiState(null);
  updateMirrorInputState();
  updateUI();

  // A brand-new session starts with no prior aggregate or context usage.
  resetHeaderStatusBar();

  // Existing Pi sessions already expose a persisted JSONL path. Only a fresh
  // runtime needs one discovery refresh after its first message round-trip.
  markPendingNewSessionRefresh();
}

async function newSession() {
  if (nativeAvailable()) {
    // Always create the new chat in-place on the current pi process: pi's
    // `new_session` RPC aborts/settles any active stream safely (it persists
    // the aborted turn to the old session before switching), so there is no
    // need to spawn a dedicated process while streaming. Spawning a parallel
    // process would force a full page navigation and white flash, which we
    // deliberately avoid.
    await startInWindowNewSession({
      transport,
      getCurrentCwd: () =>
        getCurrentWorkspacePath() ||
        sidebar.projects.find((project) => project.path === foregroundWorkspacePath)?.path ||
        sidebar.projects.find((project) => project.source === "registry")?.path ||
        "",
      navigate: navigateInWindow,
      onBeforeSwap: onBeforeInstanceSwap,
      renderError: (message) => messageRenderer.renderError(message),
    });
    return;
  }

  // Browser/dev fallback: classic in-place "new session" against the same
  // pi process (no Tauri windows available in this mode).
  lastInputTokens = 0;
  resetHeaderStatusBar();
  updateTokenUsage();
  const data = await rpcCommand({ type: "new_session" }, t("status.startingNewSession"));
  if (data?.success === false || data?.data?.cancelled) {
    messageRenderer.renderError(data?.error || t("errors.newSessionCancelled"));
    return;
  }
  await resetUiForNewSession();

  if (isMobile()) {
    sidebarEl.classList.add("collapsed");
    sidebarOverlay.classList.remove("visible");
  }
  if (!isMobile()) messageInput.focus();
}

async function handleNewProjectChat(project) {
  if (workspaceLaunchInProgress) return;
  setWorkspaceLaunchInProgress(true);
  try {
    if (!canUseSessionControl()) {
      const targetPath = project?.path || "";
      const currentPath = getCurrentWorkspacePath();
      const singleProject =
        Array.isArray(sidebar.projects) && sidebar.projects.length === 1
          ? sidebar.projects[0]
          : null;
      const isCurrentProject =
        !targetPath ||
        targetPath === currentPath ||
        (!currentPath && singleProject?.path === targetPath);
      if (isCurrentProject) {
        await newSession();
      } else {
        messageRenderer.renderError(t("errors.mobileBrokerRequired"));
      }
      if (isMobile()) {
        sidebarEl.classList.add("collapsed");
        sidebarOverlay.classList.remove("visible");
      }
      return;
    }

    // Prefer reuse: same project => in-place new_session on current process,
    // even while streaming (pi's new_session RPC aborts/settles the active
    // turn safely). Only mobile mode keeps the spawn path (its window model
    // cannot reuse the current process).
    const launched = await startNewProjectChat({
      project,
      transport,
      getCurrentCwd: getCurrentWorkspacePath,
      shouldSpawnParallel: () => mobileClientMode,
      navigate: navigateInWindow,
      onBeforeSwap: onBeforeInstanceSwap,
      beforeWorkspaceTransition: prepareEphemeralWorkspaceTransition,
      onWorkspaceTransitionCancelled: cancelEphemeralWorkspaceTransition,
      renderError: (message) => messageRenderer.renderError(message),
    });
    if (!launched) return;

    if (isMobile()) {
      sidebarEl.classList.add("collapsed");
      sidebarOverlay.classList.remove("visible");
    }
  } finally {
    setWorkspaceLaunchInProgress(false);
  }
}

// Public entry point: serializes selections so overlapping clicks don't
// interleave their awaits and corrupt shared routing state.
function handleSessionSelect(session, project) {
  // Selecting another session abandons an unpersisted provisional row (Pi
  // never wrote its JSONL). This is the single choke point both sidebars
  // converge on — the normal sidebar's wrapped rows and the Focus sidebar's
  // own onSessionSelect — so the retirement cannot diverge between modes.
  // Selecting the provisional row itself (Focus renders it through the raw
  // builder) keeps the row: the chat stays in that session.
  if (
    session?.filePath &&
    sidebar.provisionalSession &&
    sidebar.provisionalSession.filePath !== session.filePath
  ) {
    sidebar.retireProvisionalSession();
  }
  const run = sessionSelectChain.then(() => handleSessionSelectImpl(session, project));
  // Keep the chain alive even if this selection rejects.
  sessionSelectChain = run.catch(() => {});
  return run;
}

async function handleSessionSelectImpl(session, project) {
  logSessionRoute("select:start", {
    selectedSession: session?.filePath,
    projectPath: project?.path,
    projectDir: project?.dirName,
  });
  // Drop the previous session's todo snapshot before any async work. The
  // history load below is the only path that re-hydrates the panel, and if
  // it fails or short-circuits (missing dirName/file, fetch error, no
  // entries) we don't want the prior turn's list still pinned in the composer.
  // Clear the previous runtime's ambient panels before loading the new history.
  widgetMirrorRegistry.handleSessionSwitch([]);
  parkActiveQuestionnaire();
  questionnaireCard.handleSessionSwitch();
  // Pending write-tool paths belong to the previous session's run; don't
  // let a stale toolCallId surface another session's file.
  filePreviewFollow.clear();
  cancelFileBrowserRefresh();
  sidebar.setActive(session.filePath);
  restoreSessionUiState(session.filePath);
  resolveAndApplyFocus();
  const targetLiveInstance =
    wsClient.getRuntimeTarget()?.sessionId === session.filePath
      ? wsClient.getRuntimeTarget()
      : null;
  // Record the deferred target BEFORE touching the workspace indicator. The
  // mirror_sync handler consumes this token to fire the authoritative file-tree
  // load once the new server's session_start confirms the switch.
  const selectedWorkspacePath = session?.cwd || project?.path || "";
  pendingFileBrowserWorkspace = deferFileBrowserWorkspace(
    session.filePath,
    selectedWorkspacePath,
    fileBrowserWorkspacePath,
  );
  // Cross-workspace select: session's recorded cwd owns workspace label and
  // file tree, regardless of whether a runtime for it exists yet. The pending
  // token defers the file-tree load until mirror sync confirms target.
  if (selectedWorkspacePath && selectedWorkspacePath !== fileBrowserWorkspacePath) {
    foregroundWorkspacePath = selectedWorkspacePath;
    updateWorkspaceIndicator(selectedWorkspacePath);
    // When the target workspace has no persisted file tabs, collapse the
    // editor panel immediately instead of waiting ~2-3s for mirror_sync to
    // drive setWorkspaceRoot. hasPersistedTabs peeks storage without
    // switching state, so switching back to a workspace that DOES have tabs
    // is left to setWorkspaceRoot (no collapse-then-reopen flicker).
    if (!filePreviewPanel.hasPersistedTabs?.(selectedWorkspacePath)) {
      filePreviewPanel.hidePanel();
    }
    // A workspace switch changes the HTTP origin that serves the file API.
    // Navigate before any branch can fall through to an in-process switch.
    try {
      const prepared = await transport.prepareWorkspaceTarget(selectedWorkspacePath, {
        sessionPath: session.filePath,
        reuseExisting: Boolean(targetLiveInstance),
      });
      if (typeof prepared?.transitionGeneration !== "number") {
        throw new Error("Workspace transition was not prepared");
      }
      await transport.commitWorkspaceTransition(prepared.transitionGeneration);
      snapshotUiStateForNavigation();
      navigateInWindow(prepared.targetOrigin, { targetCwd: selectedWorkspacePath });
      return;
    } catch (error) {
      console.error("[Session route] workspace transition failed:", error);
      messageRenderer.renderError(t("errors.failedToSwitchSession", { error }));
      return;
    }
  } else {
    updateWorkspaceIndicator(foregroundWorkspacePath);
  }

  // Native transitions above navigate to a host-origin workspace route. The
  // remaining path is browser/dev compatibility only.
  // Must not run on the native path: it names the session by its jsonl PATH,
  // while the native path below adopts the host-prepared target whose identity
  // is the runtime session id. The path form left a window in which a config
  // call keyed the readiness gate on an identity no opener ever matches — the
  // call then died as "Runtime target changed before configuration was sent" or
  // burned the gateway's 30s timeout, leaving the model list empty.
  if (!nativeAvailable() && session.filePath) {
    wsClient.setRoutingContext({
      workspaceId: `workspace:${selectedWorkspacePath || getCurrentWorkspacePath() || "unknown"}`,
      sessionId: session.filePath,
    });
  }
  logSessionRoute("select:routed", {
    selectedSession: session.filePath,
    targetLiveInstance,
  });
  lastInputTokens = 0;
  resetHeaderStatusBar();
  updateTokenUsage();

  // A Pi process owns one active session. Same-workspace selection adopts the
  // target runtime in this document: no WebView reload, no extension bootstrap
  // warnings, and no flash before the snapshot atomically replaces history.
  if (nativeAvailable() && session.filePath) {
    try {
      const prepared = await transport.prepareWorkspaceTarget(selectedWorkspacePath, {
        sessionPath: session.filePath,
        reuseExisting: Boolean(targetLiveInstance),
      });
      if (typeof prepared?.transitionGeneration !== "number") {
        throw new Error("Session runtime transition was not prepared");
      }
      await transport.commitWorkspaceTransition(prepared.transitionGeneration);
      const instances = (await transport.runtimeInstances())?.instances || [];
      const target = resolvePreparedRuntimeTarget(instances, prepared);
      if (!target) throw new Error("Prepared session runtime is unavailable");
      wsClient.setRoutingContext(target);
      // Keep reload and browser navigation anchored to the adopted runtime.
      // The target comes from host prepare/commit, never sidebar input.
      const targetRoute = new URL(
        `/workspaces/${encodeURIComponent(target.workspaceId)}/sessions/${encodeURIComponent(target.sessionId)}`,
        window.location.href,
      );
      history.pushState(
        null,
        "",
        withFocusParam(selectedWorkspacePath, currentFocusProject, targetRoute).toString(),
      );
      mirrorActiveSessionFile = session.filePath;
      pendingMirrorSessionFile = session.filePath;
      viewingActiveSession = true;
      updateMirrorInputState();
      wsClient.subscribeRuntimeTarget(target);
      wsClient.requestRuntimeSnapshot(target);
      // Upstream switch hydration: fetch the session file in parallel with
      // the snapshot; whichever lands first paints, and the snapshot render
      // then chooses between the two sources.
      void fetchDiskHistory(target, session.filePath);
    } catch (error) {
      console.error("[Session route] runtime transition failed:", error);
      messageRenderer.renderError(t("errors.failedToSwitchSession", { error }));
    }
    return;
  }

  // Native runtime owns session selection; the browser-only transport has no
  // supported session-switch fallback.

  // Close sidebar on mobile after selecting
  if (isMobile()) {
    sidebarEl.classList.add("collapsed");
    sidebarOverlay.classList.remove("visible");
  }
}

// ═══════════════════════════════════════
// Mirror mode sync
// ═══════════════════════════════════════

/**
 * Rebuild a parked background questionnaire now that its session is the
 * foreground runtime. Card state (or the parked tool args) recreates the
 * card, then queued walker requests replay in arrival order — an
 * already-submitted card drains them immediately.
 */
function restoreParkedQuestionnaire(sessionFile, runtimeId) {
  const entry = backgroundQuestionnaires.take(sessionFile, runtimeId);
  if (!entry) return;
  let revealed = false;
  if (entry.cardState) {
    revealed = questionnaireCard.restore(entry.cardState);
  } else if (Array.isArray(entry.questions) && entry.questions.length > 0) {
    revealed = questionnaireCard.start({
      toolCallId: entry.toolCallId,
      toolName: "ask_user_question",
      args: { questions: entry.questions },
    });
  }
  if (revealed) messagesScrollOwner.scrollToBottom();
  for (const request of entry.queuedRequests) {
    handleExtensionUIRequest(request, runtimeId);
  }
}

function handleMirrorSync(data) {
  logSessionRoute("mirrorSync:received", {
    sessionFile: data.sessionFile,
    sessionId: data.sessionId,
    workspaceId: data.workspaceId,
    entries: data.entries?.length || 0,
    isStreaming: data.isStreaming,
  });
  if (!sessionsLoaded) {
    deferredMirrorSync = data;
    return;
  }

  // The broker broadcasts every upstream's `mirror_sync` to all UI clients,
  // including snapshots a *background* pi process emits on its own
  // `session_start` (e.g. the previously-running session that keeps streaming
  // after the user switched to an older session). Such a stray snapshot must
  // NOT hijack the foreground UI: applying it would clobber the rendered
  // history AND — critically — reset the routing context to the background
  // process's session, causing the user's next message to be sent into
  // that previous session instead of the one they're now viewing.
  const receivedSessionFile = data.sessionFile || data.stats?.sessionFile || null;
  const currentTarget = wsClient.getRuntimeTarget();
  const snapshotRuntimeId = runtimeIdForTarget(
    currentTarget || {
      workspaceId: data.runtimeWorkspaceId || data.workspaceId,
      sessionId: data.runtimeSessionId || data.sessionId,
      instanceId: data.runtimeInstanceId || data.instanceId,
    },
  );
  widgetMirrorRegistry.handleRuntimeChange(snapshotRuntimeId);
  const snapshotBelongsToCurrentRuntime =
    data.runtimeWorkspaceId === currentTarget?.workspaceId &&
    data.runtimeSessionId === currentTarget?.sessionId &&
    (!data.runtimeInstanceId || data.runtimeInstanceId === currentTarget?.instanceId);
  const pendingSessionMatches =
    pendingMirrorSessionFile && pendingMirrorSessionFile === receivedSessionFile;
  const appliedForegroundSession =
    snapshotBelongsToCurrentRuntime ||
    pendingSessionMatches ||
    (!currentTarget && Boolean(receivedSessionFile));
  if (appliedForegroundSession && receivedSessionFile) {
    mirrorActiveSessionFile = receivedSessionFile;
    // Follow-ups queued before the first snapshot sit in the anonymous
    // bucket; adopt them into the session's bucket or they would never
    // render or drain (they are the oldest, so they go in front).
    followUpQueue.migrate("", receivedSessionFile);
    // The provisional "new chat" row is keyed by the runtime session id,
    // which never equals the scanned JSONL path. Rebind it to the real file
    // now so the next sidebar refresh converges the placeholder into the
    // persisted row instead of showing both.
    sidebar.rebindProvisionalSession(receivedSessionFile);
    sidebar.setActive(receivedSessionFile);
    restoreSessionUiState(receivedSessionFile);
    resolveAndApplyFocus();
  }
  if (!appliedForegroundSession) {
    logSessionRoute("mirrorSync:ignored-background", {
      sessionFile: receivedSessionFile,
    });
    const bgFile = data.sessionFile || data.stats?.sessionFile || data.runtimeSessionId;
    if (bgFile) {
      const bgStreaming = Boolean(data.isStreaming);
      sidebar.setStreaming(bgFile, bgStreaming);
      updateMirrorLiveIndicator();
    }
    return;
  }

  // Consume a fork's deferred composer prefill. setSidebarActive above has
  // already rebound activeUiSessionFile and restored the saved draft, so this
  // is the first point where applying the prefill cannot be wiped — and
  // Apply the fork's explicit composer prefill after the new session is bound.
  if (
    pendingPostSyncComposer &&
    activeUiSessionFile !== pendingPostSyncComposer.previousSessionFile
  ) {
    const prefill = pendingPostSyncComposer;
    pendingPostSyncComposer = null;
    setComposerDraft(prefill.text);
  }

  if (
    pendingMirrorSessionFile &&
    (pendingMirrorSessionFile === receivedSessionFile ||
      pendingMirrorSessionFile === data.sessionId)
  ) {
    pendingMirrorSessionFile = null;
  }

  console.log("[Mirror] Received state snapshot:", data.entries?.length, "entries");
  isMirrorMode = true;

  // Track the foreground session route.
  const pendingWorkspace = confirmDeferredFileBrowserWorkspace(
    pendingFileBrowserWorkspace,
    receivedSessionFile,
  );
  // The server's mirror workspace is authoritative. A sidebar project path can
  // differ from the session cwd (for example, when a session was moved or
  // imported), and using it would make host data reject the path as outside
  // the process workspace.
  // Host snapshot ids are registry DB uuids; display paths live in the
  // sidebar's registry rows (registryId -> path). The workspace: prefix is
  // a legacy display form no host operation resolves.
  const registryWorkspacePath = (workspaceId) =>
    sidebar.projects.find(
      (project) => project.source === "registry" && project.registryId === workspaceId,
    )?.path || "";
  const syncWorkspacePath =
    workspacePathFromId(data.workspaceId) ||
    registryWorkspacePath(data.workspaceId) ||
    pendingWorkspace?.path ||
    "";
  if (syncWorkspacePath) {
    foregroundWorkspacePath = syncWorkspacePath;
    updateWorkspaceIndicator(syncWorkspacePath);
    infoPanel?.updateSessionFile(receivedSessionFile || "");
  }
  if (pendingWorkspace) {
    pendingFileBrowserWorkspace = null;
  }
  // Host data operations resolve DB uuids — route the uuid, never the
  // path-prefixed display form (workspace:<path> fails workspace_root).
  const authoritativeWorkspaceId =
    data.workspaceId ||
    (syncWorkspacePath && `workspace:${syncWorkspacePath}`) ||
    `workspace:${getCurrentWorkspacePath() || "unknown"}`;
  const authoritativeSessionId =
    data.runtimeSessionId || data.sessionId || data.sessionFile || null;
  // Commit authoritative route before any host data or Git probe. Those v2
  // requests derive workspace scope from WebSocketClient's current route.
  wsClient.setRoutingContext({
    workspaceId: authoritativeWorkspaceId,
    sessionId: authoritativeSessionId,
    instanceId: data.instanceId,
  });
  widgetMirrorRegistry.handleRuntimeChange(runtimeIdForTarget(wsClient.getRuntimeTarget()));
  // Refresh the file tree whenever the mirror-synced workspace differs from
  // the one currently shown. The select path defers its load to here (via
  // pendingWorkspace), but a workspace can also change without a deferred
  // token — e.g. a reload landing on a new runtime, or an in-place switch
  // whose pending token was cleared by an earlier sync. Force-refreshing on any
  // divergence is what keeps the tree in sync once the server's session_start
  // has rebound latestCtx to the right workspace.
  if (syncWorkspacePath && syncWorkspacePath !== lastGitProbePath) {
    // Probe the workspace's Git status whenever the foreground workspace
    // changes — INCLUDING re-entries of the same path on a fresh page, which
    // the file-tree diff below deliberately skips. The generation-guarded
    // git_status / git_command_failed result drives the Git tab's visibility
    // and the toolbar branch pill (refreshGitBranch reads the snapshot).
    gitPanel.snapshot = null;
    gitPanel.notGitRepo = false;
    gitPanel.gitUnavailable = false;
    // The old workspace's branch must not survive the switch: the probe that
    // follows either re-shows the pill with the new branch or clears it.
    updateGitBranchIndicator("");
    gitPanel.selected = new Set();
    gitPanel.commitMessage = "";
    gitPanel.pendingConfirmationToken = null;
    gitPanel.historyPanel?.clearSession();
    // Mark the path probed only when the probe actually left the client:
    // refresh() returns null while the workspace generation is still unknown
    // (owner_bootstrap has not arrived), and swallowing the marker here would
    // permanently hide the branch pill for a git workspace.
    void gitPanel.refresh().then((probeId) => {
      if (probeId) lastGitProbePath = syncWorkspacePath;
    });
    void syncGitEntryForWorkspace(data.workspaceId);
  }
  if (syncWorkspacePath && syncWorkspacePath !== fileBrowserWorkspacePath) {
    void refreshFileBrowserForWorkspace(syncWorkspacePath, { force: true }).catch((error) => {
      console.error("[App] Failed to refresh file browser after session switch:", error);
    });
  }
  viewingActiveSession = true;
  // The snapshot's `isStreaming` comes from the pi process's instantaneous
  // `!ctx.isIdle()`, which can momentarily read false between messages / tool
  // calls of an agent run that is still actively going. The sidebar's
  // streaming set is driven by real `agent_start` / `agent_end` events and is
  // the more reliable signal for a background session we're switching into, so
  // OR the two: only treat the session as idle when both agree it is idle.
  const liveFile = data.sessionFile || data.stats?.sessionFile || mirrorActiveSessionFile;
  const sidebarStreaming = liveFile ? sidebar.isStreaming(liveFile) : false;
  // Three signals, OR'd: the host state machine (authoritative, event-driven),
  // the sidebar set (driven by agent_start/agent_end), and Pi's instantaneous
  // sample. Only when all of them say idle is the session treated as idle.
  const hostWorking = data.lifecycle === "Working";
  const isStreaming = hostWorking || Boolean(data.isStreaming) || sidebarStreaming;
  state.setStreaming(isStreaming);
  showTypingIndicator(isStreaming);
  if (liveFile) sidebar.setStreaming(liveFile, isStreaming);
  updateMirrorInputState();
  updateMirrorLiveIndicator();
  updateUI();

  // Update model display
  if (data.model) {
    currentModelProvider = data.model.provider || "";
    currentModelId = data.model.id || "";
    updateModelLabel();
    if (data.model.contextWindow) {
      contextWindowSize = data.model.contextWindow;
    }
  }

  // Update thinking level
  if (data.thinkingLevel) {
    currentThinkingLevel = data.thinkingLevel;
    updateThinkingBtn();
  }
  if (data.defaultThinkingLevel) {
    currentDefaultThinkingLevel = data.defaultThinkingLevel;
    renderThinkingEffort(currentDefaultThinkingLevel, {
      thinkingSteps: thinkingEffortSteps,
      thinkingMarker: thinkingEffortMarker,
      thinkingName: thinkingEffortName,
    });
  }
  void applySessionUiProfile(data.sessionFile || data.stats?.sessionFile || null);

  // Clear and render message history. A lifecycle event from the previous
  // route must not leave the newly selected session's compact controls busy.
  compactCoordinator.reset();
  clearConversationRenderers();
  lastInputTokens = 0;
  // Reset the aggregate before history replay; the authoritative hydration
  // runs after render so replayed totals never double-count.
  resetHeaderStatusBar();

  // Keep Welcome stable when there are already sessions in the sidebar and
  // the user has not explicitly selected one yet.
  if (!sidebar.activeSessionFile && hasAnySessionsLoaded()) {
    renderWorkspaceWelcome();
    updateTokenUsage();
    // The parked questionnaire belongs to exactly this session/runtime; with no
    // transcript to host it inline it falls back to the modal container.
    restoreParkedQuestionnaire(receivedSessionFile, snapshotRuntimeId);
    void hydrateHeaderSessionStats();
    return;
  }

  const snapshotEntries = data.entries || [];
  // Dr. Lin's contract: the session file is the history source of truth.
  // Matching disk history wins UNCONDITIONALLY — the count heuristic it
  // replaces let a compacted session's snapshot (inflated with synthesized
  // compactionSummary messages) displace the anchored disk render on every
  // sync. Live growth after load streams via message events, not syncs; the
  // snapshot serves only when no disk history exists (brand-new session).
  const diskMatches = diskHistory && diskHistory.sessionId === authoritativeSessionId;
  const useDisk = Boolean(diskMatches) && diskHistory.messages.length > 0;
  if (useDisk) {
    sessionDebug.hydrate = {
      source: "disk-fallback",
      snapshot: snapshotEntries.length,
      disk: diskHistory.messages.length,
      session: authoritativeSessionId,
      at: Date.now(),
    };
    renderSessionHistory(diskHistoryEntries(diskHistory.messages), {
      searchQuery: sidebar.searchQuery,
    });
  } else if (snapshotEntries.length > 0) {
    sessionDebug.hydrate = {
      source: "snapshot",
      snapshot: snapshotEntries.length,
      disk: diskMatches ? diskHistory.messages.length : null,
      session: authoritativeSessionId,
      at: Date.now(),
    };
    renderSessionHistory(snapshotEntries, {
      searchQuery: sidebar.searchQuery,
      leafId: data.leafId,
    });
  } else {
    renderWorkspaceWelcome();
    // A brand-new session has no record of its own, so it inherits the last
    // manually selected model instead of falling back to pi's default.
    // Sessions with history keep whatever pi restored from their own record.
    void inheritLastModel();
  }

  // Snapshot messages carry no entry ids — they cannot build the session
  // tree. Re-fetch the authoritative tree; refreshInfoTree no-ops while the
  // Info tab is hidden and the tab-open click re-fetches.
  void refreshInfoTree();

  updateTokenUsage();
  // The returning session's live turn (when its run is still going) now exists,
  // so a parked blocking prompt lands inline in it instead of in the modal
  // container shared with the plain dialogs.
  maybeOpenAdoptedLiveTurn();
  // The parked questionnaire (if any) belongs to exactly this session/runtime.
  restoreParkedQuestionnaire(receivedSessionFile, snapshotRuntimeId);
  // An authoritative idle snapshot also releases the local follow-up queue
  // (spec D5): a session that came back finished drains its pending items.
  if (!state.isStreaming) maybeDrainFollowUpQueue();
  // Hydrate the aggregate from the authoritative server totals now that
  // history replay is done. Repeated mirror syncs replace (never accumulate).
  void hydrateHeaderSessionStats();
}

// Session-load diagnostic: one-glance state for field debugging. Each
// decision overwrites its slot; read via JSON.stringify(window.__picotSessionDebug).
const sessionDebug = {
  hydrate: null, // { source, snapshot, disk, session, at }
  diskFetch: null, // { status, messages, session, at }
  tree: null, // { source, entries, session, at }
};
globalThis.__picotSessionDebug = sessionDebug;

// Test/inspection seam for the session select path and the config readiness
// gate it feeds (same pattern as __picotSessionDebug).
globalThis.__picotSessionView = {
  select: handleSessionSelect,
  configReady: () => configReadiness.isReady(),
};

// Upstream switch hydration state: the disk history captured once per
// startup/switch. The snapshot render compares against it instead of
// re-fetching (upstream's diskHistoryFallback).
let diskHistory = null; // { sessionId: string, messages: Array } | null

/** Map the data plane's disk messages (entryId-bearing) to session-file
 * entry shape for renderSessionHistory. */
function diskHistoryEntries(messages) {
  return messages.map((message) => ({
    id: typeof message?.entryId === "string" ? message.entryId : null,
    parentId: null,
    type: "message",
    message,
  }));
}

/** renderSessionHistory appends; the CALLER owns clearing. Every standalone
 * transcript re-render funnels through this so the pre-render reset sequence
 * is written exactly once — a path that skips it duplicates the previous
 * render's tail at the top of the transcript. */
function renderTranscriptEntries(entries, { leafId = null } = {}) {
  compactCoordinator.reset();
  clearConversationRenderers();
  lastInputTokens = 0;
  resetHeaderStatusBar();
  renderSessionHistory(entries, { searchQuery: sidebar.searchQuery, leafId });
  maybeOpenAdoptedLiveTurn();
}

/** Upstream switch hydration: read the session file in parallel with the
 * runtime (the file read does not wait for Pi to start), render it when it
 * lands, and keep it as the per-session source the snapshot render chooses
 * against. Best-effort: a session whose file does not exist yet (brand-new
 * chat) keeps the snapshot as the only source. */
async function fetchDiskHistory(target, sessionFile = null) {
  try {
    // Pass the sidebar's scanned JSONL path when known so the host skips its
    // bucket rescan; the host re-validates it and falls back to its own id
    // lookup, so a stale hint cannot fail the read.
    const data = await transport.readSessionMessages(target, sessionFile);
    const messages = Array.isArray(data?.messages) ? data.messages : [];
    if (messages.length === 0) {
      sessionDebug.diskFetch = {
        status: "empty",
        messages: 0,
        session: target.sessionId,
        at: Date.now(),
      };
      return;
    }
    diskHistory = { sessionId: target.sessionId, messages };
    // The user may have switched sessions while the fetch was in flight.
    const current = wsClient.getRuntimeTarget();
    if (current?.sessionId !== target.sessionId) {
      sessionDebug.diskFetch = {
        status: "stale-skipped",
        messages: messages.length,
        session: target.sessionId,
        at: Date.now(),
      };
      return;
    }
    renderTranscriptEntries(diskHistoryEntries(messages));
  } catch (error) {
    // Best-effort: brand-new sessions have no file; the snapshot serves.
    sessionDebug.diskFetch = {
      status: `failed: ${error?.message || error}`,
      session: target?.sessionId,
      at: Date.now(),
    };
    console.warn("[SESSION-LOAD] disk history failed:", error);
  }
}

// Mark sessions in the sidebar with a green dot only when actively streaming
function updateMirrorLiveIndicator() {
  document.querySelectorAll(".session-item").forEach((el) => {
    el.classList.toggle("mirror-live", sidebar.streamingFiles.has(el.dataset.filePath));
  });
}

// Enable/disable input based on whether we're viewing the live session
function updateMirrorInputState() {
  if (!isMirrorMode) return;

  const inputArea = document.querySelector(".input-area");
  if (viewingActiveSession) {
    messageInput.disabled = false;
    messageInput.placeholder = t("input.messagePlaceholder");
    inputArea?.classList.remove("mirror-readonly");
  } else {
    messageInput.disabled = true;
    messageInput.placeholder = t("input.mirrorReadOnly");
    inputArea?.classList.add("mirror-readonly");
  }
}

// ═══════════════════════════════════════
// Session history rendering
// ═══════════════════════════════════════

/** True when an assistant message has at least one text block worth showing. */
// assistantHasText / splitFinalAssistantBlocks moved to ui/turn-model.js so
// the live segment classifier and history rendering share one implementation.

/**
 * Render an assistant message's tool-call blocks as history cards. With a
 * targetContainer they land inside the turn's "Process details" group body
 * instead of the main messages flow. Returns the number of cards created.
 */
function historyTurnWrites(messages, start, end, toolResults) {
  // Mirror the live path: successful write-tool paths for the turn, in
  // first-write order, deduplicated. Tool args carry raw paths (often
  // workspace-relative); chips route through openPath which resolves them
  // the same way the live onWriteApplied path does.
  const writes = [];
  const seen = new Set();
  for (let i = start; i < end; i++) {
    const msg = messages[i];
    if (msg?.role !== "assistant" || !Array.isArray(msg.content)) continue;
    for (const block of msg.content) {
      if (block?.type !== "toolCall" || !isWriteTool(block.name)) continue;
      const result = toolResults.get(block.id);
      if (result?.isError) continue;
      const path = pathFromToolArgs(block.arguments ?? {});
      if (!path || seen.has(path)) continue;
      seen.add(path);
      writes.push({ filePath: path });
    }
  }
  return writes;
}

function renderHistoryToolCallBlocks(blocks, toolResults, targetContainer) {
  let count = 0;
  for (const block of blocks) {
    if (block?.type !== "toolCall") continue;
    count += 1;
    const card = toolCardRenderer.createHistoryCard(
      { toolCallId: block.id, toolName: block.name, args: block.arguments ?? {} },
      targetContainer,
    );
    const result = toolResults.get(block.id);
    if (result) toolCardRenderer.addHistoryResult(block.id, result, result.isError);
    void card;
  }
  return count;
}

// ── History fold gate (spec P2) ─────────────────────────────────────────────
// Only the newest HISTORY_FULL_MOUNT_TURNS settled turns mount; older turns
// sit behind one centred batch control. Revealing mounts incrementally before
// the control, preserving the viewport anchor; a search render mounts
// everything; the gate re-applies on the next plain render.
const HISTORY_GATE_BATCH_TURNS = 10; // load-all chunk size per frame

let historyGate = null;

function historyGateKey() {
  return activeUiSessionFile ?? mirrorActiveSessionFile ?? "transient";
}

function buildHistoryGateControl({ remaining, onOlder, onAll }) {
  const control = document.createElement("div");
  control.className = "history-gate";
  const older = document.createElement("button");
  older.type = "button";
  older.className = "history-gate-btn";
  older.textContent = `${t("messages.loadOlderHistory")} (${remaining})`;
  older.addEventListener("click", onOlder);
  const all = document.createElement("button");
  all.type = "button";
  all.className = "history-gate-all";
  all.textContent = t("messages.loadAllHistory");
  all.addEventListener("click", onAll);
  control.append(older, all);
  return control;
}

/** Insert a rendered turn fragment just below the gate control. */
function insertTurnFragmentBelowControl(fragment) {
  // The control is the transcript's first element and stays there: a reader
  // who scrolled up to the newly mounted history finds the control (and its
  // remaining count) directly above, instead of buried under the turns it
  // just revealed.
  const control = historyGate.control;
  const before = messagesElement.scrollHeight;
  control.parentNode.insertBefore(fragment, control.nextSibling);
  // The batch lands between the control and the turns the reader is looking
  // at, so compensate by exactly the inserted height: the content in view
  // holds still — a reveal loads history, it never scrolls the view. The
  // compensation also leaves the reveal room to run again: the reader scrolls
  // up into the threshold instead of sitting pinned at the top.
  messagesElement.scrollTop += messagesElement.scrollHeight - before;
}

function updateHistoryGateControl() {
  const gate = historyGate;
  if (!gate?.control?.isConnected) return;
  const remaining = gate.turnCount - gate.revealedCount;
  if (remaining <= 0) {
    disconnectGateAutoReveal();
    gate.control.remove();
    return;
  }
  const btn = gate.control.querySelector(".history-gate-btn");
  if (btn) btn.textContent = `${t("messages.loadOlderHistory")} (${remaining})`;
}

function renderSessionHistory(entries, { searchQuery = "", leafId = null } = {}) {
  console.log(`[History] Rendering ${entries.length} entries`);
  // P5.1: the rail's turn source is this registry, rebuilt per render. A
  // session switch therefore resets it with the transcript.
  turnRegistry.clear();
  let userCount = 0,
    assistantCount = 0,
    toolCardCount = 0,
    toolResultCount = 0;

  // Active-branch filter (Info panel design): when Pi reported the active
  // leaf, the main chat renders ONLY the root→leaf ancestor path — sibling
  // branches live in the Info tree, never inline. Without a leafId (session
  // file previews) the previous render-all behavior applies. The ancestor
  // chain walks Pi's own parentId links; nothing is derived client-side.
  if (typeof leafId === "string" && leafId) {
    const byId = new Map();
    for (const entry of entries) {
      if (entry?.id) byId.set(entry.id, entry);
    }
    const activeIds = new Set();
    let cursor = byId.get(leafId);
    while (cursor && !activeIds.has(cursor.id)) {
      activeIds.add(cursor.id);
      cursor = typeof cursor.parentId === "string" ? byId.get(cursor.parentId) : undefined;
    }
    entries = entries.filter((entry) => activeIds.has(entry?.id));
  }

  // Flatten entries to bare messages; pre-index tool results by toolCallId so
  // tool cards can attach their result during rendering regardless of order.
  // Keep each message's Pi entry id alongside for Info-tree scroll anchors.
  const messages = [];
  const messageEntryIds = [];
  const toolResults = new Map();
  for (const entry of entries) {
    if (entry?.type !== "message") continue;
    const msg = entry.message;
    if (!msg) continue;
    messages.push(msg);
    messageEntryIds.push(typeof entry.id === "string" ? entry.id : null);
    if (msg.role === "toolResult") toolResults.set(msg.toolCallId, msg);
  }

  // Split into turns anchored at each user message so each turn's
  // thinking/tool-call noise can be folded into one collapsed group, leaving
  // only the user prompt and the final answer visible (mirrors pi-web).
  // A leading prefix with no user row (Pi sessions open with a system entry
  // the renderer never draws) belongs to no turn: counting it as one gates an
  // empty section on every session.
  const turns = [];
  let turnStart = messages.findIndex((m) => m?.role === "user");
  if (turnStart < 0) turnStart = 0;
  for (let i = turnStart; i < messages.length; i++) {
    if (messages[i].role === "user" && i !== turnStart) {
      turns.push([turnStart, i]);
      turnStart = i;
    }
  }
  turns.push([turnStart, messages.length]);
  // Record (or clear) this session's unanswered ask_user_question so an
  // arriving blocking request can rebuild its card from the log alone. Keyed by
  // the identity dialogs use, and only the newest turn counts: an unanswered
  // call in an older turn is history, not someone waiting on an answer.
  const pendingQuestion = findPendingQuestionnaire(
    messages,
    toolResults,
    turns[turns.length - 1][0],
  );
  const historySessionKey = sessionKeyForDialogs();
  if (historySessionKey) {
    if (pendingQuestion) pendingQuestionsBySession.set(historySessionKey, pendingQuestion);
    else pendingQuestionsBySession.delete(historySessionKey);
  }

  const renderUserFromMsg = (msg, entryId = null, host = null) => {
    const content =
      typeof msg.content === "string"
        ? msg.content
        : (msg.content || [])
            .filter((b) => b.type === "text")
            .map((b) => b.text)
            .join("\n");
    const images = Array.isArray(msg.content)
      ? msg.content
          .filter((b) => b.type === "image")
          .map((b) => ({
            data: b.source?.data || b.data || "",
            mimeType: b.source?.media_type || b.media_type || "image/png",
          }))
      : [];
    if (content || images.length > 0) {
      userCount++;
      const el = messageRenderer.renderUserMessage(
        {
          content: content || "",
          images: images.length > 0 ? images : undefined,
          timestamp: msg.timestamp,
        },
        true,
        host,
      );
      // Pi entry anchor: the Info tree scrolls the chat to these.
      if (el && entryId) el.dataset.entryId = entryId;
      return el ?? null;
    }
    return null;
  };

  // One turn's history render, hostable: the initial pass appends to the
  // messages element; gate reveals render into a fragment inserted before the
  // batch control. Folding only affects mounting — entry ids, the Info tree
  // and file chips render exactly as before (spec P2).
  // P1: each turn renders through the same turn section the live stream uses
  // (spec: "one turn model that both the live stream and history rendering
  // use"), minus the status header — the session log carries no run duration
  // and a live status is not reconstructable. The section is the registry's
  // mounted anchor, so ensureTurnMounted returns a real [data-turn-id]
  // element whose top is the user bubble's top.
  const renderHistoryTurnInto = ([start, end], host) => {
    const anchor = messages[start];
    // The LAST turn is never folded: its thinking, tool calls and pending
    // question are the newest thing the reader came back for, and a run waiting
    // on an answer must be visible the moment the session is on screen. This is
    // unconditional on purpose — a streaming flag, a snapshot or any other cached
    // signal can be momentarily wrong exactly when the model is blocked, and a
    // folded question reads as "still working" when it is actually waiting.
    const newestTurn = start === turns[turns.length - 1]?.[0];
    let bodyStart = start;
    let turnUserEl = null;
    const turnId = messageEntryIds[start] ?? `hist-${start}`;
    const turnSection = createTurnSection({ turnId, withStatus: false });
    host.appendChild(turnSection.element);
    if (anchor?.role === "user") {
      turnUserEl = renderUserFromMsg(anchor, messageEntryIds[start], turnSection.element);
      bodyStart = start + 1;
    }
    // The section is already built as rail → answer, so a bubble rendered into
    // it lands BELOW its own answer. Claim it into the user slot (the same seam
    // the live path uses) so history reads: user → rail → answer.
    if (turnUserEl) turnSection.claimUserElement(turnUserEl);

    // The last assistant message that still has visible text is the final
    // answer; everything before it in this turn is process noise.
    let finalAssistantIdx = -1;
    for (let i = end - 1; i >= bodyStart; i--) {
      if (messages[i].role === "assistant" && assistantHasText(messages[i].content)) {
        finalAssistantIdx = i;
        break;
      }
    }

    let group = null;
    let stepCount = 0;
    let toolCallCount = 0;
    const ensureGroup = () => {
      if (!group) {
        group = turnSection.rail;
        group.setDisclosure(newestTurn);
      }
      return group;
    };

    for (let i = bodyStart; i < end; i++) {
      const msg = messages[i];
      if (msg?.role !== "assistant") continue;

      if (i === finalAssistantIdx) {
        const { processBlocks, answerBlocks } = splitFinalAssistantBlocks(msg.content);
        if (processBlocks.some((b) => b.type === "text" || b.type === "thinking")) {
          const el = messageRenderer.renderAssistantMessage(
            { content: processBlocks, usage: msg.usage },
            false,
            true,
            ensureGroup().host,
            /* suppressToolbar */ true,
          );
          if (el) stepCount += 1;
        }
        if (processBlocks.some((b) => b.type === "toolCall")) {
          toolCallCount += renderHistoryToolCallBlocks(
            processBlocks,
            toolResults,
            ensureGroup().host,
          );
        }
        if (answerBlocks.length > 0) {
          const finalEl = messageRenderer.renderAssistantMessage(
            { content: answerBlocks, usage: msg.usage, timestamp: msg.timestamp },
            false,
            true,
            turnSection.answer.host,
          );
          assistantCount++;
          if (finalEl) {
            registerTurn({ id: turnId, answerPreview: finalEl.textContent ?? "" });
            // Pi entry anchor for the turn's final answer (Info tree target).
            const answerEntryId = messageEntryIds[i];
            if (answerEntryId) finalEl.dataset.entryId = answerEntryId;
            // Rebuild the turn's written-file chips from history so returning to
            // a session shows the same affordance the live turn had.
            const writes = historyTurnWrites(messages, bodyStart, end, toolResults);
            // History cards list files only — frozen stats are never
            // persisted (spec Q3-A).
            const row = renderTurnFilesCard({
              writes,
              statsByPath: null,
              history: true,
            });
            if (row) mountTurnFilesCard(finalEl, row);
          }
        }
        if (msg.usage?.input) {
          lastInputTokens = msg.usage.input + (msg.usage.cacheRead || 0);
          lastUsage = msg.usage;
        }
      } else {
        const el = messageRenderer.renderAssistantMessage(
          msg,
          false,
          true,
          ensureGroup().host,
          /* suppressToolbar */ true,
        );
        if (el) stepCount += 1;
        toolCallCount += renderHistoryToolCallBlocks(
          msg.content ?? [],
          toolResults,
          group?.host ?? ensureGroup().host,
        );
      }
    }

    if (group) {
      if (group.host.children.length > 0) {
        group.setLabel(summarizeProcessGroup(stepCount, toolCallCount));
      } else {
        group.wrapper.remove();
      }
    }

    // P5.1: every rendered turn enters the registry with the turn section as
    // its mounted anchor — the section top is the user bubble's top, so rail
    // jumps and the reading-line spy behave exactly as before.
    registerTurn({
      id: turnId,
      promptPreview: turnUserEl?.textContent ?? "",
      entryId: messageEntryIds[start] ?? null,
      mountedElement: turnSection.element,
    });
  };

  // P5.1: the registry is COMPLETE — folded turns register too (mountedElement
  // null until revealed), extracted from the raw messages so the rail never
  // loses turns to the fold gate. Mounted renders merge their DOM anchors in.
  const textOf = (msg) => {
    if (typeof msg?.content === "string") return msg.content;
    if (!Array.isArray(msg?.content)) return "";
    return msg.content
      .filter((b) => b?.type === "text")
      .map((b) => b.text)
      .join("\n");
  };
  for (const [start, end] of turns) {
    let answerPreview = "";
    for (let i = end - 1; i > start; i -= 1) {
      if (messages[i]?.role === "assistant" && assistantHasText(messages[i].content)) {
        const { answerBlocks } = splitFinalAssistantBlocks(messages[i].content);
        answerPreview = answerBlocks
          .filter((b) => b?.type === "text")
          .map((b) => b.text)
          .join(" ");
        break;
      }
    }
    registerTurn({
      id: messageEntryIds[start] ?? `hist-${start}`,
      promptPreview: textOf(messages[start]),
      answerPreview,
      entryId: messageEntryIds[start] ?? null,
      mountedElement: null,
    });
  }

  // ── P2 fold gate: which turns mount ──
  const searchRender = Boolean(searchQuery);
  const gateSessionKey = historyGateKey();
  if (!historyGate || historyGate.sessionKey !== gateSessionKey || historyGate.forceReset) {
    const restoredReveal =
      pendingRevealRestore?.sessionKey === gateSessionKey &&
      Number.isFinite(pendingRevealRestore.count)
        ? Math.max(HISTORY_FULL_MOUNT_TURNS, pendingRevealRestore.count)
        : HISTORY_FULL_MOUNT_TURNS;
    historyGate = {
      sessionKey: gateSessionKey,
      revealedCount: restoredReveal,
      forceReset: false,
      turnCount: turns.length,
      control: null,
      renderTurn: renderHistoryTurnInto,
      turns,
      messageEntryIds,
      loadToken: 0,
    };
  } else {
    historyGate.turnCount = turns.length;
    historyGate.renderTurn = renderHistoryTurnInto;
    historyGate.turns = turns;
    historyGate.messageEntryIds = messageEntryIds;
  }
  // A plain render never shrinks the reveal below the mount default: Pi's
  // compacted snapshot renders first with fewer turns (compaction drops
  // pre-compaction messages the disk read keeps), and that smaller render
  // must not re-gate the disk render that follows it.
  const revealedCount = searchRender
    ? turns.length
    : Math.min(Math.max(historyGate.revealedCount, HISTORY_FULL_MOUNT_TURNS), turns.length);
  const gateApplies = turns.length > revealedCount && !searchRender;

  // Batch control first, then the newest revealed turns.
  if (gateApplies) {
    const mountOlder = (count) => {
      const previous = historyGate.revealedCount;
      historyGate.revealedCount = Math.min(
        historyGate.turnCount,
        Math.max(historyGate.revealedCount + count, count),
      );
      const from = Math.max(0, historyGate.turnCount - historyGate.revealedCount);
      const to = historyGate.turnCount - previous;
      const fragment = document.createDocumentFragment();
      for (let i = from; i < to; i++) historyGate.renderTurn(turns[i], fragment);
      insertTurnFragmentBelowControl(fragment);
      updateHistoryGateControl();
    };
    const mountAll = () => {
      const token = ++historyGate.loadToken;
      const mountChunk = () => {
        const gate = historyGate;
        if (!gate?.control?.isConnected || token !== gate.loadToken) return;
        const previous = gate.revealedCount;
        gate.revealedCount = Math.min(
          gate.turnCount,
          gate.revealedCount + HISTORY_GATE_BATCH_TURNS,
        );
        const from = Math.max(0, gate.turnCount - gate.revealedCount);
        const to = gate.turnCount - previous;
        // Cancellable rAF batches at the control's position, anchored (spec P2).
        const fragment = document.createDocumentFragment();
        for (let i = from; i < to; i++) gate.renderTurn(gate.turns[i], fragment);
        insertTurnFragmentBelowControl(fragment);
        updateHistoryGateControl();
        if (gate.revealedCount < gate.turnCount) requestAnimationFrame(mountChunk);
      };
      requestAnimationFrame(mountChunk);
    };
    historyGate.control = buildHistoryGateControl({
      remaining: turns.length - revealedCount,
      onOlder: () => mountOlder(HISTORY_REVEAL_BATCH_TURNS),
      onAll: mountAll,
    });
    messagesElement.appendChild(historyGate.control);
    observeGateAutoReveal(messagesElement, () => {
      mountOlder(HISTORY_REVEAL_BATCH_TURNS);
      return historyGate.turnCount - historyGate.revealedCount;
    });
  } else {
    historyGate.control = null;
    historyGate.loadToken += 1; // cancel any in-flight load-all batches
    disconnectGateAutoReveal();
  }
  historyGate.revealedCount = searchRender
    ? historyGate.revealedCount // a search render never shrinks the reveal
    : revealedCount;

  for (let i = turns.length - revealedCount; i < turns.length; i++) {
    renderHistoryTurnInto(turns[i], messagesElement);
  }
  if (searchRender) {
    // The gate re-applies on the next plain render (spec P2).
    historyGate.forceReset = true;
  }
  toolCardCount = toolCardRenderer.toolCards.size;
  toolResultCount = toolResults.size;
  console.log(
    `[History] Done: ${userCount} users, ${assistantCount} assistants, ${toolCardCount} tools, ${toolResultCount} results`,
  );
  console.log(`[History] DOM tool-card count:`, document.querySelectorAll(".tool-card").length);
  console.log(
    `[History] DOM thinking-block count:`,
    document.querySelectorAll(".thinking-block").length,
  );

  if (searchQuery) {
    messageRenderer.highlightSearchQuery(searchQuery);
  }

  // Replay the most recent `todo` tool-result snapshot onto the floating
  // rpiv-todo panel so the user sees prior state when navigating back.
  const todoEntries = [];
  for (const entry of entries) {
    if (entry?.type === "message" && entry.message?.role === "toolResult") {
      todoEntries.push(entry.message);
    }
  }
  widgetMirrorRegistry.handleSessionSwitch(todoEntries);

  updateTokenUsage();
  fetchContextWindow();

  anchorHistoryToBottom(document.getElementById("messages"), {
    preserveScrollTarget: Boolean(searchQuery),
  });
  // The transcript wipe above can leave a still-pending questionnaire
  // detached: re-anchor it at the tail of the fresh stream.
  questionnaireCard.remountIfNeeded();
}

// ═══════════════════════════════════════
// UI helpers
// ═══════════════════════════════════════

function showTypingIndicator(show) {
  typingIndicator.classList.toggle("hidden", !show);
}

/**
 * Clear pi's queued steering/followUp and restore the returned text to the
 * composer (Q2-A/Q3-A). Empty-result merge keeps newer drafts untouched.
 * Returns the cleared text (empty string when nothing was queued or the call
 * failed — the caller proceeds with abort regardless, per spec).
 */
async function clearPiQueueAndRestore() {
  // The queue only visibly empties when pi CONFIRMS the clear — a failed or
  // unknown command must leave the pills alone (they are still queued at pi;
  // hiding them would be the silent degradation the review flagged).
  const result = await rpcCommand({ type: "clear_queue" }, null, true);
  if (!result?.success) return "";
  const texts = [
    ...(Array.isArray(result.data?.steering) ? result.data.steering : []),
    ...(Array.isArray(result.data?.followUp) ? result.data.followUp : []),
  ];
  // The cleared texts are back in the composer now; drop their in-flight C3
  // records so a late acceptance cannot wipe what this restore just put back.
  promptDelivery.pullBackTexts(texts);
  renderQueuedMessages();
  const cleared = texts.filter((text) => typeof text === "string" && text.trim()).join("\n");
  if (cleared) {
    if (!messageInput.value.trim()) messageInput.value = cleared;
    else if (!messageInput.value.includes(cleared))
      messageInput.value = `${messageInput.value}\n${cleared}`;
    messageInput.dispatchEvent(new Event("input", { bubbles: true }));
  }
  // pi re-emits queue_update on clear; drop the pills now too so they do not
  // linger, and park the confirmed-empty queue so a session switch in that
  // window cannot repaint what pi just cleared. A late queue_update with
  // content would restore the truth on its own.
  piQueuePark.set(activeUiSessionFile, { steering: [], followUp: [] });
  renderPiQueue(null);
  return cleared;
}

async function abortCurrentRun() {
  // Pi-native (2026-09-25, supersedes the composer spec's Q3-A): abort ONLY.
  // Queued steer/followUp stay at pi — it continues with them once the run
  // terminates ("abort continues queued messages") — so no clear_queue and no
  // composer restore on the stop path.
  //
  // The stop must be CONFIRMED before the UI unlocks (2026-09-26 fix): an
  // optimistic unlock over a dropped abort leaves a blue composer on a run
  // that never stopped — exactly the state that stranded queued messages. A
  // pi abort replies only once the session is idle, so agent_end/
  // agent_settled usually unlock first; this path is the fallback.
  const command = { type: "abort" };
  let response = null;
  let failure = null;
  try {
    // raw: pi's abort reply carries `success` at the top level with no
    // `data` payload — the default unwrap would erase the verdict.
    response = await wsClient.sendRuntime(command, { timeoutMs: 30000, raw: true });
  } catch (error) {
    failure = error;
    response = null;
  }
  // stale_turn (a turn that is no longer active) carries no `success`: pi
  // aborted nothing, so it is a failure for this stop, not a silent pass.
  if (response?.success !== true) {
    const detail = failure?.message || response?.error || "";
    messageRenderer.renderError(t("errors.abortFailed", { detail }));
    updateUI();
    return;
  }
  messageRenderer.renderError(t("errors.abortedByUser"));
  showTypingIndicator(false);

  if (state.isStreaming) {
    state.setStreaming(false);
    currentStreamingElement = null;
    currentStreamingText = "";
    currentStreamingThinking = "";
    updateUI();
  }
  // A delayed/missing agent_end must not leave a live turn behind: close it
  // (settled with no duration — an aborted run never completed) so the rail
  // folds and the status timer stops.
  if (activeTurn) closeLiveTurn();
}

function updateTokenUsage() {
  // app.js and index.html have shipped desynced before (donut refs landed in
  // adc78df, the markup only in b37e1dc): a missing element must degrade to a
  // no-op instead of throwing inside the RPC event path.
  if (!tokenUsageEl || !contextDonutArc || !contextDonutLabel) return;
  if (lastInputTokens <= 0) {
    tokenUsageEl.classList.remove("visible", "warning", "critical");
    contextVizController?.invalidateUsage();
    return;
  }

  if (contextWindowSize > 0) {
    const pct = Math.min(100, Math.round((lastInputTokens / contextWindowSize) * 100));
    contextDonutArc.style.strokeDasharray = String(CONTEXT_DONUT_CIRCUMFERENCE);
    contextDonutArc.style.strokeDashoffset = String(CONTEXT_DONUT_CIRCUMFERENCE * (1 - pct / 100));
    contextDonutLabel.textContent = pct;
    tokenUsageEl.classList.add("visible");
    tokenUsageEl.classList.remove("warning", "critical");
    if (pct >= 80) {
      tokenUsageEl.classList.add("critical");
    } else if (pct >= 60) {
      tokenUsageEl.classList.add("warning");
    }
    tokenUsageEl.title = t("usage.contextTokens", {
      used: (lastInputTokens / 1000).toFixed(1),
      limit: (contextWindowSize / 1000).toFixed(0),
    });
    // Compact lives only in the context popover; thresholds just recolor
    // the donut. Clicking it opens the popover with the action.
  } else {
    // No context window info yet, just show raw tokens and suppress the
    // threshold-based action because no percentage can be computed.
    contextDonutArc.style.strokeDasharray = String(CONTEXT_DONUT_CIRCUMFERENCE);
    contextDonutArc.style.strokeDashoffset = String(CONTEXT_DONUT_CIRCUMFERENCE);
    contextDonutLabel.textContent = "–";
    tokenUsageEl.classList.add("visible");
    tokenUsageEl.classList.remove("warning", "critical");
  }

  // Keep an open context popover in sync with fresh usage so the breakdown
  // does not freeze on the snapshot taken when the popover was opened.
  const popover = document.getElementById("context-viz");
  if (popover && !popover.classList.contains("hidden")) contextVizController?.sync();
  // Mirror the current-context signal into the aggregate status bar so its
  // threshold classes (warning/critical) track the same usage value.
  headerStatusBar?.sync({ currentUsage: lastUsage });
}

function requestCompact() {
  void compactCoordinator.request().then((accepted) => {
    statusText.textContent = accepted ? t("status.compacting") : t("status.failed");
  });
}

function syncCompactControls() {
  // Compact lives only in the context popover; keep its busy state in sync.
  contextVizController?.sync();
}

async function fetchContextWindow() {
  // Delegate to fetchModelInfo which also updates the model button
  await fetchModelInfo();
}

let tailscaleUrl = "";
let lanUrl = "";
let lanUrls = [];

async function refreshLanUrl() {
  try {
    const data = await transport.hostHealth();
    tailscaleUrl = typeof data?.tailscaleUrl === "string" ? data.tailscaleUrl : tailscaleUrl;
    lanUrls = Array.isArray(data?.lanUrls)
      ? data.lanUrls.filter((value) => typeof value === "string" && value.trim())
      : [];
    lanUrl = typeof data?.lanUrl === "string" ? data.lanUrl : "";
    if (!lanUrl && lanUrls.length > 0) lanUrl = lanUrls[0];
    if (tailscaleUrl) {
      statusText.textContent = t("status.connectedTS");
      statusText.title = tailscaleUrl;
    } else if (lanUrl) {
      statusText.textContent = t("status.connectedLAN");
      statusText.title = lanUrl;
    }
  } catch {
    /* LAN/Tailscale fields are optional: the plain connected label stays. */
  }
}

function updateConnectionStatus(status) {
  statusIndicator.className = `status-indicator ${status}`;

  if (status === "connected") {
    if (tailscaleUrl) {
      statusText.textContent = t("status.connectedTS");
      statusText.title = tailscaleUrl;
    } else if (lanUrl) {
      statusText.textContent = t("status.connectedLAN");
      statusText.title = lanUrl;
    } else {
      statusText.textContent = t("status.connected");
      statusText.title = "";
    }
    // Fetch network link metadata on first connect
    if (!tailscaleUrl && !lanUrl) {
      void refreshLanUrl();
    }
  } else if (status === "disconnected") {
    statusText.textContent = t("status.disconnected");
  }
}

function updateUI() {
  const isStreaming = state.isStreaming;
  const onboarding = updateOnboardingUI();

  composerCard.classList.toggle("streaming", isStreaming);

  if (isStreaming) {
    statusIndicator.classList.add("streaming");
    statusIndicator.classList.remove("connected");
    statusText.textContent = t("status.working");
  } else {
    statusIndicator.classList.remove("streaming");
    statusIndicator.classList.add("connected");
    statusText.textContent = t("status.connected");
  }

  messageInput.disabled = !onboarding.canType;
  sendBtn.disabled = !onboarding.canQuery || promptDelivery.hasAwaiting(composerIdentity());

  if (isStreaming) {
    abortBtn.classList.remove("hidden");
    sendBtn.classList.add("hidden");
    // The caret (delayed send) only exists while a run is active — the
    // follow-up it sends is meaningless when idle (2026-09-19 spec).
    sendCaretBtn.classList.remove("hidden");
  } else {
    abortBtn.classList.add("hidden");
    sendBtn.classList.remove("hidden");
    sendCaretBtn.classList.add("hidden");
  }

  if (onboarding.canQuery) {
    messageInput.placeholder = `${t("input.typeMessage")}${t("input.typeMessageFollowUpHint")}`;
  }
}

// ═══════════════════════════════════════
// WebSocket session switch handler
// ═══════════════════════════════════════

// ═══════════════════════════════════════
// Theme / Settings
// ═══════════════════════════════════════

const settingsBtn = document.getElementById("settings-btn");
replaceButtonGlyph(settingsBtn, "settings", { size: 18 });
const settingsPanel = document.getElementById("settings-panel");
const settingsOverlay = document.getElementById("settings-overlay");
const settingsClose = document.getElementById("settings-close");
const settingsBackIcon = settingsClose?.querySelector(".settings-nav-back-icon");
if (settingsBackIcon) {
  const icon = createIcon("chevron-left", { size: 14 });
  if (icon) settingsBackIcon.replaceChildren(icon);
}
const settingsNavItems = Array.from(document.querySelectorAll(".settings-nav-item"));
const settingsTabs = Array.from(document.querySelectorAll(".settings-tab"));
const themeGrid = document.getElementById("theme-grid");
const languageSelect = document.getElementById("settings-language-select");
const previewThemeSelect = document.getElementById("settings-preview-theme-select");
const terminalThemeSelect = document.getElementById("settings-terminal-theme-select");
// Three font-size sliders (chat / preview / terminal) share the thinking-effort
// segmented-control markup; each is a radiogroup with a sliding thumb.
const fontSizeControls = [
  { prefix: "settings-chat-font-size", get: () => chatFontSizeLevel, set: setChatFontSizeLevel },
  {
    prefix: "settings-preview-font-size",
    get: () => previewFontSizeLevel,
    set: setPreviewFontSizeLevel,
  },
  {
    prefix: "settings-terminal-font-size",
    get: () => terminalFontSizeLevel,
    set: setTerminalFontSizeLevel,
  },
].map((control) => ({
  ...control,
  steps: document.getElementById(`${control.prefix}-steps`),
  marker: document.getElementById(`${control.prefix}-marker`),
  name: document.getElementById(`${control.prefix}-name`),
}));
const terminalScrollbackInput = document.getElementById("settings-terminal-scrollback-input");
const terminalSmoothScrollInput = document.getElementById("settings-terminal-smooth-scroll-input");
const toggleTerminalWebgl = document.getElementById("toggle-terminal-webgl");

const toggleAutoCompact = document.getElementById("toggle-auto-compact");
const toggleCodemode = document.getElementById("toggle-codemode");
const thinkingEffortSteps = document.getElementById("thinking-effort-steps");
const thinkingEffortMarker = document.getElementById("thinking-effort-marker");
const thinkingEffortName = document.getElementById("thinking-effort-name");
const toggleShowThinking = document.getElementById("toggle-show-thinking");
const piVersionValue = document.getElementById("setting-pi-version-value");
let piVersionCache = null;
let piVersionInflight = null;
let loadInlineConfigEditor = async () => {};
let loadAgentsMdEditor = async () => {};
let loadAppendSystemMdEditor = async () => {};
let modelsPage = { activate: async () => {} };
let extensionsTabs = null;
let packageManager = null;
const subagentsPage = setupSubagentsTab({
  container: document.getElementById("settings-subagents"),
  transport,
  configGateway,
  getWorkspaceIdentity: () => {
    const workspaceId = wsClient.getRuntimeTarget()?.workspaceId;
    return workspaceId && gitClient.generation != null
      ? { workspaceId, workspaceGeneration: gitClient.generation }
      : null;
  },
});

// Nothing here probes on open: the host is only asked when the user clicks
// Check, and a running maintenance job is resumed from the host's snapshot.
const environmentPage = setupEnvironmentPage({
  transport,
  openExternal: (url) => transport.openExternal(url),
});

function selectSettingsTab(tabKey = "general") {
  const targetTabKey = tabKey === "auth" ? "configuration" : tabKey;
  if (targetTabKey !== "subagents" && !subagentsPage.leave()) return false;
  if (targetTabKey !== "environment") environmentPage.leave();
  // The MCP nav item stays hidden until the pi-mcp-adapter package is
  // detected; refreshAvailability caches after the first check.
  void mcpPage.refreshAvailability();
  settingsNavItems.forEach((item) => {
    item.classList.toggle("active", item.dataset.settingsTab === targetTabKey);
  });
  settingsTabs.forEach((tab) => {
    tab.classList.toggle("active", tab.dataset.settingsPanel === targetTabKey);
  });
  if (targetTabKey === "configuration") {
    loadInlineConfigEditor();
    loadAgentsMdEditor();
    loadAppendSystemMdEditor();
  }
  if (targetTabKey === "models") {
    void modelsPage.activate();
  }
  if (targetTabKey === "extensions") {
    extensionsTabs?.select(document.querySelector('[data-extensions-tab="installed"]'));
    // Tab activation is one-time; re-entering the Extensions page re-checks
    // updates only when the cached result is stale, so a fresh badge paints
    // instantly instead of re-running the expensive network check.
    void packageManager?.auto();
  }
  if (targetTabKey === "skills") {
    void skillsPage.activate();
  }
  if (targetTabKey === "mcp") {
    void mcpPage.activate();
  }
  if (targetTabKey === "subagents") void subagentsPage.activate();
  if (targetTabKey === "environment") void environmentPage.activate();
  if (targetTabKey === "usage") {
    void document.getElementById("settings-cost-dashboard")?.ensureLoaded();
    // The quota panel loads once at boot; re-request on every entry so a
    // failed boot-time probe (runtime not yet ready) does not leave the
    // default quota view blank until the next sub-tab click. Non-force
    // serves the 5-minute cache instantly when warm.
    void settingsQuotaPanel?.loadReports();
  }
  return true;
}

function formatPiVersionError(err, fallback = "unknown error") {
  const raw = String(err?.message || err?.error || err || fallback).trim();
  if (!raw) return fallback;
  return raw.length > 56 ? `${raw.slice(0, 56)}...` : raw;
}

async function loadPiVersion() {
  if (!piVersionValue) return;
  if (piVersionCache) {
    piVersionValue.textContent = piVersionCache;
    return;
  }
  if (piVersionInflight) {
    return;
  }
  piVersionInflight = (async () => {
    try {
      if (nativeAvailable()) {
        const version = await transport.getPiVersion();
        if (version) {
          piVersionCache = version;
          piVersionValue.textContent = piVersionCache;
        } else {
          piVersionValue.textContent = t("status.unavailableVersion");
        }
      } else {
        const data = await rpcCommand({ type: "get_pi_version" });
        if (data?.success && data.data?.version) {
          piVersionCache = data.data.version;
          piVersionValue.textContent = piVersionCache;
        } else {
          const reason = formatPiVersionError(data?.error, "version missing in response");
          console.error("[settings] failed to load pi version:", data);
          piVersionValue.textContent = t("status.unavailableReason", { reason });
        }
      }
    } catch (err) {
      const reason = formatPiVersionError(err);
      console.error("[settings] failed to load pi version:", err);
      piVersionValue.textContent = t("status.unavailableReason", { reason });
    } finally {
      piVersionInflight = null;
    }
  })();
}

function setExtensionActionButton(button, label, loading = false) {
  if (!button) return;
  if (loading) {
    const spinner = document.createElement("span");
    spinner.className = "settings-btn-spinner";
    spinner.setAttribute("aria-hidden", "true");
    const text = document.createElement("span");
    text.textContent = label;
    button.replaceChildren(spinner, text);
    return;
  }
  button.textContent = label;
}

// ═══════════════════════════════════════
// Auto-updater (Tauri-only)
// ═══════════════════════════════════════

const sidebarUpdateBtn = document.getElementById("sidebar-update-btn");
const updater = createAppUpdater({
  transport,
  appVersionValue: document.getElementById("setting-app-version-value"),
  updaterSection: document.getElementById("setting-updater-section"),
  checkUpdatesBtn: document.getElementById("btn-check-updates"),
  updateStatusRow: document.getElementById("setting-update-status-row"),
  updateStatusEl: document.getElementById("setting-update-status"),
  updateInstallRow: document.getElementById("setting-update-install-row"),
  updateInstallLabel: document.getElementById("setting-update-install-label"),
  installUpdateBtn: document.getElementById("btn-install-update"),
  sidebarUpdateBtn,
  onOpenSettings: async () => {
    await openSettings();
    selectSettingsTab("general");
  },
});
void updater.initUpdaterUI();

// Host capabilities arrive asynchronously after the v2 hello handshake.
// Re-evaluate native-gated UI once authenticated.
// App-global registry changed on the host (any window or prune). Refresh the
// sidebar everywhere so all native windows stay in sync.
wsClient.addEventListener("registryChanged", () => {
  // The initiating window is already navigating to the new owner-bound
  // session; refreshing its old page here would render an empty workspace
  // before Pi has prepared the session. Other windows still refresh normally.
  if (workspaceLaunchInProgress) return;
  void sidebar.refresh();
});

wsClient.addEventListener("hostCapabilities", () => {
  refreshHeaderOpenAppButton();
  // C4 drafts: the startup loadAll ran before hello_ack, when the native
  // capability was false. Retry now that the DB channel is real (idempotent;
  // in-memory drafts written during the gap win over stale DB rows).
  void composerDraftStore.loadAll();
  void loadHeaderOpenApps();
  void updater.initUpdaterUI();
  mountTerminalPanelIfNative();
  // Ephemeral chat entry points are native-only.
  const showEphemeral = nativeAvailable();
  document.getElementById("side-chat-btn")?.classList.toggle("hidden", !showEphemeral);
  document.getElementById("quick-chat-btn")?.classList.toggle("hidden", !showEphemeral);
  if (showEphemeral) {
    // The indicator's check feeds packageManager via onUpdates; the Installed
    // list itself stays lazy and first loads when Settings opens.
    void extensionUpdateIndicator?.refresh();
  }
  // Registry rows require authenticated host capability. Native startup
  // refreshes exactly once here, after the hello handshake.
  void refreshInitialSidebar();
  // A same-workspace navigation creates a fresh page. Restore subscriptions
  // for background runtimes so their green-dot lifecycle remains live.
  subscribeToLiveRuntimeTargets();
  void reconcilePreferencesOnce();
});

// DB is the durable preference truth; cookies are only the render cache that
// bootstrap reads synchronously before this async reconciliation runs.
let preferencesReconciled = false;
// One shared client serves startup reconciliation and user-initiated
// theme/locale persistence (SPEC §6.2/§6.3); constructed near `transport`.
async function reconcilePreferencesOnce() {
  if (preferencesReconciled || !nativeAvailable()) return;
  preferencesReconciled = true;
  const outcome = await reconcileRenderPreferences({
    client: preferencesClient,
    entries: [
      {
        key: PREFERENCE_KEYS.theme,
        readCache: () => getCurrentTheme(),
        apply: (value) => applyTheme(String(value)),
      },
      {
        key: PREFERENCE_KEYS.locale,
        readCache: () => getLanguagePreference(),
        apply: (value) => void setLocale(String(value)),
      },
      {
        key: PREFERENCE_KEYS.chatFontSize,
        readCache: () => chatFontSizeLevel,
        apply: (value) => setChatFontSizeLevel(value),
      },
      {
        key: PREFERENCE_KEYS.previewFontSize,
        readCache: () => previewFontSizeLevel,
        apply: (value) => setPreviewFontSizeLevel(value),
      },
      {
        key: PREFERENCE_KEYS.previewTheme,
        readCache: () => previewThemeMode,
        apply: (value) => setPreviewThemeMode(value),
      },
      {
        key: PREFERENCE_KEYS.terminalFontSize,
        readCache: () => terminalFontSizeLevel,
        apply: (value) => setTerminalFontSizeLevel(value),
      },
      {
        key: PREFERENCE_KEYS.terminalThemeMode,
        readCache: () => terminalThemeMode,
        apply: (value) => setTerminalThemeMode(value),
      },
      {
        key: PREFERENCE_KEYS.terminalScrollbackLimit,
        readCache: () => terminalScrollbackLimit,
        apply: (value) => setTerminalScrollbackLimit(value),
      },
      {
        key: PREFERENCE_KEYS.terminalSmoothScrollDuration,
        readCache: () => terminalSmoothScrollDuration,
        apply: (value) => setTerminalSmoothScrollDuration(value),
      },
      {
        key: PREFERENCE_KEYS.terminalWebglRenderer,
        readCache: () =>
          typeof webglRendererEnabled === "boolean" ? webglRendererEnabled : undefined,
        apply: (value) => setTerminalWebglRenderer(value),
      },
    ],
  });
  if (!outcome.ok) console.warn("[App] preference reconcile skipped:", outcome.reason);
  await reconcileAgentPreferences();
}

// Agent settings dual-track (same contract as SPEC §6.2): the DB is the
// durable truth for the show-thinking pref; the thinking level mirror is
// restored into Pi's own settings.json best-effort (fresh install / another
// machine adopts it). Auto-compaction has no DB track: Pi's settings.json is
// its global truth, written by the toggle via the config bridge.
async function reconcileAgentPreferences() {
  if (!preferencesClient.available()) return;
  try {
    const showThinking = await preferencesClient.get(PREFERENCE_KEYS.showThinking);
    if (typeof showThinking === "boolean") {
      applyShowThinking(showThinking);
    } else {
      // First run: lift the existing localStorage cache into the DB.
      const cached = localStorage.getItem("pi-studio-show-thinking") !== "false";
      await preferencesClient.set(PREFERENCE_KEYS.showThinking, cached);
    }
    const thinkingLevel = await preferencesClient.get(PREFERENCE_KEYS.agentThinkingLevel);
    if (THINKING_LEVELS.includes(thinkingLevel)) {
      currentDefaultThinkingLevel = thinkingLevel;
      renderThinkingEffort(thinkingLevel, {
        thinkingSteps: thinkingEffortSteps,
        thinkingMarker: thinkingEffortMarker,
        thinkingName: thinkingEffortName,
      });
      await rpcCommand({ type: "set_default_thinking_level", level: thinkingLevel }, null, true);
    }
  } catch (error) {
    console.warn("[App] agent preference reconcile failed:", error);
  }
}

function buildThemeGrid() {
  themeGrid.replaceChildren();
  const current = getCurrentTheme();

  for (const [id, theme] of Object.entries(themes)) {
    const btn = document.createElement("button");
    btn.className = `theme-swatch${current === id ? " active" : ""}`;
    const colors = document.createElement("span");
    colors.className = "swatch-colors";
    for (const color of theme.colors || []) {
      const dot = document.createElement("span");
      dot.className = "swatch-dot";
      dot.style.background = color;
      colors.appendChild(dot);
    }
    btn.appendChild(colors);
    btn.addEventListener("click", (event) => {
      // User-initiated theme change: render + cookie now, DB mirror for
      // durability across restarts and machines (SPEC §6.2 step 3).
      void saveUserRenderPreference({
        client: preferencesClient,
        key: PREFERENCE_KEYS.theme,
        value: id,
        apply: () => applyTheme(id, { origin: { x: event.clientX, y: event.clientY } }),
      });
      // Re-apply the terminal theme to every live xterm instance, honoring
      // the themeMode preference (forced light/dark ignore the Picot theme).
      const xtermTheme = resolveTerminalTheme(terminalThemeMode);
      for (const entry of terminalClient.tabs.values()) {
        entry.tab?.setTheme?.(xtermTheme);
      }
      themeGrid.querySelectorAll(".theme-swatch").forEach((s) => {
        s.classList.remove("active");
      });
      btn.classList.add("active");
    });
    themeGrid.appendChild(btn);
  }
}

function refreshUsageIframeLocale() {
  const iframe = document.querySelector(".settings-usage-iframe");
  if (!iframe?.contentWindow) return;
  iframe.contentWindow.location.reload();
}

function buildLanguageSelector() {
  if (!languageSelect) return;
  const current = getLanguagePreference();
  languageSelect.replaceChildren();

  for (const lang of LANGUAGES) {
    const option = document.createElement("option");
    option.value = lang.value;
    option.textContent = lang.nativeLabel ?? t(lang.labelKey);
    option.selected = current === lang.value;
    languageSelect.append(option);
  }
}

async function handleLanguageSelectChange() {
  languageSelect.disabled = true;
  try {
    // User-initiated locale change: render + cookie now, DB mirror for
    // durability across restarts and machines (SPEC §6.2 step 3).
    await saveUserRenderPreference({
      client: preferencesClient,
      key: PREFERENCE_KEYS.locale,
      value: languageSelect.value,
      apply: (preference) => setLocale(preference),
    });
    buildLanguageSelector();
    refreshUsageIframeLocale();
  } finally {
    languageSelect.disabled = false;
  }
}

languageSelect?.addEventListener("change", handleLanguageSelectChange);

// ── Terminal preferences (Settings → General) ───────────────────────────
function applyTerminalThemeToAllTabs() {
  const theme = resolveTerminalTheme(terminalThemeMode);
  for (const entry of terminalClient.tabs.values()) {
    entry.tab?.setTheme?.(theme);
  }
}

function applyTerminalPreferencesToAllTabs(prefs) {
  for (const entry of terminalClient.tabs.values()) {
    entry.tab?.applyPreferences?.(prefs);
  }
}

// ── Appearance: live application + selectors ──────────────────────────

function currentPicotThemeIsDark() {
  const themeId = document.documentElement.getAttribute("data-theme") || getCurrentTheme();
  return themes[themeId]?.dark ?? true;
}

/**
 * Mirror the appearance state onto the document (font variables, forced
 * preview theme attribute) and swap the CodeMirror highlight palette.
 */
function applyAppearanceDom() {
  const picotThemeIsDark = currentPicotThemeIsDark();
  applyAppearanceToDom({
    chatFontSize: chatFontSizeLevel,
    previewFontSize: previewFontSizeLevel,
    previewTheme: previewThemeMode,
    picotThemeIsDark,
  });
  // The editor takes the theme id itself: named themes pin their own chrome,
  // "follow" resolves against the active Picot theme.
  setEditorHighlightTheme(previewThemeMode, picotThemeIsDark);
}

// data-theme is written asynchronously inside View Transitions (and by the OS
// scheme fallback in themes.js), so the preview highlight must follow it no
// matter which path changed it — re-resolve on every attribute change.
new MutationObserver(() => applyAppearanceDom()).observe(document.documentElement, {
  attributeFilter: ["data-theme"],
});

// Render + cookie are applied by the caller; mirror into the DB truth so the
// value survives restarts and other machines (same contract as ui.theme).
function persistAppearancePreference(key, value) {
  void saveUserRenderPreference({ client: preferencesClient, key, value, apply: () => {} });
}

function setChatFontSizeLevel(level) {
  chatFontSizeLevel = normalizeFontLevel(level);
  saveAppearanceCookie({ chatFontSize: chatFontSizeLevel });
  persistAppearancePreference(PREFERENCE_KEYS.chatFontSize, chatFontSizeLevel);
  applyAppearanceDom();
}

function setPreviewFontSizeLevel(level) {
  previewFontSizeLevel = normalizeFontLevel(level);
  saveAppearanceCookie({ previewFontSize: previewFontSizeLevel });
  persistAppearancePreference(PREFERENCE_KEYS.previewFontSize, previewFontSizeLevel);
  applyAppearanceDom();
}

function setPreviewThemeMode(mode) {
  previewThemeMode = normalizePreviewThemeMode(mode);
  saveAppearanceCookie({ previewTheme: previewThemeMode });
  persistAppearancePreference(PREFERENCE_KEYS.previewTheme, previewThemeMode);
  applyAppearanceDom();
}

function setTerminalFontSizeLevel(level) {
  terminalFontSizeLevel = normalizeFontLevel(level);
  terminalFontSize = TERMINAL_FONT_SIZE_PX[terminalFontSizeLevel];
  saveAppearanceCookie({ terminalFontSize: terminalFontSizeLevel });
  persistAppearancePreference(PREFERENCE_KEYS.terminalFontSize, terminalFontSizeLevel);
  applyTerminalPreferencesToAllTabs({ fontSize: terminalFontSize });
}

const FONT_LEVEL_LABEL_KEYS = {
  small: "settings.fontLevel.small",
  normal: "settings.fontLevel.normal",
  medium: "settings.fontLevel.medium",
  large: "settings.fontLevel.large",
  xlarge: "settings.fontLevel.xlarge",
};

function renderFontSizeControl(control) {
  renderThinkingEffort(control.get(), {
    thinkingSteps: control.steps,
    thinkingMarker: control.marker,
    thinkingName: control.name,
    levels: FONT_SIZE_LEVELS,
    nameFor: (level) => t(FONT_LEVEL_LABEL_KEYS[level]),
  });
}

function buildAppearanceSelectors() {
  for (const control of fontSizeControls) renderFontSizeControl(control);
  renderPreviewThemeOptions({
    select: previewThemeSelect,
    selected: previewThemeMode,
    t,
  });
}

function buildTerminalThemeSelector() {
  if (!terminalThemeSelect) return;
  terminalThemeSelect.replaceChildren();
  const labels = {
    system: t("settings.terminal.themeSystem"),
    light: t("settings.terminal.themeLight"),
    dark: t("settings.terminal.themeDark"),
  };
  for (const mode of TERMINAL_THEME_MODES) {
    const option = document.createElement("option");
    option.value = mode;
    option.textContent = labels[mode] || mode;
    option.selected = terminalThemeMode === mode;
    terminalThemeSelect.append(option);
  }
}

function setTerminalThemeMode(mode) {
  terminalThemeMode = normalizeThemeMode(mode);
  saveAppearanceCookie({ terminalThemeMode });
  persistAppearancePreference(PREFERENCE_KEYS.terminalThemeMode, terminalThemeMode);
  applyTerminalThemeToAllTabs();
}

function handleTerminalThemeChange() {
  setTerminalThemeMode(terminalThemeSelect.value);
}

function handlePreviewThemeChange() {
  setPreviewThemeMode(previewThemeSelect.value);
}

function setTerminalScrollbackLimit(value) {
  terminalScrollbackLimit = normalizeScrollbackLimit(value);
  if (terminalScrollbackInput) terminalScrollbackInput.value = String(terminalScrollbackLimit);
  saveAppearanceCookie({ terminalScrollbackLimit });
  persistAppearancePreference(PREFERENCE_KEYS.terminalScrollbackLimit, terminalScrollbackLimit);
  applyTerminalPreferencesToAllTabs({ scrollback: terminalScrollbackLimit });
}

function handleTerminalScrollbackChange() {
  setTerminalScrollbackLimit(terminalScrollbackInput.value);
}

function setTerminalSmoothScrollDuration(value) {
  terminalSmoothScrollDuration = normalizeSmoothScrollDuration(value);
  if (terminalSmoothScrollInput) {
    terminalSmoothScrollInput.value = String(terminalSmoothScrollDuration);
  }
  saveAppearanceCookie({ terminalSmoothScrollDuration });
  persistAppearancePreference(
    PREFERENCE_KEYS.terminalSmoothScrollDuration,
    terminalSmoothScrollDuration,
  );
  applyTerminalPreferencesToAllTabs({ smoothScrollDuration: terminalSmoothScrollDuration });
}

function handleTerminalSmoothScrollChange() {
  setTerminalSmoothScrollDuration(terminalSmoothScrollInput.value);
}

function syncTerminalDisplaySettings() {
  if (terminalScrollbackInput) terminalScrollbackInput.value = String(terminalScrollbackLimit);
  if (terminalSmoothScrollInput) {
    terminalSmoothScrollInput.value = String(terminalSmoothScrollDuration);
  }
}

function syncTerminalWebglToggle() {
  toggleTerminalWebgl?.classList.toggle("on", webglRendererEnabled);
}

function setTerminalWebglRenderer(enabled) {
  webglRendererEnabled = Boolean(enabled);
  saveAppearanceCookie({ terminalWebglRenderer: webglRendererEnabled });
  persistAppearancePreference(PREFERENCE_KEYS.terminalWebglRenderer, webglRendererEnabled);
  syncTerminalWebglToggle();
  for (const entry of terminalClient.tabs.values()) {
    if (webglRendererEnabled) {
      entry.tab?.enableWebgl?.(() => new globalThis.PicotXterm.WebglAddon());
    } else {
      entry.tab?.disableWebgl?.();
    }
  }
}

function handleTerminalWebglToggle() {
  setTerminalWebglRenderer(!webglRendererEnabled);
}

terminalThemeSelect?.addEventListener("change", handleTerminalThemeChange);
previewThemeSelect?.addEventListener("change", handlePreviewThemeChange);
// Font-size sliders: click a dot to pick that level; the setter persists and
// applies, then the control re-renders thumb + label.
for (const control of fontSizeControls) {
  control.steps?.addEventListener("click", (event) => {
    const dot = event.target.closest(".thinking-effort-dot");
    if (!dot) return;
    control.set(dot.dataset.level);
    renderFontSizeControl(control);
  });
}
terminalScrollbackInput?.addEventListener("change", handleTerminalScrollbackChange);
terminalSmoothScrollInput?.addEventListener("change", handleTerminalSmoothScrollChange);
toggleTerminalWebgl?.addEventListener("click", handleTerminalWebglToggle);

onLocaleChange(buildLanguageSelector);
onLocaleChange(buildTerminalThemeSelector);
onLocaleChange(buildAppearanceSelectors);
onLocaleChange(() => taskAnalysis?.rerender());

onLocaleChange(() => {
  updateThinkingBtn();
  renderQueuedMessages();
  updateUI();
  updateTokenUsage();
  refreshGitBranch();
  repaintContextViz();
  terminalSearch.applyLocale();
});

function normalizeSettingsTabKey(tabKey) {
  const rawTabKey = typeof tabKey === "string" ? tabKey : "general";
  const decodedTabKey = decodeURIComponent(rawTabKey || "general");
  const normalizedTabKey = decodedTabKey === "auth" ? "configuration" : decodedTabKey;
  const navItem = settingsNavItems.find((item) => item.dataset.settingsTab === normalizedTabKey);
  // Disabled nav items (e.g. the temporarily disabled Agent Inbox) are not
  // routable: hash links and programmatic opens fall back to General.
  if (navItem && !navItem.disabled) return normalizedTabKey;
  return "general";
}

function settingsHashForTab(tabKey) {
  return `#/settings/${encodeURIComponent(normalizeSettingsTabKey(tabKey))}`;
}

function updateSettingsHash(tabKey) {
  const nextHash = settingsHashForTab(tabKey);
  if (window.location.hash === nextHash) return;
  window.history.replaceState(
    null,
    "",
    `${window.location.pathname}${window.location.search}${nextHash}`,
  );
}

function clearSettingsHash() {
  if (!window.location.hash.startsWith("#/settings")) return;
  window.history.replaceState(null, "", `${window.location.pathname}${window.location.search}`);
}

async function openSettings(tabKey = "general", options = {}) {
  const targetTabKey = normalizeSettingsTabKey(tabKey);
  if (!selectSettingsTab(targetTabKey)) return;
  if (options.updateHash !== false) updateSettingsHash(targetTabKey);
  settingsPanel.classList.remove("hidden");
  messagesContainer.style.display = "none";
  document.querySelector(".input-area").style.display = "none";
  document.querySelector(".mode-link:first-child")?.classList.remove("active");
  buildThemeGrid();
  buildLanguageSelector();
  buildTerminalThemeSelector();
  buildAppearanceSelectors();
  syncTerminalDisplaySettings();
  syncTerminalWebglToggle();
  if (piVersionValue) {
    piVersionValue.textContent = piVersionCache || t("status.loading");
  }
  setTimeout(() => {
    if (!settingsPanel.classList.contains("hidden")) loadPiVersion();
  }, 300);
  void refreshLanUrl();
  void mobileAccessCard.refresh();
  void piPathToggle.refresh();
  // Fetch current state for toggles
  try {
    // The auto-compaction toggle owns Pi's global default (settings.json via
    // the config bridge), not the live session's runtime value — read the
    // global truth so the switch reflects what new sessions will inherit.
    const defaults = await rpcCommand({ type: "get_default_auto_compaction" }, null, true);
    const defaultEnabled = defaults?.data?.enabled;
    if (typeof defaultEnabled === "boolean") {
      toggleAutoCompact.className = `settings-toggle${defaultEnabled ? " on" : ""}`;
    }
    // Code mode owns the same kind of global default (defaultTools in
    // settings.json); it applies to new sessions only.
    const codemode = await rpcCommand({ type: "get_default_codemode" }, null, true);
    if (typeof codemode?.data?.enabled === "boolean") {
      toggleCodemode.className = `settings-toggle${codemode.data.enabled ? " on" : ""}`;
    }
    const data = await rpcCommand({ type: "get_state" }, null, true);
    if (data.success && data.data) {
      const s = data.data;
      // Thinking level. The get_state request was issued when Settings opened;
      // if the user picked a level in the meantime, that snapshot is stale and
      // must not revert their choice (check-and-reset marker from toggles.js).
      if (!settingsToggles?.takeUserChangedLevel()) {
        currentThinkingLevel = s.thinkingLevel || "off";
        currentDefaultThinkingLevel = s.defaultThinkingLevel || "medium";
        updateThinkingBtn();
        renderThinkingEffort(currentDefaultThinkingLevel, {
          thinkingSteps: thinkingEffortSteps,
          thinkingMarker: thinkingEffortMarker,
          thinkingName: thinkingEffortName,
        });
      }
      // Session name
      inputSessionName.value = s.sessionName || "";
    }
  } catch (_e) {
    // Silent
  }
}

function closeSettings(options = {}) {
  if (!subagentsPage.leave()) return;
  if (options.clearHash !== false) clearSettingsHash();
  settingsPanel.classList.add("hidden");
  messagesContainer.style.display = "";
  document.querySelector(".input-area").style.display = "";
  document.querySelector(".mode-link:first-child")?.classList.add("active");
}

function restorePageFromHash() {
  const route = window.location.hash.slice(1);
  if (route === "/settings" || route.startsWith("/settings/")) {
    const tabKey = route.split("/")[2] || "general";
    void openSettings(tabKey, { updateHash: false });
    return;
  }
  if (!settingsPanel.classList.contains("hidden")) {
    closeSettings({ clearHash: false });
  }
}

async function openUpdatesFromSidebar() {
  await updater.openUpdatesFromSidebar();
}

settingsBtn.addEventListener("click", () => {
  void openSettings();
});
sidebarUpdateBtn?.addEventListener("click", () => {
  openUpdatesFromSidebar().catch((err) => {
    console.warn("[updater] unable to open updates from sidebar:", err);
  });
});
settingsClose.addEventListener("click", closeSettings);
settingsOverlay?.addEventListener("click", closeSettings);
settingsNavItems.forEach((item) => {
  item.addEventListener("click", () => {
    const tabKey = item.dataset.settingsTab || "general";
    if (selectSettingsTab(tabKey)) updateSettingsHash(tabKey);
  });
});

const settingsToggles = setupSettingsToggles({
  toggleAutoCompact,
  toggleCodemode,
  thinkingSteps: thinkingEffortSteps,
  thinkingMarker: thinkingEffortMarker,
  thinkingName: thinkingEffortName,
  toggleShowThinking,
  rpcCommand,
  getDefaultThinkingLevel: () => currentDefaultThinkingLevel,
  setDefaultThinkingLevel: (level) => {
    currentDefaultThinkingLevel = level;
  },
  onRuntimeLevelChanged: (level) => {
    currentThinkingLevel = level;
    updateThinkingBtn();
  },
  // Dual-track: mirror the pick into the DB after the RPC succeeded, so the
  // value survives restarts and other machines (same contract as ui.theme).
  // Auto-compaction is NOT mirrored here: its global default lives in Pi's
  // settings.json (the toggle writes it via the config bridge).
  persistThinkingLevel: (level) =>
    void preferencesClient.set(PREFERENCE_KEYS.agentThinkingLevel, level),
  persistShowThinking: (show) => void preferencesClient.set(PREFERENCE_KEYS.showThinking, show),
});

// Task notifications toggle (Settings → General → Agent): DB-backed like
// show-thinking, but self-contained because no runtime RPC is involved —
// the flag gates the frame-fed notification trigger directly.
const taskNotificationsToggle = document.getElementById("toggle-task-notifications");
if (taskNotificationsToggle) {
  void preferencesClient
    .get(PREFERENCE_KEYS.taskNotifications)
    .then((value) => {
      taskNotificationsEnabled = value !== false;
      taskNotificationsToggle.classList.toggle("on", taskNotificationsEnabled);
    })
    .catch(() => {});
  taskNotificationsToggle.addEventListener("click", async () => {
    // Derive the next state from the element itself: the visual toggle is the
    // single source of truth, so a variable/visual desync can never make a
    // click flip state invisibly (looked like "the button does nothing").
    const next = !taskNotificationsToggle.classList.contains("on");
    taskNotificationsToggle.classList.toggle("on", next);
    taskNotificationsEnabled = next;
    try {
      await preferencesClient.set(PREFERENCE_KEYS.taskNotifications, next);
    } catch (error) {
      console.error("[settings] task notifications preference save failed:", error);
      taskNotificationsEnabled = !next;
      taskNotificationsToggle.classList.toggle("on", !next);
    }
  });
}

const piPathToggle = setupPiPathToggle({
  transport,
  toggle: document.getElementById("toggle-pi-path"),
  note: document.getElementById("pi-path-note"),
});

const mobileAccessCard = setupMobileAccess({
  transport,
  toggle: document.getElementById("toggle-mobile-access"),
  details: document.getElementById("mobile-access-details"),
  pairBtn: document.getElementById("mobile-pair-btn"),
  pairing: document.getElementById("mobile-access-pairing"),
  qrCanvas: document.getElementById("mobile-qr-canvas"),
  tokenEl: document.getElementById("mobile-pair-token"),
  restartHint: document.getElementById("mobile-restart-hint"),
});
({ loadInlineConfigEditor, loadAgentsMdEditor, loadAppendSystemMdEditor } = setupSettingsConfig({
  configGateway,
  clearSettingsSaveMessage,
  setSettingsSaveButtonSaving,
  showSettingsSaveError,
  showSettingsSaveSuccess,
}));

modelsPage = setupModelsPage({
  configGateway,
  oauthGateway,
  onModelConfigurationChanged: async () => {
    await fetchModelInfo();
    updateUI();
  },
  clearSettingsSaveMessage,
  setSettingsSaveButtonSaving,
  showSettingsSaveError,
  showSettingsSaveSuccess,
});

const skillsSaveMessageEl = document.getElementById("settings-skills-save-message");
const skillsPage = setupSkillsPage({
  container: document.getElementById("settings-skills"),
  rpcCommand,
  showSuccess: (msg) => showSettingsSaveSuccess(skillsSaveMessageEl, msg),
  showError: (msg) => showSettingsSaveError(skillsSaveMessageEl, msg),
  onInstallRequest: (scope, trigger) => {
    if (skillsInstallPage.open(scope, { trigger })) syncSkillsInstallArea();
  },
});
const packageSkillsPage = setupPackageSkillsTab({
  container: document.getElementById("settings-package-skills"),
  rpcCommand,
  showSuccess: (msg) => showSettingsSaveSuccess(skillsSaveMessageEl, msg),
  showError: (msg) => showSettingsSaveError(skillsSaveMessageEl, msg),
});
const skillsInstallPanelEl = document.getElementById("settings-install-skills");
const skillsInstallPage = setupSkillsInstallTab({
  container: skillsInstallPanelEl,
  transport,
  isProjectTrusted: () => skillsPage.isProjectTrusted(),
  hasWorkspace: () => true,
  showSuccess: (msg) => showSettingsSaveSuccess(skillsSaveMessageEl, msg),
  showError: (msg) => showSettingsSaveError(skillsSaveMessageEl, msg),
  onStateChange: () => syncSkillsInstallArea(),
  onClose: () => syncSkillsInstallArea(),
});

// The install area is not a tab: the custom tab's install entry opens it
// below the custom list for the scope being displayed. While it is open the
// custom tab locks its own controls; switching to the packages tab hides the
// panel but keeps the install session alive.
let skillsCustomTabActive = true;
function syncSkillsInstallArea() {
  skillsPage.setInstallLocked(skillsInstallPage.isOpen());
  if (skillsCustomTabActive) {
    skillsInstallPanelEl.classList.toggle("hidden", !skillsInstallPage.isOpen());
  }
}

const packageBrowse = setupPackageBrowse({
  root: document,
  transport,
  nativeAvailable,
  t,
  createIcon,
  renderPackageInstallFailure,
  setExtensionActionButton,
  onInstalledChanged: () => packageManager?.load({ recheck: false }),
});

const mcpPage = setupMcpPage({
  masterEl: document.getElementById("mcp-master"),
  detailEl: document.getElementById("mcp-detail"),
  tabs: document.querySelectorAll("[data-mcp-tab]"),
  navItem: document.querySelector('[data-settings-tab="mcp"]'),
  configGateway,
  // Sign-in rides the host control plane (the Rust host spawns `pi mcp`), not
  // the session runtime the config gateway needs.
  mcpLogin: createMcpHostOps(transport),
  openExternal: (url) => transport.openExternal(url),
  captionEl: document.getElementById("mcp-tab-caption"),
  // Project MCP actions are bound to the workspace identity (workspace +
  // generation) and to the exact routing triple the user acted on: a switch
  // while readiness opens rejects instead of writing into the new workspace.
  getContextKey: () => {
    const target = getSettingsRuntimeTarget();
    return [target?.workspaceId ?? "no-workspace", gitClient.generation ?? 0].join("\u0000");
  },
  getRuntimeTarget: getSettingsRuntimeTarget,
});
packageManager = setupPackageManager({
  root: document,
  transport,
  nativeAvailable,
  t,
  getWorkspaceId: () => `workspace:${getCurrentWorkspacePath() || "unknown"}`,
  getSessionId: () => mirrorActiveSessionFile || sidebar.activeSessionFile || wsClient.sessionId,
  onRestarted: () => wsClient.forceReconnect(),
  onUpdatesChecked: (count) => extensionUpdateIndicator.setCount(count),
  configGateway,
});
const extensionUpdateIndicator = setupExtensionUpdateIndicator({
  transport,
  nativeAvailable,
  t,
  buttonEl: document.getElementById("sidebar-extension-update-btn"),
  onOpen: () => void openSettings("extensions"),
  onUpdates: (updates) => packageManager?.applyUpdateStates(updates),
});
extensionsTabs = setupExtensionsTabShell({
  tabs: document.querySelectorAll("[data-extensions-tab]"),
  panels: {
    installed: document.getElementById("extensions-installed"),
    community: document.getElementById("extensions-community"),
  },
  activate: (name) => {
    if (name === "installed") return packageManager.auto();
    if (name === "community") return packageBrowse.load();
  },
});

setupSkillsTabShell({
  tabs: document.querySelectorAll("[data-skills-page-tab]"),
  panels: {
    discovered: document.getElementById("settings-skills"),
    packages: document.getElementById("settings-package-skills"),
  },
  activate: (name) => {
    if (name === "discovered") {
      skillsCustomTabActive = true;
      syncSkillsInstallArea();
      return skillsPage.activate();
    }
    if (name === "packages") {
      skillsCustomTabActive = false;
      skillsInstallPanelEl.classList.add("hidden");
      return packageSkillsPage.activate();
    }
  },
});

contextVizController = setupContextViz({
  tokenUsageEl,
  contextViz: document.getElementById("context-viz"),
  contextBar: document.getElementById("context-bar"),
  contextLegend: document.getElementById("context-legend"),
  contextVizUsed: document.getElementById("context-viz-used"),
  contextVizTotal: document.getElementById("context-viz-total"),
  getUsage: () => lastUsage,
  getContextWindowSize: () => contextWindowSize,
  requestCompact,
  getCompactState: () => compactCoordinator.state,
});

// Composer session-aggregate cluster (↑in ↓out ⚡cache) — separate from the
// current-context lastUsage lifecycle so Compact can invalidate stale context
// without fabricating usage. Session cost is not shown here: each turn's own
// footer already reports it.
const headerStatusBar = createHeaderStatusBar({
  sessionUsageEl,
  tokenUsageEl,
  getContextWindowSize: () => contextWindowSize,
});

/** Monotonic generation for invalidating stale stats responses on switches. */
let statsHydrationGeneration = 0;

function resetHeaderStatusBar() {
  statsHydrationGeneration += 1;
  headerStatusBar?.reset();
}

/** Current active session file, for aggregate-vs-session identity checks. */
function activeSessionFileForStatusBar() {
  return mirrorActiveSessionFile || sidebar.activeSessionFile || null;
}

/** Hydrate the authoritative session aggregate from get_session_stats. */
async function hydrateHeaderSessionStats() {
  const generation = ++statsHydrationGeneration;
  try {
    const data = await rpcCommand({ type: "get_session_stats" }, null, true);
    if (!data?.success || !data.data) return;
    // Drop a response whose session changed, or that lost a generation race
    // against a newer session switch / mirror sync.
    if (generation !== statsHydrationGeneration) return;
    if (!data.data.sessionFile) return;
    const activeSessionFile = activeSessionFileForStatusBar();
    if (!activeSessionFile || data.data.sessionFile !== activeSessionFile) return;
    headerStatusBar?.hydrateSessionStats({
      sessionFile: data.data.sessionFile,
      tokens: data.data.tokens,
    });
  } catch (error) {
    // Aggregate hydration is best-effort; the current-context path still works.
    console.warn("[header-status] failed to hydrate session stats:", error);
  }
}

setupVoiceInput({
  micBtn: document.getElementById("mic-btn"),
  messageInput,
});
setButtonIcon(document.getElementById("mic-btn"), "mic", { size: 16 });

// Composer fit: hide the least important controls when the row genuinely stops
// fitting, so nothing is ever pushed outside the card. Driven by measured
// overflow, not a viewport breakpoint — the chat column also narrows when the
// sidebars are dragged wider.
setupComposerFit({
  card: document.getElementById("composer-card"),
  toolbar: document.querySelector(".composer-toolbar"),
});

// ═══════════════════════════════════════
// Initialize
// ═══════════════════════════════════════

// On mobile, collapse model bar above input
if (isMobile()) {
  sidebarEl.classList.add("collapsed");

  const mobileBar = document.getElementById("mobile-model-bar");

  // Start collapsed
  mobileBar.classList.add("collapsed");

  // Toggle via chevron
  const contextToggle = document.getElementById("mobile-context-toggle");
  const syncContextToggleIcon = () => {
    const iconName = mobileBar.classList.contains("collapsed") ? "chevron-down" : "chevron-up";
    setButtonIcon(contextToggle, iconName, { size: 12 });
  };
  syncContextToggleIcon();
  contextToggle.addEventListener("click", () => {
    mobileBar.classList.toggle("collapsed");
    contextToggle.classList.toggle("flipped", !mobileBar.classList.contains("collapsed"));
    syncContextToggleIcon();
  });
}

// Make the Picot icon in sidebar switch back to chat
document.querySelector(".mode-link:first-child")?.addEventListener("click", () => {
  closeSettings();
});

// ═══════════════════════════════════════
// Register workspace and create its first primary session
// ═══════════════════════════════════════

function handleRegisteredWorkspace(targetCwd) {
  return startRegisteredWorkspaceSession({
    targetCwd,
    transport,
    navigate: navigateInWindow,
    onBeforeSwap: onBeforeInstanceSwap,
    beforeWorkspaceTransition: prepareEphemeralWorkspaceTransition,
    onWorkspaceTransitionCancelled: cancelEphemeralWorkspaceTransition,
    renderError: (message) => messageRenderer.renderError(message),
  });
}

// ═══════════════════════════════════════
// Open Folder as workspace
// ═══════════════════════════════════════

async function handleOpenFolder() {
  if (workspaceLaunchInProgress) return;
  setWorkspaceLaunchInProgress(true);
  try {
    await openFolderAsWorkspace({
      transport,
      navigate: navigateInWindow,
      onBeforeSwap: onBeforeInstanceSwap,
      beforeWorkspaceTransition: prepareEphemeralWorkspaceTransition,
      onWorkspaceTransitionCancelled: cancelEphemeralWorkspaceTransition,
      renderError: (message) => messageRenderer.renderError(message),
    });
  } finally {
    setWorkspaceLaunchInProgress(false);
  }
}

window.addEventListener("hashchange", restorePageFromHash);
restorePageFromHash();

wsClient.connect();
dismissBootSwapOverlayWhenReady();

// --- Native navigation state restore (B) + sidebar cache (C) ---
// Restore expansion/search before the first registry load: expanded rows must
// fetch their lazy session lists during that load, not only after a manual refresh.
const pendingNavState = consumeNavState();
if (pendingNavState) {
  sidebar.expandedWorkspaces = pendingNavState.expandedWorkspaces;
  sidebar.searchQuery = pendingNavState.searchQuery;
}
// Separately, hydrate sidebar from cached project tree before real loadSessions.
const cachedProjects = readCachedSidebarProjects();
// readCachedSidebarProjects already filters to complete projects (valid
// workspaceId + path + sessions array), so hydration never renders broken
// rows or empty titles while the session list is still loading.
if (cachedProjects) {
  sidebar.projects = cachedProjects;
  // Hydrated registry rows already know their pinned flags; seed the pin
  // view so the PINNED section renders instantly, then the broker list
  // response replaces both.
  sidebar._registryPins = registryPinsFromProjects(cachedProjects);
  sidebar.render();
}

renderWorkspaceWelcome();
let initialSidebarRefreshStarted = false;
function refreshInitialSidebar() {
  if (initialSidebarRefreshStarted) return;
  initialSidebarRefreshStarted = true;
  const runtimeTarget = wsClient.getRuntimeTarget();
  if (shouldShowProvisionalSession({ runtimeTarget })) {
    sidebar.setProvisionalSession({
      workspaceId: runtimeTarget.workspaceId,
      sessionId: runtimeTarget.sessionId,
    });
  }
  sidebar.refresh().then((projects) => {
    // Native startup normally has the route target before the first refresh.
    // Keep the fallback for a late route bootstrap without delaying the normal
    // provisional row until after the registry request completes.
    if (!runtimeTarget) {
      const lateRuntimeTarget = wsClient.getRuntimeTarget();
      if (shouldShowProvisionalSession({ runtimeTarget: lateRuntimeTarget })) {
        sidebar.setProvisionalSession({
          workspaceId: lateRuntimeTarget.workspaceId,
          sessionId: lateRuntimeTarget.sessionId,
        });
      }
    }
    sessionsLoaded = true;
    // Cache fresh sidebar data for next native navigation (C).
    try {
      cacheSidebarProjects(projects || sidebar.projects);
    } catch {
      /* best-effort */
    }
    updateUI();
    // Registry data is now rendered with restored expansion/search. Apply the
    // remaining positional state only after the DOM exists.
    if (pendingNavState) {
      if (pendingNavState.sidebarScroll != null && sidebarEl) {
        requestAnimationFrame(() => {
          sidebarEl.scrollTop = pendingNavState.sidebarScroll;
        });
      }
      if (pendingNavState.messageScroll != null && messagesContainer) {
        requestAnimationFrame(() => {
          messagesContainer.scrollTop = pendingNavState.messageScroll;
        });
      }
      if (pendingNavState.inputDraft && messageInput && !messageInput.value) {
        messageInput.value = pendingNavState.inputDraft;
      }
    }
    if (!hasAnySessionsLoaded()) {
      renderWorkspaceWelcome();
    }
    if (deferredMirrorSync) {
      const syncData = deferredMirrorSync;
      deferredMirrorSync = null;
      handleMirrorSync(syncData);
    }
    if (isMirrorMode) updateMirrorLiveIndicator();
    resolveAndApplyFocus();
  });
}
// Mobile mode has no native host-capability handshake.
if (mobileClientMode) refreshInitialSidebar();

// Dismiss mobile splash screen
const splash = document.getElementById("mobile-splash");
if (splash) {
  requestAnimationFrame(() => {
    splash.classList.add("hidden");
    setTimeout(() => splash.remove(), 300);
  });
}

console.log("🚀 Picot initialized");
