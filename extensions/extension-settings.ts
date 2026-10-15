// ABOUTME: Bridge ops for advisor's config (XDG-aware rpiv-advisor/advisor.json); pi-fff moved to
// ABOUTME: the host control plane (src-tauri/crates/picot-host/src/fff_config.rs) so landing pages can configure it.

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { getSupportedThinkingLevels } from "@earendil-works/pi-ai";

/** Advisor's GradedEffort ordinal (messages.ts) — excludes "off", which the
 * advisor stores as an absent key, never a value. */
const ADVISOR_EFFORTS = ["minimal", "low", "medium", "high", "xhigh", "max"] as const;

interface CatalogModelLike {
  provider?: string;
  id?: string;
  name?: string;
  reasoning?: unknown;
  thinkingLevelMap?: Record<string, unknown>;
}

interface RegistryLike {
  getAll: () => CatalogModelLike[];
  getAvailable: () => CatalogModelLike[] | Promise<CatalogModelLike[]>;
}

function homeDir(): string {
  // Mirrors picot-config.ts's resolveHomeDir precedence so tests can hermetically
  // redirect via HOME (os.homedir() on macOS ignores the env override).
  const fromEnv = process.env.HOME ?? process.env.USERPROFILE;
  if (fromEnv?.trim()) return fromEnv;
  return os.homedir();
}

/** Default config directory: `~/.config` — the legacy, pre-XDG location and
 * the base when XDG_CONFIG_HOME doesn't qualify. */
function defaultConfigDir(): string {
  return path.join(homeDir(), ".config");
}

/** Expand a leading `~` / `~/…` to home. `~user` forms are NOT expanded
 * (XDG defines no `~user`), so they fail the absolute check and route to the
 * default config dir. Mirrors rpiv-config's expandTilde. */
function expandTilde(p: string): string {
  if (p === "~") return homeDir();
  if (p.startsWith("~/")) return path.join(homeDir(), p.slice(2));
  return p;
}

/** rpiv-config's resolveConfigDir: XDG_CONFIG_HOME (trimmed) wins only when
 * it expands (tilde) to an absolute path; unset / empty / whitespace-only /
 * relative values all route to the default `~/.config`. */
function resolveConfigDir(): string {
  const xdg = process.env.XDG_CONFIG_HOME?.trim();
  if (!xdg) return defaultConfigDir();
  const expanded = expandTilde(xdg);
  return path.isAbsolute(expanded) ? expanded : defaultConfigDir();
}

/** rpiv-config's configPath("rpiv-advisor", "advisor.json") — the advisor's
 * own write path (ADVISOR_CONFIG_PATH); Picot must write the exact same file. */
export function advisorConfigPath(): string {
  return path.join(resolveConfigDir(), "rpiv-advisor", "advisor.json");
}

/** Always-legacy path under `~/.config`, deliberately ignoring
 * XDG_CONFIG_HOME — the read-fallback target so a pre-XDG config file is
 * still discovered after an operator sets XDG_CONFIG_HOME. */
function legacyAdvisorConfigPath(): string {
  return path.join(defaultConfigDir(), "rpiv-advisor", "advisor.json");
}

/** rpiv-config's loadJsonConfigWithLegacyFallback semantics: the XDG path
 * wins when present (malformed there → {}, never silently masked by legacy);
 * only when it is missing does the legacy path serve. Both missing/malformed → {}. */
function readAdvisorConfigFile(): Record<string, unknown> {
  const xdgPath = advisorConfigPath();
  const filePath = fs.existsSync(xdgPath) ? xdgPath : legacyAdvisorConfigPath();
  try {
    const parsed = JSON.parse(fs.readFileSync(filePath, "utf8")) as unknown;
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    return parsed as Record<string, unknown>;
  } catch {
    return {};
  }
}

/**
 * Run a single-key patch (`{ key, value }`) or a batch (`{ entries: [{ key,
 * value }, …] }`) against one document. Batches exist so paired fields
 * (provider + modelId) land in one atomic write instead of two.
 */
