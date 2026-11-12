// ABOUTME: Verifies the per-window host connection layer: URL host resolution,
// ABOUTME: per-host ws URL, capability fallback, and the reload-based switch.
import { describe, expect, test, vi } from "vitest";
import {
  applyLastHostDefault,
  connectHost,
  hostUrl,
  isCredentialRejection,
  LAST_HOST_KEY,
  LOCAL_HOST,
  readCapabilities,
  resolveHostFromUrl,
  resolveWsUrlForHost,
  supportsRemoteTerminal,
  switchHost,
  TERMINAL_BINDING_REVISION,
  watchCredentialInvalid,
} from "./host-connection.js";

const localEnv = () => ({ location: { protocol: "http:", host: "127.0.0.1:47821", search: "" } });

describe("resolveHostFromUrl", () => {
  test("no host param is the local host", () => {
    expect(resolveHostFromUrl(localEnv())).toBe(LOCAL_HOST);
    expect(resolveHostFromUrl({ location: { href: "http://127.0.0.1:4/?workspaceId=w" } })).toBe(
      LOCAL_HOST,
    );
  });

  test("host param names the window's host", () => {
    expect(resolveHostFromUrl({ location: { href: "http://127.0.0.1:4/?host=8f1c-remote" } })).toBe(
      "8f1c-remote",
    );
  });

  test("blank host param and unreadable location fall back to local", () => {
    expect(resolveHostFromUrl({ location: { href: "http://127.0.0.1:4/?host=%20" } })).toBe(
      LOCAL_HOST,
    );
    expect(resolveHostFromUrl({})).toBe(LOCAL_HOST);
  });
});

describe("resolveWsUrlForHost", () => {
  test("local keeps the current origin rule", () => {
    expect(resolveWsUrlForHost(LOCAL_HOST, localEnv())).toBe("ws://127.0.0.1:47821/v2/ws");
    expect(resolveWsUrlForHost(null, localEnv())).toBe("ws://127.0.0.1:47821/v2/ws");
    expect(resolveWsUrlForHost({ id: LOCAL_HOST }, localEnv())).toBe("ws://127.0.0.1:47821/v2/ws");
  });

  test("remote points at the bridge origin with the canonical path", () => {
    expect(
      resolveWsUrlForHost({ id: "h1", bridgeOrigin: "http://127.0.0.1:55123" }, localEnv()),
    ).toBe("ws://127.0.0.1:55123/v2/ws"); // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket -- expected loopback bridge URL
  });

  test("a host_status wsUrl is used as reported", () => {
    // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket -- loopback fixture URL
    expect(resolveWsUrlForHost({ id: "h1", wsUrl: "ws://127.0.0.1:55124/v2/ws" }, localEnv())).toBe(
      "ws://127.0.0.1:55124/v2/ws", // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket -- expected loopback bridge URL
    );
  });

  test("remote without a usable bridge URL is an explicit failure, never local", () => {
    expect(() => resolveWsUrlForHost({ id: "h1" }, localEnv())).toThrow(/bridge/i);
    // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket -- rejected malicious endpoint test input, never connected
    expect(() => resolveWsUrlForHost({ id: "h1", wsUrl: "ws://evil/v1/ws" }, localEnv())).toThrow(
      /bridge/i,
    );
  });
});

