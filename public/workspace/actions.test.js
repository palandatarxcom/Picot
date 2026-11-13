import { readFileSync } from "node:fs";
import { join } from "node:path";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { initI18n } from "../i18n.js";
import {
  openFolderAsWorkspace,
  openProjectWorkspace,
  startInWindowNewSession,
  startNewProjectChat,
} from "./actions.js";

beforeEach(async () => {
  global.fetch = vi.fn(async (url) => {
    const u = String(url);
    if (u.includes("/locales/en.json")) {
      return {
        ok: true,
        status: 200,
        json: async () => ({
          errors: {
            newSessionFailed: "Failed to start new session",
            newSessionOnlyNative: "New session is only supported with a native host.",
            newChatFailed: "Failed to start new chat",
            openProjectFailed: "Failed to open project",
            openFolderFailed: "Failed to open folder",
            attachWorkspaceFailed: "Failed to attach to workspace",
          },
          sidebar: {
            startingSession: "Starting session…",
          },
        }),
      };
    }
    return { ok: false, status: 404, json: async () => ({}) };
  });
  await initI18n();
});

function makeTransport() {
  return {
    addWorkspace: vi.fn().mockResolvedValue({
      added: true,
      workspace: { canonicalPath: "/work" },
    }),
    prepareWorkspaceTarget: vi.fn().mockResolvedValue({
      classification: "same",
      transitionGeneration: 1,
      targetOrigin: "http://studio.example.test/workspaces/ws/sessions/s",
    }),
    commitWorkspaceTransition: vi.fn().mockResolvedValue(undefined),
    cancelWorkspaceTransition: vi.fn().mockResolvedValue(undefined),
  };
}