function applyPatches(
  params: Record<string, unknown>,
  apply: (key: string, value: unknown) => void,
): void {
  if (params.entries !== undefined) {
    const entries = params.entries;
    if (!Array.isArray(entries) || entries.length === 0) {
      throw new Error("entries must be a non-empty array");
    }
    for (const entry of entries) {
      const candidate = (entry ?? {}) as { key?: unknown; value?: unknown };
      if (typeof candidate.key !== "string" || !candidate.key) {
        throw new Error("each entry needs a key");
      }
      apply(candidate.key, candidate.value);
    }
    return;
  }
  const key = params.key;
  if (typeof key !== "string" || !key) throw new Error("key is required");
  apply(key, "value" in params ? params.value : undefined);
}

/** Atomic write with the same format as rpiv-config's saveJsonConfig
 * (2-space JSON + trailing newline), but tmp+rename so an interrupted write
 * can never truncate the file. chmod 0600 rides on the tmp file. */
function writeConfigAtomic(filePath: string, doc: Record<string, unknown>): void {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  const tmp = path.join(path.dirname(filePath), `.${path.basename(filePath)}.${process.pid}.tmp`);
  fs.writeFileSync(tmp, `${JSON.stringify(doc, null, 2)}\n`, "utf8");
  try {
    fs.chmodSync(tmp, 0o600);
  } catch {
    // Best-effort — same posture as saveJsonConfig; some filesystems ignore chmod.
  }
  fs.renameSync(tmp, filePath);
}

/**
 * Advisor config + the model catalog the renderer needs, in one round trip.
 * The TUI's effort list (command.ts buildEffortItems) intersects
 * getSupportedThinkingLevels with the GradedEffort ordinal; the same
 * intersection is computed here so the GUI can never offer an effort the
 * advisor would refuse to rank.
 */
export async function advisorConfigGet(registry: RegistryLike): Promise<{
  modelKey?: string;
  effort?: string;
  models: Array<{ key: string; name: string; levels: string[]; available: boolean }>;
}> {
  const config = readAdvisorConfigFile();
  const all = registry.getAll();
  const available = new Set((await registry.getAvailable()).map((m) => `${m.provider}/${m.id}`));
  const models = all
    .filter((m): m is CatalogModelLike & { provider: string; id: string } =>
      Boolean(m.provider && m.id),
    )
    .map((m) => ({
      key: `${m.provider}/${m.id}`,
      name: typeof m.name === "string" ? m.name : (m.id as string),
      levels: getSupportedThinkingLevels(m as never).filter((level) =>
        (ADVISOR_EFFORTS as readonly string[]).includes(level),
      ),
      available: available.has(`${m.provider}/${m.id}`),
    }))
    .sort((a, b) => a.key.localeCompare(b.key));

  return {
    modelKey: typeof config.modelKey === "string" ? config.modelKey : undefined,
    effort: typeof config.effort === "string" ? config.effort : undefined,
    models,
  };
}

/**
 * Read-modify-write mirroring the advisor's own saveAdvisorConfig spread:
 * guidance / disabledForModels / unknown keys survive untouched. Each field
 * is absent = leave unchanged, null = clear, string = set.
 */
export function advisorConfigSet(params: Record<string, unknown>): {
  config: Record<string, unknown>;
} {
  const doc = readAdvisorConfigFile();

  if ("modelKey" in params) {
    if (params.modelKey === null || params.modelKey === "") delete doc.modelKey;
    else if (typeof params.modelKey === "string") doc.modelKey = params.modelKey;
    else throw new Error("modelKey must be a string or null");
  }
  if ("effort" in params) {
    if (params.effort === null || params.effort === "") delete doc.effort;
    else if (typeof params.effort === "string") {
      if (!(ADVISOR_EFFORTS as readonly string[]).includes(params.effort)) {
        throw new Error(`effort must be one of ${ADVISOR_EFFORTS.join(", ")}`);
      }
      doc.effort = params.effort;
    } else throw new Error("effort must be a string or null");
  }

  writeConfigAtomic(advisorConfigPath(), doc);
  return { config: doc };
}

// ─── pi-plan-mode bridge ops ────────────────────────────────────────────────

const PLAN_THINKING_LEVELS = [
  "inherit",
  "off",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
] as const;
const PLAN_IMPL_THINKING_LEVELS = [
  "off",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
] as const;
const PLAN_RETENTIONS = ["clear-on-start", "clear-after-first-run", "keep"] as const;