describe("readCapabilities", () => {
  test("reads the advertised faces from hello_ack", () => {
    expect(
      readCapabilities({
        capabilities: {
          native: true,
          navigation: true,
          git: true,
          terminal: true,
          management: true,
        },
      }),
    ).toEqual({
      native: true,
      class: "native",
      navigation: true,
      git: true,
      terminal: true,
      management: true,
      terminalBindingVersion: 0,
    });
    expect(
      readCapabilities({
        capabilities: {
          native: false,
          navigation: true,
          git: true,
          terminal: true,
          management: false,
        },
      }),
    ).toEqual({
      native: false,
      class: "remote",
      navigation: true,
      git: true,
      terminal: true,
      management: false,
      terminalBindingVersion: 0,
    });
  });

  test("a host that advertises no capabilities keeps native but denies management", () => {
    expect(readCapabilities({ type: "hello_ack" })).toEqual({
      native: true,
      class: "native",
      navigation: true,
      git: true,
      terminal: true,
      management: false,
      terminalBindingVersion: 0,
    });
    expect(readCapabilities(null).management).toBe(false);
  });

  test("non-boolean face values are denied", () => {
    const caps = readCapabilities({ capabilities: { native: "yes", navigation: 1, git: true } });
    expect(caps.native).toBe(false);
    expect(caps.navigation).toBe(false);
    expect(caps.git).toBe(true);
    expect(caps.management).toBe(false);
  });

  test("the approved terminal binding revision survives the face mapper", () => {
    const caps = readCapabilities({
      capabilities: { native: true, terminal: true, terminalBindingVersion: 1 },
    });
    expect(caps.terminalBindingVersion).toBe(TERMINAL_BINDING_REVISION);
    expect(caps.terminal).toBe(true);
    expect(supportsRemoteTerminal(caps)).toBe(true);
  });

  test("absent, string, boolean, fractional, and unknown revisions never authorize", () => {
    const revisions = [undefined, "1", true, 1.5, 0, 2, -1, null];
    for (const revision of revisions) {
      const caps = readCapabilities({
        capabilities: { native: true, terminal: true, terminalBindingVersion: revision },
      });
      // Only a positive safe integer is preserved verbatim; everything else
      // collapses to 0. Either way the gate refuses it.
      expect(caps.terminalBindingVersion, `revision ${revision}`).toBe(revision === 2 ? 2 : 0);
      expect(supportsRemoteTerminal(caps), `revision ${revision}`).toBe(false);
    }
  });

  test("a legacy ack keeps its face defaults but still cannot authorize terminal", () => {
    const caps = readCapabilities({ type: "hello_ack" });
    // Unrelated legacy defaults are unchanged (Desktop included).
    expect(caps).toEqual({
      native: true,
      class: "native",
      navigation: true,
      git: true,
      terminal: true,
      management: false,
      terminalBindingVersion: 0,
    });
    // …and the terminal gate still refuses it: `terminal: true` alone is not
    // evidence of the approved contract.
    expect(supportsRemoteTerminal(caps)).toBe(false);
  });

  test("terminal face must be true as well as the revision", () => {
    expect(
      supportsRemoteTerminal({
        terminal: false,
        terminalBindingVersion: TERMINAL_BINDING_REVISION,
      }),
    ).toBe(false);
    expect(supportsRemoteTerminal(undefined)).toBe(false);
  });
});

describe("credential invalid", () => {
  test("only an explicit unauthorized_device rejection counts", () => {
    expect(isCredentialRejection({ code: "unauthorized_device" })).toBe(true);
    expect(isCredentialRejection({ code: "handshake_rejected" })).toBe(false);
    expect(isCredentialRejection({ code: "invalid_client_type" })).toBe(false);
    expect(isCredentialRejection(new Error("WebSocket disconnected"))).toBe(false);
    expect(isCredentialRejection(undefined)).toBe(false);
  });

  test("the watcher fires on hostError frames and can be detached", () => {
    const target = new EventTarget();
    const seen = vi.fn();
    const detach = watchCredentialInvalid(target, seen);
    target.dispatchEvent(new CustomEvent("hostError", { detail: { code: "event_sequence_gap" } }));
    expect(seen).not.toHaveBeenCalled();
    target.dispatchEvent(new CustomEvent("hostError", { detail: { code: "unauthorized_device" } }));
    expect(seen).toHaveBeenCalledTimes(1);
    detach();
    target.dispatchEvent(new CustomEvent("hostError", { detail: { code: "unauthorized_device" } }));
    expect(seen).toHaveBeenCalledTimes(1);
  });
});

describe("switchHost", () => {
  test("navigates to the host-parameterized URL after the flush", async () => {
    const calls = [];
    const url = await switchHost("remote-1", {
      env: { location: { href: "http://127.0.0.1:47821/workspaces/w1" } },
      flush: () => calls.push("flush"),
      navigate: (href) => calls.push(`navigate:${href}`),
    });
    expect(calls).toEqual(["flush", `navigate:${url}`]);
    expect(url).toBe("http://127.0.0.1:47821/?host=remote-1");
  });

  test("switching back to local drops the host param", () => {
    expect(hostUrl(LOCAL_HOST, { location: { href: "http://127.0.0.1:47821/?host=r1" } })).toBe(
      "http://127.0.0.1:47821/",
    );
  });

  test("a failed flush keeps the window on the current host", async () => {
    const navigate = vi.fn();
    expect(
      await switchHost("remote-1", {
        env: { location: { href: "http://127.0.0.1:47821/" } },
        flush: () => {
          throw new Error("draft save failed");
        },
        navigate,
      }),
    ).toBeNull();
    expect(navigate).not.toHaveBeenCalled();
  });

  test("a flush that reports false keeps the window on the current host", async () => {
    const navigate = vi.fn();
    expect(
      await switchHost("remote-1", {
        env: { location: { href: "http://127.0.0.1:47821/" } },
        flush: async () => false,
        navigate,
      }),
    ).toBeNull();
    expect(navigate).not.toHaveBeenCalled();
  });

  test("a flush that reports true switches", async () => {
    const navigate = vi.fn();
    const url = await switchHost("remote-1", {
      env: { location: { href: "http://127.0.0.1:47821/" } },
      flush: async () => true,
      navigate,
    });
    expect(navigate).toHaveBeenCalledWith(url);
  });
});

