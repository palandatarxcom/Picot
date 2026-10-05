// ABOUTME: MCP server inventory and mutation ops over Pi's two native config files.
// ABOUTME: Host-verified project root gates every project write; strict native parity drives override classification.

import { createHash, randomUUID } from "node:crypto";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import {
  classifyProjectEntry,
  isMcpOverride,
  type McpExposure,
  type McpIdentity,
  type McpOverrideValues,
  type NativeProjection,
  parseNativeMcpDocument,
  projectGlobalServers,
  snapshotMcpOverride,
  validateNativeMcpEntry,
} from "./mcp-native-config";
import { withSettingsLock } from "./skill-inventory";

export type McpWriteScope = "piGlobal" | "project";

/**
 * Host-verified project settings context. `projectRoot` comes from the host
 * launch marker (never from browser params); `null` means global-only (landing
 * or scratch runtime), not "use ctx.cwd".
 */
export type McpSettingsContext = {
  agentDir: string;
  projectRoot: string | null;
  projectTrusted: boolean;
  /**
   * Re-reads the saved trust decision from disk. Called inside the write lock:
   * a revocation that lands while a mutation waits for the lock must not be
   * written through.
   */
  verifyProject?: () => void;
};

/** SHA-256 of the exact file bytes; "missing" when the file is absent or unusable. */
export type McpRevision = string;

export interface McpListEntry {
  name: string;
  entry: Record<string, unknown>;
  /** Absolute path of the file this entry lives in. */
  sourceFile: string;
  /** Native files are pi-owned and editable through the existing forms. */
  editable: true;
  /** Effective enabled state for the row (override: base + override). */
  enabled: boolean;
  kind: "definition" | "override" | "invalid";
  /** Override rows only: effective three-field snapshot (no connection data). */
  effective?: McpOverrideValues;
  identity?: McpIdentity;
  /** Native eligibility diagnostic; definitions keep their editor regardless. */
  validationError?: string;
  revision: McpRevision;
}

export interface McpMigrationTarget {
  id: "adapterGlobal" | "adapterProject" | "sharedGlobal" | "sharedProject";
  sourceFile: string;
  /** Entry names present in the source but missing from the native target file. */
  missing: string[];
}

export type McpImportSkip = {
  name: string;
  reason: "existing" | "namespace-conflict" | "invalid-global";
  conflictWith?: string;
  detail?: string;
};

export type McpImportResult = {
  imported: string[];
  skipped: McpImportSkip[];
  changed: boolean;
  path: string;
  revision: McpRevision;
  runtimeReloadRequired: true;
};

export type McpMutationResult = {
  scope: McpWriteScope;
  name: string;
  path: string;
  revision: McpRevision;
  changed: boolean;
  runtimeReloadRequired: true;
};

/** Toggle additionally reports the resulting state the page should render. */
export type McpToggleResult = McpMutationResult & { enabled: boolean };

export type McpListResult = {
  groups: { piGlobal: McpListEntry[]; project: McpListEntry[] };
  groupErrors: { piGlobal?: string; project?: string };
  migrations: McpMigrationTarget[];
  /** True when the host verified a project root (the project tab exists). */
  projectAvailable: boolean;
  projectTrusted: boolean;
  revisions: { piGlobal: McpRevision; project: McpRevision | null };
};

interface McpLayerRead {
  doc: Record<string, unknown> | null;
  serverKey: string;
  error?: string;
  /** Non-ENOENT read failure: the file exists but could not be read. */
  readError?: string;
}

/** Native server names: letters, digits, `_`, `-` (Pi docs, configuration rules). */
const SERVER_NAME_RE = /^[A-Za-z0-9_-]+$/;
const EXPOSURE_VALUES = new Set(["direct", "codemode", "codemode-deferred", "deferred", "hidden"]);
const ENTRY_STRING_KEYS = new Set(["url", "cwd", "description"]);
const ENTRY_OBJECT_KEYS = new Set(["env", "headers", "toolExposure"]);
const ADAPTER_DROP_FIELDS = ["directTools", "inheritEnv", "lifecycle", "disabled"] as const;
const OVERRIDE_KEYS = ["enabled", "exposure", "toolExposure"] as const;

const SHARED_GLOBAL_FILENAMES = [
  path.join(".config", "mcp", "mcp.json"),
  path.join(".agents", "mcp.json"),
  path.join(".agents", "mcp", "mcp.json"),
] as const;

export const UNTRUSTED_PROJECT_ERROR =
  "This project is not trusted; project MCP settings are unavailable until you trust it";

function homeDir(): string {
  // Mirrors picot-config.ts's resolveHomeDir precedence: env override first,
  // passwd fallback second. Keeps temp-dir tests hermetic on macOS where
  // os.homedir() ignores process.env.HOME.
  const fromEnv = process.env.HOME ?? process.env.USERPROFILE;
  if (fromEnv?.trim()) return fromEnv;
  return os.homedir();
}

function errMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** JSONC tolerance shared with the adapter era: strips // and block comments
 * plus trailing commas, string-aware so `//` inside a value survives. */
