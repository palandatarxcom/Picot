// ABOUTME: Resolves the window's host from its URL, its ws URL and capabilities,
// ABOUTME: and owns the reload-based host switch (contract 4) and credential state.

import { resolveWebSocketUrl } from "./websocket-client.js";

/**
 * Per-window host connection layer (spec `2026-10-19-relay-phase2-5` contract
 * 2/4). The host lives on the window URL, not in shared state: window A
 * switching host navigates only window A, and a reload is the whole switch.
 *
 * Injected-only: no import side effects, no globals mutated at load.
 */

export const HOST_PARAM = "host";
export const LOCAL_HOST = "local";

/** Where the last explicit host choice is kept (contract 4 window default). */
export const LAST_HOST_KEY = "picot-last-host";

/** Faces a hello_ack may advertise (contract 1 / D2 action matrix). */
const FACES = ["native", "navigation", "git", "terminal", "management"];

/**
 * The only Remote terminal binding revision this client knows (plan
 * 2026-11-03 §1). A server that enforces the whole admission contract
 * advertises exactly this integer; anything else is unavailable.
 */
export const TERMINAL_BINDING_REVISION = 1;

/**
 * The only bridge grant revision this client knows (plan 2026-11-10 §1). A
 * bridge advertises it from `host_status`; anything else means the app build
 * and the bridge cannot authorize each other and the entry stays blocked.
 */
export const BRIDGE_GRANT_VERSION = 1;

/** `bg1_` plus 43 unpadded base64url characters: 32 random bytes. */
export const BRIDGE_GRANT_PATTERN = /^bg1_[A-Za-z0-9_-]{43}$/;

/** Fixed, nonsecret failure for every grant refusal. */
export const GRANT_UNAVAILABLE = "Host grant unavailable";

function locationOf(env) {
  return env?.location || globalThis.location;
}

function paramsOf(env) {
  const location = locationOf(env);
  try {
    const href = location?.href;
    if (typeof href === "string") return new URL(href).searchParams;
    return new URLSearchParams(location?.search || "");
  } catch {
    return new URLSearchParams("");
  }
}

/** The window's host: the `host` URL param, or the local host when absent. */
export function resolveHostFromUrl(env = globalThis.window || globalThis) {
  const value = paramsOf(env).get(HOST_PARAM)?.trim();
  return value ? value : LOCAL_HOST;
}

/**
 * Socket endpoint for a host. Local keeps today's origin rule byte for byte;
 * a remote host is reached through its app-owned loopback bridge, which speaks
 * the same `/v2/ws` path (`host_status` reports the bridge origin / wsUrl).
 */
export function resolveWsUrlForHost(host, env = globalThis.window || globalThis) {
  const local = host == null || host === LOCAL_HOST || host?.id === LOCAL_HOST;
  if (local) return resolveWebSocketUrl(env);
  const reported =
    host?.wsUrl || (host?.bridgeOrigin && `${host.bridgeOrigin.replace(/^http/, "ws")}/v2/ws`);
  if (typeof reported !== "string" || !reported.endsWith("/v2/ws")) {
    throw new Error("Host bridge WebSocket URL is unavailable");
  }
  return reported;
}

/**
 * Faces admitted for this connection. A host that advertises no capabilities
 * predates the field: keep the native surface such a host always gave this
 * client and deny the management face we cannot verify (R2-04 sane fallback).
 */
export function readCapabilities(ack) {
  const advertised = ack?.capabilities;
  const known = advertised && typeof advertised === "object";
  const face = (key) => (known ? advertised[key] === true : key !== "management");
  const native = face("native");
  const capabilities = { native, class: native ? "native" : "remote" };
  for (const name of FACES) if (name !== "native") capabilities[name] = face(name);
  // Non-boolean revision: kept verbatim so the terminal gate can require the
  // exact approved value. Absent, string, boolean, or unknown revisions all
  // collapse to 0 — no optimistic downgrade, and the legacy face defaults above
  // stay byte-identical for every other consumer.
  const revision = advertised?.terminalBindingVersion;
  capabilities.terminalBindingVersion =
    Number.isSafeInteger(revision) && revision > 0 ? revision : 0;
  return capabilities;
}

/**
 * Whether this connection may run Remote terminal commands at all. Both the
 * face and the approved revision are required on the *current* authenticated
 * connection: `terminal: true` alone, an old server, or a stale ack from a
 * previous connection never authorizes a terminal frame.
 */
export function supportsRemoteTerminal(capabilities) {
  return (
    capabilities?.terminal === true &&
    capabilities?.terminalBindingVersion === TERMINAL_BINDING_REVISION
  );
}

