// ABOUTME: Renders Daemon runtime master-detail with stable runtime selection.
// ABOUTME: Reads JSONL session names per workspace, rejecting stale or hidden-page results.

export const runtimeKey = (agent) =>
  JSON.stringify([agent?.workspaceId, agent?.sessionId, agent?.instanceId]);

/**
 * A workspace heading has to stay recognisable inside one narrow column, so a
 * deep absolute path keeps its last two segments and ellipsises the rest. The
 * full path stays available as the heading's tooltip.
 */
export function workspaceLabel(path) {
  const text = typeof path === "string" ? path.trim() : "";
  if (!text) return "";
  const segments = text.split("/").filter(Boolean);
  if (!text.startsWith("/") || segments.length <= 2) return text;
  return `…/${segments.slice(-2).join("/")}`;
}

export function setupDaemonRuntimes({
  root,
  t,
  stateLabel,
  onStop,
  onSelect,
  loadWorkspaceSessions,
}) {
  const groups = root.getElementById("daemon-agents");
  const empty = root.getElementById("daemon-agents-empty");
  const content = root.getElementById("daemon-runtime-content");
  const detailEmpty = root.getElementById("daemon-runtime-empty");
  const stop = root.getElementById("btn-daemon-agent-stop");
  const field = (name) => root.getElementById(`daemon-detail-${name}`);
  let agents = [];
  let selected = null;
  const names = new Map();
  const generations = new Map();
  const pending = new Map();
  const nameNodes = new Map();

  const current = () => agents.find((agent) => runtimeKey(agent) === selected);
  const shortId = (agent) => String(agent.sessionId ?? "—").slice(0, 8);
  const nameFor = (agent) => {
    const result = names.get(agent.workspaceId);
    const name = result?.sessions?.find((session) => session.id === agent.sessionId)?.name;
    return typeof name === "string" ? name.trim() : "";
  };
  const nameText = (agent) => {
    const result = names.get(agent.workspaceId);
    if (!result) return t("settings.daemon.sessionNameLoading");
    if (result.failed) return t("settings.daemon.sessionNameUnavailable");
    return nameFor(agent) || "—";
  };

  function renderNames() {
    for (const agent of agents) {
      const node = nameNodes.get(runtimeKey(agent));
      if (node) node.textContent = nameFor(agent) || shortId(agent);
    }
    const agent = current();
    if (agent) field("session-name").textContent = nameText(agent);
  }

  function renderDetail(stoppingId) {
    const agent = current();
    content.hidden = !agent;
    detailEmpty.hidden = !!agent;
    if (!agent) return;
    stop.textContent = t(
      stoppingId !== null && agent.instanceId === stoppingId
        ? "settings.daemon.stoppingAgent"
        : "settings.daemon.stopAgent",
    );
    for (const [key, value] of Object.entries({
      "runtime-id": agent.instanceId,
      "session-name": nameText(agent),
      "session-id": agent.sessionId,
      workspace: agent.workspaceId,
      state: stateLabel(agent.state),
      cwd: agent.cwd,
    })) {
      field(key).textContent = value ?? "—";
    }
    field("state").dataset.tone = tone(agent.state);
  }

  function tone(state) {
    return state === "working" ? "ok" : state === "crashed" ? "error" : "";
  }

  stop.addEventListener("click", () => {
    const agent = current();
    if (agent && !stop.disabled) onStop(agent);
  });

  return {
    render(nextAgents, { busy, stoppingId }) {
      const focused = root.activeElement?.closest(".daemon-runtime-row");
      const focusedKey = groups.contains(focused) ? focused.dataset.runtimeKey : null;
      agents = nextAgents;
      if (!current()) {
        selected = agents.length ? runtimeKey(agents[0]) : null;
        onSelect();
      }
      nameNodes.clear();
      const workspaces = new Map();
      for (const agent of agents) {
        if (!workspaces.has(agent.workspaceId)) workspaces.set(agent.workspaceId, []);
        workspaces.get(agent.workspaceId).push(agent);
      }
      const nodes = [];
      for (const workspaceAgents of workspaces.values()) {
        const heading = root.createElement("h4");
        heading.className = "pkg-manager-group-header";
        heading.style.whiteSpace = "normal";
        heading.style.overflowWrap = "anywhere";
        const workspace = workspaceAgents[0];
        const fullPath = workspace.cwd || workspace.workspaceId || "";
        heading.textContent = workspaceLabel(fullPath) || "—";
        heading.title = fullPath;
        nodes.push(heading);
        for (const agent of workspaceAgents) {
          const key = runtimeKey(agent);
          const row = root.createElement("button");
          row.type = "button";
          row.className = "pkg-manager-sidebar-row daemon-runtime-row";
          row.dataset.runtimeKey = key;
          row.classList.toggle("is-selected", key === selected);
          row.setAttribute("aria-pressed", String(key === selected));
          const name = root.createElement("span");
          name.className = "pkg-manager-sidebar-name";
          nameNodes.set(key, name);
          const meta = root.createElement("span");
          meta.className = "pkg-manager-sidebar-meta";
          const id = root.createElement("span");
          id.className = "daemon-runtime-session-id";
          id.textContent = shortId(agent);
          const badge = root.createElement("span");
          badge.className = "pill settings-static-value daemon-agent-state";
          badge.textContent = stateLabel(agent.state);
          badge.dataset.tone = tone(agent.state);
          meta.append(id, badge);
          row.append(name, meta);
          row.addEventListener("click", () => {
            if (selected !== key) onSelect();
            selected = key;
            for (const candidate of groups.querySelectorAll(".daemon-runtime-row")) {
              candidate.classList.toggle("is-selected", candidate === row);
              candidate.setAttribute("aria-pressed", String(candidate === row));
            }
            renderDetail(stoppingId);
          });
          nodes.push(row);
        }
      }
      empty.textContent = t("settings.daemon.agentsEmpty");
      empty.hidden = agents.length > 0;
      groups.replaceChildren(...nodes, empty);
      if (focusedKey) {
        const target = nameNodes.get(focusedKey) || nameNodes.get(selected);
        target?.parentElement.focus({ preventScroll: true });
      }
      stop.disabled = busy || stoppingId !== null;
      renderDetail(stoppingId);
      renderNames();
    },
    refreshNames() {
      // A malformed agent without a workspaceId must not trigger a whole-registry
      // scan every poll, so it simply has no workspace to look names up in.
      const live = new Set(agents.map((agent) => agent.workspaceId).filter(Boolean));
      // A workspace gone from the current agents invalidates its in-flight scan
      // and cached name; a rejoin bumps to a new generation, so the old scan's
      // late response dies. Ordinary status ticks never get here for a live
      // workspace: a still-relevant scan stays valid across polls.
      for (const workspaceId of new Set([...pending.keys(), ...names.keys()])) {
        if (live.has(workspaceId)) continue;
        generations.set(workspaceId, (generations.get(workspaceId) ?? 0) + 1);
        names.delete(workspaceId);
      }
      renderNames();
      for (const workspaceId of live) {
        // A bucket scan cannot be cancelled. Do not overlap it; retry on the next visible poll.
        if (pending.has(workspaceId)) continue;
        const generation = generations.get(workspaceId) ?? 0;
        const request = Promise.resolve().then(() => loadWorkspaceSessions(workspaceId));
        pending.set(workspaceId, request);
        const apply = (result) => {
          if ((generations.get(workspaceId) ?? 0) !== generation) return;
          names.set(workspaceId, result);
          renderNames();
        };
        void request
          .then(
            (result) => apply({ sessions: Array.isArray(result?.sessions) ? result.sessions : [] }),
            () => apply({ failed: true }),
          )
          .finally(() => pending.delete(workspaceId));
      }
    },
    invalidate() {
      for (const workspaceId of pending.keys()) {
        generations.set(workspaceId, (generations.get(workspaceId) ?? 0) + 1);
      }
    },
  };
}
