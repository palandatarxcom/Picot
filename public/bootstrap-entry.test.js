// ABOUTME: Locks the browser bootstrap dispatch: native `/` boots landing.js,
// ABOUTME: every canonical workspace route and non-native route boots app.js.
import { readFileSync } from "node:fs";
import { expect, test, vi } from "vitest";

const bootstrapEntry = readFileSync("public/bootstrap-entry.js", "utf8");

const loaded = vi.hoisted(() => ({ entries: [], capability: null }));

async function boot(pathname, capability) {
  // vi.doMock (not vi.mock): each boot re-registers the stubs so the module
  // factories re-run and record exactly which entry this boot imported.
  vi.resetModules();
  loaded.entries.length = 0;
  loaded.capability = capability;
  vi.doMock("./app/host-origin.js", () => ({
    readInjectedCapability: () => loaded.capability,
  }));
  vi.doMock("./landing.js", () => {
    loaded.entries.push("landing");
    return {};
  });
  vi.doMock("./app.js", () => {
    loaded.entries.push("app");
    return {};
  });
  window.history.replaceState({}, "", pathname);
  await import("./bootstrap-entry.js");
  // The entry import resolves asynchronously; under full-suite load one
  // macrotask is not always enough, so poll instead of sleeping once.
  await vi.waitFor(() => expect(loaded.entries).toHaveLength(1));
  vi.doUnmock("./app/host-origin.js");
  vi.doUnmock("./landing.js");
  vi.doUnmock("./app.js");
  return loaded.entries;
}

test("source keeps the canonical app entry and never revives retired entries", () => {
  expect(bootstrapEntry).toContain('"./app.js"');
  expect(bootstrapEntry).not.toContain("./native/app.js");
});

test("reads but never consumes the capability global", () => {
  // WebSocketClient consumes the injected capability later for the
  // authenticated hello; consuming it here would leave every native window
  // unauthenticated and the sidebar permanently empty.
  expect(bootstrapEntry).toContain("readInjectedCapability");
  expect(bootstrapEntry).not.toContain("consumeInjectedCapability");
});

test("native / boots the landing entry", async () => {
  expect(await boot("/", "native-capability")).toEqual(["landing"]);
});

test("non-native / keeps the browser app entry", async () => {
  expect(await boot("/", null)).toEqual(["app"]);
});

test("native canonical workspace routes keep the app entry", async () => {
  expect(await boot("/workspaces/wid/sessions/sid", "native-capability")).toEqual(["app"]);
});

const lastHostBoot = async (saved, hosts) => {
  const invoke = vi.fn(async () => hosts);
  globalThis.__TAURI__ = { core: { invoke } };
  window.localStorage.setItem("picot-last-host", saved);
  try {
    const entries = await boot("/", "native-capability");
    return { entries, search: window.location.search, invoke };
  } finally {
    delete globalThis.__TAURI__;
    window.localStorage.removeItem("picot-last-host");
    window.history.replaceState({}, "", "/");
  }
};

test("native / opens the window on the last host the user picked", async () => {
  const { entries, search, invoke } = await lastHostBoot("r1", [{ serverId: "r1" }]);
  expect(entries).toEqual(["landing"]);
  expect(search).toBe("?host=r1");
  expect(invoke.mock.calls[0][0]).toBe("host_list");
});

test("a remembered host that is gone still boots the landing on the local host", async () => {
  const { entries, search } = await lastHostBoot("r9", [{ serverId: "r1" }]);
  expect(entries).toEqual(["landing"]);
  expect(search).toBe("");
});
