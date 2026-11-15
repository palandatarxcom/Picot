// ABOUTME: Reproduces the full canonical-page snapshot chain against the
// ABOUTME: production document: bootstrap target echo, hello_ack, subscribe,
// ABOUTME: runtime_snapshot with Pi AgentMessage payloads, and history render.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

const enMessages = JSON.parse(readFileSync(join(process.cwd(), "public/locales/en.json"), "utf8"));

const BOOTSTRAP_TARGET = {
  workspaceId: "ws-uuid-1",
  sessionId: "session-a",
  instanceId: "instance-1",
  ownerId: "owner-1",
  workspaceGeneration: 2,
};

// Pi get_messages returns AgentMessage objects (no {type:"message"} wrapper).
// The snapshot must bridge them into the session-file entry shape the
// renderer consumes, or every entry is dropped and the transcript stays empty.
const AGENT_MESSAGES = [
  {
    id: "entry-1",
    role: "user",
    content: [{ type: "text", text: "hello from snapshot" }],
  },
  {
    id: "entry-2",
    role: "assistant",
    content: [{ type: "text", text: "snapshot reply" }],
    usage: { input: 10, output: 5, cacheRead: 0, cacheWrite: 0, cost: 0.01 },
  },
];

function hostSnapshotFrame() {
  return {
    type: "runtime_snapshot",
    requestId: "snapshot-1",
    target: BOOTSTRAP_TARGET,
    sequence: 1,
    state: {
      lifecycle: "Ready",
      pi: {
        model: { id: "claude-sonnet", provider: "anthropic", contextWindow: 200000 },
        thinkingLevel: "off",
        isStreaming: false,
        sessionFile: "/pi/sessions/--ws-1--/session-a.jsonl",
        sessionId: "pi-session-id",
      },
      messages: AGENT_MESSAGES,
      stats: {
        sessionFile: "/pi/sessions/--ws-1--/session-a.jsonl",
        tokens: { input: 10, output: 5, cacheRead: 0, cacheWrite: 0, total: 15 },
        cost: 0.01,
      },
    },
  };
}

class FakeWebSocket extends EventTarget {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static instances = [];
  // Test toggle: when true the fake host never answers snapshot requests,
  // simulating a runtime that is still spawning.
  static suppressSnapshot = false;
  // Test fixture: disk transcript served for read_session_messages requests.
  static diskMessages = null;
  static diskTruncated = false;
  // Test fixture: tree served for read_session_tree requests.
  static treeData = null;
  // Test fixture: payload for runtime get_entries requests. "fail" forces
  // the fallback to the disk data plane.
  static getEntriesData = null;

  constructor() {
    super();
    this.readyState = FakeWebSocket.CONNECTING;
    this.sent = [];
    FakeWebSocket.instances.push(this);
    // connect() assigns onopen synchronously after construction; the
    // microtask fires after that assignment, mirroring a real socket open.
    queueMicrotask(() => {
      this.readyState = FakeWebSocket.OPEN;
      this.onopen?.();
    });
  }

  reply(frame) {
    queueMicrotask(() => this.onmessage?.({ data: JSON.stringify(frame) }));
  }

