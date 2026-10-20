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
  return capabilities;
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
  const url = new URL(location?.href ?? "http://localhost/");
  // A host switch starts at that host's landing page: the current path's
  // workspace/session belongs to the outgoing host.
  url.pathname = "/";
  url.hash = "";
  if (!hostId || hostId === LOCAL_HOST) url.searchParams.delete(HOST_PARAM);
  else url.searchParams.set(HOST_PARAM, hostId);
  return url.toString();
}

/**
 * Switch this window's host (contract 4): flush unsent input, release the
 * window's grant, then reload onto the host param. No in-page transaction —
 * every module closure, pending request, and projection dies with the page.
 * Returns the navigation URL, or null when the flush refused to switch.
 */
export function switchHost(
  hostId,
  { env = globalThis.window || globalThis, flush, release, navigate } = {},
) {
  try {
    flush?.();
  } catch {
    // A draft that could not be saved keeps the window on the current host.
    return null;
  }
  // Synchronous IPC: unload cleanup is unreliable, so this call is the main
  // release path (host_client host_release; window destroy is the safety net).
  try {
    void Promise.resolve(release?.()).catch(() => {});
  } catch {
    // A release failure must never strand the window on a dead host.
  }
  const url = hostUrl(hostId, env);
  const go = navigate || ((href) => locationOf(env).assign(href));
  go(url);
  return url;
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

function tauriInvoke(env = globalThis.window || globalThis) {
  const host = env || globalThis;
  const invoke = host.__TAURI__?.core?.invoke ?? globalThis.__TAURI__?.core?.invoke;
  if (typeof invoke !== "function") return null;
  return (command, args) => invoke(command, args);
}

/**
 * Boot an entry module's connection: local hosts resolve without any IPC, a
 * remote host resolves its bridge URL and window grant from the app commands.
 * Never falls back to the local socket for a remote host (contract 2).
 */
export async function connectHost({
  env = globalThis.window || globalThis,
  invoke = tauriInvoke(env),
} = {}) {
  const host = resolveHostFromUrl(env);
  if (host === LOCAL_HOST) {
    return {
      host: LOCAL_HOST,
      identity: "desktop",
      wsUrl: resolveWsUrlForHost(LOCAL_HOST, env),
      clientOptions: { capabilitiesFromAck: readCapabilities },
    };
  }
  if (!invoke) throw new Error("Remote host needs the app shell (Tauri IPC unavailable)");
  const status = await invoke("host_status", { serverId: host });
  const grant = await invoke("host_grant", { serverId: host });
  if (!grant?.grant) throw new Error("Host grant is unavailable");
  return {
    host,
    identity: "bridge",
    wsUrl: resolveWsUrlForHost({ wsUrl: status?.wsUrl, bridgeOrigin: status?.bridgeOrigin }, env),
    clientOptions: {
      identity: "bridge",
      grant: grant.grant,
      capabilitiesFromAck: readCapabilities,
    },
  };
}