describe("connectHost", () => {
  test("local resolves synchronously without any app IPC", async () => {
    const invoke = vi.fn();
    const connection = await connectHost({ env: localEnv(), invoke });
    expect(connection).toEqual({
      host: LOCAL_HOST,
      identity: "desktop",
      wsUrl: "ws://127.0.0.1:47821/v2/ws",
      clientOptions: { capabilitiesFromAck: readCapabilities },
    });
    expect(invoke).not.toHaveBeenCalled();
  });

  test("remote asks host_status and uses the bridge URL", async () => {
    const invoke = vi.fn(async (command) => {
      if (command === "host_status")
        return {
          serverId: "r1",
          status: "online",
          wsUrl: "ws://127.0.0.1:55123/v2/ws",
          bridgeGrantVersion: 1,
        };
      throw new Error(`unexpected command ${command}`);
    });
    const connection = await connectHost({
      env: { location: { href: "http://127.0.0.1:47821/?host=r1" } },
      invoke,
    });
    expect(invoke.mock.calls).toEqual([["host_status", { serverId: "r1" }]]);
    expect(connection).toMatchObject({
      host: "r1",
      identity: "bridge",
      wsUrl: "ws://127.0.0.1:55123/v2/ws",
      clientOptions: {
        identity: "bridge",
        capabilitiesFromAck: readCapabilities,
      },
    });
    // The connection authorizes through the dedicated command, never the status
    // call and never an endpoint the page can dial on its own.
    expect(connection.clientOptions.bridgeGrantProvider).toEqual(expect.any(Function));
  });

  const recoveryEnv = (href, storage = new Map()) => {
    const replaced = [];
    const rewrites = [];
    return {
      replaced,
      rewrites,
      env: {
        location: {
          href,
          protocol: "http:",
          host: "127.0.0.1:47821",
          replace: (u) => replaced.push(u),
        },
        sessionStorage: {
          getItem: (key) => (storage.has(key) ? storage.get(key) : null),
          setItem: (key, value) => storage.set(key, value),
          removeItem: (key) => storage.delete(key),
        },
        history: { replaceState: (_state, _title, url) => rewrites.push(url) },
      },
    };
  };
  const deadHost = () =>
    vi.fn(async () => {
      throw new Error("Host not found");
    });

  test("a remote host that cannot resolve falls back to local, never unauthenticated", async () => {
    const { env, replaced } = recoveryEnv("http://127.0.0.1:47821/?workspaceId=w1&host=gone");
    const connection = await connectHost({ env, invoke: deadHost() });
    expect(connection).toEqual({
      host: LOCAL_HOST,
      identity: "desktop",
      wsUrl: "ws://127.0.0.1:47821/v2/ws",
      clientOptions: { capabilitiesFromAck: readCapabilities },
    });
    // Self-rescue: reload onto the same URL without the dead host param.
    expect(replaced).toEqual(["http://127.0.0.1:47821/?workspaceId=w1"]);
  });

  test("a host that reports no bridge URL is blocked instead of dialing the origin", async () => {
    const { env, replaced } = recoveryEnv("http://127.0.0.1:47821/?host=r1");
    const invoke = vi.fn(async (command) => {
      if (command === "host_status") return { serverId: "r1", status: "offline" };
      throw new Error(`unexpected command ${command}`);
    });
    const connection = await connectHost({ env, invoke });
    // No bridge means no grant authority: the entry blocks rather than dialing
    // the page origin (an Origin-only bridge is exactly what T3 closed) and
    // rather than silently downgrading this window to the local host.
    expect(connection).toEqual({ host: "r1", identity: "bridge", blocked: true });
    expect(replaced).toEqual([]);
  });

  test("the recovery redirect runs once, so it can never loop on itself", async () => {
    const storage = new Map();
    const first = recoveryEnv("http://127.0.0.1:47821/?host=gone", storage);
    const second = recoveryEnv("http://127.0.0.1:47821/?host=gone", storage);
    await connectHost({ env: first.env, invoke: deadHost() });
    await connectHost({ env: second.env, invoke: deadHost() });
    expect(first.replaced).toHaveLength(1);
    expect(second.replaced).toHaveLength(0);
    // The second failure rewrites the URL in place instead of reloading again.
    expect(second.rewrites).toEqual(["http://127.0.0.1:47821/"]);
  });
});