  send(message) {
    const frame = JSON.parse(message);
    this.sent.push(frame);
    if (frame.type === "hello") {
      this.reply({ type: "hello_ack", protocolVersion: 2 });
      return;
    }
    if (frame.type === "runtime_subscribe") {
      this.reply({
        type: "runtime_subscribed",
        requestId: frame.requestId,
      });
      return;
    }
    if (frame.type === "runtime_snapshot_request") {
      if (FakeWebSocket.suppressSnapshot) return;
      const reply = hostSnapshotFrame();
      reply.requestId = frame.requestId;
      this.reply(reply);
      return;
    }
    if (frame.type === "host_request") {
      let response = null;
      if (frame.operation === "workspace.list") {
        response = {
          workspaces: [
            {
              workspaceId: "ws-uuid-1",
              canonicalPath: "/work/repo",
              displayName: "repo",
              pinned: false,
              lastOpenedAt: 100,
            },
          ],
          removed: [],
        };
      } else if (frame.operation === "runtime_instances") {
        response = { instances: [] };
      } else if (frame.operation === "list_skill_inventory") {
        response = { skills: [] };
      }
      if (response) {
        this.reply({
          type: "host_response",
          requestId: frame.requestId,
          ok: true,
          response,
        });
      }
      return;
    }
    if (frame.type === "runtime_request" && frame.command?.type === "prompt") {
      const msg = String(frame.command?.message || "");
      if (msg.includes("/picot-config")) {
        this.reply({
          type: "runtime_response",
          requestId: frame.requestId,
          ok: true,
          response: { success: true },
        });
        // The bridge answers asynchronously via a __picotConfig notify event
        // (extension ctx.ui.notify), consumed by the config gateway.
        const parsed = JSON.parse(msg.replace("/picot-config ", ""));
        queueMicrotask(() => {
          this.onmessage?.({
            data: JSON.stringify({
              type: "runtime_event",
              target: BOOTSTRAP_TARGET,
              sequence: 9000 + this.sent.length,
              event: {
                type: "extension_ui_request",
                method: "notify",
                message: JSON.stringify({
                  __picotConfig: parsed.id,
                  ok: true,
                  data: msg.includes('"op":"list_package_skill_inventory"')
                    ? { packages: [], diagnostics: [] }
                    : {},
                }),
              },
            }),
          });
        });
        return;
      }
    }
    if (frame.type === "runtime_request" && frame.command?.type === "get_entries") {
      const fail = FakeWebSocket.getEntriesData === "fail";
      this.reply({
        type: "runtime_response",
        requestId: frame.requestId,
        ok: true,
        response: {
          success: !fail,
          data: fail ? null : (FakeWebSocket.getEntriesData ?? { entries: [], leafId: null }),
        },
      });
      return;
    }
    if (frame.type === "data_request") {
      if (frame.operation === "read_session_messages" && FakeWebSocket.diskMessages) {
        this.reply({
          type: "data_response",
          requestId: frame.requestId,
          ok: true,
          messages: FakeWebSocket.diskMessages,
          truncated: FakeWebSocket.diskTruncated === true,
        });
        return;
      }
      if (frame.operation === "read_session_tree" && FakeWebSocket.treeData) {
        this.reply({
          type: "data_response",
          requestId: frame.requestId,
          ok: true,
          tree: FakeWebSocket.treeData,
        });
        return;
      }
      if (frame.operation === "workspace_info") {
        this.reply({
          type: "data_response",
          requestId: frame.requestId,
          ok: true,
          isGit: false,
        });
      } else {
        // Generic ack for every other data op so no request dangles.
        this.reply({
          type: "data_response",
          requestId: frame.requestId,
          ok: true,
        });
      }
      return;
    }
  }

  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }
}

beforeEach(async () => {
  // Canonical route page: the shell boots at /workspaces/:wid/sessions/:sid,
  // bootstraps its target from the host, and renders the runtime snapshot.
  window.history.pushState(null, "", "/workspaces/ws-uuid-1/sessions/session-a");

  const fixture = new DOMParser().parseFromString(
    readFileSync(join(process.cwd(), "public/index.html"), "utf8"),
    "text/html",
  );
  document.documentElement.replaceChildren(...fixture.documentElement.childNodes);
  const storage = new Map();
  const storageApi = {
    getItem: (key) => storage.get(key) ?? null,
    setItem: (key, value) => storage.set(key, String(value)),
    removeItem: (key) => storage.delete(key),
  };
  vi.stubGlobal("localStorage", storageApi);
  vi.stubGlobal("sessionStorage", storageApi);
  globalThis.WebSocket = FakeWebSocket;
  globalThis.fetch = vi.fn(async (input) => {
    const url = String(input);
    if (url.startsWith("/locales/en.json")) {
      return new Response(JSON.stringify(enMessages));
    }
    if (url.startsWith("/v2/bootstrap")) {
      return new Response(JSON.stringify(BOOTSTRAP_TARGET));
    }
    return new Response(JSON.stringify({}), { status: 404 });
  });
  vi.spyOn(console, "debug").mockImplementation(() => {});
  vi.spyOn(console, "log").mockImplementation(() => {});
  globalThis.requestAnimationFrame = (callback) => callback();
  window.requestAnimationFrame = globalThis.requestAnimationFrame;
  globalThis.ResizeObserver = class {
    observe() {}
    disconnect() {}
  };
  window.matchMedia = vi.fn(() => ({
    matches: false,
    addEventListener() {},
    removeEventListener() {},
  }));
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  window.history.pushState(null, "", "/");
  document.documentElement.replaceChildren();
  delete globalThis.WebSocket;
  delete globalThis.fetch;
  delete globalThis.requestAnimationFrame;
  delete globalThis.ResizeObserver;
});