describe("startInWindowNewSession parallel-spawn", () => {
  it("starts a native runtime without an in-place port switch", async () => {
    const transport = makeTransport();
    const navigate = vi.fn();
    const dismiss = vi.fn();
    const onBeforeSwap = vi.fn(() => dismiss);

    const ok = await startInWindowNewSession({
      transport,
      getCurrentCwd: () => "/work",
      navigate,
      onBeforeSwap,
      shouldSpawnParallel: () => true,
      renderError: vi.fn(),
    });

    expect(ok).toBe(true);
    expect(transport.prepareWorkspaceTarget).toHaveBeenCalledWith("/work", {
      forceNewSession: true,
      reuseExisting: false,
    });
    expect(navigate).toHaveBeenCalledWith(expect.stringContaining("/workspaces/"), {
      targetCwd: "/work",
    });
    expect(onBeforeSwap).toHaveBeenCalled();
    expect(dismiss).not.toHaveBeenCalled();
  });

  it("cancels a cross-workspace transition when ephemeral settlement is rejected", async () => {
    const transport = makeTransport();
    transport.prepareWorkspaceTarget = vi.fn().mockResolvedValue({
      classification: "cross",
      transitionGeneration: 7,
      targetOrigin: "http://127.0.0.1:47826/",
    });
    transport.commitWorkspaceTransition = vi.fn();
    transport.cancelWorkspaceTransition = vi.fn().mockResolvedValue(undefined);
    const beforeWorkspaceTransition = vi.fn().mockResolvedValue(false);
    const onWorkspaceTransitionCancelled = vi.fn();
    const navigate = vi.fn();

    const ok = await startInWindowNewSession({
      transport,
      getCurrentCwd: () => "/other",
      navigate,
      onBeforeSwap: vi.fn(),
      shouldSpawnParallel: () => true,
      beforeWorkspaceTransition,
      onWorkspaceTransitionCancelled,
      renderError: vi.fn(),
    });

    expect(ok).toBe(false);
    expect(beforeWorkspaceTransition).toHaveBeenCalled();
    expect(onWorkspaceTransitionCancelled).toHaveBeenCalledTimes(1);
    expect(transport.cancelWorkspaceTransition).toHaveBeenCalledWith(7);
    expect(transport.commitWorkspaceTransition).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });

  it("invalidates the old binding immediately before the host commit", async () => {
    const order = [];
    const transport = makeTransport();
    transport.prepareWorkspaceTarget = vi.fn().mockResolvedValue({
      classification: "cross",
      transitionGeneration: 9,
      targetOrigin: "http://127.0.0.1:47826/workspaces/ws/sessions/s",
    });
    transport.commitWorkspaceTransition = vi.fn(async () => {
      order.push("commit");
    });

    const ok = await startInWindowNewSession({
      transport,
      getCurrentCwd: () => "/other",
      navigate: () => order.push("navigate"),
      onBeforeSwap: vi.fn(),
      shouldSpawnParallel: () => true,
      // Deferred checkpoint: the freeze finishes before anything else runs.
      beforeWorkspaceTransition: async () => {
        order.push("freeze");
        return true;
      },
      beforeWorkspaceCommit: () => order.push("invalidate"),
      onWorkspaceTransitionCancelled: () => order.push("cancel"),
      renderError: () => order.push("error"),
    });

    expect(ok).toBe(true);
    // Invalidation sits between the freeze and the commit, never after it.
    expect(order).toEqual(["freeze", "invalidate", "commit", "navigate"]);
  });

  it("still invalidates before a commit that fails, and cancels the transition", async () => {
    const order = [];
    const transport = makeTransport();
    transport.prepareWorkspaceTarget = vi.fn().mockResolvedValue({
      classification: "cross",
      transitionGeneration: 9,
      targetOrigin: "http://127.0.0.1:47826/workspaces/ws/sessions/s",
    });
    transport.commitWorkspaceTransition = vi.fn(async () => {
      order.push("commit");
      throw new Error("commit refused");
    });
    transport.cancelWorkspaceTransition = vi.fn().mockResolvedValue(undefined);

    const ok = await startInWindowNewSession({
      transport,
      getCurrentCwd: () => "/other",
      navigate: () => order.push("navigate"),
      onBeforeSwap: vi.fn(),
      shouldSpawnParallel: () => true,
      beforeWorkspaceTransition: async () => {
        order.push("freeze");
        return true;
      },
      beforeWorkspaceCommit: () => order.push("invalidate"),
      onWorkspaceTransitionCancelled: () => order.push("cancel"),
      renderError: () => order.push("error"),
    });

    expect(ok).toBe(false);
    expect(order).toEqual(["freeze", "invalidate", "commit", "cancel", "error"]);
    expect(transport.cancelWorkspaceTransition).toHaveBeenCalledWith(9);
  });

  // Native preparation spawns the Pi child and waits for its first
  // `get_state`: it is the slowest step of the whole flow. The overlay must
  // already be on screen when it starts, or the wait has no progress
  // indication at all.
  it("shows the swap overlay before the native preparation round-trip starts", async () => {
    const order = [];
    const transport = makeTransport();
    transport.prepareWorkspaceTarget = vi.fn(async () => {
      order.push("prepare");
      return {
        classification: "same",
        transitionGeneration: 1,
        targetOrigin: "http://studio.example.test/workspaces/ws/sessions/s",
      };
    });

    const ok = await startInWindowNewSession({
      transport,
      getCurrentCwd: () => "/work",
      navigate: vi.fn(),
      onBeforeSwap: () => {
        order.push("overlay");
        return vi.fn();
      },
      shouldSpawnParallel: () => true,
      renderError: vi.fn(),
    });

    expect(ok).toBe(true);
    expect(order).toEqual(["overlay", "prepare"]);
  });

  it("dismisses the swap overlay when native preparation fails", async () => {
    const transport = makeTransport();
    transport.prepareWorkspaceTarget.mockRejectedValue(new Error("boom"));
    const dismiss = vi.fn();
    const renderError = vi.fn();

    const ok = await startInWindowNewSession({
      transport,
      getCurrentCwd: () => "/work",
      navigate: vi.fn(),
      onBeforeSwap: () => dismiss,
      shouldSpawnParallel: () => true,
      renderError,
    });

    expect(ok).toBe(false);
    expect(renderError).toHaveBeenCalled();
    expect(dismiss).toHaveBeenCalled();
  });

  it("dismisses the swap overlay when ephemeral settlement is rejected", async () => {
    const transport = makeTransport();
    transport.prepareWorkspaceTarget = vi.fn().mockResolvedValue({
      classification: "cross",
      transitionGeneration: 7,
      targetOrigin: "http://127.0.0.1:47826/",
    });
    const dismiss = vi.fn();

    const ok = await startInWindowNewSession({
      transport,
      getCurrentCwd: () => "/other",
      navigate: vi.fn(),
      onBeforeSwap: () => dismiss,
      shouldSpawnParallel: () => true,
      beforeWorkspaceTransition: vi.fn().mockResolvedValue(false),
      onWorkspaceTransitionCancelled: vi.fn(),
      renderError: vi.fn(),
    });

    expect(ok).toBe(false);
    expect(dismiss).toHaveBeenCalled();
  });

  // Settlement waits for the user (side chat discard confirmation), and it must
  // stay reachable: it is a native `window.confirm`, which the overlay cannot
  // cover regardless of z-index. This locks in that the overlay is raised
  // exactly once for the whole prepare→settle→commit window rather than being
  // torn down and re-raised around the settlement.
  it("keeps the swap overlay up across a pending ephemeral settlement", async () => {
    const transport = makeTransport();
    transport.prepareWorkspaceTarget = vi.fn().mockResolvedValue({
      classification: "cross",
      transitionGeneration: 7,
      targetOrigin: "http://127.0.0.1:47826/",
    });
    const events = [];
    let releaseSettlement;
    const settlementGate = new Promise((resolve) => {
      releaseSettlement = resolve;
    });
    const dismiss = vi.fn(() => events.push("dismiss"));
    let swapCount = 0;

    const run = startInWindowNewSession({
      transport,
      getCurrentCwd: () => "/other",
      navigate: vi.fn(),
      onBeforeSwap: () => {
        swapCount += 1;
        events.push(`swap-${swapCount}`);
        return dismiss;
      },
      shouldSpawnParallel: () => true,
      beforeWorkspaceTransition: async () => {
        events.push("settlement-open");
        await settlementGate;
        events.push("settlement-close");
        return true;
      },
      onWorkspaceTransitionCancelled: vi.fn(),
      renderError: vi.fn(),
    });

    await vi.waitFor(() => expect(events).toContain("settlement-open"));
    // Still covered while the settlement is pending, and raised only once.
    expect(dismiss).not.toHaveBeenCalled();
    expect(swapCount).toBe(1);

    releaseSettlement();
    expect(await run).toBe(true);
    // Success keeps the overlay for the navigation handoff.
    expect(swapCount).toBe(1);
    expect(dismiss).not.toHaveBeenCalled();
  });

  it("dismisses the swap overlay when a preparation error interrupts an attach", async () => {
    const transport = makeTransport();
    transport.prepareWorkspaceTarget.mockRejectedValue(new Error("boom"));
    const dismiss = vi.fn();

    const ok = await openProjectWorkspace({
      project: { path: "/work/gamma", sessions: [{ cwd: "/work/gamma" }] },
      transport,
      navigate: vi.fn(),
      onBeforeSwap: () => dismiss,
      renderError: vi.fn(),
    });

    expect(ok).toBe(false);
    expect(dismiss).toHaveBeenCalled();
  });

  it("shows the swap overlay before an attach starts its preparation", async () => {
    const order = [];
    const transport = makeTransport();
    transport.prepareWorkspaceTarget = vi.fn(async () => {
      order.push("prepare");
      return {
        classification: "same",
        transitionGeneration: 1,
        targetOrigin: "http://studio.example.test/workspaces/ws/sessions/s",
      };
    });

    const ok = await openProjectWorkspace({
      project: { path: "/work/gamma", sessions: [{ cwd: "/work/gamma" }] },
      transport,
      navigate: vi.fn(),
      onBeforeSwap: () => {
        order.push("overlay");
        return vi.fn();
      },
      renderError: vi.fn(),
    });

    expect(ok).toBe(true);
    expect(order).toEqual(["overlay", "prepare"]);
  });

  it("dismisses the swap overlay when an attach settlement is rejected", async () => {
    const transport = makeTransport();
    transport.prepareWorkspaceTarget = vi.fn().mockResolvedValue({
      classification: "cross",
      transitionGeneration: 7,
      targetOrigin: "http://127.0.0.1:47826/",
    });
    const dismiss = vi.fn();

    const ok = await openProjectWorkspace({
      project: { path: "/work/gamma", sessions: [{ cwd: "/work/gamma" }] },
      transport,
      navigate: vi.fn(),
      onBeforeSwap: () => dismiss,
      beforeWorkspaceTransition: vi.fn().mockResolvedValue(false),
      onWorkspaceTransitionCancelled: vi.fn(),
      renderError: vi.fn(),
    });

    expect(ok).toBe(false);
    expect(dismiss).toHaveBeenCalled();
  });

  it("starts a fresh native runtime through workspace transition", async () => {
    const transport = makeTransport();
    const navigate = vi.fn();

    const ok = await startInWindowNewSession({
      transport,
      getCurrentCwd: () => "/work",
      navigate,
      onBeforeSwap: vi.fn(),
      shouldSpawnParallel: () => false,
      renderError: vi.fn(),
    });

    expect(ok).toBe(true);
    expect(transport.prepareWorkspaceTarget).toHaveBeenCalledWith("/work", {
      forceNewSession: true,
      reuseExisting: false,
    });
    expect(navigate).toHaveBeenCalledWith(expect.stringContaining("/workspaces/"), {
      targetCwd: "/work",
    });
  });
});