function planModeConfigPath(): string {
  const agentRoot =
    process.env.PI_CODING_AGENT_DIR ||
    path.join(os.homedir(), process.platform === "win32" ? ".pi\\agent" : ".pi/agent");
  return path.join(agentRoot, "pi-plan-mode.json");
}

/** Read a plan-mode config file. A missing file is the package's "missing"
 * load result (empty doc); unreadable content is reported so a write can
 * refuse instead of overwriting the user's file wholesale. */
function readPlanModeConfig(): { doc: Record<string, unknown>; invalid?: string } {
  let raw: string;
  try {
    raw = fs.readFileSync(planModeConfigPath(), "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return { doc: {} };
    return { doc: {}, invalid: `cannot read config file (${(error as Error).message})` };
  }
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return { doc: parsed as Record<string, unknown> };
    }
    return { doc: {}, invalid: "config root is not an object" };
  } catch (error) {
    // A JSON.parse message carries a position, never file content.
    return { doc: {}, invalid: (error as Error).message };
  }
}

function planModeModelCatalog(registry: RegistryLike) {
  const all = registry.getAll();
  return all
    .filter((m): m is CatalogModelLike & { provider: string; id: string } =>
      Boolean(m.provider && m.id),
    )
    .map((m) => ({
      key: `${m.provider}/${m.id}`,
      name: typeof m.name === "string" ? m.name : (m.id as string),
      levels: getSupportedThinkingLevels(m as never),
    }))
    .sort((a, b) => a.key.localeCompare(b.key));
}

export async function planModeConfigGet(registry: RegistryLike): Promise<{
  settings: Record<string, unknown> | null;
  models: Array<{ key: string; name: string; levels: string[] }>;
  invalid?: { reason: string };
}> {
  const { doc, invalid } = readPlanModeConfig();
  const models = planModeModelCatalog(registry);
  if (invalid) return { settings: null, models, invalid: { reason: invalid } };
  return { settings: doc, models };
}

/** Apply one patch to a plan-mode document (the package's
 * `PlanModeSettingsPatch` semantics: null/empty clears, absent is rejected). */
function patchPlanModeDoc(doc: Record<string, unknown>, key: string, value: unknown): void {
  const setString = (field: string, validate?: (raw: string) => void) => {
    if (value === null || value === "") delete doc[field];
    else if (typeof value === "string") {
      validate?.(value);
      doc[field] = value;
    } else throw new Error(`${field} must be a string or null`);
  };
  const setObject = (field: string, validate: (raw: unknown) => unknown) => {
    if (value === null) delete doc[field];
    else {
      doc[field] = validate(value);
    }
  };
  switch (key) {
    case "thinkingLevel":
      setString("thinkingLevel", (raw) => {
        if (!(PLAN_THINKING_LEVELS as readonly string[]).includes(raw)) {
          throw new Error(`thinkingLevel must be one of ${PLAN_THINKING_LEVELS.join(", ")}`);
        }
      });
      break;
    case "defaultImplementationModel":
      setString("defaultImplementationModel");
      break;
    case "defaultImplementationThinkingLevel":
      setString("defaultImplementationThinkingLevel", (raw) => {
        if (!(PLAN_IMPL_THINKING_LEVELS as readonly string[]).includes(raw)) {
          throw new Error(
            `defaultImplementationThinkingLevel must be one of ${PLAN_IMPL_THINKING_LEVELS.join(", ")}`,
          );
        }
      });
      break;
    case "implementationPlanRetention":
      setString("implementationPlanRetention", (raw) => {
        if (!(PLAN_RETENTIONS as readonly string[]).includes(raw)) {
          throw new Error(
            `implementationPlanRetention must be one of ${PLAN_RETENTIONS.join(", ")}`,
          );
        }
      });
      break;
    case "defaultPlanExportPath":
      setString("defaultPlanExportPath");
      break;
    case "toggleShortcut":
      setString("toggleShortcut");
      break;
    case "defaultPlanTools": {
      // string[] or null; advanced JSON editor supplies a parsed array.
      setObject("defaultPlanTools", (raw) => {
        if (!Array.isArray(raw) || raw.some((entry) => typeof entry !== "string")) {
          throw new Error("defaultPlanTools must be an array of strings");
        }
        return raw;
      });
      break;
    }
    case "safeSubcommands": {
      // Record<string, string[] | undefined>; advanced JSON editor supplies
      // a parsed object.
      setObject("safeSubcommands", (raw) => {
        if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
          throw new Error("safeSubcommands must be an object");
        }
        for (const [command, subcommands] of Object.entries(raw as Record<string, unknown>)) {
          if (
            subcommands !== undefined &&
            (!Array.isArray(subcommands) || subcommands.some((entry) => typeof entry !== "string"))
          ) {
            throw new Error(`safeSubcommands.${command} must be an array of strings`);
          }
        }
        return raw;
      });
      break;
    }
    default:
      throw new Error(`unknown plan-mode config key: ${String(key)}`);
  }
}

