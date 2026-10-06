// ABOUTME: Tests the Settings > Environment page against the host probe/install contract.
// ABOUTME: Locks no-probe-on-open, host-driven status, prompt copying, polling and refusals.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setupEnvironmentPage } from "./environment-page.js";

const GIT_URL = "https://git-scm.com/downloads";
const PYTHON_URL = "https://www.python.org/downloads/";

const TOOL_NAMES = {
  git: "Git",
  python3: "Python 3",
  npm: "npm",
  uv: "uv",
  officecli: "OfficeCLI",
  dws: "dws",
};

function t(key, params = {}) {
  const tool = key.match(/^settings\.environment\.tools\.([^.]+)\.(name|purpose)$/);
  if (tool) return tool[2] === "name" ? TOOL_NAMES[tool[1]] : `${tool[1]} purpose`;
  const values = {
    "settings.environment.check": "Check environment",
    "settings.environment.recheck": "Check again",
    "settings.environment.checking": "Checking…",
    "settings.environment.neverChecked": "Not checked yet",
    "settings.environment.checked": `${params.ready}/${params.total} ready`,
    "settings.environment.checkFailed": `check failed: ${params.message}`,
    "settings.environment.statusFailed": `status failed: ${params.message}`,
    "settings.environment.startFailed": `start failed: ${params.message}`,
    "settings.environment.cancelFailed": `cancel failed: ${params.message}`,
    "settings.environment.copyFailed": `copy failed: ${params.message}`,
    "settings.environment.promptCopied": "Prompt copied",
    "settings.environment.tier.basic": "Basic",
    "settings.environment.tier.optional": "Optional",
    "settings.environment.statusLabel": "Status",
    "settings.environment.status.unchecked": "Not checked yet",
    "settings.environment.status.ready": "Ready",
    "settings.environment.status.missing": "Missing",
    "settings.environment.status.failed": "Not runnable",
    "settings.environment.version": "Version",
    "settings.environment.path": "Path",
    "settings.environment.siteLabel": "Official page",
    "settings.environment.install": "Install",
    "settings.environment.update": "Update",
    "settings.environment.retry": "Retry",
    "settings.environment.cancel": "Cancel",
    "settings.environment.log": "Log",
    "settings.environment.logEmpty": "No output yet",
    "settings.environment.copyPrompt": "Copy prompt",
    "settings.environment.runAction.install": `Installing ${params.tool}`,
    "settings.environment.runAction.update": `Updating ${params.tool}`,
    "settings.environment.runPhase.running": "Running…",
    // A stopped run renders a verdict instead of the running line.
    "settings.environment.runVerdict.done": `${params.tool} finished`,
    "settings.environment.runVerdict.failed": `${params.tool} failed`,
    "settings.environment.runVerdict.cancelled": "Cancelled",
    "settings.environment.notifyTitle": "Environment",
    "settings.environment.recheckResult": `re-check ${params.status} ${params.version} ${params.path}`,
  };
  return values[key] ?? key;
}

function probe(overrides) {
  return {
    status: "ready",
    version: null,
    executablePath: null,
    reason: null,
    tier: "basic",
    ...overrides,
  };
}

// Deliberately out of host order and with host-owned tiers: the page renders
// its own canonical order and trusts the probe's tier.
const PROBES = [
  probe({
    toolId: "npm",
    version: "10.8.2",
    executablePath: "/usr/local/bin/npm",
    officialUrl: "https://nodejs.org/en/download",
  }),
  probe({
    toolId: "python3",
    status: "missing",
    reason: "python3 was not found on PATH",
    officialUrl: PYTHON_URL,
  }),
  probe({
    toolId: "git",
    version: "2.43.0",
    executablePath: "/usr/bin/git",
    officialUrl: GIT_URL,
  }),
  probe({
    toolId: "uv",
    status: "failed",
    reason: "uv --version timed out",
    officialUrl: "https://docs.astral.sh/uv/getting-started/installation/",
  }),
  probe({
    toolId: "officecli",
    tier: "optional",
    status: "missing",
    officialUrl: "https://github.com/iOfficeAI/OfficeCLI#readme",
  }),
  probe({
    toolId: "dws",
    tier: "optional",
    version: "0.4.1",
    executablePath: "/Users/test/.local/bin/dws",
    officialUrl: "https://github.com/DingTalk-Real-AI/dingtalk-workspace-cli#readme",
  }),
];