describe("native navigation stays on host origin", () => {
  it("startInWindowNewSession passes native target origin through unchanged", async () => {
    const transport = makeTransport();
    const navigate = vi.fn();

    const ok = await startInWindowNewSession({
      transport,
      getCurrentCwd: () => "/work",
      navigate,
      onBeforeSwap: vi.fn(),
      shouldSpawnParallel: () => true,
      renderError: vi.fn(),
    });

    expect(ok).toBe(true);
    expect(navigate).toHaveBeenCalledWith("http://studio.example.test/workspaces/ws/sessions/s", {
      targetCwd: "/work",
    });
  });

  it("startInWindowNewSession propagates targetCwd to navigate metadata", async () => {
    const transport = makeTransport();
    const navigate = vi.fn();

    await startInWindowNewSession({
      transport,
      getCurrentCwd: () => "/work/alpha",
      navigate,
      onBeforeSwap: vi.fn(),
      shouldSpawnParallel: () => true,
      renderError: vi.fn(),
    });

    expect(navigate).toHaveBeenCalledWith(expect.any(String), { targetCwd: "/work/alpha" });
  });

  it("startNewProjectChat passes native target origin through unchanged", async () => {
    const transport = makeTransport();
    const navigate = vi.fn();

    const ok = await startNewProjectChat({
      project: { path: "/work", sessions: [{ cwd: "/work" }] },
      transport,
      getCurrentCwd: () => "/other",
      shouldSpawnParallel: () => true,
      navigate,
      onBeforeSwap: vi.fn(),
      renderError: vi.fn(),
    });

    expect(ok).toBe(true);
    expect(navigate).toHaveBeenCalledWith("http://studio.example.test/workspaces/ws/sessions/s", {
      targetCwd: "/work",
    });
  });

  it("startNewProjectChat cross-workspace attach propagates targetCwd", async () => {
    const transport = makeTransport();
    const navigate = vi.fn();

    await startNewProjectChat({
      project: { path: "/work/beta", sessions: [{ cwd: "/work/beta" }] },
      transport,
      getCurrentCwd: () => "/work/alpha",
      shouldSpawnParallel: () => false,
      navigate,
      onBeforeSwap: vi.fn(),
      renderError: vi.fn(),
    });

    expect(navigate).toHaveBeenCalledWith(expect.any(String), { targetCwd: "/work/beta" });
  });

  it("openProjectWorkspace propagates the project cwd to navigate", async () => {
    const transport = makeTransport();
    const navigate = vi.fn();

    const ok = await openProjectWorkspace({
      project: { path: "/work/gamma", sessions: [{ cwd: "/work/gamma" }] },
      transport,
      navigate,
      onBeforeSwap: vi.fn(),
      renderError: vi.fn(),
    });

    expect(ok).toBe(true);
    expect(navigate).toHaveBeenCalledWith(expect.any(String), { targetCwd: "/work/gamma" });
  });

  it("openFolderAsWorkspace registers and starts a fresh session", async () => {
    const transport = makeTransport();
    transport.pickFolder = vi.fn().mockResolvedValue("/work/delta");
    transport.addWorkspace.mockResolvedValue({
      added: true,
      workspace: { canonicalPath: "/work/delta" },
    });
    const navigate = vi.fn();

    const ok = await openFolderAsWorkspace({
      transport,
      navigate,
      onBeforeSwap: vi.fn(),
      renderError: vi.fn(),
    });

    expect(ok).toBe(true);
    expect(transport.pickFolder).toHaveBeenCalled();
    expect(transport.addWorkspace).toHaveBeenCalledWith("/work/delta");
    expect(transport.prepareWorkspaceTarget).toHaveBeenCalledWith("/work/delta", {
      forceNewSession: true,
      reuseExisting: false,
    });
    expect(navigate).toHaveBeenCalledWith(expect.any(String), { targetCwd: "/work/delta" });
  });

  it("openFolderAsWorkspace starts a fresh session for an existing registration", async () => {
    const transport = makeTransport();
    transport.pickFolder = vi.fn().mockResolvedValue("/work/existing");
    transport.addWorkspace.mockResolvedValue({
      added: false,
      workspace: { canonicalPath: "/work/existing" },
    });
    const navigate = vi.fn();

    const ok = await openFolderAsWorkspace({
      transport,
      navigate,
      onBeforeSwap: vi.fn(),
      renderError: vi.fn(),
    });

    expect(ok).toBe(true);
    expect(transport.prepareWorkspaceTarget).toHaveBeenCalledWith("/work/existing", {
      forceNewSession: true,
      reuseExisting: false,
    });
    expect(navigate).toHaveBeenCalledWith(expect.any(String), { targetCwd: "/work/existing" });
  });
});