test("canonical page renders history from the runtime snapshot", async () => {
  const warn = vi.spyOn(console, "warn").mockImplementation(() => {});

  await import("./app.js?canonical-snapshot");

  // The snapshot's AgentMessages must surface in the transcript.
  await vi.waitFor(() => {
    expect(document.getElementById("messages").textContent).toContain("hello from snapshot");
    expect(document.getElementById("messages").textContent).toContain("snapshot reply");
  });

  // Viewing the live session must not lock the composer into mirror-readonly.
  // (The composer may still be disabled by onboarding when no API key is
  // configured — that is product behavior, not a snapshot-chain bug.)
  const inputArea = document.querySelector(".input-area");
  expect(inputArea.classList.contains("mirror-readonly")).toBe(false);

  // Every host reply type is recognized (no unknown-frame warnings).
  expect(warn).not.toHaveBeenCalledWith(
    expect.stringContaining("Unknown HostServer v2 message type"),
  );

  // Frames carry only the routing triple; owner/generation are host-side.
  const socket = FakeWebSocket.instances.at(-1);
  const snapshotRequest = socket.sent.find((frame) => frame.type === "runtime_snapshot_request");
  expect(snapshotRequest.target).toEqual({
    workspaceId: "ws-uuid-1",
    sessionId: "session-a",
    instanceId: "instance-1",
  });

  // Workspace identity flows natively: the snapshot's DB uuid resolves the
  // registry row's display path for the toolbar indicator...
  await vi.waitFor(() => {
    expect(document.body.textContent).toContain("/work/repo");
  });
  // ...and data-plane frames carry the DB uuid, never a display id.
  const dataFrames = socket.sent.filter((frame) => frame.type === "data_request");
  expect(dataFrames.length).toBeGreaterThan(0);
  expect(
    dataFrames.every(
      (frame) => frame.workspaceId === undefined || frame.workspaceId === "ws-uuid-1",
    ),
  ).toBe(true);
});

test("a foreground snapshot re-fetches the authoritative tree instead of poisoning it", async () => {
  await import("./app.js?info-tree-sync");
  const socket = FakeWebSocket.instances.at(-1);

  // Open the Info panel: the first snapshot arrived while the panel was
  // hidden, so opening immediately fetches pi's live tree.
  document.getElementById("file-sidebar-info-tab").click();
  await new Promise((resolve) => setTimeout(resolve, 50));
  const treeRequests = () =>
    socket.sent.filter(
      (frame) => frame.type === "data_request" && frame.operation === "read_session_tree",
    );
  const afterOpen = treeRequests().length;
  expect(afterOpen).toBeGreaterThanOrEqual(1);

  // A second foreground snapshot arrives while the panel is open. Snapshot
  // messages carry no entry ids (verified against pi 0.84.2: get_messages
  // returns {role, content, timestamp}), so absorbing them as a clean sync
  // would poison the cache into an unbuildable tree.
  socket.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      target: BOOTSTRAP_TARGET,
      sequence: 2,
      state: {
        pi: {},
        messages: [{ role: "user", content: [{ type: "text", text: "x" }] }],
        stats: {},
      },
    }),
  });
  await new Promise((resolve) => setTimeout(resolve, 50));
  // The tree is re-fetched from the session FILE via the data plane, never
  // absorbed from the runtime snapshot's id-less messages.
  expect(treeRequests().length).toBe(afterOpen + 1);
});