export function stripJsonComments(raw: string): string {
  let out = "";
  let i = 0;
  let inString = false;
  while (i < raw.length) {
    const ch = raw[i];
    const next = raw[i + 1];
    if (inString) {
      out += ch;
      if (ch === "\\") {
        if (i + 1 < raw.length) out += raw[i + 1];
        i += 2;
        continue;
      }
      if (ch === '"') inString = false;
      i += 1;
      continue;
    }
    if (ch === '"') {
      inString = true;
      out += ch;
      i += 1;
      continue;
    }
    if (ch === "/" && next === "/") {
      while (i < raw.length && raw[i] !== "\n") i += 1;
      continue;
    }
    if (ch === "/" && next === "*") {
      i += 2;
      while (i < raw.length && !(raw[i] === "*" && raw[i + 1] === "/")) i += 1;
      i += 2;
      continue;
    }
    out += ch;
    i += 1;
  }
  return out.replace(/,(\s*[}\]])/g, "$1");
}

/** Legacy-tolerant parse used for the existing editor contract (JSONC, both key spellings). */
function parseLegacyDocument(raw: string): McpLayerRead {
  try {
    const parsed: unknown = JSON.parse(stripJsonComments(raw));
    // A top-level array/scalar is a broken document, never an empty config:
    // treating it as one would let a later write report success without
    // storing anything.
    if (!isRecord(parsed)) {
      return { doc: null, serverKey: "mcpServers", error: "MCP config must be an object" };
    }
    const serverKey =
      parsed.mcpServers !== undefined
        ? "mcpServers"
        : parsed["mcp-servers"] !== undefined
          ? "mcp-servers"
          : "mcpServers";
    const servers = parsed[serverKey];
    if (
      servers !== undefined &&
      (typeof servers !== "object" || servers === null || Array.isArray(servers))
    ) {
      return { doc: parsed, serverKey, error: `${serverKey} is not an object` };
    }
    return { doc: parsed, serverKey };
  } catch (error) {
    return { doc: null, serverKey: "mcpServers", error: errMessage(error) };
  }
}

function readLegacyFile(filePath: string): McpLayerRead {
  let raw: string;
  try {
    raw = fs.readFileSync(filePath, "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") {
      return { doc: null, serverKey: "mcpServers" }; // ENOENT: layer absent.
    }
    // Unreadable is not absent: a caller treating this as "no entries" would
    // report servers as missing (or already present) out of silence.
    return { doc: null, serverKey: "mcpServers", readError: errMessage(error) };
  }
  return parseLegacyDocument(raw);
}

function hashBytes(bytes: string): McpRevision {
  return createHash("sha256").update(bytes, "utf8").digest("hex");
}

/** Strict read for mutations: only ENOENT means "absent", every other I/O failure throws. */
function readStrictFile(filePath: string): { text: string | null; revision: McpRevision } {
  try {
    const text = fs.readFileSync(filePath, "utf8");
    return { text, revision: hashBytes(text) };
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT")
      return { text: null, revision: "missing" };
    throw new Error(`Cannot read MCP config ${filePath}: ${errMessage(error)}`);
  }
}

/** Unique private temp name (pid + uuid), 0600, tmp+rename; the temp file never survives. */
function writeAtomic(filePath: string, bytes: string): void {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  const tmp = path.join(
    path.dirname(filePath),
    `.${path.basename(filePath)}.${process.pid}.${randomUUID()}.tmp`,
  );
  try {
    fs.writeFileSync(tmp, bytes, { encoding: "utf8", mode: 0o600 });
    fs.renameSync(tmp, filePath);
  } finally {
    fs.rmSync(tmp, { force: true });
  }
}

function serializeDocument(doc: Record<string, unknown>): string {
  return `${JSON.stringify(doc, null, 2)}\n`;
}

function userPath(agentDir: string): string {
  return path.join(agentDir, "mcp.json");
}

function nameKey(name: string): string {
  return name.replace(/[-_]/g, "_");
}

/** Reject any target that is not strictly inside the project root. */
function assertInsideRoot(root: string, target: string): void {
  const rel = path.relative(root, target);
  if (!rel || rel.startsWith(`..${path.sep}`) || rel === ".." || path.isAbsolute(rel)) {
    throw new Error("MCP project target escapes the project root");
  }
}

function lstatOrNull(target: string): fs.Stats | null {
  try {
    return fs.lstatSync(target);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
    throw new Error(`Cannot inspect MCP path ${target}: ${errMessage(error)}`);
  }
}

/**
 * The admitted root is the host-verified canonical path (the Rust launch marker
 * resolved through `realpath`). Re-check it on every access: if the directory
 * was renamed away, replaced by a symlink, or reached through a swapped
 * ancestor, `.pi` below would silently address a directory outside the project
 * the host authorized. Runs before any lock, so it also covers the in-lock
 * re-check of a mutation that waited.
 */
function assertRootIdentity(root: string): void {
  let canonical: string;
  try {
    canonical = fs.realpathSync(root);
  } catch (error) {
    throw new Error(`MCP project root is not accessible: ${errMessage(error)}`);
  }
  if (canonical !== root) {
    throw new Error("MCP project root changed since it was admitted");
  }
}

/**
 * Project write/read admission. The root is host-verified and must be
 * absolute; `.pi` and `mcp.json` may not be symlinks, so a link cannot move
 * the target outside the project.
 */
function projectTarget(context: McpSettingsContext): { root: string; file: string } {
  const root = context.projectRoot;
  if (!root) throw new Error("No active project for project-scoped MCP writes");
  if (!context.projectTrusted) throw new Error(UNTRUSTED_PROJECT_ERROR);
  if (!path.isAbsolute(root)) throw new Error("MCP project root must be an absolute path");
  assertRootIdentity(root);
  const dir = path.join(root, ".pi");
  const file = path.join(dir, "mcp.json");
  assertInsideRoot(root, dir);
  assertInsideRoot(root, file);
  const dirStat = lstatOrNull(dir);
  if (dirStat?.isSymbolicLink()) throw new Error("MCP project directory .pi must not be a symlink");
  if (dirStat && !dirStat.isDirectory()) throw new Error("MCP project path .pi is not a directory");
  const fileStat = lstatOrNull(file);
  if (fileStat?.isSymbolicLink()) throw new Error("MCP project file must not be a symlink");
  if (fileStat && !fileStat.isFile()) throw new Error("MCP project file is not a regular file");
  return { root, file };
}