describe("startNewProjectChat parallel-spawn", () => {
  it("starts project chat through native workspace transition", async () => {
    const transport = makeTransport();
    const dismiss = vi.fn();
    const navigate = vi.fn();

    const ok = await startNewProjectChat({
      project: { path: "/work", sessions: [{ cwd: "/work" }] },
      transport,
      getCurrentCwd: () => "/work",
      shouldSpawnParallel: () => true,
      navigate,
      onBeforeSwap: () => dismiss,
      renderError: vi.fn(),
    });

    expect(ok).toBe(true);
    expect(transport.prepareWorkspaceTarget).toHaveBeenCalledWith("/work", {
      forceNewSession: true,
      reuseExisting: false,
    });
    expect(navigate).toHaveBeenCalledWith(expect.stringContaining("/workspaces/"), {
      targetCwd: "/work",
    });
    expect(dismiss).not.toHaveBeenCalled();
  });
});

describe("renderError i18n safety", () => {
  const sourcePath = join(process.cwd(), "public/workspace/actions.js");

  it("has no renderError template literals with raw English text", () => {
    const src = readFileSync(sourcePath, "utf8");
    const lines = src.split("\n");
    const renderErrorLines = lines.filter((l) => l.includes("renderError("));
    expect(renderErrorLines.length).toBeGreaterThan(0);
    // A template literal starting with English text (not ${t(…) or ${variable})
    // is a raw English literal that bypasses i18n.
    const rawEnglishLines = renderErrorLines.filter((line) => /renderError\(`[A-Za-z]/.test(line));
    expect(rawEnglishLines).toEqual([]);
  });

  it('wraps all renderError calls with t("errors.*", …)', () => {
    const src = readFileSync(sourcePath, "utf8");
    const lines = src.split("\n");
    const renderErrorLines = lines.filter((l) => l.includes("renderError("));
    expect(renderErrorLines.length).toBeGreaterThan(0);
    // Every renderError call must use t("errors.…") directly or via the
    // errorLabel variable (which is always assigned from t("errors.…")).
    const unwrapped = renderErrorLines.filter(
      (line) => !line.includes('t("errors.') && !line.includes("errorLabel"),
    );
    expect(unwrapped).toEqual([]);
    // Every errorLabel assignment must derive from t("errors.…").
    const errorLabelLines = lines.filter((l) => /errorLabel\s*[:=]/.test(l));
    expect(errorLabelLines.length).toBeGreaterThan(0);
    const badErrorLabels = errorLabelLines.filter((line) => !line.includes('t("errors.'));
    expect(badErrorLabels).toEqual([]);
  });
});