test("disk upgrade re-renders the transcript with entry-id anchors", async () => {
  // Disk holds one MORE message than the snapshot: the transcript must
  // upgrade to the disk render — the upgrade carries stable entry ids,
  // which is what makes Info-panel click-to-locate (and fork/edit) work.
  FakeWebSocket.diskMessages = [
    { role: "user", entryId: "d1", content: [{ type: "text", text: "disk question" }] },
    {
      role: "assistant",
      entryId: "d2",
      content: [{ type: "text", text: "disk answer" }],
    },
    {
      role: "assistant",
      entryId: "d3",
      content: [{ type: "text", text: "disk final word" }],
    },
  ];
  try {
    await import("./app.js?disk-upgrade");
    const socket = FakeWebSocket.instances.at(-1);

    // The snapshot rendered its (id-less in production) messages; the app
    // then asks the host data plane for the id-bearing disk transcript.
    const upgradeRequest = await vi.waitFor(() => {
      const frame = socket.sent.find(
        (frame) => frame.type === "data_request" && frame.operation === "read_session_messages",
      );
      expect(frame).toBeTruthy();
      return frame;
    });
    expect(upgradeRequest.workspaceId).toBe("ws-uuid-1");
    expect(upgradeRequest.sessionId).toBe("session-a");

    await vi.waitFor(() => {
      expect(document.getElementById("messages").textContent).toContain("disk final word");
    });
    // The upgrade REPLACES the snapshot render (caller-owned clear), never
    // appends after it: snapshot-only content must be gone, or the tail's
    // last messages appear duplicated at the top of the transcript.
    expect(document.getElementById("messages").textContent).not.toContain("hello from snapshot");
    const anchors = document.querySelectorAll("#messages [data-entry-id]");
    // d2 and d3 are consecutive assistants in one turn: the renderer keeps
    // only the final answer (d3) as a row — user d1 + final d3 are anchored.
    const anchorIds = [...anchors].map((el) => el.dataset.entryId);
    expect(anchorIds).toContain("d1");
    expect(anchorIds).toContain("d3");
    expect(anchorIds).not.toContain("d2");
  } finally {
    FakeWebSocket.diskMessages = null;
  }
});

test("a truncated disk read does not paint as the whole session", async () => {
  // A very long session's disk read hits the host's byte budget and comes
  // back with the newest turns only. Painting that as the whole transcript
  // would silently start mid-conversation, so the snapshot must win.
  FakeWebSocket.diskMessages = [
    { role: "user", entryId: "x1", content: [{ type: "text", text: "truncated disk tail" }] },
  ];
  FakeWebSocket.diskTruncated = true;
  try {
    await import("./app.js?disk-truncated");
    // The snapshot stays on screen: the truncated tail never replaces it.
    await vi.waitFor(() => {
      expect(document.getElementById("messages").textContent).toContain("hello from snapshot");
    });
    expect(document.getElementById("messages").textContent).not.toContain("truncated disk tail");
  } finally {
    FakeWebSocket.diskMessages = null;
    FakeWebSocket.diskTruncated = false;
  }
});

test("a disk read that merely TIES the snapshot still upgrades for entry-id anchors", async () => {
  // A settled session's file count EQUALS the snapshot count; upgrading only
  // on a strictly longer disk read would leave the common case anchorless
  // and Info-panel click-to-locate dead. On a tie the id-bearing disk render
  // wins (upstream renders disk first for exactly this reason).
  FakeWebSocket.diskMessages = [
    { role: "user", entryId: "t1", content: [{ type: "text", text: "tie disk question" }] },
    {
      role: "assistant",
      entryId: "t2",
      content: [{ type: "text", text: "tie disk answer" }],
    },
  ];
  try {
    await import("./app.js?disk-tie");
    await vi.waitFor(() => {
      expect(document.getElementById("messages").textContent).toContain("tie disk answer");
    });
    const anchors = document.querySelectorAll("#messages [data-entry-id]");
    expect([...anchors].some((el) => el.dataset.entryId === "t1")).toBe(true);
  } finally {
    FakeWebSocket.diskMessages = null;
  }
});

test("message_end refreshes the open Info panel from pi's live tree", async () => {
  // Upstream contract: a persisted user or final assistant message triggers a
  // full tree refresh of the OPEN panel from pi's live get_entries snapshot —
  // no incremental append machinery, no staleness windows.
  await import("./app.js?boundary-refresh");
  const socket = FakeWebSocket.instances.at(-1);
  document.getElementById("file-sidebar-info-tab").click();
  const treeRequests = () =>
    socket.sent.filter(
      (frame) => frame.type === "data_request" && frame.operation === "read_session_tree",
    );
  await vi.waitFor(() => expect(treeRequests().length).toBeGreaterThanOrEqual(1));
  await new Promise((resolve) => setTimeout(resolve, 50));
  socket.sent.length = 0;
  expect(treeRequests().length).toBe(0);

  socket.onmessage({
    data: JSON.stringify({
      type: "runtime_event",
      target: BOOTSTRAP_TARGET,
      sequence: 9,
      event: {
        type: "message_end",
        entryId: "e2",
        message: {
          role: "assistant",
          content: [{ type: "text", text: "turn answer" }],
          stopReason: "stop",
        },
      },
    }),
  });
  await vi.waitFor(() => expect(treeRequests().length).toBe(1));
});