export function planModeConfigSet(params: Record<string, unknown>): {
  config: Record<string, unknown>;
} {
  const { doc, invalid } = readPlanModeConfig();
  if (invalid) throw new Error(`cannot write onto an invalid file (${invalid})`);
  applyPatches(params, (key, value) => patchPlanModeDoc(doc, key, value));
  writeConfigAtomic(planModeConfigPath(), doc);
  return { config: doc };
}

// ─── pi-extension-safety-guard bridge ops ───────────────────────────────────

const SAFETY_CATEGORIES = [
  "git",
  "filesystem",
  "docker",
  "package",
  "system",
  "database",
  "secrets",
] as const;

function safetyGuardConfigPath(): { path: string; relocated: boolean } {
  const relocated = process.env.PI_SAFETY_GUARD_CONFIG_FILE?.trim();
  if (relocated) return { path: relocated, relocated: true };
  const agentRoot =
    process.env.PI_CODING_AGENT_DIR ||
    path.join(os.homedir(), process.platform === "win32" ? ".pi\\agent" : ".pi/agent");
  return { path: path.join(agentRoot, "safety-guard.json"), relocated: false };
}

/** Same discipline as plan-mode: a missing file is empty, unreadable content
 * is reported so a write refuses instead of dropping the guard's config. */
function readSafetyGuardConfig(): { doc: Record<string, unknown>; invalid?: string } {
  let raw: string;
  try {
    raw = fs.readFileSync(safetyGuardConfigPath().path, "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return { doc: {} };
    return { doc: {}, invalid: `cannot read config file (${(error as Error).message})` };
  }
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return { doc: parsed as Record<string, unknown> };
    }
    return { doc: {}, invalid: "config root is not an object" };
  } catch (error) {
    return { doc: {}, invalid: (error as Error).message };
  }
}

function safetyGuardAllowCounts(): { global: number } {
  const agentRoot =
    process.env.PI_CODING_AGENT_DIR ||
    path.join(os.homedir(), process.platform === "win32" ? ".pi\\agent" : ".pi/agent");
  try {
    const parsed = JSON.parse(
      fs.readFileSync(path.join(agentRoot, "safety-guard-allow.json"), "utf8"),
    ) as unknown;
    if (Array.isArray(parsed)) return { global: parsed.length };
    if (parsed && typeof parsed === "object") return { global: Object.keys(parsed).length };
  } catch {
    // missing or malformed → zero; project-scope counts are workspace-only.
  }
  return { global: 0 };
}

export function safetyGuardConfigGet(): {
  config: Record<string, unknown> | null;
  configPath: string;
  relocatedByEnv: boolean;
  allowCounts: { global: number };
  invalid?: { reason: string };
} {
  const { path: configPath, relocated } = safetyGuardConfigPath();
  const { doc, invalid } = readSafetyGuardConfig();
  const base = {
    configPath,
    relocatedByEnv: relocated,
    allowCounts: safetyGuardAllowCounts(),
  };
  if (invalid) return { ...base, config: null, invalid: { reason: invalid } };
  return { ...base, config: doc };
}

