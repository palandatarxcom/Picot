// ABOUTME: Tests for the native-only Terminal Panel: lazy first creation, remote
// ABOUTME: gating, height clamping, close-risk reporting, and DOM teardown.
import { afterEach, expect, test, vi } from "vitest";
import { formatTerminalStartError, TerminalPanel } from "./terminal-panel.js";

// Tests assert aria-labels against the i18n key (fallback form), so stub
// i18n to return the key itself and avoid missing-key console warns.
vi.mock("./i18n.js", () => ({ t: (key) => key }));

function mountedPanel(opts = {}) {
  const client = opts.client || { create: vi.fn(), closeAll: vi.fn(), checkpointAll: vi.fn() };
  const panel = new TerminalPanel({
    native: opts.native !== undefined ? opts.native : true,
    client,
    subscribeLocale: opts.subscribeLocale,
    getAvailableHeight: opts.getAvailableHeight || (() => opts.availableHeight || 800),
  });
  panel.mount({ toggleContainer: document.body, panelContainer: document.body });
  return { panel, client };
}

afterEach(() => {
  document.body.textContent = "";
});

test("formats Git for Windows start failures with localized guidance", () => {
  expect(formatTerminalStartError("Git for Windows was not found.")).toBe(
    "terminal.gitBashMissing",
  );
  expect(formatTerminalStartError("pty spawn failed")).toBe("pty spawn failed");
  expect(formatTerminalStartError("")).toBe("terminal.statusFailed");
});

test("failed tab metadata shows the host error inside the panel", () => {
  const { panel } = mountedPanel();
  panel.setTabs([
    {
      terminalId: "t-fail",
      generation: 1,
      label: "Terminal",
      profileId: "default",
      status: "failed",
      failReason: "Git for Windows was not found.",
    },
  ]);
  const errorEl = panel.bodyEl.querySelector("[data-terminal-start-error]");
  expect(errorEl).not.toBeNull();
  expect(errorEl.textContent).toBe("terminal.gitBashMissing");
  expect(errorEl.getAttribute("role")).toBe("alert");
});

test("running tabs clear a previous start error", () => {
  const { panel } = mountedPanel();
  panel.showStartError("spawn failed");
  panel.setTabs([
    { terminalId: "t1", generation: 1, label: "zsh", profileId: "default", status: "running" },
  ]);
  expect(panel.bodyEl.querySelector("[data-terminal-start-error]")).toBeNull();
});

test("a failed tab shows the banner while other tabs stay interactive", () => {
  const restart = vi.fn();
  const close = vi.fn();
  const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
  const { panel } = mountedPanel({ client: { restart, close } });
  panel.setTabs([
    {
      terminalId: "t-fail",
      generation: 1,
      label: "broken",
      profileId: "default",
      status: "failed",
      failReason: "pty spawn failed",
    },
    { terminalId: "t-ok", generation: 1, label: "zsh", profileId: "default", status: "running" },
  ]);

  const banner = panel.bodyEl.querySelector("[data-terminal-start-error]");
  expect(banner).not.toBeNull();

  // The healthy tab still renders and its controls keep working.
  const okItem = panel.tabButtons.get("t-ok").parentElement;
  okItem.querySelector(".terminal-tab-restart").click();
  expect(restart).toHaveBeenCalledWith("t-ok", 1);
  okItem.querySelector(".terminal-tab-close").click();
  expect(close).toHaveBeenCalledWith("t-ok", 1);

  // The error exists only as the body banner, never inside tab-bar DOM.
  expect(panel.bodyEl.querySelectorAll("[data-terminal-start-error]")).toHaveLength(1);
  expect(panel.tabBarEl.querySelector("[data-terminal-start-error]")).toBeNull();
  confirm.mockRestore();
});