describe("applyLastHostDefault", () => {
  const bootEnv = (href, stored) => {
    const replaced = [];
    return {
      replaced,
      env: {
        location: { href },
        localStorage: { getItem: (key) => (key === LAST_HOST_KEY ? stored : null) },
        history: { replaceState: (_state, _title, url) => replaced.push(url) },
      },
    };
  };
  const hosts = [{ serverId: "local-gone" }, { serverId: "r1", label: "desk" }];

  test("a window with no host param opens the remembered host", async () => {
    const { env, replaced } = bootEnv("http://127.0.0.1:47821/", "r1");
    const invoke = vi.fn(async () => hosts);
    expect(await applyLastHostDefault({ env, invoke })).toBe("r1");
    expect(invoke.mock.calls).toEqual([["host_list"]]);
    // The param is written on this window's own URL, with no navigation.
    expect(replaced).toEqual(["http://127.0.0.1:47821/?host=r1"]);
    expect(resolveHostFromUrl({ location: { href: replaced[0] } })).toBe("r1");
  });

  test("a remembered host that was deleted leaves the window local", async () => {
    const { env, replaced } = bootEnv("http://127.0.0.1:47821/", "r9");
    expect(await applyLastHostDefault({ env, invoke: vi.fn(async () => hosts) })).toBeNull();
    expect(replaced).toEqual([]);
  });

  test("no cache stays local without asking the app anything", async () => {
    const { env, replaced } = bootEnv("http://127.0.0.1:47821/", null);
    const invoke = vi.fn(async () => hosts);
    expect(await applyLastHostDefault({ env, invoke })).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
    expect(replaced).toEqual([]);
  });

  test("the local host is never a default worth writing into the URL", async () => {
    const { env, replaced } = bootEnv("http://127.0.0.1:47821/", LOCAL_HOST);
    const invoke = vi.fn(async () => hosts);
    expect(await applyLastHostDefault({ env, invoke })).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
    expect(replaced).toEqual([]);
  });

  test("an explicit host param wins over the cache", async () => {
    const { env, replaced } = bootEnv("http://127.0.0.1:47821/?host=r2", "r1");
    const invoke = vi.fn(async () => hosts);
    expect(await applyLastHostDefault({ env, invoke })).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
    expect(replaced).toEqual([]);
  });

  test("a failing or absent host list never blocks startup", async () => {
    const { env } = bootEnv("http://127.0.0.1:47821/", "r1");
    const failing = vi.fn(async () => {
      throw new Error("host_list unavailable");
    });
    expect(await applyLastHostDefault({ env, invoke: failing })).toBeNull();
    expect(await applyLastHostDefault({ env, invoke: null })).toBeNull();
    const unreadable = {
      ...env,
      localStorage: {
        getItem: () => {
          throw new Error("storage blocked");
        },
      },
    };
    expect(await applyLastHostDefault({ env: unreadable, invoke: failing })).toBeNull();
  });
});