function targetFor(scope: McpWriteScope, context: McpSettingsContext): string {
  return scope === "project" ? projectTarget(context).file : userPath(context.agentDir);
}

/**
 * True when the project root and its `.pi` layout still pass the same path
 * rules as a project write. Read-side callers (inventory, migration scanning)
 * use it so a rejected path is never read through.
 */
function projectPathAdmissible(context: McpSettingsContext): boolean {
  if (!context.projectRoot || !context.projectTrusted) return false;
  try {
    projectTarget(context);
    return true;
  } catch {
    return false;
  }
}

function serversOf(layer: McpLayerRead): Record<string, Record<string, unknown>> {
  if (!layer.doc) return {};
  const servers = layer.doc[layer.serverKey];
  return isRecord(servers) ? (servers as Record<string, Record<string, unknown>>) : {};
}

/** Migration sources, in stable order. Each is read-only forever: the
 * migration is a user-confirmed copy of missing entries, never a move. */
function migrationSources(
  context: McpSettingsContext,
): { id: McpMigrationTarget["id"]; files: string[]; target: "user" | "project" }[] {
  const sources: { id: McpMigrationTarget["id"]; files: string[]; target: "user" | "project" }[] = [
    {
      id: "adapterGlobal",
      files: [path.join(context.agentDir, "mcp-adapter.json")],
      target: "user",
    },
  ];
  // Project migration is project I/O: it needs the same trust and path gate as
  // CRUD — a rejected `.pi` must not be scanned for migration candidates.
  if (projectPathAdmissible(context)) {
    sources.push({
      id: "adapterProject",
      files: [path.join(context.projectRoot, ".pi", "mcp-adapter.json")],
      target: "project",
    });
    sources.push({
      id: "sharedProject",
      files: [path.join(context.projectRoot, ".mcp.json")],
      target: "project",
    });
  }
  sources.push({
    id: "sharedGlobal",
    files: SHARED_GLOBAL_FILENAMES.map((rel) => path.join(homeDir(), rel)),
    target: "user",
  });
  return sources;
}

/** Merge the source files' entries later-wins, like the adapter used to. */
function mergeFiles(files: string[]): {
  merged: Map<string, Record<string, unknown>>;
  error?: string;
  readError?: string;
} {
  const merged = new Map<string, Record<string, unknown>>();
  let error: string | undefined;
  let readError: string | undefined;
  for (const file of files) {
    const layer = readLegacyFile(file);
    if (layer.error) error ??= layer.error;
    if (layer.readError) readError ??= layer.readError;
    for (const [name, entry] of Object.entries(serversOf(layer))) merged.set(name, entry);
  }
  return { merged, error, readError };
}

/** Global file projection from strict native bytes; throws when the file is unusable. */
function readGlobalProjection(agentDir: string): {
  file: string;
  revision: McpRevision;
  projection: NativeProjection;
} {
  const file = userPath(agentDir);
  const state = readStrictFile(file);
  if (state.text === null) {
    return {
      file,
      revision: state.revision,
      projection: { servers: new Map(), errors: new Map() },
    };
  }
  let doc: Record<string, unknown>;
  try {
    doc = parseNativeMcpDocument(state.text);
  } catch (error) {
    throw new Error(`Global MCP config is unreadable: ${errMessage(error)}`);
  }
  return { file, revision: state.revision, projection: projectGlobalServers(doc, file) };
}

function validateServerName(name: unknown, existingNames: string[]): string {
  if (typeof name !== "string" || !SERVER_NAME_RE.test(name)) {
    throw new Error("Server name may contain only letters, digits, _ and -");
  }
  const key = nameKey(name);
  const collision = existingNames.find(
    (existing) => existing !== name && nameKey(existing) === key,
  );
  if (collision) {
    throw new Error(
      `Server names that differ only in - and _ count as the same server (conflicts with ${collision})`,
    );
  }
  return name;
}

function asStringMap(
  value: Record<string, unknown>,
  key: string,
  validateValues?: (v: string) => void,
) {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(value)) {
    if (typeof v !== "string") throw new Error(`${key}.${k} must be a string`);
    validateValues?.(v);
    out[k] = v;
  }
  return out;
}

/**
 * Keeps only native-known entry fields from `entry`, preserving unknown keys
 * already present in `existing` (Pi may grow fields Picot doesn't know).
 */