test("pi's live tree is the panel's primary source", async () => {
  // Upstream contract: prefer pi's live get_entries — the full tree WITH the
  // authoritative active leaf (Resume/Edit flip the panel immediately). The
  // session-file read is not consulted while the runtime serves.
  FakeWebSocket.getEntriesData = {
    entries: [
      {
        type: "message",
        id: "e1",
        parentId: null,
        message: { role: "user", content: [{ type: "text", text: "live seed question" }] },
      },
    ],
    leafId: "e1",
  };
  try {
    await import("./app.js?tree-primary");
    document.getElementById("file-sidebar-info-tab").click();
    await vi.waitFor(() => {
      expect(document.getElementById("info-panel")?.textContent).toContain("live seed question");
    });
    const socket = FakeWebSocket.instances.at(-1);
    expect(
      socket.sent.some(
        (frame) => frame.type === "data_request" && frame.operation === "read_session_tree",
      ),
    ).toBe(false);
  } finally {
    FakeWebSocket.getEntriesData = null;
    FakeWebSocket.treeData = null;
  }
});

test("a runtime with no live tree falls back to the session file", async () => {
  // Runtime get_entries fails (or returns empty): the host data plane reads
  // the session file and serves the tree.
  FakeWebSocket.getEntriesData = "fail";
  FakeWebSocket.treeData = {
    entries: [
      {
        type: "message",
        id: "f1",
        parentId: null,
        message: { role: "user", content: [{ type: "text", text: "file only question" }] },
      },
    ],
    leafId: "f1",
  };
  try {
    await import("./app.js?tree-file-fallback");
    document.getElementById("file-sidebar-info-tab").click();
    await vi.waitFor(() => {
      expect(document.getElementById("info-panel")?.textContent).toContain("file only question");
    });
    const socket = FakeWebSocket.instances.at(-1);
    expect(
      socket.sent.some(
        (frame) => frame.type === "data_request" && frame.operation === "read_session_tree",
      ),
    ).toBe(true);
  } finally {
    FakeWebSocket.getEntriesData = null;
    FakeWebSocket.treeData = null;
  }
});

test("disk history is fetched once at startup, not re-fetched per sync", async () => {
  FakeWebSocket.diskMessages = [
    { role: "user", entryId: "f1", content: [{ type: "text", text: "fetch once question" }] },
    {
      role: "assistant",
      entryId: "f2",
      content: [{ type: "text", text: "fetch once answer" }],
    },
  ];
  try {
    await import("./app.js?disk-once");
    const socket = FakeWebSocket.instances.at(-1);
    const diskRequests = () =>
      socket.sent.filter(
        (frame) => frame.type === "data_request" && frame.operation === "read_session_messages",
      );
    await vi.waitFor(() => expect(diskRequests().length).toBe(1));
    // A later mirror sync for the same session must NOT re-fetch the disk
    // history: upstream captures it once per switch and chooses between the
    // two sources at render time.
    socket.onmessage({
      data: JSON.stringify({
        type: "runtime_snapshot",
        target: BOOTSTRAP_TARGET,
        sequence: 3,
        state: {
          pi: {},
          messages: [{ role: "user", content: [{ type: "text", text: "y" }] }],
          stats: {},
        },
      }),
    });
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(diskRequests().length).toBe(1);
  } finally {
    FakeWebSocket.diskMessages = null;
  }
});