describe("bridge grant delivery", () => {
  const GRANT = "bg1_abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";
  const WS = "ws://127.0.0.1:55123/v2/ws";
  const remoteEnv = () => ({ location: { href: "http://127.0.0.1:47821/?host=r1" } });
  const statusReply = (overrides = {}) => ({
    serverId: "r1",
    status: "online",
    wsUrl: WS,
    bridgeGrantVersion: 1,
    ...overrides,
  });

  async function connect(handler) {
    const invoke = vi.fn(async (command, args) => {
      if (command === "host_status") return statusReply();
      if (command === "host_grant") return handler(command, args);
      throw new Error(`unexpected command ${command}`);
    });
    const connection = await connectHost({ env: remoteEnv(), invoke });
    return { connection, invoke };
  }

  test("the bridge connection carries a command-backed grant provider", async () => {
    const { connection, invoke } = await connect(async () => ({
      bridgeGrantVersion: 1,
      bridgeGrant: GRANT,
      wsUrl: WS,
    }));
    expect(connection).toMatchObject({ host: "r1", identity: "bridge", wsUrl: WS });
    expect(connection.blocked).toBeUndefined();
    expect(typeof connection.clientOptions.bridgeGrantProvider).toBe("function");
    await expect(connection.clientOptions.bridgeGrantProvider()).resolves.toBe(GRANT);
    // Identity comes from the injected window; the command carries only the host.
    expect(invoke.mock.calls[1]).toEqual(["host_grant", { serverId: "r1" }]);
  });

  test("each call asks the app for a fresh grant", async () => {
    let issued = 0;
    const { connection } = await connect(async () => {
      issued += 1;
      return { bridgeGrantVersion: 1, bridgeGrant: GRANT, wsUrl: WS };
    });
    const { bridgeGrantProvider } = connection.clientOptions;
    await bridgeGrantProvider();
    await bridgeGrantProvider();
    expect(issued).toBe(2);
  });

  test.each([
    ["wrong version", { bridgeGrantVersion: 2 }],
    ["missing version", { bridgeGrantVersion: undefined }],
    ["another endpoint", { wsUrl: "ws://127.0.0.1:1/v2/ws" }],
    ["a missing grant", { bridgeGrant: undefined }],
    ["a malformed grant", { bridgeGrant: "not-a-grant" }],
    ["a grant with trailing data", { bridgeGrant: `${GRANT}X` }],
    ["a nonstring grant", { bridgeGrant: 42 }],
  ])("a reply with %s is a fixed nonsecret failure", async (_name, overrides) => {
    const { connection } = await connect(async () => ({
      bridgeGrantVersion: 1,
      bridgeGrant: GRANT,
      wsUrl: WS,
      ...overrides,
    }));
    await expect(connection.clientOptions.bridgeGrantProvider()).rejects.toThrow(
      "Host grant unavailable",
    );
  });

  test("a failing command reports a fixed error, never the raw IPC reply", async () => {
    const { connection } = await connect(async () => {
      throw new Error("Host grant unavailable: bg1_secretleak");
    });
    await expect(connection.clientOptions.bridgeGrantProvider()).rejects.toThrow(
      "Host grant unavailable",
    );
    await expect(connection.clientOptions.bridgeGrantProvider()).rejects.not.toThrow(
      /bg1_secretleak/,
    );
  });

  test("a bridge without grant version 1 is blocked, never dialed or downgraded", async () => {
    const { env, replaced, rewrites } = (() => {
      const replaced = [];
      const rewrites = [];
      return {
        replaced,
        rewrites,
        env: {
          location: {
            href: "http://127.0.0.1:47821/?host=r1",
            protocol: "http:",
            host: "127.0.0.1:47821",
            replace: (url) => replaced.push(url),
          },
          sessionStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
          history: { replaceState: (_state, _title, url) => rewrites.push(url) },
        },
      };
    })();
    const invoke = vi.fn(async () => ({
      serverId: "r1",
      status: "online",
      wsUrl: WS,
    }));
    const connection = await connectHost({ env, invoke });
    expect(connection).toEqual({ host: "r1", identity: "bridge", blocked: true });
    expect(connection.wsUrl).toBeUndefined();
    expect(connection.clientOptions).toBeUndefined();
    expect(replaced).toEqual([]);
    expect(rewrites).toEqual([]);
    expect(invoke.mock.calls).toEqual([["host_status", { serverId: "r1" }]]);
  });

  test("no grant ever reaches browser storage, history or the URL", async () => {
    const sentinel = `bg1_${"S".repeat(43)}`;
    const stored = [];
    const rewrites = [];
    const env = {
      location: { href: "http://127.0.0.1:47821/?host=r1" },
      localStorage: { getItem: () => null, setItem: (...args) => stored.push(args) },
      sessionStorage: {
        getItem: () => null,
        setItem: (...args) => stored.push(args),
        removeItem: () => {},
      },
      history: { replaceState: (...args) => rewrites.push(args) },
    };
    const invoke = vi.fn(async (command) => {
      if (command === "host_status")
        return { serverId: "r1", status: "online", wsUrl: WS, bridgeGrantVersion: 1 };
      return { bridgeGrantVersion: 1, bridgeGrant: sentinel, wsUrl: WS };
    });
    const connection = await connectHost({ env, invoke });
    await connection.clientOptions.bridgeGrantProvider();
    expect(JSON.stringify(connection)).not.toContain(sentinel);
    expect(JSON.stringify(stored)).not.toContain(sentinel);
    expect(JSON.stringify(rewrites)).not.toContain(sentinel);
  });
});