function normalizeEntry(
  entry: unknown,
  existing: Record<string, unknown> | undefined,
): Record<string, unknown> {
  if (!isRecord(entry)) {
    throw new Error("Server entry must be an object");
  }
  const source = entry;
  const merged: Record<string, unknown> = { ...existing };

  // Transport: mutually exclusive — setting one clears the other.
  if (source.command === undefined) {
    // absent: keep existing (form omits unchanged fields)
  } else if (typeof source.command === "string" && source.command.trim()) {
    merged.command = source.command.trim();
  } else if (Array.isArray(source.command) && source.command.length > 0) {
    merged.command = source.command;
  } else if (source.command === null || source.command === "") {
    delete merged.command;
  } else {
    // Exposed RPC boundary: a non-string/non-array command must error,
    // not silently fall through and keep the previous value.
    throw new Error("command must be a string or an array of strings");
  }
  if (source.url !== undefined) {
    if (source.url === null || source.url === "") {
      delete merged.url;
      delete merged.headers;
    } else if (typeof source.url === "string" && source.url.trim()) {
      merged.url = source.url.trim();
      delete merged.command;
      delete merged.args;
    } else {
      throw new Error("url must be a non-empty string");
    }
  }

  if (source.args === null) delete merged.args;
  else if (source.args !== undefined) {
    if (!Array.isArray(source.args) || !source.args.every((a) => typeof a === "string")) {
      throw new Error("args must be an array of strings");
    }
    merged.args = source.args;
  }

  for (const key of ENTRY_STRING_KEYS) {
    if (source[key] === null || source[key] === "") {
      delete merged[key];
      continue;
    }
    if (source[key] === undefined) continue;
    if (typeof source[key] !== "string") throw new Error(`${key} must be a string`);
    merged[key] = source[key];
  }
  for (const key of ENTRY_OBJECT_KEYS) {
    if (source[key] === null) {
      delete merged[key];
      continue;
    }
    if (source[key] === undefined) continue;
    const value = source[key];
    if (!isRecord(value)) {
      throw new Error(`${key} must be an object`);
    }
    if (key === "toolExposure") {
      merged[key] = asStringMap(value, key, (v) => {
        if (!EXPOSURE_VALUES.has(v))
          throw new Error(`toolExposure value must be an exposure: ${v}`);
      });
    } else {
      merged[key] = asStringMap(value, key);
    }
  }

  if (source.exposure !== undefined && source.exposure !== null) {
    if (typeof source.exposure !== "string" || !EXPOSURE_VALUES.has(source.exposure)) {
      throw new Error(`exposure must be one of ${[...EXPOSURE_VALUES].join(", ")}`);
    }
    merged.exposure = source.exposure;
  } else if (source.exposure === null) {
    delete merged.exposure;
  }

  if (source.timeout !== undefined && source.timeout !== null) {
    if (typeof source.timeout !== "number" || !(source.timeout > 0)) {
      throw new Error("timeout must be a positive number of seconds");
    }
    merged.timeout = source.timeout;
  } else if (source.timeout === null) {
    delete merged.timeout;
  }

  if (source.enabled !== undefined && source.enabled !== null) {
    if (typeof source.enabled !== "boolean") throw new Error("enabled must be a boolean");
    merged.enabled = source.enabled;
  } else if (source.enabled === null) {
    delete merged.enabled;
  }

  // Final transport arbitration: whichever transport is present wins and the
  // other transport's fields are dropped — regardless of the order the form
  // sent them in.
  if (merged.url !== undefined) {
    delete merged.command;
    delete merged.args;
  } else if (merged.command !== undefined) {
    delete merged.url;
    delete merged.headers;
  }
  if (merged.command === undefined && merged.url === undefined) {
    throw new Error("Server entry needs a command (stdio) or a url (remote)");
  }
  return merged;
}

/** Explicit three-field override payload; every key must be present and valid. */
function normalizeOverrideEntry(name: string, entry: unknown): McpOverrideValues {
  if (!isRecord(entry)) throw new Error("Server entry must be an object");
  const extra = Object.keys(entry).filter((key) => !OVERRIDE_KEYS.includes(key as never));
  if (extra.length > 0) {
    throw new Error(`server "${name}": an override can only set ${OVERRIDE_KEYS.join(", ")}`);
  }
  if (OVERRIDE_KEYS.some((key) => !(key in entry))) {
    throw new Error(
      `server "${name}": an override must set ${OVERRIDE_KEYS.join(", ")} explicitly`,
    );
  }
  const validated = validateNativeMcpEntry(name, {
    command: "picot-override-probe",
    args: [],
    ...entry,
  });
  if (typeof validated === "string") throw new Error(validated);
  const snapshot = snapshotMcpOverride(validated);
  return {
    enabled: snapshot.enabled,
    exposure: snapshot.exposure,
    toolExposure: snapshot.toolExposure,
  };
}

/** Every mutation carries the revision the page listed; a missing one is refused. */
function assertExpectedRevision(expected: unknown, current: McpRevision): void {
  if (typeof expected !== "string" || expected.length === 0) {
    throw new Error("MCP mutations require the file revision reported by the list");
  }
  if (expected !== current) {
    throw new Error("MCP config changed on disk since it was loaded; reload before saving");
  }
}

/** Overrides replace a global server's three fields, so the global revision matters too. */
function assertExpectedGlobalRevision(expected: unknown, current: McpRevision): void {
  if (typeof expected !== "string" || expected.length === 0) {
    throw new Error("MCP overrides require the global revision reported by the list");
  }
  if (expected !== current) {
    throw new Error("Global MCP config changed since the page was loaded; reload before saving");
  }
}

/** kind and intent are part of the save contract, not optional hints. */
function parseKind(value: unknown): "definition" | "override" {
  if (value === "definition" || value === "override") return value;
  throw new Error("kind must be definition or override");
}

function parseIntent(value: unknown): "create" | "edit" {
  if (value === "create" || value === "edit") return value;
  throw new Error("intent must be create or edit");
}

function parseWriteScope(value: unknown): McpWriteScope {
  if (value === "piGlobal" || value === "project") return value;
  throw new Error("scope must be piGlobal or project");
}

function requireName(value: unknown): string {
  if (typeof value !== "string" || !SERVER_NAME_RE.test(value)) {
    throw new Error("Server name may contain only letters, digits, _ and -");
  }
  return value;
}