/** Apply one dotted-path patch to a safety-guard document. */
function patchSafetyGuardDoc(doc: Record<string, unknown>, key: string, value: unknown): void {
  const expectType = (raw: unknown, expect: "boolean" | "string"): boolean | string => {
    if (expect === "boolean" && typeof raw === "boolean") return raw;
    if (expect === "string" && typeof raw === "string") return raw;
    throw new Error(`${key} must be a ${expect}`);
  };
  const segments = key.split(".");
  let cursor: Record<string, unknown> = doc;
  for (let i = 0; i < segments.length - 1; i++) {
    const segment = segments[i];
    const next = cursor[segment];
    if (!next || typeof next !== "object" || Array.isArray(next)) {
      cursor[segment] = {};
    }
    cursor = cursor[segment] as Record<string, unknown>;
  }
  const leaf = segments[segments.length - 1];
  if (key === "enabled" || key.startsWith("categories.") || key.startsWith("protectedPaths.")) {
    if (key.startsWith("categories.") && !SAFETY_CATEGORIES.includes(leaf as never)) {
      throw new Error(`unknown category: ${leaf}`);
    }
    cursor[leaf] = expectType(value, "boolean");
  } else if (key === "contextLines.before" || key === "contextLines.after") {
    const number = value;
    if (typeof number !== "number" || !Number.isInteger(number) || number < 0 || number > 20) {
      throw new Error(`${key} must be an integer 0–20`);
    }
    cursor[leaf] = number;
  } else if (
    key === "autoReview.enabled" ||
    key === "autoReview.model.provider" ||
    key === "autoReview.model.modelId" ||
    key === "autoReview.model.thinkingLevel"
  ) {
    cursor[leaf] = expectType(value, key === "autoReview.enabled" ? "boolean" : "string");
  } else {
    throw new Error(`unknown safety-guard config key: ${key}`);
  }
}

/** Dotted-path patches merged through the package's merge semantics (spread
 * over the stored doc, unknown keys preserved); `entries` batches several
 * keys into one write so a paired model cannot land half-applied. */
export function safetyGuardConfigSet(params: Record<string, unknown>): {
  config: Record<string, unknown>;
} {
  if (safetyGuardConfigPath().relocated) {
    throw new Error("config relocated by PI_SAFETY_GUARD_CONFIG_FILE: edit the file at that path");
  }
  const { doc, invalid } = readSafetyGuardConfig();
  if (invalid) throw new Error(`cannot write onto an invalid file (${invalid})`);
  applyPatches(params, (key, value) => patchSafetyGuardDoc(doc, key, value));
  writeConfigAtomic(safetyGuardConfigPath().path, doc);
  return { config: doc };
}

// ─── pi-web-access bridge ops (credential store discipline) ────────────────

/** Provider API keys writable through the GUI. Never returned in full:
 * the get op reports { configured, preview } where preview is the last 4
 * characters only — full keys never transit the bridge → WebView. */
const WEBACCESS_SECRET_KEYS = [
  "openaiApiKey",
  "braveApiKey",
  "exaApiKey",
  "tinyfishApiKey",
  "search1apiApiKey",
  "searchinfinityApiKey",
  "queritApiKey",
  "jinaApiKey",
  "bochaApiKey",
  "perplexityApiKey",
  "geminiApiKey",
  "mistralApiKey",
  "serpapiApiKey",
  "xaiApiKey",
  "valyuApiKey",
  "anysearchApiKey",
  "datalabApiKey",
  "firecrawlApiKey",
] as const;
/** Endpoint credentials, flat like every other key in this file: the names
 * are the package's own (crawl4ai.ts, brightdata.ts, brightdata-unlocker.ts
 * @ pi-web-access 0.30.0). SearxNG's `searxngHeaders` is a
 * `Record<string, string>` — it has no single-string row and stays a
 * hand-edited field. */
const WEBACCESS_ENDPOINT_SECRET_KEYS = ["crawl4aiApiToken", "brightdataApiKey"] as const;
const WEBACCESS_NON_SECRET_KEYS = [
  "proxy",
  "openaiResponsesUrl",
  "allowBrowserCookies",
  "image.enabled",
  "searxngBaseUrl",
  "crawl4aiBaseUrl",
  "brightdataSerpZone",
  "brightdataUnlockerZone",
] as const;
/** Fetched-page answer model: `fetch.answerProvider` + `fetch.answerModel`,
 * which the package only accepts as a pair (page-query.ts). */