const READY_COUNT = PROBES.filter((entry) => entry.status === "ready").length;

function snapshot(overrides = {}) {
  return {
    tool: "git",
    action: "install",
    phase: "running",
    prompt: "HOST PROMPT",
    log: "",
    probe: null,
    reason: null,
    ...overrides,
  };
}

function createRoot() {
  const root = document.implementation.createHTMLDocument("environment");
  root.body.innerHTML = `
    <div class="settings-tab" data-settings-panel="environment">
      <div class="env-toolbar">
        <button type="button" class="settings-value-btn is-primary" id="environment-check-btn">
          Check environment
        </button>
        <p class="settings-help" id="environment-status" aria-live="polite">Not checked yet</p>
      </div>
      <p class="settings-help env-notice hidden" id="environment-notice" aria-live="polite"></p>
      <div class="env-tools" id="environment-tools"></div>
    </div>`;
  return root;
}

function makeTransport(overrides = {}) {
  return {
    checkEnvironment: vi.fn().mockResolvedValue({ tools: PROBES }),
    // The host echoes the request back in the first snapshot.
    startEnvironmentInstall: vi.fn((request) => Promise.resolve(snapshot(request))),
    getEnvironmentInstall: vi.fn().mockResolvedValue(null),
    cancelEnvironmentInstall: vi.fn().mockResolvedValue(null),
    notifyEnvironmentFinished: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}

// A click does not propagate its async handler, so handlers need a few
// microtask flushes before their state settles.
async function settle(ticks = 24) {
  for (let i = 0; i < ticks; i += 1) await Promise.resolve();
}

function setup(overrides = {}) {
  const root = createRoot();
  const transport = makeTransport(overrides);
  const page = setupEnvironmentPage({ root, transport, t });
  const row = (id) => root.querySelector(`.env-tool[data-tool-id="${id}"]`);
  return { root, transport, page, row };
}

function rowNames(root) {
  return [...root.querySelectorAll(".env-tool")].map(
    (entry) => entry.querySelector(".env-tool-name").textContent,
  );
}

function statusOf(row) {
  return row.querySelector(".env-status .env-fact-value").textContent;
}

describe("Environment page", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("renders the six-tool skeleton without touching the host", () => {
    const { root, transport } = setup();

    expect(transport.checkEnvironment).not.toHaveBeenCalled();
    expect(transport.getEnvironmentInstall).not.toHaveBeenCalled();
    expect(rowNames(root)).toEqual(["Git", "Python 3", "npm", "uv", "OfficeCLI", "dws"]);
    expect(root.querySelector("#environment-status").textContent).toBe("Not checked yet");
    expect(root.querySelector(".env-status .env-fact-value").textContent).toBe("Not checked yet");
    // Level is visible before any probe: red basic rows, yellow optional ones.
    expect(root.querySelectorAll(".env-tool.env-tool-basic")).toHaveLength(4);
    expect(root.querySelectorAll(".env-tool.env-tool-optional")).toHaveLength(2);
    expect(root.querySelectorAll(".env-tier.is-basic")).toHaveLength(4);
    expect(root.querySelectorAll(".env-tier.is-optional")).toHaveLength(2);
    // Nothing is actionable before a check, and no URL is invented by the page.
    expect(root.querySelectorAll(".env-action")).toHaveLength(0);
    expect(root.querySelectorAll(".env-link")).toHaveLength(0);
    expect(root.querySelector("#environment-check-btn").textContent).toBe("Check environment");
  });

  it("renders host probes: order, tiers, facts, links and per-status buttons", async () => {
    const { root, page, row } = setup();
    await page.check();

    expect(rowNames(root)).toEqual(["Git", "Python 3", "npm", "uv", "OfficeCLI", "dws"]);
    expect(root.querySelector("#environment-status").textContent).toBe(`${READY_COUNT}/6 ready`);
    expect(root.querySelector("#environment-check-btn").textContent).toBe("Check again");

    expect(statusOf(row("git"))).toBe("Ready");
    expect(statusOf(row("python3"))).toBe("Missing");
    expect(statusOf(row("uv"))).toBe("Not runnable");
    expect(row("git").querySelector(".env-version .env-fact-value").textContent).toBe("2.43.0");
    expect(row("git").querySelector(".env-path .env-fact-value").textContent).toBe("/usr/bin/git");
    // A missing value renders as a dash, never as an empty fact.
    expect(row("python3").querySelector(".env-path .env-fact-value").textContent).toBe("—");

    // Host tier wins and drives the level styling.
    expect(row("officecli").classList.contains("env-tool-optional")).toBe(true);
    expect(row("officecli").querySelector(".env-tier").classList.contains("is-optional")).toBe(
      true,
    );
    expect(row("git").querySelector(".env-tier").textContent).toBe("Basic");

    const link = row("git").querySelector(".env-link");
    expect(link.getAttribute("href")).toBe(GIT_URL);
    expect(link.getAttribute("target")).toBe("_blank");
    expect(link.getAttribute("rel")).toBe("noopener noreferrer");
    expect(link.textContent).toBe(GIT_URL);
    expect(row("dws").querySelector(".env-link").getAttribute("href")).toBe(
      "https://github.com/DingTalk-Real-AI/dingtalk-workspace-cli#readme",
    );

    // ready → Update, missing → Install, failed → Retry.
    expect(row("git").querySelector(".env-action").textContent).toBe("Update");
    expect(row("python3").querySelector(".env-action").textContent).toBe("Install");
    expect(row("uv").querySelector(".env-action").textContent).toBe("Retry");
    expect(row("git").querySelector(".env-action").disabled).toBe(false);
  });

  it("starts the host action for the clicked row and locks every other button", async () => {
    const { transport, row, page } = setup();
    await page.check();
    await row("python3").querySelector(".env-action").click();
    await settle();

    expect(transport.startEnvironmentInstall).toHaveBeenCalledWith({
      tool: "python3",
      action: "install",
    });
    expect(row("python3").querySelector(".env-run.is-running")).not.toBeNull();
    expect(row("python3").querySelector(".env-run-phase").textContent).toBe("Running…");
    expect(row("python3").querySelector(".env-run-action").textContent).toBe("Installing Python 3");
    expect(row("python3").querySelector(".env-cancel").textContent).toBe("Cancel");
    // Every other row is locked while the host runs one job at a time.
    for (const id of ["git", "npm", "uv", "officecli", "dws"]) {
      expect(row(id).querySelector(".env-action").disabled).toBe(true);
    }
  });

  it("copies the exact host prompt and never injects the log as markup", async () => {
    const prompt = 'HOST PROMPT {"toolId":"git"}\n<script>alert(1)</script>';
    const log = "collecting output\n<script>alert(2)</script>";
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(globalThis.navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });
    const { root, row, page } = setup({
      startEnvironmentInstall: vi.fn().mockResolvedValue(snapshot({ prompt, log })),
    });
    await page.check();
    await row("git").querySelector(".env-action").click();
    await settle();

    const logText = row("git").querySelector(".env-log-text");
    expect(logText.textContent).toBe(log);
    expect(logText.children).toHaveLength(0);
    expect(root.querySelector(".env-log-text script")).toBeNull();
    // The log is folded away by default.
    expect(row("git").querySelector(".env-log").open).toBe(false);

    await row("git").querySelector(".env-copy").click();
    await settle();
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText.mock.calls[0][0]).toBe(prompt);
    expect(root.querySelector("#environment-notice").textContent).toBe("Prompt copied");
    expect(root.querySelector("#environment-notice").classList.contains("is-error")).toBe(false);
  });

  it("stays silent when the run finishes while the user watches the page", async () => {
    const running = snapshot({ tool: "uv", action: "update", prompt: "HOST PROMPT" });
    const done = snapshot({
      tool: "uv",
      action: "update",
      phase: "done",
      probe: probe({
        toolId: "uv",
        version: "0.5.0",
        officialUrl: "https://docs.astral.sh/uv/getting-started/installation/",
      }),
    });
    const { row, transport, page } = setup({
      startEnvironmentInstall: vi.fn().mockResolvedValue(running),
      getEnvironmentInstall: vi.fn().mockResolvedValue(done),
    });

    // The page is open, so the verdict line is the answer and no banner fires.
    await page.activate();
    await row("uv").querySelector(".env-action").click();
    await settle();
    await vi.advanceTimersByTimeAsync(1000);

    expect(transport.notifyEnvironmentFinished).not.toHaveBeenCalled();
  });

  it("notifies when the run finishes after the user left the page", async () => {
    const running = snapshot({ tool: "uv", action: "update", prompt: "HOST PROMPT" });
    const done = snapshot({
      tool: "uv",
      action: "update",
      phase: "done",
      probe: probe({
        toolId: "uv",
        version: "0.5.0",
        officialUrl: "https://docs.astral.sh/uv/getting-started/installation/",
      }),
    });
    const { row, transport, page } = setup({
      startEnvironmentInstall: vi.fn().mockResolvedValue(running),
      getEnvironmentInstall: vi.fn().mockResolvedValue(done),
    });

    await page.activate();
    await row("uv").querySelector(".env-action").click();
    await settle();
    // Walking away is exactly when the banner matters, so the timer keeps going.
    page.leave();
    await vi.advanceTimersByTimeAsync(1000);

    expect(transport.notifyEnvironmentFinished).toHaveBeenCalledTimes(1);
    expect(transport.notifyEnvironmentFinished).toHaveBeenCalledWith({
      title: "Environment",
      body: "uv finished — 0.5.0",
    });

    // One finished run notifies once, however often the page polls afterwards.
    await vi.advanceTimersByTimeAsync(3000);
    expect(transport.notifyEnvironmentFinished).toHaveBeenCalledTimes(1);
  });

  it("notifies when the window is hidden even if the page is open", async () => {
    const done = snapshot({
      tool: "git",
      action: "update",
      phase: "failed",
      reason: "the re-check after the run failed",
    });
    const { transport, page } = setup({
      getEnvironmentInstall: vi.fn().mockResolvedValue(done),
    });
    const hidden = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");

    await page.activate();
    await settle();

    expect(transport.notifyEnvironmentFinished).toHaveBeenCalledTimes(1);
    expect(transport.notifyEnvironmentFinished.mock.calls[0][0].body).toBe(
      "Git failed — the re-check after the run failed",
    );
    hidden.mockRestore();
  });

  it("cancels through the host and shows the terminal snapshot", async () => {
    const cancelled = snapshot({
      tool: "git",
      action: "update",
      phase: "cancelled",
      prompt: "HOST PROMPT",
      log: "stopped",
      probe: probe({ toolId: "git", status: "missing", version: null, officialUrl: GIT_URL }),
      reason: "install was cancelled",
    });
    const { row, transport, page } = setup({
      startEnvironmentInstall: vi
        .fn()
        .mockResolvedValue(snapshot({ tool: "git", action: "update", prompt: "HOST PROMPT" })),
      cancelEnvironmentInstall: vi.fn().mockResolvedValue(cancelled),
    });
    await page.check();
    await row("git").querySelector(".env-action").click();
    await settle();
    await row("git").querySelector(".env-cancel").click();
    await settle();

    expect(transport.cancelEnvironmentInstall).toHaveBeenCalledTimes(1);
    expect(row("git").querySelector(".env-run.is-cancelled")).not.toBeNull();
    // A stopped run must not keep the running line: it used to read "Updating…".
    expect(row("git").querySelector(".env-run-phase")).toBeNull();
    expect(row("git").querySelector(".env-run-verdict").textContent).toBe("Cancelled");
    expect(row("git").querySelector(".env-run-reason").textContent).toBe("install was cancelled");
    expect(row("git").querySelector(".env-recheck").textContent).toBe("re-check Missing — —");
    expect(row("git").querySelector(".env-cancel")).toBeNull();
    // The row now reports the host's re-check, not the pre-install probe.
    expect(statusOf(row("git"))).toBe("Missing");
  });

  it("polls the host while running and stops at the terminal phase", async () => {
    const running = snapshot({ tool: "git", action: "update", prompt: "HOST PROMPT" });
    const recheck = probe({
      toolId: "git",
      version: "2.43.0",
      executablePath: "/usr/bin/git",
      officialUrl: GIT_URL,
    });
    const done = snapshot({
      tool: "git",
      action: "update",
      phase: "done",
      log: "done",
      probe: recheck,
      reason: "version is unchanged (2.43.0)",
    });
    const getEnvironmentInstall = vi
      .fn()
      .mockResolvedValueOnce(running)
      .mockResolvedValueOnce(running)
      .mockResolvedValueOnce(done);
    const { row, page } = setup({ getEnvironmentInstall });

    // Entering the page resumes from the host instead of starting anything.
    await page.activate();
    expect(row("git").querySelector(".env-run.is-running")).not.toBeNull();

    await vi.advanceTimersByTimeAsync(1000);
    expect(getEnvironmentInstall).toHaveBeenCalledTimes(2);
    expect(row("git").querySelector(".env-run.is-running")).not.toBeNull();

    await vi.advanceTimersByTimeAsync(1000);
    expect(getEnvironmentInstall).toHaveBeenCalledTimes(3);
    const finished = row("git").querySelector(".env-run.is-done");
    expect(finished).not.toBeNull();
    // The verdict replaces "Updating…" once the run stopped.
    expect(row("git").querySelector(".env-run-phase")).toBeNull();
    expect(row("git").querySelector(".env-run-verdict").textContent).toBe("Git finished");
    expect(row("git").querySelector(".env-run-reason").textContent).toBe(
      "version is unchanged (2.43.0)",
    );
    expect(row("git").querySelector(".env-recheck").textContent).toBe(
      "re-check Ready 2.43.0 /usr/bin/git",
    );
    expect(statusOf(row("git"))).toBe("Ready");
    // A finished row is actionable again, with the post-run status deciding
    // the action: the host re-checked it as ready, so it offers Update.
    expect(row("git").querySelector(".env-action").textContent).toBe("Update");
    // Terminal: no further polls.
    await vi.advanceTimersByTimeAsync(5000);
    expect(getEnvironmentInstall).toHaveBeenCalledTimes(3);
  });

  it("resumes a job started elsewhere and never starts a second one", async () => {
    const running = snapshot({ tool: "dws", action: "install", prompt: "HOST PROMPT" });
    const { row, page, transport } = setup({
      getEnvironmentInstall: vi.fn().mockResolvedValue(running),
    });
    await page.activate();
    await settle();

    expect(transport.startEnvironmentInstall).not.toHaveBeenCalled();
    expect(row("dws").querySelector(".env-run.is-running")).not.toBeNull();
    // Leaving the page keeps the timer while a job runs: the host owns the job,
    // and watching it to its end is what lets the completion banner fire while
    // the user is on another page. An idle page still stops polling.
    page.leave();
    await vi.advanceTimersByTimeAsync(3000);
    expect(transport.getEnvironmentInstall).toHaveBeenCalledTimes(4);
    await page.activate();
    expect(transport.getEnvironmentInstall).toHaveBeenCalledTimes(5);
    expect(transport.startEnvironmentInstall).not.toHaveBeenCalled();
  });

  it("clears a job the host no longer tracks instead of polling forever", async () => {
    const getEnvironmentInstall = vi
      .fn()
      .mockResolvedValueOnce(snapshot({ tool: "git", action: "install" }))
      .mockResolvedValue(null);
    const { row, page, transport } = setup({ getEnvironmentInstall });
    await page.activate();
    expect(row("git").querySelector(".env-run.is-running")).not.toBeNull();

    await vi.advanceTimersByTimeAsync(1000);
    expect(transport.getEnvironmentInstall).toHaveBeenCalledTimes(2);
    expect(row("git").querySelector(".env-run")).toBeNull();
    await vi.advanceTimersByTimeAsync(5000);
    expect(transport.getEnvironmentInstall).toHaveBeenCalledTimes(2);
  });

  it("keeps polling through a dropped connection and recovers", async () => {
    const getEnvironmentInstall = vi
      .fn()
      .mockResolvedValueOnce(snapshot({ tool: "git", action: "install" }))
      .mockRejectedValueOnce(new Error("Transport is not connected"))
      .mockResolvedValue(snapshot({ tool: "git", action: "install", phase: "done" }));
    const { root, row, page, transport } = setup({ getEnvironmentInstall });
    await page.activate();

    await vi.advanceTimersByTimeAsync(1000);
    expect(root.querySelector("#environment-notice").textContent).toBe(
      "status failed: Transport is not connected",
    );
    expect(root.querySelector("#environment-notice").classList.contains("is-error")).toBe(true);
    // The row stays in its last known state instead of locking up silently.
    expect(row("git").querySelector(".env-run.is-running")).not.toBeNull();

    await vi.advanceTimersByTimeAsync(1000);
    expect(transport.getEnvironmentInstall).toHaveBeenCalledTimes(3);
    expect(row("git").querySelector(".env-run.is-done")).not.toBeNull();
    expect(root.querySelector("#environment-notice").classList.contains("hidden")).toBe(true);
  });

  it("shows the host's maintenance_busy refusal instead of a dead button", async () => {
    const { root, row, transport, page } = setup({
      startEnvironmentInstall: vi
        .fn()
        .mockRejectedValue(new Error("maintenance_busy: an install is already running")),
    });
    await page.check();
    await row("git").querySelector(".env-action").click();
    await settle();

    expect(transport.startEnvironmentInstall).toHaveBeenCalledTimes(1);
    const notice = root.querySelector("#environment-notice");
    expect(notice.classList.contains("hidden")).toBe(false);
    expect(notice.classList.contains("is-error")).toBe(true);
    expect(notice.textContent).toBe(
      "start failed: maintenance_busy: an install is already running",
    );
    expect(row("git").querySelector(".env-run")).toBeNull();
    // The row stays actionable so the user can retry once the job ends.
    expect(row("git").querySelector(".env-action").disabled).toBe(false);
  });

  it("keeps the pre-check state and reports a failed check", async () => {
    const { root, page, transport } = setup({
      checkEnvironment: vi.fn().mockRejectedValue(new Error("host offline")),
    });
    await page.check();

    expect(transport.checkEnvironment).toHaveBeenCalledTimes(1);
    const notice = root.querySelector("#environment-notice");
    expect(notice.textContent).toBe("check failed: host offline");
    expect(notice.classList.contains("is-error")).toBe(true);
    expect(root.querySelector("#environment-status").textContent).toBe("Not checked yet");
    expect(root.querySelector("#environment-check-btn").textContent).toBe("Check environment");
    expect(root.querySelectorAll(".env-action")).toHaveLength(0);
  });

  it("shows the checking state while the host is busy", async () => {
    let resolveCheck;
    const checkEnvironment = vi.fn(
      () =>
        new Promise((resolve) => {
          resolveCheck = resolve;
        }),
    );
    const { root, page } = setup({ checkEnvironment });
    const pending = page.check();

    expect(root.querySelector("#environment-check-btn").textContent).toBe("Checking…");
    expect(root.querySelector("#environment-check-btn").disabled).toBe(true);
    expect(root.querySelector("#environment-status").textContent).toBe("Checking…");
    // A second click while a check is in flight must not start another one.
    root.querySelector("#environment-check-btn").click();
    await settle();
    expect(checkEnvironment).toHaveBeenCalledTimes(1);

    resolveCheck({ tools: PROBES });
    await pending;
    expect(root.querySelector("#environment-check-btn").disabled).toBe(false);
  });
});