/**
 * Inventory across the two native files. Entries are listed with the legacy
 * tolerance the editors rely on, while `kind`/`effective`/`validationError`
 * come from strict native parity so a legacy-only entry is reported instead of
 * being presented as an active Pi server.
 */
export function listMcpServers(context: McpSettingsContext): McpListResult {
  const globalFile = userPath(context.agentDir);
  const globalState = documentState(globalFile);
  const globals: NativeProjection = globalState.native
    ? projectGlobalServers(globalState.native, globalFile)
    : { servers: new Map(), errors: new Map() };
  const globalDiagnostic = globalState.strictError ?? globalState.layer.error;

  const projectAvailable = context.projectRoot !== null;
  const projectTrusted = projectAvailable && context.projectTrusted;
  const projectFile = context.projectRoot
    ? path.join(context.projectRoot, ".pi", "mcp.json")
    : null;
  let projectGroup: McpListEntry[] = [];
  let projectError: string | undefined;
  let projectRevision: McpRevision | null = null;

  if (projectAvailable && !projectTrusted) {
    // Untrusted project contents are never read; the tab still exists so the
    // page can explain the trust requirement.
    projectError = UNTRUSTED_PROJECT_ERROR;
  } else if (projectFile) {
    // Reads obey the same project-path rules as writes: a symlinked `.pi` or
    // mcp.json must not make the page list a file outside the project.
    let pathError: string | undefined;
    try {
      projectTarget(context);
    } catch (error) {
      pathError = errMessage(error);
    }
    const state = pathError
      ? {
          layer: { doc: null, serverKey: "mcpServers" },
          native: null,
          revision: "missing" as McpRevision,
        }
      : documentState(projectFile);
    projectRevision = pathError ? null : state.revision;
    projectError = pathError ?? state.strictError ?? state.layer.error;
    projectGroup = Object.entries(serversOf(state.layer)).map(([name, raw]) =>
      projectEntry(
        name,
        raw,
        projectFile,
        state.revision,
        globals,
        state.native,
        state.strictError,
      ),
    );
  }

  return {
    groups: {
      piGlobal: Object.entries(serversOf(globalState.layer)).map(([name, raw]) =>
        globalEntry(name, raw, globalFile, globalState.revision, globalDiagnostic),
      ),
      project: projectGroup,
    },
    groupErrors: { piGlobal: globalDiagnostic, project: projectError },
    migrations: buildMigrations(context),
    projectAvailable,
    projectTrusted,
    revisions: {
      piGlobal: globalState.revision,
      project: projectAvailable ? projectRevision : null,
    },
  };
}

type DocumentState = {
  /** Legacy-tolerant document (JSONC/both key spellings) for the existing editors. */
  layer: McpLayerRead;
  /** Strict native document, or null when the bytes are absent / not strict JSON. */
  native: Record<string, unknown> | null;
  /** Native diagnostic for the whole file: broken JSON or JSONC-only syntax. */
  strictError?: string;
  /** SHA-256 of the bytes while they describe a config Picture can hand back to a save. */
  revision: McpRevision;
};

function documentState(filePath: string): DocumentState {
  let read: { text: string | null; revision: McpRevision };
  try {
    read = readStrictFile(filePath);
  } catch (error) {
    // Unreadable (EACCES/…) is not an empty config: report and refuse writes.
    return {
      layer: { doc: null, serverKey: "mcpServers" },
      native: null,
      strictError: errMessage(error),
      revision: "missing",
    };
  }
  if (read.text === null) {
    return { layer: { doc: null, serverKey: "mcpServers" }, native: null, revision: "missing" };
  }
  const layer = parseLegacyDocument(read.text);
  let native: Record<string, unknown> | null = null;
  let strictError: string | undefined;
  try {
    native = parseNativeMcpDocument(read.text);
  } catch (error) {
    strictError = errMessage(error);
  }
  // A revision is meaningful only while the file is a usable config: callers
  // send it back as the expected revision and a broken file refuses writes.
  const revision = layer.doc ? read.revision : "missing";
  if (!layer.doc || layer.error) {
    return {
      layer: { doc: null, serverKey: layer.serverKey, error: layer.error },
      native,
      strictError,
      revision,
    };
  }
  return { layer, native, strictError, revision };
}

function globalEntry(
  name: string,
  raw: Record<string, unknown>,
  filePath: string,
  revision: McpRevision,
  strictError: string | undefined,
): McpListEntry {
  const base = {
    name,
    entry: raw,
    sourceFile: filePath,
    editable: true as const,
    revision,
  };
  if (!isRecord(raw)) {
    // Never read fields off a non-record entry: a null/array/number entry is a
    // bad row to report, not a crash for the whole inventory.
    return {
      ...base,
      enabled: true,
      kind: "invalid",
      validationError: `server "${name}" must be an object`,
    };
  }
  // Native eligibility is a diagnostic only: the full-definition editor keeps
  // its existing command-array/JSONC compatibility contract.
  const native = strictError ?? asError(validateNativeMcpEntry(name, raw));
  return {
    ...base,
    enabled: raw.enabled !== false,
    kind: "definition",
    ...(native ? { validationError: native } : {}),
  };
}