test("tree navigation (edit/resume) goes through the config bridge and re-anchors the transcript", async () => {
  // Native migration overwrote the Resume/Edit path with a native RPC pi
  // does not have ("Unknown command: navigate_tree"). Upstream calls the
  // /picot-config bridge. After navigating, pi's active branch is the
  // authority but the file's last-message tip chain still points at the OLD
  // branch — the transcript must re-anchor from pi's live entries (flat,
  // id-bearing, pi-owned leafId), not the stale disk chain.
  FakeWebSocket.getEntriesData = {
    entries: [
      {
        type: "message",
        id: "r1",
        parentId: null,
        message: { role: "user", content: [{ type: "text", text: "resumed branch question" }] },
      },
      {
        type: "message",
        id: "r2",
        parentId: "r1",
        message: { role: "assistant", content: [{ type: "text", text: "resumed branch answer" }] },
      },
      {
        type: "message",
        id: "old1",
        parentId: "r1",
        message: { role: "assistant", content: [{ type: "text", text: "old branch tail" }] },
      },
    ],
    leafId: "r2",
  };
  try {
    await import("./app.js?navigate-bridge");
    const socket = FakeWebSocket.instances.at(-1);
    const nativeNav = () =>
      socket.sent.filter(
        (frame) => frame.type === "runtime_request" && frame.command?.type === "navigate_tree",
      );
    const bridgeNav = () =>
      socket.sent.filter(
        (frame) =>
          frame.type === "runtime_request" &&
          frame.command?.type === "prompt" &&
          String(frame.command?.message || "").includes('"op":"navigate_tree"'),
      );
    expect(bridgeNav()).toHaveLength(0);

    document
      .getElementById("messages")
      .dispatchEvent(
        new CustomEvent("messageedit", { detail: { entryId: "r1", text: "edit me" } }),
      );

    await vi.waitFor(() => expect(bridgeNav()).toHaveLength(1));
    expect(nativeNav()).toHaveLength(0);
    // Transcript re-anchored from pi's live entries: the resumed branch
    // renders with anchors; the old-branch sibling does not.
    await vi.waitFor(() => {
      expect(document.getElementById("messages").textContent).toContain("resumed branch answer");
    });
    expect(document.getElementById("messages").textContent).not.toContain("old branch tail");
    const anchors = document.querySelectorAll("#messages [data-entry-id]");
    expect([...anchors].some((el) => el.dataset.entryId === "r1")).toBe(true);
  } finally {
    FakeWebSocket.getEntriesData = null;
  }
});

test("a compacted session's summary-inflated snapshot never displaces the anchored disk render", async () => {
  // pi's get_messages synthesizes compactionSummary messages, so a compacted
  // session's snapshot can count MORE entries than the file's message chain
  // while containing less content. The count heuristic would then pick the
  // id-less snapshot on every sync and wipe the transcript anchors. The
  // session file is the history source of truth (Dr. Lin's contract): matching
  // disk history wins unconditionally.
  FakeWebSocket.diskMessages = [
    { role: "user", entryId: "c1", content: [{ type: "text", text: "compacted turn question" }] },
    {
      role: "assistant",
      entryId: "c2",
      content: [{ type: "text", text: "compacted turn answer" }],
    },
  ];
  try {
    await import("./app.js?compaction-count");
    const socket = FakeWebSocket.instances.at(-1);
    // A post-disk mirror sync whose snapshot is "longer" (summary inflation):
    // 3 entries vs the disk chain's 2.
    await vi.waitFor(() =>
      expect(
        socket.sent.some(
          (f) => f.type === "data_request" && f.operation === "read_session_messages",
        ),
      ).toBe(true),
    );
    await vi.waitFor(() => {
      expect(document.getElementById("messages").textContent).toContain("compacted turn answer");
    });
    socket.onmessage({
      data: JSON.stringify({
        type: "runtime_snapshot",
        target: BOOTSTRAP_TARGET,
        sequence: 4,
        state: {
          pi: { sessionFile: "/pi/sessions/session-a.jsonl" },
          messages: [
            { role: "compactionSummary", content: [{ type: "text", text: "summary blob" }] },
            { role: "user", content: [{ type: "text", text: "compacted turn question" }] },
            { role: "assistant", content: [{ type: "text", text: "compacted turn answer" }] },
          ],
          stats: {},
        },
      }),
    });
    await new Promise((resolve) => setTimeout(resolve, 80));
    // The disk render survives: anchors intact, snapshot-only content absent.
    expect(document.getElementById("messages").textContent).toContain("compacted turn answer");
    expect(document.getElementById("messages").textContent).not.toContain("summary blob");
    expect(
      [...document.querySelectorAll("#messages [data-entry-id]")].some(
        (el) => el.dataset.entryId === "c1",
      ),
    ).toBe(true);
  } finally {
    FakeWebSocket.diskMessages = null;
  }
});