const WEBACCESS_ANSWER_MODEL_KEYS = ["fetch.answerProvider", "fetch.answerModel"] as const;

/** Env names the camelCase derivation cannot produce: the package reads
 * `SERPAPI_KEY` and `SEARCH1API_KEY` (serpapi.ts, search1api.ts). */
const WEBACCESS_ENV_NAME_OVERRIDES: Record<string, string> = {
  serpapiApiKey: "SERPAPI_KEY",
  search1apiApiKey: "SEARCH1API_KEY",
};

const WEBACCESS_FILE = "web-search.json";

/** Port of the package's `getWebSearchConfigDir` (utils.ts): explicit agent
 * dir → existing `$XDG_CONFIG_HOME/pi` file → existing legacy `~/.pi` file →
 * agent dir for new configs. Both the legacy tier and the rule that
 * `~/.config/pi` counts only when XDG_CONFIG_HOME is set belong to that
 * contract: resolving differently makes the GUI write (or read) another file
 * than the running Pi, so a saved key silently never applies — and a GUI
 * write can shadow the legacy file the Pi was still using. */
function webAccessConfigFile(): string {
  const explicit = process.env.PI_CODING_AGENT_DIR;
  if (explicit) return path.join(explicit, WEBACCESS_FILE);
  const agentFile = path.join(os.homedir(), ".pi", "agent", WEBACCESS_FILE);
  const legacyFile = path.join(os.homedir(), ".pi", WEBACCESS_FILE);
  const xdgHome = process.env.XDG_CONFIG_HOME;
  if (xdgHome) {
    const xdgFile = path.join(xdgHome, "pi", WEBACCESS_FILE);
    if (fs.existsSync(xdgFile)) return xdgFile;
    if (fs.existsSync(legacyFile)) return legacyFile;
    return xdgFile;
  }
  if (fs.existsSync(agentFile)) return agentFile;
  if (fs.existsSync(legacyFile)) return legacyFile;
  return agentFile;
}

function readWebAccessConfig(): { doc: Record<string, unknown> | null; invalid?: string } {
  try {
    const raw = fs.readFileSync(webAccessConfigFile(), "utf8");
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return { doc: null, invalid: "config root is not an object" };
    }
    return { doc: parsed as Record<string, unknown> };
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return { doc: {} };
    // The package deliberately avoids quoting file text back (its own text
    // is the secret) — mirror that: parse failure, no content echo.
    return { doc: null, invalid: "config file is not valid JSON" };
  }
}

type WebAccessConfigValue =
  | string
  | boolean
  | number
  | Record<string, unknown>
  | unknown[]
  | undefined;

function dottedGet(doc: Record<string, unknown>, key: string): WebAccessConfigValue {
  // Navigate with an unknown cursor; parse to the named domain type once at
  // the return boundary.
  let cursor: unknown = doc;
  for (const segment of key.split(".")) {
    if (!cursor || typeof cursor !== "object" || Array.isArray(cursor)) return undefined;
    cursor = (cursor as Record<string, unknown>)[segment];
  }
  if (cursor === null || cursor === undefined) return undefined;
  if (typeof cursor === "string" || typeof cursor === "boolean" || typeof cursor === "number") {
    return cursor;
  }
  if (Array.isArray(cursor)) return cursor as unknown[];
  if (typeof cursor === "object") return cursor as Record<string, unknown>;
  return undefined;
}

function dottedSet(doc: Record<string, unknown>, key: string, value: unknown): void {
  const segments = key.split(".");
  let cursor: Record<string, unknown> = doc;
  for (let i = 0; i < segments.length - 1; i++) {
    const segment = segments[i];
    const next = cursor[segment];
    if (!next || typeof next !== "object" || Array.isArray(next)) {
      cursor[segment] = {};
    }
    cursor = cursor[segment] as Record<string, unknown>;
  }
  if (value === null) delete cursor[segments[segments.length - 1]];
  else cursor[segments[segments.length - 1]] = value;
}

