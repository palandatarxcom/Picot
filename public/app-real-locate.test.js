// ABOUTME: End-to-end locate check with a REAL session file — every Info panel
// ABOUTME: row must locate (direct anchor or ancestor fallback), zero warnings.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

const enMessages = JSON.parse(readFileSync(join(process.cwd(), "public/locales/en.json"), "utf8"));

const SESSION_FILE =
  process.env.PICOT_LOCATE_SESSION ||
  "/Users/linyong/.pi/agent/sessions/--Users-linyong-tmp-PI-picot-v3--/2026-08-26T11-17-48-874Z_01a03dca-62ca-770d-ad85-ab3ad3cf7bf6.jsonl";
// A real session file is machine-local user data, not a repo fixture: when it
// is absent (another machine, a pruned session bucket, a sandboxed HOME) the
// probe skips instead of failing the suite. Point PICOT_LOCATE_SESSION at any
// local session .jsonl to run it against your own data.
const SESSION_AVAILABLE = existsSync(SESSION_FILE);
if (!SESSION_AVAILABLE) {
  console.warn(`[app-real-locate] session file not found, skipping: ${SESSION_FILE}`);
}

const BOOTSTRAP_TARGET = {
  workspaceId: "ws-uuid-1",
  sessionId: "session-a",
  instanceId: "instance-1",
  ownerId: "owner-1",
  workspaceGeneration: 2,
};

class FakeWebSocket extends EventTarget {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static instances = [];
  static suppressSnapshot = false;
  static diskMessages = null;
  static treeData = null;
  static getEntriesData = null;

  constructor() {
    super();
    this.readyState = FakeWebSocket.CONNECTING;
    this.sent = [];
    FakeWebSocket.instances.push(this);
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
      this.reply({ type: "runtime_subscribed", requestId: frame.requestId });
      return;
    }
    if (frame.type === "runtime_snapshot_request") {
      if (FakeWebSocket.suppressSnapshot) return;
      // The snapshot mirrors the disk chain (a settled session): same content,
      // id-less — exactly what pi's get_messages would return.
      this.reply({
        type: "runtime_snapshot",
        requestId: frame.requestId,
        target: BOOTSTRAP_TARGET,
        sequence: 1,
        state: {
          lifecycle: "Ready",
          pi: {
            model: { id: "claude-sonnet", provider: "anthropic", contextWindow: 200000 },
            sessionFile: "/pi/sessions/session-a.jsonl",
            sessionId: "session-a",
          },
          messages: FakeWebSocket.diskMessages ?? [],
          stats: {},
        },
      });
      return;
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
      this.reply({ type: "data_response", requestId: frame.requestId, ok: true });
      return;
    }
  }

  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }
}

beforeEach(async () => {
  // jsdom does not implement scrollIntoView; Info panel locate rows call it.
  window.HTMLElement.prototype.scrollIntoView = () => {};
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
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

test.skipIf(!SESSION_AVAILABLE)(
  "every Info panel row locates the transcript on a real session",
  async () => {
    // Real file → fixtures replicating the host's two data-plane shapes.
    const entries = [];
    for (const line of readFileSync(SESSION_FILE, "utf8").split("\n")) {
      if (!line.trim()) continue;
      try {
        const e = JSON.parse(line);
        if (typeof e.id === "string") entries.push(e);
      } catch {
        // skip malformed
      }
    }
    const byId = new Map(entries.map((e) => [e.id, e]));
    let tip = null;
    for (const e of entries) if (e.type === "message") tip = e;
    // read_session_messages shape: tip-chain messages, entryId on user/assistant.
    const chain = [];
    {
      const visited = new Set();
      let cur = tip;
      while (cur && !visited.has(cur.id)) {
        visited.add(cur.id);
        chain.unshift(cur);
        cur = cur.parentId ? byId.get(cur.parentId) : null;
      }
    }
    const diskMessages = chain
      .filter((e) => e.type === "message" && e.message)
      .map((e) => ({
        ...e.message,
        ...(e.message.role === "user" || e.message.role === "assistant" ? { entryId: e.id } : {}),
      }));
    FakeWebSocket.treeData = { entries, leafId: tip.id };
    FakeWebSocket.diskMessages = diskMessages;

    await import("./app.js?real-locate");

    document.getElementById("file-sidebar-info-tab").click();
    await vi.waitFor(() => {
      expect(document.querySelectorAll("#info-panel .info-panel-row").length).toBeGreaterThan(0);
    });
    await vi.waitFor(() => {
      expect(document.querySelectorAll("#messages [data-entry-id]").length).toBeGreaterThan(0);
    });

    const warns = [];
    vi.spyOn(console, "warn").mockImplementation((...args) => {
      warns.push(args.join(" "));
    });
    const rows = [...document.querySelectorAll("#info-panel .info-panel-row.active")];
    expect(rows.length).toBeGreaterThan(0);
    for (const row of rows) {
      row.click();
      // jsdom has no scrollIntoView; locating = no "no transcript anchor" warn.
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
    const noAnchor = warns.filter((w) => w.includes("no transcript anchor"));
    expect(
      noAnchor,
      `rows failing to locate:\n${[...rows]
        .filter((r) => noAnchor.some((w) => w.includes(r.dataset.entryId)))
        .map((r) => `${r.dataset.entryId}: ${r.textContent.slice(0, 50)}`)
        .join("\n")}`,
    ).toEqual([]);
  },
  60000,
);