test("retrying a failed tab drives client.restart and success clears the banner", () => {
  const restart = vi.fn();
  const { panel } = mountedPanel({ client: { restart } });
  panel.setTabs([
    {
      terminalId: "t-fail",
      generation: 1,
      label: "broken",
      profileId: "default",
      status: "failed",
      failReason: "Git for Windows was not found.",
    },
  ]);
  expect(panel.bodyEl.querySelector("[data-terminal-start-error]")).not.toBeNull();

  panel.tabButtons.get("t-fail").parentElement.querySelector(".terminal-tab-restart").click();
  expect(restart).toHaveBeenCalledWith("t-fail", 1);

  // Host reports the restarted terminal as running → banner clears, tab renders.
  panel.setTabs([
    {
      terminalId: "t-fail",
      generation: 2,
      label: "broken",
      profileId: "default",
      status: "running",
    },
  ]);
  expect(panel.bodyEl.querySelector("[data-terminal-start-error]")).toBeNull();
  const btn = panel.tabButtons.get("t-fail");
  expect(btn).not.toBeNull();
  expect(btn.classList.contains("active")).toBe(true);
});

test("first native expansion lazily creates one default tab", async () => {
  const { panel, client } = mountedPanel();
  expect(panel.toggleEl.classList.contains("panel-toggle-btn")).toBe(true);
  expect(panel.toggleEl.getAttribute("aria-label")).toBe("terminal.toggle");
  expect(
    panel.toggleEl.querySelector('svg[viewBox="0 0 24 24"][aria-hidden="true"]'),
  ).not.toBeNull();
  expect(panel.toggleEl.querySelector('svg[stroke="currentColor"]')).not.toBeNull();
  expect(panel.toggleEl.dataset.terminalCount).toBeUndefined();
  expect(panel.isExpanded()).toBe(false);
  await panel.expand();
  expect(client.create).toHaveBeenCalledWith("default");
});

test("remote client renders no toggle, panel, metadata, or activity", () => {
  mountedPanel({ native: false });
  expect(document.querySelector("[data-terminal-toggle]")).toBeNull();
  expect(document.querySelector("[data-terminal-panel]")).toBeNull();
});

test("resizer clamps to 160px minimum and 70 percent maximum", () => {
  const { panel } = mountedPanel({ availableHeight: 1000 });
  expect(panel.setHeight(10)).toBe(160);
  expect(panel.setHeight(900)).toBe(700);
  expect(panel.setHeight(400)).toBe(400);
});

test("resizing the panel refits the xterm viewport", () => {
  const refitAll = vi.fn();
  const { panel } = mountedPanel({ client: { refitAll } });

  panel.setHeight(360);

  expect(refitAll).toHaveBeenCalledTimes(1);
});

test("closing the final tab collapses the panel", async () => {
  const { panel } = mountedPanel();
  await panel.expand();

  panel.setTabs([]);

  expect(panel.isExpanded()).toBe(false);
  expect(panel.root.classList.contains("hidden")).toBe(true);
});

test("body resize refits xterm and destroy disconnects the observer", () => {
  const observer = { observe: vi.fn(), disconnect: vi.fn() };
  vi.stubGlobal(
    "ResizeObserver",
    vi.fn(() => observer),
  );
  const refitAll = vi.fn();
  const { panel } = mountedPanel({ client: { refitAll } });

  expect(observer.observe).toHaveBeenCalledWith(panel.bodyEl);
  globalThis.ResizeObserver.mock.calls[0][0]();
  expect(refitAll).toHaveBeenCalledTimes(1);
  panel.destroy();
  expect(observer.disconnect).toHaveBeenCalledTimes(1);
  vi.unstubAllGlobals();
});

test("terminal tab close control is a sibling of the tab button", () => {
  const { panel } = mountedPanel();
  panel.setTabs([
    { terminalId: "t1", generation: 1, label: "zsh", profileId: "default", status: "running" },
  ]);

  const tabButton = panel.tabButtons.get("t1");
  const close = panel.root.querySelector(".terminal-tab-close");

  expect(tabButton).not.toBeNull();
  expect(close).not.toBeNull();
  expect(close.parentElement).toBe(tabButton.parentElement);
  expect(tabButton.contains(close)).toBe(false);
});