const mask = (raw: unknown): { configured: boolean; preview?: string } => {
  if (typeof raw !== "string" || raw.length === 0) return { configured: false };
  return { configured: true, preview: raw.slice(-4) };
};

export function webAccessConfigGet(): {
  fields: Record<string, { configured: boolean; preview?: string }>;
  nonSecrets: Record<string, unknown>;
  routing: Record<string, unknown>;
  envKeyed: string[];
  invalid?: { reason: string };
} {
  const { doc, invalid } = readWebAccessConfig();
  if (!doc)
    return {
      fields: {},
      nonSecrets: {},
      routing: {},
      envKeyed: [],
      invalid: { reason: invalid ?? "unreadable" },
    };
  const fields: Record<string, { configured: boolean; preview?: string }> = {};
  for (const key of WEBACCESS_SECRET_KEYS) fields[key] = mask(doc[key]);
  for (const key of WEBACCESS_ENDPOINT_SECRET_KEYS) fields[key] = mask(dottedGet(doc, key));
  const nonSecrets: Record<string, unknown> = {};
  for (const key of WEBACCESS_NON_SECRET_KEYS) nonSecrets[key] = dottedGet(doc, key) ?? null;
  const routing = {
    searchRouting: doc.searchRouting ?? null,
    fetchRouting: doc.fetchRouting ?? null,
    answerModel: {
      provider: dottedGet(doc, "fetch.answerProvider") ?? null,
      modelId: dottedGet(doc, "fetch.answerModel") ?? null,
    },
  };
  // Env parallels (package hasCredentialSource: config > env; env present
  // counts as configured for display but config stays writable).
  const envKeyed = WEBACCESS_SECRET_KEYS.filter((key) => {
    const derived = key
      .replace(/ApiKey$/, "_API_KEY")
      .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
      .toUpperCase();
    return Boolean(process.env[WEBACCESS_ENV_NAME_OVERRIDES[key] ?? derived]?.trim());
  });
  return { fields, nonSecrets, routing, envKeyed };
}

/** Apply one dotted-path patch to a web-access document. */
function patchWebAccessDoc(doc: Record<string, unknown>, key: string, value: unknown): void {
  const isSecret =
    (WEBACCESS_SECRET_KEYS as readonly string[]).includes(key) ||
    (WEBACCESS_ENDPOINT_SECRET_KEYS as readonly string[]).includes(key);
  const isNonSecret = (WEBACCESS_NON_SECRET_KEYS as readonly string[]).includes(key);
  const isAnswerModel = (WEBACCESS_ANSWER_MODEL_KEYS as readonly string[]).includes(key);
  if (!isSecret && !isNonSecret && !isAnswerModel) {
    throw new Error(`unknown web-access config key: ${key}`);
  }
  if ((isSecret || isAnswerModel) && value !== null && typeof value !== "string") {
    throw new Error(`${key} must be a string or null`);
  }
  if (key === "allowBrowserCookies" || key === "image.enabled") {
    if (value !== null && typeof value !== "boolean") {
      throw new Error(`${key} must be a boolean or null`);
    }
  }
  // Empty string on a secret row clears it — the input is the only place a
  // full key exists and save-on-change writes only when non-empty.
  dottedSet(doc, key, value === "" ? null : value);
}

/** Provider keys never round-trip through the WebView, and a paired answer
 * model (provider + modelId) must land in one write. */
export function webAccessConfigSet(params: Record<string, unknown>): { ok: true } {
  const { doc, invalid } = readWebAccessConfig();
  if (!doc) throw new Error(`cannot write onto an invalid file (${invalid ?? "unreadable"})`);
  applyPatches(params, (key, value) => patchWebAccessDoc(doc, key, value));
  // The package rejects a half-configured answer model (page-query.ts:
  // "must be configured together"), so a write can never leave one half.
  const hasProvider = dottedGet(doc, "fetch.answerProvider") !== undefined;
  const hasModel = dottedGet(doc, "fetch.answerModel") !== undefined;
  if (hasProvider !== hasModel) {
    throw new Error("fetch.answerProvider and fetch.answerModel must be configured together");
  }
  writeConfigAtomic(webAccessConfigFile(), doc);
  return { ok: true };
}