test("tree navigation flips the Info panel's active branch to pi's live leaf", async () => {
  // After a Resume, pi's active leaf is the resumed branch — but the session
  // file still ends with the abandoned branch's messages, so read_session_tree's
  // file-tip leaf keeps pointing at the OLD branch. The panel must follow pi's
  // live leaf (get_entries), not the file tip.
  const u1 = {
    type: "message",
    id: "u1",
    parentId: null,
    message: { role: "user", content: [{ type: "text", text: "root question" }] },
  };
  const b1 = {
    type: "message",
    id: "b1",
    parentId: "u1",
    message: { role: "assistant", content: [{ type: "text", text: "resumed branch answer" }] },
  };
  const a1 = {
    type: "message",
    id: "a1",
    parentId: "u1",
    message: { role: "assistant", content: [{ type: "text", text: "abandoned branch answer" }] },
  };
  // File order: the abandoned branch's message is LAST (appended later), so
  // the file-tip leaf is a1.
  FakeWebSocket.treeData = { entries: [u1, b1, a1], leafId: "a1" };
  // Before navigation pi's live leaf agrees with the file tip; after the
  // bridge navigate it becomes b1.
  FakeWebSocket.getEntriesData = { entries: [u1, b1, a1], leafId: "a1" };
  try {
    await import("./app.js?panel-leaf-flip");
    document.getElementById("file-sidebar-info-tab").click();
    await vi.waitFor(() => {
      const current = document.querySelector("#info-panel .info-panel-row.current-leaf");
      expect(current?.dataset.entryId).toBe("a1"); // live leaf before navigation
    });

    // pi navigated to b1: its live leaf flips.
    FakeWebSocket.getEntriesData = { entries: [u1, b1, a1], leafId: "b1" };
    document
      .getElementById("messages")
      .dispatchEvent(new CustomEvent("messageedit", { detail: { entryId: "u1", text: "edit" } }));

    await vi.waitFor(() => {
      const current = document.querySelector("#info-panel .info-panel-row.current-leaf");
      expect(current?.dataset.entryId).toBe("b1"); // pi's live leaf after navigation
    });
    const current = document.querySelector("#info-panel .info-panel-row.current-leaf");
    expect(current?.dataset.entryId).not.toBe("a1");
  } finally {
    FakeWebSocket.treeData = null;
    FakeWebSocket.getEntriesData = null;
  }
});

test("/picot-config reads wait for a foreground snapshot before dispatching", async () => {
  // Since 8c84b0b the Skills tab no longer activates eagerly at startup;
  // the readiness gate still defers any config read (e.g. the user opening
  // Settings → Skills) until the CURRENT target's first foreground snapshot
  // proves the runtime live.
  FakeWebSocket.suppressSnapshot = true;
  try {
    await import("./app.js?config-gate");
    const socket = FakeWebSocket.instances.at(-1);
    await new Promise((resolve) => setTimeout(resolve, 150));
    const configFrames = () =>
      socket.sent.filter(
        (frame) =>
          frame.type === "runtime_request" &&
          String(frame.command?.message || "").includes("list_package_skill_inventory"),
      );

    // A user-initiated package-skills read queues behind the gate: no
    // snapshot has proven the runtime live yet, so nothing dispatches.
    // (Discovered-skills inventory rides host control ops since the
    // landing single-path switch; the packages sub-tab is the bridge
    // consumer this gate still protects.)
    document.getElementById("settings-btn").click();
    document.querySelector('[data-settings-tab="skills"]').click();
    document.querySelector('[data-skills-page-tab="packages"]').click();
    expect(configFrames()).toHaveLength(0);

    // Deliver the first foreground snapshot: the gate opens and exactly the
    // queued inventory read dispatches. The reply must carry the PENDING
    // snapshot request's id — snapshot ids share the client's request
    // counter, so a hardcoded "snapshot-1" breaks whenever an earlier
    // startup request consumed a number.
    const snapshotRequest = socket.sent.find((frame) => frame.type === "runtime_snapshot_request");
    expect(snapshotRequest).toBeTruthy();
    const snapshot = hostSnapshotFrame();
    snapshot.requestId = snapshotRequest.requestId;
    socket.reply(snapshot);
    await vi.waitFor(() => expect(configFrames()).toHaveLength(1));
    expect(configFrames()[0].command.type).toBe("prompt");
  } finally {
    FakeWebSocket.suppressSnapshot = false;
  }
});
