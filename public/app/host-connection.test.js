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
  switchHost,
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
    ).toBe("ws://127.0.0.1:55123/v2/ws");
  });

  test("a host_status wsUrl is used as reported", () => {
    expect(resolveWsUrlForHost({ id: "h1", wsUrl: "ws://127.0.0.1:55124/v2/ws" }, localEnv())).toBe(
      "ws://127.0.0.1:55124/v2/ws",
    );
  });

  test("remote without a usable bridge URL is an explicit failure, never local", () => {
    expect(() => resolveWsUrlForHost({ id: "h1" }, localEnv())).toThrow(/bridge/i);
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
  test("navigates to the host-parameterized URL after flush and release", () => {
    const calls = [];
    const url = switchHost("remote-1", {
      env: { location: { href: "http://127.0.0.1:47821/workspaces/w1" } },
      flush: () => calls.push("flush"),
      release: () => calls.push("release"),
      navigate: (href) => calls.push(`navigate:${href}`),
    });
    expect(calls).toEqual(["flush", "release", `navigate:${url}`]);
    expect(url).toBe("http://127.0.0.1:47821/?host=remote-1");
  });

  test("switching back to local drops the host param", () => {
    expect(hostUrl(LOCAL_HOST, { location: { href: "http://127.0.0.1:47821/?host=r1" } })).toBe(
      "http://127.0.0.1:47821/",
    );
  });

  test("a failed flush keeps the window on the current host", () => {
    const navigate = vi.fn();
    const release = vi.fn();
    expect(
      switchHost("remote-1", {
        env: { location: { href: "http://127.0.0.1:47821/" } },
        flush: () => {
          throw new Error("draft save failed");
        },
        release,
        navigate,
      }),
    ).toBeNull();
    expect(release).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });

  test("a failing release never blocks the switch", () => {
    const calls = [];
    const url = switchHost("remote-1", {
      env: { location: { href: "http://127.0.0.1:47821/" } },
      flush: () => calls.push("flush"),
      release: () => {
        calls.push("release");
        throw new Error("host_release unavailable");
      },
      navigate: (href) => calls.push(`navigate:${href}`),
    });
    expect(calls).toEqual(["flush", "release", `navigate:${url}`]);
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

  test("remote asks host_status and host_grant, then uses the bridge URL", async () => {
    const invoke = vi.fn(async (command) => {
      if (command === "host_status")
        return { serverId: "r1", status: "online", wsUrl: "ws://127.0.0.1:55123/v2/ws" };
      if (command === "host_grant") return { serverId: "r1", grant: "grant-token" };
      throw new Error(`unexpected command ${command}`);
    });
    const connection = await connectHost({
      env: { location: { href: "http://127.0.0.1:47821/?host=r1" } },
      invoke,
    });
    expect(invoke.mock.calls).toEqual([
      ["host_status", { serverId: "r1" }],
      ["host_grant", { serverId: "r1" }],
    ]);
    expect(connection).toEqual({
      host: "r1",
      identity: "bridge",
      wsUrl: "ws://127.0.0.1:55123/v2/ws",
      clientOptions: {
        identity: "bridge",
        grant: "grant-token",
        capabilitiesFromAck: readCapabilities,
      },
    });
  });

  test("remote without a grant fails instead of connecting unauthenticated", async () => {
    const invoke = vi.fn(async (command) => {
      if (command === "host_status") return { serverId: "r1", wsUrl: "ws://127.0.0.1:55123/v2/ws" };
      return { serverId: "r1" };
    });
    await expect(
      connectHost({ env: { location: { href: "http://127.0.0.1:47821/?host=r1" } }, invoke }),
    ).rejects.toThrow(/grant/i);
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
