// ABOUTME: Multi-window §7 listener wiring: the routed DOM events the app-side Rust
// executor dispatches into a chosen window (navigate / open), unit-tested.
import { afterEach, describe, expect, test, vi } from "vitest";
import { setupNotificationWindowEvents } from "./notification-window-events.js";

const en = {
  "workspace.focusedExisting": "already open — focused it for you.",
  "sidebar.openInNewWindowFailed": "Could not open in a new window. Try again.",
};

vi.mock("./i18n.js", () => ({
  t: (key) => en[key] ?? key,
}));

afterEach(() => {
  document.documentElement.innerHTML = "";
});

function dispatchRoutedEvent(name, detail) {
  // The Rust executor's exact channel: one eval, one DOM event on the document.
  document.dispatchEvent(new CustomEvent(name, { detail }));
}

describe("picot:notification-navigate (rule 2)", () => {
  test("routes the chosen window to the session through the sidebar's select flow", () => {
    const handleSessionSelect = vi.fn();
    setupNotificationWindowEvents({ handleSessionSelect });

    dispatchRoutedEvent("picot:notification-navigate", {
      sessionFile: "/tmp/sessions/w1/s1.jsonl",
      cwd: "/tmp/proj",
    });

    expect(handleSessionSelect).toHaveBeenCalledWith({
      filePath: "/tmp/sessions/w1/s1.jsonl",
      cwd: "/tmp/proj",
    });
  });

  test("ignores an activation without a session file", () => {
    const handleSessionSelect = vi.fn();
    setupNotificationWindowEvents({ handleSessionSelect });

    dispatchRoutedEvent("picot:notification-navigate", { cwd: "/tmp/proj" });

    expect(handleSessionSelect).not.toHaveBeenCalled();
  });
});

describe("picot:notification-open (rule 3)", () => {
  test("rides the production openWorkspace chain with the session path", async () => {
    const transport = { openWorkspace: vi.fn(async () => ({ focusedExisting: false })) };
    const notices = [];
    setupNotificationWindowEvents({ transport, onNotice: (message) => notices.push(message) });

    dispatchRoutedEvent("picot:notification-open", {
      cwd: "/tmp/proj",
      sessionFile: "/tmp/sessions/w1/s1.jsonl",
    });
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(transport.openWorkspace).toHaveBeenCalledWith("/tmp/proj", {
      sessionPath: "/tmp/sessions/w1/s1.jsonl",
      openWindow: true,
    });
    expect(notices).toEqual([]);
  });

  test("a focusedExisting answer becomes the focus toast, a failure the retry hint", async () => {
    const transport = { openWorkspace: vi.fn(async () => ({ focusedExisting: true })) };
    const notices = [];
    setupNotificationWindowEvents({ transport, onNotice: (message) => notices.push(message) });

    dispatchRoutedEvent("picot:notification-open", {
      cwd: "/tmp/proj",
      sessionFile: "/tmp/a.jsonl",
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(notices).toEqual([en["workspace.focusedExisting"]]);

    transport.openWorkspace.mockRejectedValue(new Error("spawn failed"));
    dispatchRoutedEvent("picot:notification-open", {
      cwd: "/tmp/proj",
      sessionFile: "/tmp/a.jsonl",
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(notices).toEqual([en["workspace.focusedExisting"], en["sidebar.openInNewWindowFailed"]]);
  });
});
