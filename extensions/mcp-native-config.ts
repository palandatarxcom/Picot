// ABOUTME: Strict native MCP parity projection: validation, override classification and effective snapshot.
// ABOUTME: Mirrors upstream core/mcp-servers.ts + extensions/mcp/config.ts; never returns connection credentials.

export type McpExposure = "codemode" | "direct" | "deferred" | "hidden";

export type McpOverrideValues = {
  enabled: boolean;
  exposure: McpExposure;
  toolExposure: Record<string, McpExposure>;
};

export type McpIdentity = {
  scope: "global" | "project";
  source: string;
  /** Project mcp.json that overrides `source`; absent for full definitions. */
  override?: string;
};

export type NativeMcpServer = {
  name: string;
  /** Backend-only: never serialized into override detail. */
  config: Record<string, unknown>;
  identity: McpIdentity;
};

export type NativeProjection = {
  servers: Map<string, NativeMcpServer>;
  /** Per-name native diagnostics; the "" key carries a document-level problem. */
  errors: Map<string, string>;
};

export type ProjectEntryClassification = {
  kind: "definition" | "override" | "invalid";
  effective?: McpOverrideValues;
  identity?: McpIdentity;
  error?: string;
};

const MCP_EXPOSURES: readonly string[] = ["codemode", "deferred", "direct", "hidden"];
const MCP_EXPOSURE_ALIASES: Readonly<Record<string, McpExposure>> = {
  "codemode-deferred": "codemode",
};
const OVERRIDE_KEYS = ["enabled", "exposure", "toolExposure"];
const LOOPBACK_HOSTS = ["localhost", "127.0.0.1", "[::1]"];
const SERVER_NAME = /^[A-Za-z0-9_-]+$/;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isStringRecord(value: unknown): value is Record<string, string> {
  return isRecord(value) && Object.values(value).every((entry) => typeof entry === "string");
}

function isExposure(value: unknown): value is McpExposure {
  return typeof value === "string" && MCP_EXPOSURES.includes(value);
}

function resolveExposureAlias(value: string): string {
  return MCP_EXPOSURE_ALIASES[value] ?? value;
}

/** A copy of the entry with exposure aliases replaced by their current names. */
function resolveExposureAliases(value: Record<string, unknown>): Record<string, unknown> {
  const { exposure, toolExposure } = value;
  const resolved: Record<string, unknown> = { ...value };
  if (typeof exposure === "string") resolved.exposure = resolveExposureAlias(exposure);
  if (isRecord(toolExposure)) {
    resolved.toolExposure = Object.fromEntries(
      Object.entries(toolExposure).map(([tool, entry]) => [
        tool,
        typeof entry === "string" ? resolveExposureAlias(entry) : entry,
      ]),
    );
  }
  return resolved;
}

/** Namespace of a server's tools, like upstream `mcpNamespace`. */
export function mcpNamespace(name: string): string {
  return `mcp__${name.replace(/-/g, "_")}`;
}

/** Names that differ only in `-` and `_` share one tool namespace. */
function namespaceKey(name: string): string {
  return name.replace(/[-_]/g, "_");
}

function isLoopbackRedirectUri(value: string): boolean {
  if (!URL.canParse(value)) return false;
  const url = new URL(value);
  return (
    url.protocol === "http:" &&
    LOOPBACK_HOSTS.includes(url.hostname) &&
    url.search === "" &&
    url.hash === ""
  );
}