function projectEntry(
  name: string,
  raw: Record<string, unknown>,
  filePath: string,
  revision: McpRevision,
  globals: NativeProjection,
  nativeDoc: Record<string, unknown> | null,
  strictError: string | undefined,
): McpListEntry {
  const base = {
    name,
    entry: raw,
    sourceFile: filePath,
    editable: true as const,
    revision,
  };
  if (!isRecord(raw)) {
    return {
      ...base,
      enabled: true,
      kind: "invalid",
      validationError: `server "${name}" must be an object`,
    };
  }
  if (!nativeDoc) {
    // Legacy-only (JSONC/key variant) or broken project file: report the
    // compatibility gap instead of pretending native Pi reads these entries.
    const error = strictError ?? "project MCP config is unreadable";
    const kind = isRecord(raw) && !isMcpOverride(raw) ? "definition" : "invalid";
    return { ...base, kind, validationError: error };
  }
  const classified = classifyProjectEntry(name, raw, globals, filePath);
  return {
    ...base,
    kind: classified.kind,
    ...(classified.effective ? { effective: classified.effective } : {}),
    ...(classified.identity ? { identity: classified.identity } : {}),
    ...(classified.error ? { validationError: classified.error } : {}),
    enabled: classified.effective ? classified.effective.enabled : raw.enabled !== false,
  };
}

/**
 * A project definition shares Pi's tool namespace with the global layer: a
 * different name that differs only in `-`/`_` collides and Pi would reject it,
 * so the save is refused instead of writing a config that never activates.
 */
function assertNoGlobalNamespaceClash(context: McpSettingsContext, name: string): void {
  const global = readGlobalProjection(context.agentDir);
  const clash = [...global.projection.servers.keys()].find(
    (other) => other !== name && nameKey(other) === nameKey(name),
  );
  if (clash) {
    throw new Error(
      `Server names that differ only in - and _ count as the same server (conflicts with global ${clash})`,
    );
  }
}

function asError(value: Record<string, unknown> | string): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function buildMigrations(context: McpSettingsContext): McpMigrationTarget[] {
  const migrations: McpMigrationTarget[] = [];
  for (const source of migrationSources(context)) {
    const targetLayer =
      source.target === "user"
        ? readLegacyFile(userPath(context.agentDir))
        : context.projectRoot
          ? readLegacyFile(path.join(context.projectRoot, ".pi", "mcp.json"))
          : null;
    if (!targetLayer) continue;
    // An unreadable target or source is not an empty one: what is missing
    // cannot be told apart from a failed read, so offer no migration at all.
    if (targetLayer.readError) continue;
    const existing = new Set(Object.keys(serversOf(targetLayer)));
    const { merged, readError } = mergeFiles(source.files);
    if (readError) continue;
    const missing = [...merged.keys()].filter((name) => !existing.has(name));
    if (missing.length > 0) {
      const sourceFile = source.files.find((f) => fs.existsSync(f)) ?? source.files[0];
      migrations.push({ id: source.id, sourceFile, missing });
    }
  }
  return migrations;
}

/**
 * Every same-target write takes the settings lock on the *project* target and
 * then re-verifies, inside the lock, that the root/path layout is still the one
 * that was admitted and that trust has not been withdrawn while waiting. A
 * revocation or a `.pi`/`mcp.json` symlink swap during the wait therefore
 * rejects instead of writing through.
 */
function writeForScope<T>(
  scope: McpWriteScope,
  context: McpSettingsContext,
  critical: (filePath: string) => T,
): Promise<T> {
  const filePath = targetFor(scope, context);
  return withSettingsLock(filePath, () => {
    if (scope === "project") assertProjectAccess(context, filePath);
    return critical(filePath);
  });
}

/** In-lock project re-check: same layout, still trusted. */
function assertProjectAccess(context: McpSettingsContext, expectedFile: string): void {
  const rechecked = projectTarget(context);
  if (rechecked.file !== expectedFile) {
    throw new Error("MCP project target changed while waiting for the lock");
  }
  context.verifyProject?.();
}

/** Reads the target document for editing; a broken file is refused, never replaced. */
function readTargetDocument(filePath: string): {
  text: string | null;
  revision: McpRevision;
  layer: McpLayerRead;
} {
  const state = readStrictFile(filePath);
  if (state.text === null) {
    return { ...state, layer: { doc: {}, serverKey: "mcpServers" } };
  }
  const layer = parseLegacyDocument(state.text);
  if (!layer.doc || layer.error) {
    throw new Error(
      `Existing MCP config ${filePath} is not readable: ${layer.error ?? "invalid JSON"}`,
    );
  }
  return { ...state, layer };
}