test("enlarging keeps the terminal tab bar and panel controls mounted", async () => {
  const { panel } = mountedPanel();
  await panel.expand();
  panel.setTabs([
    { terminalId: "t1", generation: 1, label: "zsh", profileId: "default", status: "running" },
  ]);

  panel.toggleEnlarge();

  expect(panel.root.classList.contains("enlarged")).toBe(true);
  expect(panel.root.querySelector(".terminal-tab-bar")).not.toBeNull();
  expect(panel.root.querySelector(".terminal-tab")).not.toBeNull();
  expect(panel.root.querySelector("[data-terminal-new-tab]")).not.toBeNull();
  expect(panel.root.querySelector("[data-terminal-enlarge]")).not.toBeNull();
  expect(panel.root.querySelector("[data-terminal-collapse]")).not.toBeNull();
  expect(panel.root.querySelector(".terminal-body")).not.toBeNull();

  // The panel-level control only hides the panel, so it must not wear the X
  // that closes a tab: it is the minus glyph, and it is labelled.
  const collapse = panel.root.querySelector("[data-terminal-collapse]");
  expect(collapse.querySelector("svg path").getAttribute("d")).toBe("M5 12h14");
  // This suite mocks t() as key passthrough; the wording lives in the locales.
  expect(collapse.getAttribute("aria-label")).toBe("terminal.collapse");
  expect(collapse.title).toBe("terminal.collapse");

  panel.toggleEnlarge();
  expect(panel.root.classList.contains("enlarged")).toBe(false);
});

test("enlarged panel reserves the header offset and controls remain interactive", async () => {
  const header = document.createElement("div");
  header.className = "header";
  document.body.appendChild(header);
  Object.defineProperty(header, "offsetHeight", { configurable: true, value: 42 });
  const { panel } = mountedPanel();
  await panel.expand();

  expect(panel.root.style.getPropertyValue("--terminal-header-offset")).toBe("42px");
  panel.toggleEnlarge();
  expect(panel.root.classList.contains("enlarged")).toBe(true);
  expect(panel.root.style.getPropertyValue("--terminal-header-offset")).toBe("42px");

  panel.enlargeButton.click();
  expect(panel.root.classList.contains("enlarged")).toBe(false);
  panel.root.querySelector("[data-terminal-collapse]").click();
  expect(panel.root.classList.contains("hidden")).toBe(true);
});

test("closing a running tab requires confirmation", () => {
  const close = vi.fn();
  const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
  const { panel } = mountedPanel({ client: { close } });
  panel.setTabs([
    { terminalId: "t1", generation: 1, label: "zsh", profileId: "default", status: "running" },
  ]);

  panel.tabButtons.get("t1").parentElement.querySelector(".terminal-tab-close").click();

  expect(confirm).toHaveBeenCalledTimes(1);
  expect(close).not.toHaveBeenCalled();
  confirm.mockReturnValue(true);
  panel.tabButtons.get("t1").parentElement.querySelector(".terminal-tab-close").click();
  expect(close).toHaveBeenCalledWith("t1", 1);
  confirm.mockRestore();
});

test("getCloseRisk reports only live terminals with stable labels", async () => {
  const { panel } = mountedPanel();
  await panel.expand();
  panel.setTabs([
    { terminalId: "t1", generation: 1, label: "zsh", profileId: "default", status: "running" },
    { terminalId: "t2", generation: 1, label: "gone", profileId: "default", status: "exited" },
  ]);
  const risk = panel.getCloseRisk();
  expect(risk.terminalTabs).toEqual([{ terminalId: "t1", label: "zsh" }]);
});