/**
 * Credential invalidity is only ever an explicit remote rejection (spec §2.5):
 * a disconnect, a timeout, or any other error keeps the stored token.
 */
export function isCredentialRejection(error) {
  return error?.code === "unauthorized_device";
}

/** Fires `onInvalid` when the socket reports an `unauthorized_device` error. */
export function watchCredentialInvalid(target, onInvalid) {
  const listener = (event) => {
    if (isCredentialRejection(event?.detail)) onInvalid?.(event.detail);
  };
  target.addEventListener("hostError", listener);
  return () => target.removeEventListener("hostError", listener);
}

/** This window's URL for a host: the host param is the whole selection. */
export function hostUrl(hostId, env = globalThis.window || globalThis) {
  const location = locationOf(env);
  let url;
  try {
    url = new URL(location?.href ?? "http://localhost/");
  } catch {
    // A page whose own href will not parse cannot be re-targeted; keep it.
    return location?.href ?? "";
  }
  // A host switch starts at that host's landing page: the current path's
  // workspace/session belongs to the outgoing host.
  url.pathname = "/";
  url.hash = "";
  if (!hostId || hostId === LOCAL_HOST) url.searchParams.delete(HOST_PARAM);
  else url.searchParams.set(HOST_PARAM, hostId);
  return url.toString();
}

/**
 * Switch this window's host (contract 4): flush unsent input, then reload onto
 * the host param. No in-page transaction — every module closure, pending
 * request, and projection dies with the page (and its socket with it, which
 * releases the window's bridge reference through the window-destroyed path).
 * Returns the navigation URL, or null when the flush refused to switch — a
 * throwing flush, or one that reports `false` (F-04: not durable).
 */
export async function switchHost(
  hostId,
  { env = globalThis.window || globalThis, flush, navigate } = {},
) {
  try {
    if ((await flush?.()) === false) return null;
  } catch {
    // A draft that could not be saved keeps the window on the current host.
    return null;
  }
  const url = hostUrl(hostId, env);
  const go = navigate || ((href) => locationOf(env).assign(href));
  go(url);
  return url;
}

/** Session flag marking a host whose recovery redirect already ran. */
export const HOST_RECOVERY_KEY = "picot:host-recovery";

const recoveryMark = (host) => `${HOST_RECOVERY_KEY}:${host}`;

/**
 * F-03 self-rescue: drop the dead `host` param and come home. The first
 * attempt is a real reload (clean module state); if the same host fails again
 * — or sessionStorage is unavailable — the URL is rewritten in place, so the
 * rescue path can never loop on itself.
 */
function recoverToLocal(host, env) {
  let href = null;
  try {
    const url = new URL(locationOf(env).href);
    url.searchParams.delete(HOST_PARAM);
    href = url.toString();
  } catch {
    return;
  }
  let recovered = true;
  try {
    recovered = Boolean(env?.sessionStorage?.getItem(recoveryMark(host)));
    env?.sessionStorage?.setItem(recoveryMark(host), "1");
  } catch {
    // No storage to remember the attempt: the in-place rewrite cannot loop.
  }
  const location = locationOf(env);
  if (!recovered && typeof location?.replace === "function") {
    location.replace(href);
    return;
  }
  try {
    (env?.history ?? globalThis.history).replaceState(null, "", href);
  } catch {
    // An unrewritable URL still boots local below, so the window keeps working.
  }
}

/**
 * Cold-start default (contract 4): a window opened at the landing route with
 * no host param opens the last host the user picked, when that host still
 * exists. The URL is rewritten in place — no reload — so every later reader
 * (the socket, the per-host cache keys) agrees with what this window shows.
 * A stale or unreadable default never blocks startup: any doubt stays local.
 */
export async function applyLastHostDefault({
  env = globalThis.window || globalThis,
  invoke = tauriInvoke(env),
} = {}) {
  if (paramsOf(env).get(HOST_PARAM)) return null;
  let saved = null;
  try {
    saved = env?.localStorage?.getItem(LAST_HOST_KEY)?.trim() || null;
  } catch {
    return null;
  }
  if (!saved || saved === LOCAL_HOST || typeof invoke !== "function") return null;
  let hosts = null;
  try {
    hosts = await invoke("host_list");
  } catch {
    return null;
  }
  // A deleted host must not reopen this window on a dead host.
  if (!Array.isArray(hosts) || !hosts.some((host) => host?.serverId === saved)) return null;
  try {
    const url = new URL(locationOf(env).href);
    url.searchParams.set(HOST_PARAM, saved);
    (env?.history ?? globalThis.history).replaceState(null, "", url.toString());
  } catch {
    return null;
  }
  return saved;
}