export async function saveMcpServer(
  params: Record<string, unknown>,
  context: McpSettingsContext,
): Promise<McpMutationResult> {
  const scope = parseWriteScope(params.scope);
  const kind = parseKind(params.kind);
  const intent = parseIntent(params.intent);
  const name = requireName(params.name);

  return writeForScope(scope, context, (filePath) => {
    const { revision: currentRevision, layer } = readTargetDocument(filePath);
    assertExpectedRevision(params.expectedRevision, currentRevision);
    const servers = serversOf(layer);
    const existingRaw = servers[name];
    const existing = isRecord(existingRaw) ? existingRaw : undefined;

    let next: Record<string, unknown>;
    if (kind === "override") {
      if (scope !== "project") throw new Error("Override saves are project-only");
      if (existingRaw !== undefined && !isMcpOverride(existingRaw)) {
        throw new Error(`Project entry "${name}" is a full definition, not an override`);
      }
      // Existence must match the stated intent, exactly like a definition.
      if (intent === "create" && existingRaw !== undefined) {
        throw new Error(`MCP server "${name}" already exists`);
      }
      if (intent === "edit" && existingRaw === undefined) {
        throw new Error(`Unknown MCP server "${name}"`);
      }
      const entry = normalizeOverrideEntry(name, params.entry);
      validateServerName(name, Object.keys(servers));
      const global = readGlobalProjection(context.agentDir);
      assertExpectedGlobalRevision(params.expectedGlobalRevision, global.revision);
      const base = global.projection.servers.get(name);
      if (!base) {
        throw new Error(`No global MCP server "${name}" to override`);
      }
      // The override only makes sense if the merged server is valid for Pi.
      const merged = validateNativeMcpEntry(name, { ...base.config, ...entry });
      if (typeof merged === "string") throw new Error(merged);
      next = { ...entry };
    } else {
      if (intent === "create" && existingRaw !== undefined) {
        throw new Error(`MCP server "${name}" already exists`);
      }
      if (intent === "edit" && existingRaw === undefined) {
        throw new Error(`Unknown MCP server "${name}"`);
      }
      if (intent === "edit" && existingRaw !== undefined && isMcpOverride(existingRaw)) {
        throw new Error(`Project entry "${name}" is an override, not a full definition`);
      }
      validateServerName(name, Object.keys(servers));
      if (scope === "project") assertNoGlobalNamespaceClash(context, name);
      next = normalizeEntry(params.entry, existing);
      // Project files must not choose where a credential goes: Pi rejects
      // auth.provider there, so a project URL entry carrying it is refused
      // whether it is inherited from the file or sent by the form.
      const requestedAuth = isRecord(params.entry) ? params.entry.auth : undefined;
      if (
        scope === "project" &&
        next.url !== undefined &&
        (next.auth !== undefined || requestedAuth !== undefined)
      ) {
        throw new Error(`server "${name}": auth is only allowed in the global mcp.json`);
      }
    }

    const changed = JSON.stringify(existingRaw) !== JSON.stringify(next);
    if (!changed) {
      return {
        scope,
        name,
        path: filePath,
        revision: currentRevision,
        changed: false,
        runtimeReloadRequired: true,
      };
    }
    servers[name] = next;
    layer.doc[layer.serverKey] = servers;
    const bytes = serializeDocument(layer.doc as Record<string, unknown>);
    writeAtomic(filePath, bytes);
    return {
      scope,
      name,
      path: filePath,
      revision: hashBytes(bytes),
      changed: true,
      runtimeReloadRequired: true,
    };
  });
}

/**
 * Native disable semantics for full definitions: the `enabled` flag lives in
 * the defining file, where disabling sets `enabled: false` and enabling removes
 * the flag so the default applies. A project override instead keeps an explicit
 * `enabled` value on both sides (removing it would fall back to the global one).
 */
export async function toggleMcpServer(
  params: Record<string, unknown>,
  context: McpSettingsContext,
): Promise<McpToggleResult> {
  const scope = parseWriteScope(params.scope);
  const name = requireName(params.name);
  const disable = params.disable === true;

  return writeForScope(scope, context, (filePath) => {
    const { revision: currentRevision, layer } = readTargetDocument(filePath);
    assertExpectedRevision(params.expectedRevision, currentRevision);
    const servers = serversOf(layer);
    const existingRaw = servers[name];
    if (!isRecord(existingRaw)) {
      throw new Error(`Unknown MCP server in ${filePath}: ${name}`);
    }
    const result = (revision: McpRevision, changed: boolean): McpToggleResult => ({
      scope,
      name,
      path: filePath,
      revision,
      changed,
      enabled: !disable,
      runtimeReloadRequired: true,
    });

    let next: Record<string, unknown>;
    if (scope === "project" && isMcpOverride(existingRaw)) {
      const extra = Object.keys(existingRaw).filter((key) => !OVERRIDE_KEYS.includes(key as never));
      if (extra.length > 0) {
        throw new Error(`server "${name}": an override can only set ${OVERRIDE_KEYS.join(", ")}`);
      }
      const global = readGlobalProjection(context.agentDir);
      assertExpectedGlobalRevision(params.expectedGlobalRevision, global.revision);
      const base = global.projection.servers.get(name);
      if (!base) {
        throw new Error(`No global MCP server "${name}" to override`);
      }
      next = { ...existingRaw, enabled: !disable };
      const merged = validateNativeMcpEntry(name, { ...base.config, ...next });
      if (typeof merged === "string") throw new Error(merged);
    } else if (disable) {
      next = { ...existingRaw, enabled: false };
    } else {
      next = Object.fromEntries(Object.entries(existingRaw).filter(([key]) => key !== "enabled"));
    }

    const changed = JSON.stringify(existingRaw) !== JSON.stringify(next);
    if (!changed) return result(currentRevision, false);
    servers[name] = next;
    layer.doc[layer.serverKey] = servers;
    const bytes = serializeDocument(layer.doc as Record<string, unknown>);
    writeAtomic(filePath, bytes);
    return result(hashBytes(bytes), true);
  });
}

/** Removing an entry never touches the other scope; an absent entry is a no-op. */
export async function deleteMcpServer(
  params: Record<string, unknown>,
  context: McpSettingsContext,
): Promise<McpMutationResult> {
  const scope = parseWriteScope(params.scope);
  const name = requireName(params.name);

  return writeForScope(scope, context, (filePath) => {
    const { revision: currentRevision, layer } = readTargetDocument(filePath);
    assertExpectedRevision(params.expectedRevision, currentRevision);
    const servers = serversOf(layer);
    if (!(name in servers)) {
      return {
        scope,
        name,
        path: filePath,
        revision: currentRevision,
        changed: false,
        runtimeReloadRequired: true,
      };
    }
    delete servers[name];
    layer.doc[layer.serverKey] = servers;
    const bytes = serializeDocument(layer.doc as Record<string, unknown>);
    writeAtomic(filePath, bytes);
    return {
      scope,
      name,
      path: filePath,
      revision: hashBytes(bytes),
      changed: true,
      runtimeReloadRequired: true,
    };
  });
}