test("collapse retains the saved expanded height", async () => {
  const { panel } = mountedPanel({ availableHeight: 1000 });
  await panel.expand();
  panel.setHeight(300);
  panel.collapse();
  expect(panel.heightPx).toBe(300);
});

test("destroy removes toggle and panel from the DOM", async () => {
  const { panel } = mountedPanel();
  await panel.expand();
  panel.destroy();
  expect(document.querySelector("[data-terminal-toggle]")).toBeNull();
  expect(document.querySelector("[data-terminal-panel]")).toBeNull();
});

test("beforeWorkspaceTransition locks interaction and checkpoints", async () => {
  const checkpointAll = vi.fn(async () => {});
  const { panel } = mountedPanel({ client: { checkpointAll } });
  await panel.expand();
  await panel.beforeWorkspaceTransition();
  expect(checkpointAll).toHaveBeenCalledTimes(1);
  expect(panel.locked).toBe(true);
});

test("setInteractionLocked toggles the lock", () => {
  const { panel } = mountedPanel();
  panel.setInteractionLocked(true);
  expect(panel.locked).toBe(true);
  panel.setInteractionLocked(false);
  expect(panel.locked).toBe(false);
});

test("a transition that finishes cannot unfreeze a window-close transaction (R3)", async () => {
  const checkpointAll = vi.fn(async () => {});
  const { panel } = mountedPanel({ client: { checkpointAll } });
  panel.setTabs([
    { terminalId: "t1", generation: 1, label: "zsh", profileId: "default", status: "running" },
  ]);
  // A same-workspace commit is in flight when the host asks to close the window.
  await panel.beforeWorkspaceTransition();
  panel.setInteractionLocked(true);
  // The commit succeeds and the page keeps its binding.
  panel.finishWorkspaceTransition();
  expect(panel.locked).toBe(true);
  // The close transaction still owns its own lock, so the panel stays frozen
  // until the coordinator releases it.
  panel.setInteractionLocked(false);
  expect(panel.locked).toBe(false);
});

test("a close transaction that settles cannot unfreeze a running transition (R3)", async () => {
  const checkpointAll = vi.fn(async () => {});
  const { panel } = mountedPanel({ client: { checkpointAll } });
  panel.setInteractionLocked(true);
  await panel.beforeWorkspaceTransition();
  panel.setInteractionLocked(false);
  expect(panel.locked).toBe(true);
  panel.finishWorkspaceTransition();
  expect(panel.locked).toBe(false);
});

test("clearProjection drops tabs, active tab and activity without touching the client", async () => {
  const closeAll = vi.fn(async () => {});
  const { panel } = mountedPanel({ client: { closeAll } });
  panel.setTabs([
    { terminalId: "t1", generation: 1, label: "zsh", profileId: "default", status: "running" },
    { terminalId: "t2", generation: 1, label: "zsh", profileId: "default", status: "running" },
  ]);
  panel.setActiveTerminalId("t2");
  panel.markActivity("t1");
  panel.clearProjection();
  expect(panel.tabs).toEqual([]);
  expect(panel.activeTerminalId).toBeNull();
  expect(panel.getProjection()).toEqual({ count: 0, hasActivity: false });
  // Close risk follows the projection: no live tab is reported any more.
  expect(panel.getCloseRisk().terminalTabs).toEqual([]);
  // The projection is page state only: no daemon PTY is closed here.
  expect(closeAll).not.toHaveBeenCalled();
  expect(document.querySelectorAll(".terminal-tab[data-terminal-id]")).toHaveLength(0);
});

test("markActivity tracks background output and clearActivity resets it", () => {
  const { panel } = mountedPanel();
  panel.setTabs([
    { terminalId: "t1", generation: 1, label: "zsh", profileId: "default", status: "running" },
  ]);
  expect(panel.getProjection()).toEqual({ count: 1, hasActivity: false });
  panel.markActivity("t1");
  expect(panel.getProjection()).toEqual({ count: 1, hasActivity: true });
  expect(panel.toggleEl.classList.contains("has-activity")).toBe(false);
  expect(panel.toggleEl.dataset.terminalCount).toBeUndefined();
  panel.clearActivity("t1");
  expect(panel.getProjection()).toEqual({ count: 1, hasActivity: false });
});