/** Upstream `validateOAuth`, message text included (minus the caller's name prefix). */
function validateOAuth(value: unknown): string | undefined {
  if (value === undefined) return undefined;
  if (!isRecord(value)) return "oauth must be an object";
  if (value.clientId !== undefined && typeof value.clientId !== "string") {
    return "oauth.clientId must be a string";
  }
  if (value.clientSecret !== undefined && typeof value.clientSecret !== "string") {
    return "oauth.clientSecret must be a string";
  }
  const port = value.callbackPort;
  if (
    port !== undefined &&
    (typeof port !== "number" || !Number.isInteger(port) || port < 1 || port > 65535)
  ) {
    return "oauth.callbackPort must be a port number";
  }
  if (value.callbackUrl !== undefined) {
    if (typeof value.callbackUrl !== "string" || !isLoopbackRedirectUri(value.callbackUrl)) {
      return "oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment";
    }
    const urlPort = new URL(value.callbackUrl).port;
    if (urlPort && port !== undefined && Number(urlPort) !== port) {
      return "oauth.callbackUrl and oauth.callbackPort name different ports";
    }
  }
  if (value.scope !== undefined && typeof value.scope !== "string")
    return "oauth.scope must be a string";
  if (
    value.clientName !== undefined &&
    (typeof value.clientName !== "string" || !value.clientName.trim())
  ) {
    return "oauth.clientName must be a non-empty string";
  }
  if (value.clientRegistration !== undefined && value.clientRegistration !== "dcr") {
    if (value.clientRegistration !== "cimd")
      return 'oauth.clientRegistration must be "dcr" or "cimd"';
    if (value.clientId !== undefined || value.clientName !== undefined) {
      return 'oauth.clientRegistration "cimd" cannot be combined with oauth.clientId or oauth.clientName';
    }
    const callback = typeof value.callbackUrl === "string" ? new URL(value.callbackUrl) : undefined;
    if (callback && (callback.hostname === "[::1]" || callback.pathname !== "/callback")) {
      return 'oauth.clientRegistration "cimd" requires oauth.callbackUrl on localhost or 127.0.0.1 with path /callback';
    }
  }
  const metadataUrl = value.authServerMetadataUrl;
  if (metadataUrl !== undefined) {
    const url =
      typeof metadataUrl === "string" && URL.canParse(metadataUrl)
        ? new URL(metadataUrl)
        : undefined;
    if (
      !url ||
      !(
        url.protocol === "https:" ||
        (url.protocol === "http:" && LOOPBACK_HOSTS.includes(url.hostname))
      )
    ) {
      return "oauth.authServerMetadataUrl must be an https URL, or http on localhost, 127.0.0.1, or [::1]";
    }
  }
  return undefined;
}

/**
 * Strict parse of a native `mcp.json`. Native Pi reads strict JSON (no comments)
 * and only the `mcpServers` key; a broken document throws instead of degrading
 * to an empty config.
 */
export function parseNativeMcpDocument(text: string): Record<string, unknown> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (error) {
    throw new Error(error instanceof Error ? error.message : String(error));
  }
  if (!isRecord(parsed)) throw new Error("MCP config must be an object");
  const servers = parsed.mcpServers;
  if (servers !== undefined && !isRecord(servers)) {
    throw new Error('expected an object with an "mcpServers" object');
  }
  return parsed;
}

/**
 * Upstream `validateMcpServerConfig` parity. Returns a copy of the entry with
 * exposure aliases resolved, or an error message. Project-only restrictions
 * (auth.provider) belong to `classifyProjectEntry`, which knows the scope.
 */
export function validateNativeMcpEntry(
  name: string,
  raw: unknown,
): Record<string, unknown> | string {
  if (!SERVER_NAME.test(name))
    return `invalid server name "${name}" (use letters, digits, "_" and "-")`;
  if (!isRecord(raw)) return `server "${name}" must be an object`;
  const value = resolveExposureAliases(raw);
  const { type, exposure, enabled, timeout, toolExposure, description } = value;
  const exposures = MCP_EXPOSURES.map((entry) => `"${entry}"`).join(", ");
  if (exposure !== undefined && !isExposure(exposure)) {
    return `server "${name}": exposure must be one of ${exposures}`;
  }
  if (toolExposure !== undefined) {
    if (!isRecord(toolExposure))
      return `server "${name}": toolExposure must map tool names to exposures`;
    for (const [tool, entry] of Object.entries(toolExposure)) {
      if (!isExposure(entry))
        return `server "${name}": toolExposure "${tool}" must be one of ${exposures}`;
    }
  }
  if (enabled !== undefined && typeof enabled !== "boolean")
    return `server "${name}": enabled must be a boolean`;
  if (description !== undefined && typeof description !== "string") {
    return `server "${name}": description must be a string`;
  }
  if (timeout !== undefined && (typeof timeout !== "number" || !(timeout > 0))) {
    return `server "${name}": timeout must be a positive number of seconds`;
  }
  if (type === "sse")
    return `server "${name}": legacy SSE transport is not supported; use the streamable HTTP URL`;
  if (
    typeof value.url === "string" &&
    (type === undefined || type === "http" || type === "streamable-http")
  ) {
    if (!URL.canParse(value.url) || !/^https?:$/.test(new URL(value.url).protocol)) {
      return `server "${name}": url must be an http or https URL`;
    }
    if (value.headers !== undefined && !isStringRecord(value.headers)) {
      return `server "${name}": headers must map names to strings`;
    }
    const oauthError = validateOAuth(value.oauth);
    if (oauthError) return `server "${name}": ${oauthError}`;
    if (value.auth !== undefined) {
      if (
        !isRecord(value.auth) ||
        typeof value.auth.provider !== "string" ||
        !value.auth.provider
      ) {
        return `server "${name}": auth.provider must be a provider name`;
      }
      const url = new URL(value.url);
      if (url.protocol !== "https:" && !LOOPBACK_HOSTS.includes(url.hostname)) {
        return `server "${name}": auth requires an https URL, or http on localhost, 127.0.0.1, or [::1]`;
      }
    }
    return value;
  }
  if (typeof value.command === "string" && (type === undefined || type === "stdio")) {
    if (
      value.args !== undefined &&
      !(Array.isArray(value.args) && value.args.every((arg) => typeof arg === "string"))
    ) {
      return `server "${name}": args must be an array of strings`;
    }
    if (value.env !== undefined && !isStringRecord(value.env))
      return `server "${name}": env must map names to strings`;
    if (value.cwd !== undefined && typeof value.cwd !== "string")
      return `server "${name}": cwd must be a string`;
    return value;
  }
  return `server "${name}" needs either "command" (stdio) or "url" (streamable HTTP)`;
}