/**
 * Snapshot every valid global file server (disabled ones included) into the
 * trusted project file as an explicit enabled/exposure/toolExposure override.
 * One re-read, one merge, one atomic write; existing names are never touched.
 */
export async function importGlobalMcpOverrides(
  context: McpSettingsContext,
): Promise<McpImportResult> {
  const { file } = projectTarget(context);
  const filePath = file;
  return withSettingsLock(filePath, () => {
    assertProjectAccess(context, filePath);
    const global = readGlobalProjection(context.agentDir);
    const state = readStrictFile(filePath);
    let doc: Record<string, unknown> = {};
    if (state.text !== null) {
      try {
        doc = parseNativeMcpDocument(state.text);
      } catch (error) {
        throw new Error(`Project MCP config is unreadable: ${errMessage(error)}`);
      }
    }
    const servers: Record<string, unknown> = isRecord(doc.mcpServers) ? doc.mcpServers : {};
    const existingNames = Object.keys(servers);
    const usedNamespaces = new Map<string, string>();
    for (const existing of existingNames) {
      const key = nameKey(existing);
      if (!usedNamespaces.has(key)) usedNamespaces.set(key, existing);
    }

    const imported: string[] = [];
    const skipped: McpImportSkip[] = [];
    for (const [name, server] of global.projection.servers) {
      if (existingNames.includes(name)) {
        skipped.push({ name, reason: "existing" });
        continue;
      }
      const key = nameKey(name);
      const conflict = usedNamespaces.get(key);
      if (conflict) {
        skipped.push({
          name,
          reason: "namespace-conflict",
          conflictWith: conflict,
          detail: `collides with existing project entry ${conflict}`,
        });
        continue;
      }
      const snapshot = snapshotMcpOverride(server.config);
      servers[name] = {
        enabled: snapshot.enabled,
        exposure: snapshot.exposure,
        toolExposure: snapshot.toolExposure,
      };
      usedNamespaces.set(key, name);
      imported.push(name);
    }
    for (const [name, message] of global.projection.errors) {
      if (name === "") continue;
      skipped.push({ name, reason: "invalid-global", detail: message });
    }

    if (imported.length === 0) {
      return {
        imported,
        skipped,
        changed: false,
        path: filePath,
        revision: state.revision,
        runtimeReloadRequired: true as const,
      };
    }
    // Source recheck before publish: a global file rewritten during the batch
    // would make the snapshot describe a config that no longer exists.
    if (readStrictFile(userPath(context.agentDir)).revision !== global.revision) {
      throw new Error("Global MCP config changed during import; reload and retry");
    }
    doc.mcpServers = servers as Record<string, unknown>;
    const bytes = serializeDocument(doc);
    writeAtomic(filePath, bytes);
    return {
      imported,
      skipped,
      changed: true,
      path: filePath,
      revision: hashBytes(bytes),
      runtimeReloadRequired: true as const,
    };
  });
}

/** Adapter→native field mapping for the migration copy. Returns the mapped
 * entry plus whether adapter-only fields had to be dropped. */
function mapAdapterEntry(raw: Record<string, unknown>): {
  entry: Record<string, unknown>;
  lossy: boolean;
} {
  const entry: Record<string, unknown> = { ...raw };
  let lossy = false;
  if (entry.disabled === true) entry.enabled = false;
  if (entry.directTools === true) entry.exposure = "direct";
  for (const field of ADAPTER_DROP_FIELDS) {
    if (field in entry) {
      delete entry[field];
      if (field === "inheritEnv" || field === "lifecycle") lossy = true;
    }
  }
  return { entry, lossy };
}

/**
 * User-confirmed one-shot copy from an orphaned adapter/shared layer into the
 * matching native file. Only names missing from the target are merged; the
 * source file is always left in place.
 */
export async function migrateAdapterConfig(
  params: Record<string, unknown>,
  context: McpSettingsContext,
): Promise<{ migrated: string[]; skipped: string[]; lossy: string[] }> {
  const target = params.target;
  const source = migrationSources(context).find((s) => s.id === target);
  if (!source) throw new Error(`Unknown migration target: ${String(target)}`);
  const filePath =
    source.target === "user" ? userPath(context.agentDir) : projectTarget(context).file;

  return withSettingsLock(filePath, () => {
    if (source.target === "project") assertProjectAccess(context, filePath);
    const { layer } = readTargetDocument(filePath);
    const servers = serversOf(layer);
    const { merged } = mergeFiles(source.files);

    const migrated: string[] = [];
    const skipped: string[] = [];
    const lossy: string[] = [];
    for (const [name, raw] of merged) {
      if (name in servers) {
        skipped.push(name);
        continue;
      }
      const mapped = mapAdapterEntry(raw);
      servers[name] = mapped.entry;
      migrated.push(name);
      if (mapped.lossy) lossy.push(name);
    }
    if (migrated.length > 0) {
      layer.doc[layer.serverKey] = servers;
      writeAtomic(filePath, serializeDocument(layer.doc as Record<string, unknown>));
    }
    return { migrated, skipped, lossy };
  });
}

export type { McpExposure };
