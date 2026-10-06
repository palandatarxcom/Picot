// ABOUTME: Renders the Settings > Environment tool list and drives host check/install ops.
// ABOUTME: The host owns every fact (probe, re-check, prompt); the page only displays them.

import { t as i18n, onLocaleChange } from "../i18n.js";

// Canonical tool order and tier, mirrored from the host catalog. Only the
// pre-check rows use this: once a probe arrives, the host's own tier wins.
// Nothing here triggers a probe — the list must render without touching PATH.
const TOOLS = [
  { id: "git", tier: "basic" },
  { id: "python3", tier: "basic" },
  { id: "npm", tier: "basic" },
  { id: "uv", tier: "basic" },
  { id: "officecli", tier: "optional" },
  { id: "dws", tier: "optional" },
];

// The action a row offers for a probe status. `failed` means the binary was
// found but no usable version came back, so the agent has to (re)install;
// only a runnable tool can be updated.
const ACTIONS = { missing: "install", failed: "install", ready: "update" };

const POLL_INTERVAL_MS = 1000;

function element(root, tag, className, value) {
  const node = root.createElement(tag);
  if (className) node.className = className;
  if (value !== undefined) node.textContent = value;
  return node;
}

function errorMessage(error) {
  return String(error?.message || error || "unknown error");
}

// Every dynamic value (version, path, log, prompt) is a host string: an empty
// one reads as "no value", never as missing markup.
function display(text) {
  const raw = text == null ? "" : String(text);
  return raw.length ? raw : "—";
}