test("expand clears background activity", async () => {
  const { panel } = mountedPanel();
  panel.setTabs([
    { terminalId: "t1", generation: 1, label: "zsh", profileId: "default", status: "running" },
  ]);
  panel.markActivity("t1");
  expect(panel.getProjection().hasActivity).toBe(true);
  await panel.expand();
  expect(panel.getProjection().hasActivity).toBe(false);
});

test("beforeWorkspaceTransition checkpoints even when collapsed", async () => {
  const checkpointAll = vi.fn(async () => {});
  const { panel } = mountedPanel({ client: { checkpointAll } });
  expect(panel.isExpanded()).toBe(false);
  const ok = await panel.beforeWorkspaceTransition();
  expect(ok).toBe(true);
  expect(checkpointAll).toHaveBeenCalledTimes(1);
});

test("settleCloseRisk unlocks on cancel and closes every terminal on discard", async () => {
  const closeAll = vi.fn(async () => {});
  const { panel } = mountedPanel({ client: { closeAll } });

  panel.setInteractionLocked(true);
  await panel.settleCloseRisk("cancel");
  expect(panel.locked).toBe(false);
  expect(closeAll).not.toHaveBeenCalled();

  await panel.settleCloseRisk("discard");
  expect(panel.locked).toBe(true);
  expect(closeAll).toHaveBeenCalledTimes(1);
});

test("restored terminal metadata shows one restart notice and recreates tabs", async () => {
  const create = vi.fn(async () => {});
  const { panel } = mountedPanel({ client: { create } });
  panel.setTabs([
    {
      terminalId: "restored-1",
      generation: 0,
      label: "zsh",
      profileId: "bash",
      status: "restoredMetadata",
    },
  ]);

  await panel.expand();
  expect(create).toHaveBeenCalledWith("bash");
  expect(panel.bodyEl.querySelectorAll(".terminal-restart-notice")).toHaveLength(1);
  await panel.expand();
  expect(panel.bodyEl.querySelectorAll(".terminal-restart-notice")).toHaveLength(1);
});

test("pointer resizing persists the new height after the drag ends", () => {
  vi.useFakeTimers();
  const refitAll = vi.fn();
  const setPanelHeight = vi.fn();
  const { panel } = mountedPanel({ client: { refitAll, setPanelHeight } });
  panel.setHeight(300);
  panel._beginResize({ clientY: 500, preventDefault: vi.fn() });

  const move = new Event("pointermove");
  Object.defineProperty(move, "clientY", { value: 450 });
  window.dispatchEvent(move);
  expect(panel.heightPx).toBe(350);
  vi.advanceTimersByTime(100);

  window.dispatchEvent(new Event("pointerup"));
  expect(setPanelHeight).toHaveBeenCalledWith(350);
  expect(refitAll).toHaveBeenCalled();
  vi.useRealTimers();
});

test("keyboard resize and tab roving update the active terminal", () => {
  const refitTab = vi.fn();
  const focusTab = vi.fn();
  const { panel } = mountedPanel({ client: { refitTab, focusTab } });
  panel.setHeight(300);
  const preventResize = vi.fn();
  panel._keyboardResize({ key: "ArrowUp", shiftKey: false, preventDefault: preventResize });
  expect(panel.heightPx).toBe(320);
  expect(preventResize).toHaveBeenCalledTimes(1);

  panel.setTabs([
    { terminalId: "t1", generation: 1, label: "one", profileId: "default", status: "running" },
    { terminalId: "t2", generation: 1, label: "two", profileId: "default", status: "running" },
  ]);
  const preventRove = vi.fn();
  panel._tabKeydown({ key: "ArrowRight", preventDefault: preventRove }, "t1");
  expect(panel.activeTerminalId).toBe("t2");
  expect(refitTab).toHaveBeenCalledWith("t2");
  expect(focusTab).toHaveBeenCalledWith("t2");
  expect(preventRove).toHaveBeenCalledTimes(1);
});