/**
 * Global-file projection: valid servers keyed by exact name, in document order.
 * A second entry that only differs in `-`/`_` shares the first one's namespace
 * and is reported instead of applied, like Pi.
 */
export function projectGlobalServers(
  doc: Record<string, unknown>,
  source: string,
): NativeProjection {
  const servers = new Map<string, NativeMcpServer>();
  const errors = new Map<string, string>();
  if (doc.autoEnableCodemode !== undefined && typeof doc.autoEnableCodemode !== "boolean") {
    errors.set("", "autoEnableCodemode must be a boolean");
  }
  const raw = doc.mcpServers;
  for (const [name, value] of Object.entries(isRecord(raw) ? raw : {})) {
    const config = validateNativeMcpEntry(name, value);
    if (typeof config === "string") {
      errors.set(name, config);
      continue;
    }
    const clash = [...servers.keys()].find(
      (other) => other !== name && namespaceKey(other) === namespaceKey(name),
    );
    if (clash) {
      errors.set(name, `server "${name}" conflicts with "${clash}"`);
      continue;
    }
    servers.set(name, { name, config, identity: { scope: "global", source } });
  }
  return { servers, errors };
}

/** Whether an entry overrides a global server instead of defining one. */
export function isMcpOverride(raw: unknown): boolean {
  if (!isRecord(raw)) return false;
  return raw.command === undefined && raw.url === undefined && raw.type === undefined;
}

/**
 * Explicit three-field snapshot of an already validated entry. Defaults are
 * materialized so the project file never depends on global values changing.
 */
export function snapshotMcpOverride(config: Record<string, unknown>): McpOverrideValues {
  const exposure = config.exposure ?? "codemode";
  const map = config.toolExposure;
  return {
    enabled: config.enabled !== false,
    exposure: (typeof exposure === "string"
      ? (MCP_EXPOSURE_ALIASES[exposure] ?? exposure)
      : exposure) as McpExposure,
    toolExposure: { ...(isRecord(map) ? (map as Record<string, McpExposure>) : {}) },
  };
}

/**
 * Classify one project-file entry against the global projection.
 *
 * `definition` keeps the existing editor (native-ineligible definitions carry a
 * diagnostic instead of losing the editor); `override` has an effective
 * snapshot; `invalid` is a non-record entry or a rejected override candidate,
 * which the UI can only remove.
 */
export function classifyProjectEntry(
  name: string,
  raw: unknown,
  globals: NativeProjection,
  projectPath: string,
): ProjectEntryClassification {
  if (!isRecord(raw)) return { kind: "invalid", error: `server "${name}" must be an object` };

  if (isMcpOverride(raw)) {
    const base = globals.servers.get(name);
    if (!base) {
      return {
        kind: "invalid",
        error: `server "${name}" needs "command" or "url", or a global server to override`,
      };
    }
    const extra = Object.keys(raw).filter((key) => !OVERRIDE_KEYS.includes(key));
    if (extra.length > 0) {
      return {
        kind: "invalid",
        error: `server "${name}": an override can only set ${OVERRIDE_KEYS.join(", ")}`,
      };
    }
    const merged = validateNativeMcpEntry(name, { ...base.config, ...raw });
    if (typeof merged === "string") return { kind: "invalid", error: merged };
    return {
      kind: "override",
      effective: snapshotMcpOverride(merged),
      identity: { scope: "global", source: base.identity.source, override: projectPath },
    };
  }

  // Full definitions keep the existing editor: native policy violations are
  // diagnostics, not a reason to take the editor away.
  const config = validateNativeMcpEntry(name, raw);
  if (typeof config === "string") return { kind: "definition", error: config };
  if (isRecord(config) && config.url !== undefined && config.auth !== undefined) {
    return {
      kind: "definition",
      error: `server "${name}": auth is only allowed in the global mcp.json`,
    };
  }
  const clash = [...globals.servers.keys()].find(
    (other) => other !== name && namespaceKey(other) === namespaceKey(name),
  );
  if (clash) {
    return { kind: "definition", error: `server "${name}" conflicts with "${clash}"` };
  }
  return { kind: "definition" };
}