export function setupEnvironmentPage({
  root = document,
  transport,
  openExternal = null,
  t = i18n,
} = {}) {
  const container = root.getElementById("environment-tools");
  if (!container) {
    return { activate: async () => {}, leave: () => {}, check: async () => {}, destroy: () => {} };
  }
  const statusEl = root.getElementById("environment-status");
  const noticeEl = root.getElementById("environment-notice");
  const checkButton = root.getElementById("environment-check-btn");

  let probes = null; // Map<toolId, Probe>; null until a check finished.
  let checked = false;
  let checking = false;
  let run = null; // Latest Snapshot from start / status / cancel.
  let notice = null; // { kind: "info" | "error", text }
  let logOpen = false; // The log is a <details>; keep its open state across polls.
  let pollTimer = null;
  let pageActive = false; // Whether the settings tab currently shows this page.
  let notifiedRun = null; // `<tool>|<action>|<phase>` already handled.

  const toolName = (tool) => t(`settings.environment.tools.${tool.id}.name`);
  const busy = () => run?.phase === "running";

  // After a maintenance run the host's own re-check is the only success
  // evidence, so it wins over the probe that was taken before the run.
  function probeFor(tool) {
    if (run?.tool === tool.id && run.probe) return run.probe;
    return probes?.get(tool.id) ?? null;
  }

  function tierFor(tool, probe) {
    return probe?.tier === "basic" || probe?.tier === "optional" ? probe.tier : tool.tier;
  }

  function textButton(label, onClick, { disabled = false, extra = "" } = {}) {
    const node = element(root, "button", `settings-value-btn env-btn${extra}`);
    node.type = "button";
    node.textContent = label;
    node.disabled = disabled;
    node.addEventListener("click", () => void onClick());
    return node;
  }

  function renderFact(className, label, content) {
    const wrap = element(root, "div", `env-fact ${className}`);
    wrap.append(element(root, "span", "env-fact-label", label));
    const value = element(root, "span", "env-fact-value");
    // A host string is set as text; the official link is a node to append.
    if (typeof content === "string") value.textContent = content;
    else value.append(content);
    wrap.append(value);
    return wrap;
  }

  function renderRun(tool) {
    const snapshot = run;
    const block = element(root, "div", `env-run is-${snapshot.phase}`);
    // While it runs the line says what is happening; once it stopped the line
    // must say what happened, or a finished run still reads as "updating…".
    if (snapshot.phase === "running") {
      block.append(
        element(
          root,
          "div",
          "env-run-action",
          t(`settings.environment.runAction.${snapshot.action}`, { tool: toolName(tool) }),
        ),
      );
      block.append(
        element(root, "div", "env-run-phase", t("settings.environment.runPhase.running")),
      );
    } else {
      block.append(
        element(
          root,
          "div",
          `env-run-verdict is-${snapshot.phase}`,
          t(`settings.environment.runVerdict.${snapshot.phase}`, { tool: toolName(tool) }),
        ),
      );
    }
    if (snapshot.reason) block.append(element(root, "div", "env-run-reason", snapshot.reason));
    if (snapshot.probe) {
      block.append(
        element(
          root,
          "div",
          "env-recheck",
          t("settings.environment.recheckResult", {
            status: t(`settings.environment.status.${snapshot.probe.status}`),
            version: display(snapshot.probe.version),
            path: display(snapshot.probe.executablePath),
          }),
        ),
      );
    }

    const details = element(root, "details", "env-log");
    details.open = logOpen;
    details.addEventListener("toggle", () => {
      logOpen = details.open;
    });
    details.append(element(root, "summary", null, t("settings.environment.log")));
    details.append(
      element(root, "pre", "env-log-text", snapshot.log || t("settings.environment.logEmpty")),
    );
    block.append(details);

    const actions = element(root, "div", "env-run-actions");
    actions.append(
      textButton(t("settings.environment.copyPrompt"), () => copyPrompt(snapshot.prompt), {
        extra: " env-copy",
      }),
    );
    if (snapshot.phase === "running") {
      actions.append(
        textButton(t("settings.environment.cancel"), () => cancel(), {
          extra: " env-cancel is-danger",
        }),
      );
    }
    block.append(actions);
    return block;
  }

  function renderRow(tool) {
    const probe = probeFor(tool);
    const status = probe?.status ?? "unchecked";
    const tier = tierFor(tool, probe);
    const row = element(root, "div", `env-tool env-tool-${tier}`);
    row.dataset.toolId = tool.id;

    const head = element(root, "div", "env-tool-head");
    head.append(element(root, "span", "env-tool-name", toolName(tool)));
    head.append(
      element(root, "span", `env-tier is-${tier}`, t(`settings.environment.tier.${tier}`)),
    );
    row.append(head);
    row.append(
      element(root, "p", "env-tool-purpose", t(`settings.environment.tools.${tool.id}.purpose`)),
    );

    const facts = element(root, "div", "env-facts");
    facts.append(
      renderFact(
        `env-status is-${status}`,
        t("settings.environment.statusLabel"),
        t(`settings.environment.status.${status}`),
      ),
    );
    facts.append(
      renderFact("env-version", t("settings.environment.version"), display(probe?.version)),
    );
    facts.append(
      renderFact("env-path", t("settings.environment.path"), display(probe?.executablePath)),
    );
    if (probe?.officialUrl) {
      // The host supplies the URL; the page never invents one. `href` keeps the
      // link inspectable, the host op keeps the WebView from navigating.
      // The link text is the host's URL: the page never invents or shortens one.
      const link = element(root, "a", "env-link", probe.officialUrl);
      link.href = probe.officialUrl;
      link.target = "_blank";
      link.rel = "noopener noreferrer";
      link.addEventListener("click", (event) => {
        if (!openExternal) return;
        event.preventDefault();
        void openExternal(probe.officialUrl);
      });
      facts.append(renderFact("env-site", t("settings.environment.siteLabel"), link));
    }
    row.append(facts);

    const action = ACTIONS[status];
    if (action) {
      // `failed` sends the same install action but is labelled Retry: the
      // tool is there and broken, so the user is retrying, not installing.
      const label = status === "failed" ? "retry" : action;
      row.append(
        textButton(t(`settings.environment.${label}`), () => start(tool.id, action), {
          disabled: busy(),
          extra: " env-action",
        }),
      );
    }
    if (run?.tool === tool.id) row.append(renderRun(tool));
    return row;
  }

  function statusText() {
    if (checking) return t("settings.environment.checking");
    if (!checked) return t("settings.environment.neverChecked");
    const ready = TOOLS.filter((tool) => probes.get(tool.id)?.status === "ready").length;
    return t("settings.environment.checked", { ready, total: TOOLS.length });
  }

  function render() {
    if (statusEl) statusEl.textContent = statusText();
    if (noticeEl) {
      noticeEl.textContent = notice ? notice.text : "";
      noticeEl.classList.toggle("hidden", !notice);
      noticeEl.classList.toggle("is-error", notice?.kind === "error");
    }
    if (checkButton) {
      checkButton.disabled = checking;
      checkButton.textContent = checking
        ? t("settings.environment.checking")
        : t(`settings.environment.${checked ? "recheck" : "check"}`);
    }
    container.replaceChildren(...TOOLS.map(renderRow));
  }

  async function copyPrompt(prompt) {
    try {
      const clipboard = globalThis.navigator?.clipboard;
      if (!clipboard?.writeText) throw new Error("clipboard unavailable");
      // The copy is the exact string the host passed to `pi -p`, never a
      // rebuilt one: what the user copies is what ran.
      await clipboard.writeText(prompt);
      notice = { kind: "info", text: t("settings.environment.promptCopied") };
    } catch (error) {
      notice = {
        kind: "error",
        text: t("settings.environment.copyFailed", { message: errorMessage(error) }),
      };
    }
    render();
  }

  function stopPolling() {
    if (pollTimer == null) return;
    clearInterval(pollTimer);
    pollTimer = null;
  }

  // A finished install deserves a banner only when the user is not already
  // looking at the result: this page renders the verdict itself. The host still
  // applies the user's notification preference and the desktop-owner gate.
  function maybeNotify() {
    if (!run || run.phase === "running") return;
    const key = `${run.tool}|${run.action}|${run.phase}`;
    if (notifiedRun === key) return;
    notifiedRun = key;
    const visible = typeof document === "undefined" || document.visibilityState === "visible";
    if (pageActive && visible) return;
    const tool = TOOLS.find((entry) => entry.id === run.tool);
    const verdict = t(`settings.environment.runVerdict.${run.phase}`, {
      tool: tool ? toolName(tool) : run.tool,
    });
    const detail = run.reason ?? run.probe?.version ?? "";
    void Promise.resolve(
      transport.notifyEnvironmentFinished?.({
        title: t("settings.environment.notifyTitle"),
        body: detail ? `${verdict} — ${detail}` : verdict,
      }),
    ).catch((error) => {
      // A denied OS permission or a missing notification centre must not change
      // the in-app experience.
      console.warn("[Environment] notification failed:", error);
    });
  }

  function syncPolling() {
    if (run?.phase === "running") {
      pollTimer ??= setInterval(() => void poll(), POLL_INTERVAL_MS);
      return;
    }
    stopPolling();
  }

  async function poll() {
    try {
      // The host is the only source of truth: a `null` status means it tracks
      // no job at all, so a stale local "running" display is cleared rather
      // than polled forever.
      run = (await transport.getEnvironmentInstall()) ?? null;
      // A successful read clears a stale transport error; a copy confirmation
      // is not an error and stays.
      if (notice?.kind === "error") notice = null;
    } catch (error) {
      // A dropped connection must not freeze the page in "running" with every
      // button locked, so polling continues and a reconnected host is picked up.
      notice = {
        kind: "error",
        text: t("settings.environment.statusFailed", { message: errorMessage(error) }),
      };
    }
    maybeNotify();
    syncPolling();
    render();
  }

  async function check() {
    if (checking) return;
    checking = true;
    notice = null;
    render();
    try {
      const result = await transport.checkEnvironment();
      const listed = Array.isArray(result?.tools) ? result.tools : [];
      probes = new Map(
        listed.filter((probe) => probe?.toolId).map((probe) => [probe.toolId, probe]),
      );
      checked = true;
    } catch (error) {
      notice = {
        kind: "error",
        text: t("settings.environment.checkFailed", { message: errorMessage(error) }),
      };
    }
    checking = false;
    render();
  }

  async function start(tool, action) {
    notice = null;
    notifiedRun = null; // A new run may notify even for the same tool and action.
    try {
      const snapshot = await transport.startEnvironmentInstall({ tool, action });
      if (snapshot) run = snapshot;
    } catch (error) {
      // The host refuses a second job with `maintenance_busy: ...`; show it
      // verbatim instead of leaving a dead button.
      notice = {
        kind: "error",
        text: t("settings.environment.startFailed", { message: errorMessage(error) }),
      };
    }
    maybeNotify();
    syncPolling();
    render();
  }

  async function cancel() {
    try {
      const snapshot = await transport.cancelEnvironmentInstall();
      if (snapshot) run = snapshot;
    } catch (error) {
      notice = {
        kind: "error",
        text: t("settings.environment.cancelFailed", { message: errorMessage(error) }),
      };
    }
    maybeNotify();
    syncPolling();
    render();
  }

  // Re-entering the page resumes the display from the host's own snapshot: the
  // job lives on the host, so a return must never start a second one.
  async function activate() {
    pageActive = true;
    try {
      run = (await transport.getEnvironmentInstall()) ?? null;
    } catch (error) {
      notice = {
        kind: "error",
        text: t("settings.environment.statusFailed", { message: errorMessage(error) }),
      };
    }
    maybeNotify();
    syncPolling();
    render();
  }

  function leave() {
    pageActive = false;
    // Keep polling while a job runs: this is where the completion banner is
    // decided, and the user is elsewhere by definition when it matters.
    syncPolling();
  }

  const unsubscribeLocale = onLocaleChange(() => render());

  function destroy() {
    stopPolling();
    unsubscribeLocale();
  }

  checkButton?.addEventListener("click", () => void check());
  render();

  return { activate, leave, check, destroy };
}