/**
 * Command-backed grant provider for one bridge connection (plan 2026-11-10,
 * contract 3). Each call is a fresh issuance — reconnects included — and the
 * command carries only the host id: identity comes from the injected window.
 * Every failure collapses to one fixed message so a raw IPC reply can never
 * reach a log line or the DOM.
 */
export function bridgeGrantProvider(host, { invoke, wsUrl }) {
  return async () => {
    let reply;
    try {
      reply = await invoke("host_grant", { serverId: host });
    } catch {
      throw new Error(GRANT_UNAVAILABLE);
    }
    if (reply?.bridgeGrantVersion !== BRIDGE_GRANT_VERSION) throw new Error(GRANT_UNAVAILABLE);
    // The grant must belong to the endpoint this attempt dials: a mismatched or
    // missing wsUrl is a stale bridge, never a reason to try another origin.
    if (reply?.wsUrl !== wsUrl) throw new Error(GRANT_UNAVAILABLE);
    const grant = reply?.bridgeGrant;
    if (typeof grant !== "string" || !BRIDGE_GRANT_PATTERN.test(grant)) {
      throw new Error(GRANT_UNAVAILABLE);
    }
    return grant;
  };
}

/**
 * Fixed full-page notice for a bridge that cannot authorize this window. No
 * grant, endpoint or IPC text is rendered: the page refuses to run a remote
 * host it cannot authenticate, and never falls back to the local socket.
 */
export function showBridgeUnsupportedNotice(env = globalThis.window || globalThis) {
  const doc = env?.document;
  const body = doc?.body;
  if (!body || typeof doc.createElement !== "function") return;
  const panel = doc.createElement("div");
  panel.setAttribute("role", "alert");
  panel.textContent =
    "This remote host cannot authorize this Picot window. Update or reload Picot, then reconnect the host.";
  body.replaceChildren(panel);
}

function tauriInvoke(env = globalThis.window || globalThis) {
  const host = env || globalThis;
  const invoke = host.__TAURI__?.core?.invoke ?? globalThis.__TAURI__?.core?.invoke;
  if (typeof invoke !== "function") return null;
  return (command, args) => invoke(command, args);
}

/**
 * Boot an entry module's connection: local hosts resolve without any IPC, a
 * remote host asks the app for its bridge URL. Never falls back to the local
 * socket for a remote host (contract 2).
 *
 * Both entry modules await this at module top level, so it must not throw: a
 * dead remote host (deleted, broken store, stopped bridge) hands the window
 * back to the local host instead of blanking the page (F-03).
 */
export async function connectHost({
  env = globalThis.window || globalThis,
  invoke = tauriInvoke(env),
} = {}) {
  const host = resolveHostFromUrl(env);
  const local = () => ({
    host: LOCAL_HOST,
    identity: "desktop",
    wsUrl: resolveWsUrlForHost(LOCAL_HOST, env),
    clientOptions: { capabilitiesFromAck: readCapabilities },
  });
  if (host === LOCAL_HOST) return local();
  try {
    if (!invoke) throw new Error("Remote host needs the app shell (Tauri IPC unavailable)");
    const status = await invoke("host_status", { serverId: host });
    // No grant revision: this bridge cannot authorize the window, so the entry
    // is blocked — no endpoint, no socket, and no downgrade to Origin-only.
    if (status?.bridgeGrantVersion !== BRIDGE_GRANT_VERSION) {
      return { host, identity: "bridge", blocked: true };
    }
    const wsUrl = resolveWsUrlForHost(
      { wsUrl: status?.wsUrl, bridgeOrigin: status?.bridgeOrigin },
      env,
    );
    const connection = {
      host,
      identity: "bridge",
      wsUrl,
      clientOptions: {
        identity: "bridge",
        capabilitiesFromAck: readCapabilities,
        // The page holds the secret for exactly one hello attempt.
        bridgeGrantProvider: bridgeGrantProvider(host, { invoke, wsUrl }),
      },
    };
    try {
      env?.sessionStorage?.removeItem(recoveryMark(host));
    } catch {
      // A stale recovery mark only costs one in-place rewrite on the next failure.
    }
    return connection;
  } catch (error) {
    console.warn(`[host] ${host} is unavailable, booting the local host:`, error);
    recoverToLocal(host, env);
    return local();
  }
}
