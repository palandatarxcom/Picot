// ABOUTME: Tests the sidebar session-export path against the relay Phase 2.5 inventory.
// ABOUTME: A remote host must never fetch the desktop-only one-shot export URL.

import { JSDOM } from "jsdom";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { setMessages } from "../i18n.js";
import { SessionSidebar } from "./index.js";

let dom;

beforeEach(() => {
  dom = new JSDOM('<div id="sessions"></div>', { url: "http://localhost:3001" });
  globalThis.document = dom.window.document;
  globalThis.localStorage = dom.window.localStorage;
  globalThis.CSS = dom.window.CSS || { escape: (value) => String(value).replace(/["\\]/g, "\\$&") };
  setMessages({ sidebar: { unavailable: "Unavailable" } });
  globalThis.fetch = vi.fn();
  globalThis.URL.createObjectURL = vi.fn(() => "blob:export");
  globalThis.URL.revokeObjectURL = vi.fn();
});

afterEach(() => {
  vi.restoreAllMocks();
});

function sidebarWith(transport) {
  return new SessionSidebar(document.getElementById("sessions"), vi.fn(), vi.fn(), { transport });
}

describe("sidebar session export", () => {
  test("a remote host never requests or fetches an export grant", async () => {
    const transport = {
      exportSession: vi.fn().mockResolvedValue({ exportUrl: "/v2/session-export/token" }),
      wsClient: { identity: "bridge" },
    };
    await sidebarWith(transport).exportSession({ id: "session-1" });

    expect(transport.exportSession).not.toHaveBeenCalled();
    expect(globalThis.fetch).not.toHaveBeenCalled();
  });

  test("the local desktop host keeps the one-shot export download", async () => {
    const transport = {
      exportSession: vi.fn().mockResolvedValue({ exportUrl: "/v2/session-export/token" }),
      wsClient: { identity: "desktop" },
    };
    globalThis.fetch.mockResolvedValue({ ok: true, blob: async () => new Blob(["<html></html>"]) });
    const click = vi
      .spyOn(dom.window.HTMLAnchorElement.prototype, "click")
      .mockImplementation(() => {});

    await sidebarWith(transport).exportSession({ id: "session-1" });

    expect(transport.exportSession).toHaveBeenCalledWith("session-1");
    expect(globalThis.fetch).toHaveBeenCalledWith("/v2/session-export/token");
    expect(click).toHaveBeenCalled();
  });
});