test("unavailable opens a read-only status panel instead of hiding the reason", async () => {
  const client = { create: vi.fn(), closeAll: vi.fn(), checkpointAll: vi.fn() };
  const retry = vi.fn();
  const { panel } = mountedPanel({ client });

  panel.setAvailability("unavailable", {
    message: "no live runtime",
    onRetry: retry,
  });
  // The panel is collapsed by default, so the reason is only reachable through
  // the real toggle — clicking it must open the panel, not silently do nothing.
  expect(panel.root.classList.contains("hidden")).toBe(true);
  panel.toggleEl.click();
  await Promise.resolve();
  expect(panel.isExpanded()).toBe(true);
  expect(panel.root.classList.contains("hidden")).toBe(false);

  const banner = panel.bodyEl.querySelector("[data-terminal-availability='unavailable']");
  expect(banner).not.toBeNull();
  // Reachable, not merely present in hidden DOM.
  expect(banner.closest(".terminal-panel")).toBe(panel.root);
  expect(banner.querySelector("span").textContent).toBe("no live runtime");
  const button = banner.querySelector("[data-terminal-retry]");
  expect(button).not.toBeNull();
  button.click();
  expect(retry).toHaveBeenCalledTimes(1);
  // Read-only: no PTY is created for a page without a runtime (D2).
  expect(client.create).not.toHaveBeenCalled();

  // Ready clears the banner and reopens the surface.
  panel.setAvailability("ready");
  expect(panel.bodyEl.querySelector("[data-terminal-availability]")).toBeNull();
  expect(panel.unavailable).toBe(false);
});

test("an availability state set before mount is rendered once the panel exists", () => {
  const client = { create: vi.fn(), closeAll: vi.fn(), checkpointAll: vi.fn() };
  const panel = new TerminalPanel({ native: true, client, getAvailableHeight: () => 800 });
  // Remote capability/context resolution can land before the panel is mounted.
  panel.setAvailability("unavailable", { message: "no live runtime" });
  expect(panel.bodyEl).toBeNull();
  panel.mount({ toggleContainer: document.body, panelContainer: document.body });
  const banner = panel.bodyEl.querySelector("[data-terminal-availability='unavailable']");
  expect(banner).not.toBeNull();
  expect(banner.textContent).toBe("no live runtime");
});

test("a blocked binding labels its action Reload, not a generic retry", () => {
  const { panel } = mountedPanel();
  panel.setAvailability("blocked", {
    message: "binding changed",
    onRetry: () => {},
    retryLabel: "Reload page",
  });
  const button = panel.bodyEl.querySelector("[data-terminal-retry]");
  expect(button.textContent).toBe("Reload page");
});

test("a retry action is rendered once and calls back exactly once", () => {
  const { panel } = mountedPanel();
  const retry = vi.fn();
  panel.setAvailability("unavailable", { message: "no live runtime", onRetry: retry });
  const button = panel.bodyEl.querySelector("[data-terminal-retry]");
  expect(button).not.toBeNull();
  button.click();
  button.click();
  expect(retry).toHaveBeenCalledTimes(2);
});

test("a blocked binding renders the reload guidance, not a retry loop", () => {
  const { panel } = mountedPanel();
  panel.setAvailability("blocked", { message: "binding changed", onRetry: () => {} });
  const banner = panel.bodyEl.querySelector("[data-terminal-availability='blocked']");
  expect(banner.dataset.terminalAvailability).toBe("blocked");
  expect(banner.textContent).toContain("binding changed");
});
