// Configuration data plane for the native Picot Settings → Configuration tab.
//
// The WebView talks to the Rust host, which forwards commands to pi over
// stdio RPC. pi's native RPC command set is fixed (see docs/rpc.md) and cannot
// be extended, so this module is invoked through a registered pi command
// (`/picot-config`) whose handler runs immediately without hitting the LLM or
// session history. Results are returned to the WebView via `ctx.ui.notify(JSON)`,
// correlated by request id (see public/native/config-gateway.js).
//
// All model-registry access (catalog, auth status, API keys, visibility, and
// health) goes through the live `ctx.modelRegistry`, so we never re-implement
// pi's provider knowledge.

import { execFile } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import {
  createAgentSession,
  ModelRuntime,
  readStoredCredential,
  SessionManager,
} from "@earendil-works/pi-coding-agent";
import {
  buildModelsJsonProviderEntry,
  detectProviderProtocol,
  fetchUpstreamModels,
  type ModelsJsonDocument,
  mergeProviderIntoModelsJson,
  normalizeBaseUrl,
  type ProbeModel,
  type ProviderProtocol,
  resolveProviderId,
  testProviderConnectivity,
} from "./custom-provider-probe";
import {
  advisorConfigGet,
  advisorConfigSet,
  planModeConfigGet,
  planModeConfigSet,
  safetyGuardConfigGet,
  safetyGuardConfigSet,
  webAccessConfigGet,
  webAccessConfigSet,
} from "./extension-settings";
import {
  deleteMcpServer,
  importGlobalMcpOverrides,
  listMcpServers,
  type McpSettingsContext,
  migrateAdapterConfig,
  saveMcpServer,
  toggleMcpServer,
  UNTRUSTED_PROJECT_ERROR,
} from "./mcp-settings";
import {
  createOAuthLoginOperationManager,
  type OAuthOperationEvent,
} from "./oauth-login-operations";
import { buildPackageSkillInventory, mutatePackageSkillEnabled } from "./package-skill-inventory";
import { writePasteOffloadFile } from "./paste-offload";
import { createPiOAuthLoginAdapter } from "./pi-oauth-login-adapter";
import {
  consumeCodexResetCredit,
  createQuotaProbeCache,
  inspectCodexResetCredits,
  originOfBaseUrl,
  type ProviderInstance,
  providersOfInterest,
} from "./provider-quota.ts";
import { generateTitleForSession } from "./session-title";
import {
  buildSkillInventory,
  mutateSkillEnabled,
  type SkillScope,
  type SkillTarget,
  withSettingsLock,
} from "./skill-inventory";

type ModelHealthStatus = "unknown" | "healthy" | "unhealthy";

type ModelHealth = {
  status: ModelHealthStatus;
  checkedAt?: string;
  latencyMs?: number;
  error?: string;
};

type ModelPreferencesFile = {
  visibility?: Record<string, boolean>;
  health?: Record<string, ModelHealth>;
  /**
   * One-time migration marker for the opt-in visibility flip: legacy files
   * predate "visible must be true", so their never-toggled models (which the
   * old default showed) are bulk-enabled exactly once. New files carry the
   * marker from the start, so models appearing later stay opt-in.
   */
  migratedOptIn?: boolean;
};

type CatalogModel = {
  provider?: string;
  id?: string;
  name?: string;
  contextWindow?: number;
  api?: string;
  baseUrl?: string;
  apiKey?: string;
};

type CatalogRegistry = {
  getAll: () => CatalogModel[];
  getAvailable: () => CatalogModel[] | Promise<CatalogModel[]>;
  getProviderAuthStatus: (provider: string) => {
    configured?: boolean;
    source?: string;
    label?: string;
  };
  getProviderDisplayName: (provider: string) => string;
  // The live registry resolves ModelsRefreshResult; the config plane never
  // inspects it, so the widest callable shape keeps ctx assignable.
  refresh: () => unknown;
  getApiKeyForProvider?: (provider: string) => Promise<string | undefined>;
  getApiKeyAndHeaders?: (model: CatalogModel) => Promise<{
    ok?: boolean;
    apiKey?: string;
  }>;
};

const MODEL_REGISTRY_REFRESH_TIMEOUT_MS = 2_000;

// One active Codex login per embedded pi process; the in-memory map is the
// sole operation registry (design §1) — unknown ids resolve to expired.
const oauthLoginManager = createOAuthLoginOperationManager();
// The config command runs inside one Pi process rather than one WebSocket
// connection. Request ids isolate WebView responses; this stable owner keeps
// the existing manager's owner check valid across start/cancel/status calls.
const oauthOwner = {
  readyState: 1,
  send: () => undefined,
  close: () => undefined,
  terminate: () => undefined,
  ping: () => undefined,
};

type ThinkingLevel = "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max";

type ConfigContext = {
  modelRegistry?: CatalogRegistry;
  cwd?: string;
  model?: unknown;
  sessionManager?: { getSessionFile: () => string | undefined };
  navigateTree?: (
    targetId: string,
    options?: {
      summarize?: boolean;
      customInstructions?: string;
      replaceInstructions?: boolean;
      label?: string;
    },
  ) => Promise<{ cancelled?: boolean } | undefined>;
  /** Stream an OAuth operation event to the initiating request's envelope. */
  oauthNotify?: (event: unknown) => void;
  isProjectTrusted?: () => boolean;
};

type ListedSession = { path?: string };

async function renameHistoricalSession(filePath: unknown, requestedName: unknown) {
  if (typeof filePath !== "string" || typeof requestedName !== "string") {
    throw new Error("Session path and name are required.");
  }
  const name = requestedName.trim();
  if (!name) throw new Error("Session name cannot be empty.");
  if ([...name].length > 200) throw new Error("Session name cannot exceed 200 characters.");
  if (path.extname(filePath).toLowerCase() !== ".jsonl") {
    throw new Error("Session is not available.");
  }
  let canonicalTarget: string;
  try {
    canonicalTarget = fs.realpathSync.native(filePath);
  } catch {
    throw new Error("Session is not available.");
  }
  const sessions = (await SessionManager.listAll()) as ListedSession[];
  const managed = sessions.find((session) => {
    if (typeof session.path !== "string") return false;
    try {
      return fs.realpathSync.native(session.path) === canonicalTarget;
    } catch {
      return false;
    }
  });
  if (!managed) throw new Error("Session is not available.");
  const manager = SessionManager.open(canonicalTarget);
  manager.appendSessionInfo(name);
  return { filePath: canonicalTarget, name };
}

type SkillInventoryMutation = {
  scope?: unknown;
  target?: unknown;
  enabled?: unknown;
};

type PackageSkillMutation = {
  scope?: unknown;
  target?: unknown;
  enabled?: unknown;
};

function parsePackageSkillTarget(value: unknown): {
  packageIdentity: string;
  relativePath: string;
} {
  if (!value || typeof value !== "object") throw new Error("Invalid package skill mutation");
  const target = value as { packageIdentity?: unknown; relativePath?: unknown };
  if (
    typeof target.packageIdentity !== "string" ||
    !target.packageIdentity ||
    typeof target.relativePath !== "string" ||
    !target.relativePath
  ) {
    throw new Error("Invalid package skill mutation");
  }
  return { packageIdentity: target.packageIdentity, relativePath: target.relativePath };
}

type ApiKeyCredential = { type: "api_key"; key: string };

type CredentialStoreLike = {
  modify?: (
    provider: string,
    fn: (current: unknown) => Promise<ApiKeyCredential | undefined>,
  ) => Promise<unknown>;
  delete?: (provider: string) => Promise<void>;
  read?: (provider: string) => Promise<unknown>;
};

type RegistryInternals = {
  runtime?: { credentials?: CredentialStoreLike };
  credentials?: CredentialStoreLike;
  authStorage?: {
    set?: (provider: string, value: ApiKeyCredential) => void | Promise<void>;
    remove?: (provider: string) => void | Promise<void>;
  };
};

export type PicotConfigResult = { ok: true; data?: unknown } | { ok: false; error: string };

function errMessage(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}

function resolveHomeDir(): string {
  const candidates: string[] = [];
  const add = (value?: string) => {
    if (typeof value === "string" && value.trim()) candidates.push(path.resolve(value.trim()));
  };
  add(process.env.HOME);
  add(process.env.USERPROFILE);
  if (process.env.HOMEDRIVE && process.env.HOMEPATH) {
    add(`${process.env.HOMEDRIVE}${process.env.HOMEPATH}`);
  }
  add(os.homedir());
  return candidates[0] || os.homedir();
}

function resolvePiAgentRoot(): string {
  // Pi's own agent directory is `PI_CODING_AGENT_DIR` when set (the Rust launch
  // resolver honors it too). Reading MCP configs or the trust store from a
  // different directory would make the page import the wrong globals.
  const fromEnv = process.env.PI_CODING_AGENT_DIR;
  if (typeof fromEnv === "string" && fromEnv.trim()) return path.resolve(fromEnv.trim());
  const candidates: string[] = [];
  const add = (value?: string) => {
    if (typeof value === "string" && value.trim()) candidates.push(path.resolve(value.trim()));
  };
  add(process.env.HOME);
  add(process.env.USERPROFILE);
  if (process.env.HOMEDRIVE && process.env.HOMEPATH) {
    add(`${process.env.HOMEDRIVE}${process.env.HOMEPATH}`);
  }
  add(os.homedir());
  for (const home of candidates) {
    const candidate = path.join(home, ".pi", "agent");
    if (fs.existsSync(candidate)) return candidate;
  }
  const appData = process.env.APPDATA;
  if (typeof appData === "string" && appData.trim()) {
    const roaming = path.join(path.resolve(appData), "pi", "agent");
    if (fs.existsSync(roaming)) return roaming;
  }
  return path.join(candidates[0] || os.homedir(), ".pi", "agent");
}

const HOME_DIR = resolveHomeDir();
const PI_AGENT_ROOT = resolvePiAgentRoot();
const MODELS_PREFS_PATH = path.join(PI_AGENT_ROOT, "picot-models.json");
const AGENT_CONFIG_PATH = path.join(PI_AGENT_ROOT, "settings.json");
const AGENTS_MD_PATH = path.join(PI_AGENT_ROOT, "AGENTS.md");
const APPEND_SYSTEM_MD_PATH = path.join(PI_AGENT_ROOT, "APPEND_SYSTEM.md");
const MODELS_CONFIG_PATH = path.join(PI_AGENT_ROOT, "models.json");
const CHAT_CONFIG_PATH = path.join(PI_AGENT_ROOT, "chat", "config.json");
const AUTH_CONFIG_PATH = path.join(PI_AGENT_ROOT, "auth.json");
const PROJECT_CONFIG_DIR_NAME = ".pi";
const THINKING_LEVELS = new Set<ThinkingLevel>([
  "off",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
]);

function modelPreferenceKey(provider: string, modelId: string): string {
  return `${provider}/${modelId}`;
}

function normalizeModelHealth(value: unknown): ModelHealth {
  if (!value || typeof value !== "object") return { status: "unknown" };
  const candidate = value as Partial<ModelHealth>;
  if (candidate.status !== "healthy" && candidate.status !== "unhealthy") {
    return { status: "unknown" };
  }
  const health: ModelHealth = {
    status: candidate.status,
    checkedAt: typeof candidate.checkedAt === "string" ? candidate.checkedAt : undefined,
    latencyMs: typeof candidate.latencyMs === "number" ? candidate.latencyMs : undefined,
  };
  if (typeof candidate.error === "string") health.error = candidate.error;
  return health;
}

function parseSkillScope(value: unknown): SkillScope {
  if (value === "global" || value === "project") return value;
  throw new Error("Invalid skill inventory scope");
}

function parseSkillTarget(value: unknown): SkillTarget {
  if (!value || typeof value !== "object") throw new Error("Invalid skill inventory mutation");
  const target = value as { kind?: unknown; id?: unknown };
  if (target.kind !== "skill" && target.kind !== "group") {
    throw new Error("Invalid skill inventory mutation");
  }
  if (typeof target.id !== "string" || target.id.length === 0) {
    throw new Error("Invalid skill inventory mutation");
  }
  return { kind: target.kind, id: target.id };
}

/** Host-issued launch marker naming the registry-verified project root. */
const MCP_PROJECT_ROOT_ENV = "PI_STUDIO_MCP_PROJECT_ROOT";

/**
 * MCP settings context. The project root is taken from the host launch marker
 * only: `ctx.cwd` is not authorization, because the landing/scratch runtime runs
 * in Picot's own temp directory. Missing or mismatched marker means global-only.
 */
function mcpContext(ctx: ConfigContext): McpSettingsContext {
  const projectRoot = resolveMcpProjectRoot(ctx);
  if (!projectRoot) return { agentDir: PI_AGENT_ROOT, projectRoot: null, projectTrusted: false };
  return {
    agentDir: PI_AGENT_ROOT,
    projectRoot,
    projectTrusted: isMcpProjectTrusted(ctx, projectRoot),
    // Re-read inside the write lock: a revocation that lands while the mutation
    // waits for the lock must reject instead of writing.
    verifyProject: () => {
      if (!isMcpProjectTrusted(ctx, projectRoot)) throw new Error(UNTRUSTED_PROJECT_ERROR);
    },
  };
}

function resolveMcpProjectRoot(ctx: ConfigContext): string | null {
  const marker = process.env[MCP_PROJECT_ROOT_ENV];
  const cwd = typeof ctx.cwd === "string" && ctx.cwd ? ctx.cwd : "";
  if (!marker || !cwd || !path.isAbsolute(marker) || !path.isAbsolute(cwd)) return null;
  try {
    // The host issues the marker as the already-canonical project root at spawn
    // (`pi_launch::canonical_project_root`), so the marker itself is the
    // authorization identity. Requiring it to still resolve to itself means a
    // directory that was renamed away, replaced by a symlink, or reached
    // through a swapped ancestor is rejected instead of being promoted into a
    // fresh permission for whatever the path points at now.
    if (fs.realpathSync(marker) !== marker) return null;
    // `ctx.cwd` may legitimately be a symlinked alias of the admitted root, so
    // it is still compared by realpath — but against the issued root, never
    // against a re-resolved marker.
    if (fs.realpathSync(cwd) !== marker) return null;
    return marker;
  } catch {
    return null;
  }
}

/**
 * Pi's saved decision is consulted again at request time and wins over a stale
 * in-process answer: a nearer explicit `false` withdraws the write permission,
 * while a parent `true` (or no entry at all, e.g. session-only trust) leaves the
 * runtime's own answer standing. An unreadable store fails closed.
 */
function isMcpProjectTrusted(ctx: ConfigContext, projectRoot: string): boolean {
  if (ctx.isProjectTrusted?.() !== true) return false;
  return nearestSavedTrust(projectRoot) !== false;
}

function nearestSavedTrust(projectRoot: string): boolean | null {
  const trustPath = path.join(PI_AGENT_ROOT, "trust.json");
  let raw: string;
  try {
    raw = fs.readFileSync(trustPath, "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
    return false;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return false;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return false;
  const decisions = parsed as Record<string, unknown>;
  let dir = projectRoot;
  for (;;) {
    const decision = decisions[dir];
    if (decision === true || decision === false) return decision;
    const parent = path.dirname(dir);
    if (parent === dir) return null;
    dir = parent;
  }
}

function skillInventoryOptions(scope: SkillScope, ctx: ConfigContext) {
  const cwd = typeof ctx.cwd === "string" && ctx.cwd ? ctx.cwd : process.cwd();
  return {
    scope,
    cwd,
    agentDir: PI_AGENT_ROOT,
    homeDir: HOME_DIR,
    projectTrusted: Boolean(ctx.isProjectTrusted?.()),
  };
}

function sanitizeHealthError(error: unknown): string {
  const raw = errMessage(error) || "Health check failed";
  return raw
    .replace(/sk-[A-Za-z0-9_-]{6,}/g, "[REDACTED]")
    .replace(/\bbearer\s+[A-Za-z0-9._~+/=-]{6,}/gi, "bearer [REDACTED]")
    .slice(0, 240);
}

class ModelPreferencesStore {
  readonly path: string;

  constructor(filePath = MODELS_PREFS_PATH) {
    this.path = filePath;
  }

  fileExisted(): boolean {
    return fs.existsSync(this.path);
  }

  read(): Required<ModelPreferencesFile> {
    if (!fs.existsSync(this.path)) return { visibility: {}, health: {}, migratedOptIn: false };
    try {
      const parsed = JSON.parse(fs.readFileSync(this.path, "utf8")) as ModelPreferencesFile;
      return {
        visibility:
          parsed.visibility &&
          typeof parsed.visibility === "object" &&
          !Array.isArray(parsed.visibility)
            ? parsed.visibility
            : {},
        health:
          parsed.health && typeof parsed.health === "object" && !Array.isArray(parsed.health)
            ? parsed.health
            : {},
        migratedOptIn: parsed.migratedOptIn === true,
      };
    } catch {
      return { visibility: {}, health: {}, migratedOptIn: false };
    }
  }

  write(next: Required<ModelPreferencesFile>): void {
    fs.mkdirSync(path.dirname(this.path), { recursive: true });
    fs.writeFileSync(this.path, JSON.stringify(next, null, 2), "utf8");
  }

  /**
   * Visibility is opt-in: a model appears in the composer picker only after
   * the user enabled it. Anything never toggled reads as hidden, so a provider
   * whose key is configured no longer floods the picker with every model it
   * lists (`available` only says the provider can run, not that it is wanted).
   */
  isVisible(provider: string, modelId: string): boolean {
    return this.read().visibility[modelPreferenceKey(provider, modelId)] === true;
  }

  /**
   * Run once per preferences file when the opt-in flip ships. A file that
   * predates the flip belongs to a user whose picker showed every
   * never-toggled model by default: bulk-enable the currently available set
   * so the upgrade does not empty their picker. A brand-new file only gets
   * the marker — models that appear later (new provider keys, new releases)
   * stay opt-in, which is the point of the flip.
   */
  migrateLegacyVisibility(models: { provider: string; id: string }[]): void {
    const legacyUser = this.fileExisted();
    const prefs = this.read();
    if (prefs.migratedOptIn) return;
    if (legacyUser) {
      for (const model of models) {
        const key = modelPreferenceKey(model.provider, model.id);
        if (prefs.visibility[key] === undefined) prefs.visibility[key] = true;
      }
    }
    prefs.migratedOptIn = true;
    this.write(prefs);
  }

  setVisibility(provider: string, modelId: string, visible: boolean): void {
    const prefs = this.read();
    prefs.visibility[modelPreferenceKey(provider, modelId)] = visible;
    this.write(prefs);
  }

  getHealth(provider: string, modelId: string): ModelHealth {
    return normalizeModelHealth(this.read().health[modelPreferenceKey(provider, modelId)]);
  }

  setHealth(provider: string, modelId: string, health: ModelHealth): void {
    const prefs = this.read();
    prefs.health[modelPreferenceKey(provider, modelId)] = normalizeModelHealth(health);
    this.write(prefs);
  }
}

// A short-lived cache around buildModelCatalog. Building the catalog re-probes
// every provider's auth/availability, and the composer re-reads it on each
// session switch and Settings > Models open; without the cache those probes
// repeat for a result that cannot have changed in the last few seconds.
const MODEL_CATALOG_TTL_MS = 3000;
let modelCatalogCache: {
  registry: CatalogRegistry;
  at: number;
  value: Awaited<ReturnType<typeof buildModelCatalog>>;
} | null = null;

/** Drop the cached catalog. Every write that can change its result calls this. */
function invalidateModelCatalogCache(): void {
  modelCatalogCache = null;
}

async function loadModelCatalog(
  registry: CatalogRegistry,
  preferences: ModelPreferencesStore,
): Promise<Awaited<ReturnType<typeof buildModelCatalog>>> {
  const now = Date.now();
  // The registry identity is part of the key: a different registry (a new
  // runtime context) must never be served the previous one's catalog.
  if (
    modelCatalogCache &&
    modelCatalogCache.registry === registry &&
    now - modelCatalogCache.at < MODEL_CATALOG_TTL_MS
  ) {
    return modelCatalogCache.value;
  }
  const value = await buildModelCatalog(registry, preferences);
  modelCatalogCache = { registry, at: Date.now(), value };
  return value;
}

/** Refresh the model registry and invalidate the catalog it feeds. */
async function refreshModelRegistry(registry?: CatalogRegistry | null): Promise<void> {
  if (!registry) return;
  await registry.refresh();
  invalidateModelCatalogCache();
}

async function buildModelCatalog(registry: CatalogRegistry, preferences: ModelPreferencesStore) {
  const allModels = registry.getAll();
  const availableModels = await registry.getAvailable();
  // The one-time opt-in migration needs the available set; run it before any
  // isVisible read below so a legacy user's first catalog already carries
  // their pre-flip defaults.
  preferences.migrateLegacyVisibility(
    availableModels
      .filter((model) => model.provider && model.id)
      .map((model) => ({
        provider: model.provider as string,
        id: model.id as string,
      })),
  );
  const availableKeys = new Set(
    availableModels
      .filter((model) => model.provider && model.id)
      .map((model) => modelPreferenceKey(model.provider as string, model.id as string)),
  );
  const providerNames = Array.from(
    new Set(allModels.map((model) => model.provider).filter(Boolean)),
  ).sort() as string[];

  return {
    providers: providerNames.map((providerName) => {
      const status = registry.getProviderAuthStatus(providerName);
      return {
        provider: providerName,
        displayName: registry.getProviderDisplayName(providerName),
        configured: Boolean(status.configured),
        source: status.source,
        label: status.label,
        models: allModels
          .filter(
            (model) =>
              model.provider === providerName &&
              model.id &&
              availableKeys.has(modelPreferenceKey(providerName, model.id as string)),
          )
          .sort((a, b) => String(a.id).localeCompare(String(b.id)))
          .map((model) => {
            const modelId = model.id as string;
            return {
              provider: providerName,
              id: modelId,
              name: model.name,
              contextWindow: model.contextWindow,
              available: availableKeys.has(modelPreferenceKey(providerName, modelId)),
              visible: preferences.isVisible(providerName, modelId),
              health: preferences.getHealth(providerName, modelId),
            };
          }),
      };
    }),
  };
}

function protocolFromModelApi(api: unknown): ProviderProtocol | null {
  const value = String(api || "").trim();
  if (value === "anthropic-messages") return "anthropic-messages";
  if (
    value === "openai-completions" ||
    value === "openai-responses" ||
    value === "azure-openai-responses" ||
    value === "mistral-conversations"
  ) {
    return "openai-completions";
  }
  return null;
}

function credentialKey(value: unknown): string | undefined {
  if (!value || typeof value !== "object") return undefined;
  const cred = value as { type?: string; key?: unknown; access?: unknown };
  if (cred.type === "api_key" && typeof cred.key === "string" && cred.key.trim()) {
    return cred.key.trim();
  }
  if (cred.type === "oauth" && typeof cred.access === "string" && cred.access.trim()) {
    return cred.access.trim();
  }
  return undefined;
}

async function resolveProviderApiKeyForHealthCheck(
  registry: CatalogRegistry,
  provider: string,
  model: CatalogModel,
): Promise<string | undefined> {
  if (typeof registry.getApiKeyForProvider === "function") {
    try {
      const key = await registry.getApiKeyForProvider(provider);
      if (typeof key === "string" && key.trim()) return key.trim();
    } catch {
      // fall through
    }
  }
  if (typeof registry.getApiKeyAndHeaders === "function") {
    try {
      const auth = await registry.getApiKeyAndHeaders(model);
      if (auth?.ok !== false && typeof auth?.apiKey === "string" && auth.apiKey.trim()) {
        return auth.apiKey.trim();
      }
    } catch {
      // fall through
    }
  }
  const internals = registry as CatalogRegistry & RegistryInternals;
  const store = internals.runtime?.credentials ?? internals.credentials;
  if (typeof store?.read === "function") {
    try {
      const key = credentialKey(await store.read(provider));
      if (key) return key;
    } catch {
      // fall through
    }
  }
  if (typeof model.apiKey === "string" && model.apiKey.trim()) return model.apiKey.trim();
  try {
    const key = credentialKey(readAuthConfig()[provider]);
    if (key) return key;
  } catch {
    // ignore
  }
  try {
    const parsed = JSON.parse(fs.readFileSync(MODELS_CONFIG_PATH, "utf8")) as {
      providers?: Record<string, { apiKey?: string }>;
    };
    const key = parsed.providers?.[provider]?.apiKey;
    if (typeof key === "string" && key.trim()) return key.trim();
  } catch {
    // ignore
  }
  return undefined;
}

function asProviderProtocol(value: unknown): ProviderProtocol {
  if (value === "openai-completions" || value === "anthropic-messages") return value;
  throw new Error("protocol must be openai-completions or anthropic-messages");
}

function parseProbeModels(params: Record<string, unknown>): ProbeModel[] {
  const modelsRaw = params.models;
  if (Array.isArray(modelsRaw)) {
    return modelsRaw
      .filter((item): item is Record<string, unknown> => Boolean(item) && typeof item === "object")
      .map((item) => ({
        id: typeof item.id === "string" ? item.id.trim() : "",
        ...(typeof item.name === "string" ? { name: item.name } : {}),
        ...(typeof item.contextWindow === "number" ? { contextWindow: item.contextWindow } : {}),
        ...(typeof item.maxTokens === "number" ? { maxTokens: item.maxTokens } : {}),
      }))
      .filter((model) => Boolean(model.id));
  }
  const modelIds = params.modelIds;
  if (!Array.isArray(modelIds)) return [];
  return modelIds
    .map((id) => (typeof id === "string" ? id.trim() : ""))
    .filter(Boolean)
    .map((id) => ({ id }));
}

async function runHttpModelHealthCheck(
  registry: CatalogRegistry,
  model: CatalogModel,
): Promise<{ ok: boolean; latencyMs: number; error?: string } | null> {
  const protocol = protocolFromModelApi(model.api);
  const baseUrl = typeof model.baseUrl === "string" ? model.baseUrl.trim() : "";
  const provider = typeof model.provider === "string" ? model.provider : "";
  const modelId = typeof model.id === "string" ? model.id : "";
  if (!protocol || !baseUrl || !provider || !modelId) return null;
  const apiKey = await resolveProviderApiKeyForHealthCheck(registry, provider, model);
  if (!apiKey) return null;
  const probe = await testProviderConnectivity({
    baseUrl,
    apiKey,
    protocol,
    modelId,
  });
  return {
    ok: probe.ok,
    latencyMs: probe.latencyMs,
    error: probe.ok
      ? undefined
      : probe.error || (probe.status ? `HTTP ${probe.status}` : "Health check failed"),
  };
}

async function runSessionModelHealthCheck(model: CatalogModel): Promise<{
  ok: boolean;
  error?: string;
}> {
  let sawAssistantText = false;
  const modelRuntime = await ModelRuntime.create();
  const { session } = await createAgentSession({
    model,
    tools: [],
    sessionManager: SessionManager.inMemory(),
    modelRuntime,
  } as Parameters<typeof createAgentSession>[0]);
  try {
    const unsubscribe = session.subscribe((event: unknown) => {
      const evt = event as {
        assistantMessageEvent?: { type?: string; delta?: string };
        message?: { content?: unknown };
      };
      if (
        evt.assistantMessageEvent?.type === "text_delta" &&
        typeof evt.assistantMessageEvent.delta === "string" &&
        evt.assistantMessageEvent.delta.length > 0
      ) {
        sawAssistantText = true;
      }
      const content = evt.message?.content;
      if (Array.isArray(content)) {
        for (const block of content) {
          if (
            block &&
            typeof block === "object" &&
            (block as { type?: string }).type === "text" &&
            typeof (block as { text?: unknown }).text === "string" &&
            (block as { text: string }).text.trim()
          ) {
            sawAssistantText = true;
          }
        }
      }
    });
    try {
      await session.prompt("Reply exactly: OK");
    } finally {
      unsubscribe();
    }
  } finally {
    session.dispose();
  }
  return {
    ok: sawAssistantText,
    error: sawAssistantText ? undefined : "No assistant text returned",
  };
}

async function runModelHealthCheck(
  registry: CatalogRegistry,
  model: CatalogModel,
  preferences: ModelPreferencesStore,
): Promise<{ provider: string; modelId: string } & ModelHealth> {
  const provider = model.provider as string;
  const modelId = model.id as string;
  const startedAt = Date.now();
  try {
    const httpProbe = await runHttpModelHealthCheck(registry, model);
    const probe = httpProbe ?? (await runSessionModelHealthCheck(model));
    const result: { provider: string; modelId: string } & ModelHealth = {
      provider,
      modelId,
      status: probe.ok ? "healthy" : "unhealthy",
      checkedAt: new Date().toISOString(),
      latencyMs: httpProbe?.latencyMs ?? Date.now() - startedAt,
      error: probe.ok ? undefined : sanitizeHealthError(probe.error || "Health check failed"),
    };
    preferences.setHealth(provider, modelId, result);
    invalidateModelCatalogCache();
    return result;
  } catch (e: unknown) {
    const result: { provider: string; modelId: string } & ModelHealth = {
      provider,
      modelId,
      status: "unhealthy",
      checkedAt: new Date().toISOString(),
      latencyMs: Date.now() - startedAt,
      error: sanitizeHealthError(e),
    };
    preferences.setHealth(provider, modelId, result);
    invalidateModelCatalogCache();
    return result;
  }
}

function readConfigFile(filePath: string, fallback: string): { content: string; path: string } {
  const content = fs.existsSync(filePath) ? fs.readFileSync(filePath, "utf8") : fallback;
  return { content, path: filePath };
}

function writeConfigFile(filePath: string, content: unknown): void {
  if (typeof content !== "string") throw new Error("content must be a string");
  try {
    JSON.parse(content); // validate before writing
  } catch (error) {
    throw new Error(
      `content is not valid JSON: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, content, "utf8");
}

// Plain-text counterpart of readConfigFile/writeConfigFile for agent-root
// markdown files (AGENTS.md / APPEND_SYSTEM.md). A missing file is not an
// error — it reads as empty content so the editor starts from a blank file.
function readTextFile(filePath: string): { content: string; path: string; exists: boolean } {
  const exists = fs.existsSync(filePath);
  const content = exists ? fs.readFileSync(filePath, "utf8") : "";
  return { content, path: filePath, exists };
}

function writeTextFile(filePath: string, content: unknown): void {
  if (typeof content !== "string") throw new Error("content must be a string");
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, content, "utf8");
}

/**
 * Copy the current config file to `<path>.bak` before an overwrite, so a bad
 * save can be rolled back. No-op when the file does not exist yet.
 */
function backupConfigFile(configPath: string): void {
  if (fs.existsSync(configPath)) {
    fs.copyFileSync(configPath, `${configPath}.bak`);
  }
}

async function refreshRegistryBestEffort(registry?: CatalogRegistry): Promise<boolean> {
  if (!registry) return false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const timeout = new Promise<false>((resolve) => {
      timer = setTimeout(() => resolve(false), MODEL_REGISTRY_REFRESH_TIMEOUT_MS);
      timer.unref?.();
    });
    const refresh = (async () => {
      await refreshModelRegistry(registry);
      return true;
    })().catch(() => false);
    return await Promise.race([refresh, timeout]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function readSettingsObject(filePath: string): Record<string, unknown> {
  if (!fs.existsSync(filePath)) return {};
  let parsed: unknown;
  try {
    parsed = JSON.parse(fs.readFileSync(filePath, "utf8"));
  } catch (error) {
    throw new Error(
      `Pi settings at ${filePath} must be valid JSON: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error(`Pi settings must be a JSON object: ${filePath}`);
  }
  return parsed as Record<string, unknown>;
}

function writeSettingsObject(filePath: string, settings: Record<string, unknown>): void {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  const temporary = path.join(
    path.dirname(filePath),
    `.picot-settings-${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}.tmp`,
  );
  try {
    fs.writeFileSync(temporary, `${JSON.stringify(settings, null, 2)}\n`, "utf8");
    fs.renameSync(temporary, filePath);
  } finally {
    try {
      if (fs.existsSync(temporary)) fs.rmSync(temporary, { force: true });
    } catch {
      // Best-effort cleanup only.
    }
  }
}

function asThinkingLevel(value: unknown): ThinkingLevel {
  const level = asString(value);
  if (THINKING_LEVELS.has(level as ThinkingLevel)) return level as ThinkingLevel;
  throw new Error(`Unsupported thinking level: ${level || String(value)}`);
}

function resolveSettingsPath(
  scope: unknown,
  ctx: ConfigContext,
): { scope: "global" | "project"; path: string } {
  const normalizedScope = asString(scope) || "global";
  if (normalizedScope === "global") return { scope: "global", path: AGENT_CONFIG_PATH };
  if (normalizedScope !== "project")
    throw new Error(`Unsupported settings scope: ${normalizedScope}`);
  const cwd = asString(ctx.cwd);
  if (!cwd) throw new Error("Project settings require an active workspace");
  if (ctx.isProjectTrusted && !ctx.isProjectTrusted()) {
    throw new Error("Project settings cannot be changed until the workspace is trusted");
  }
  return {
    scope: "project",
    path: path.join(cwd, PROJECT_CONFIG_DIR_NAME, "settings.json"),
  };
}

function getProjectSettings(
  ctx: ConfigContext,
): { path: string; settings: Record<string, unknown> } | null {
  const cwd = asString(ctx.cwd);
  if (!cwd || (ctx.isProjectTrusted && !ctx.isProjectTrusted())) return null;
  const settingsPath = path.join(cwd, PROJECT_CONFIG_DIR_NAME, "settings.json");
  return { path: settingsPath, settings: readSettingsObject(settingsPath) };
}

function getDefaultThinkingLevel(scope: unknown, ctx: ConfigContext) {
  const requestedScope = asString(scope) || "global";
  if (requestedScope === "project" || requestedScope === "effective") {
    const project = getProjectSettings(ctx);
    const projectValue = project?.settings.defaultThinkingLevel;
    if (typeof projectValue === "string" && THINKING_LEVELS.has(projectValue as ThinkingLevel)) {
      return { level: projectValue, source: "project", path: project.path };
    }
    if (requestedScope === "project") {
      const writableProject = resolveSettingsPath("project", ctx);
      return { level: "off", source: "pi_default", path: writableProject.path };
    }
  }
  const globalValue = readSettingsObject(AGENT_CONFIG_PATH).defaultThinkingLevel;
  if (typeof globalValue === "string" && THINKING_LEVELS.has(globalValue as ThinkingLevel)) {
    return { level: globalValue, source: "global", path: AGENT_CONFIG_PATH };
  }
  return { level: "off", source: "pi_default", path: AGENT_CONFIG_PATH };
}

async function setDefaultThinkingLevel(level: unknown, scope: unknown, ctx: ConfigContext) {
  const thinkingLevel = asThinkingLevel(level);
  const target = resolveSettingsPath(scope, ctx);
  // Same proper-lockfile protocol Pi and the skills page use: another window
  // or Pi process may hold the settings lock; a bare read-modify-write
  // would silently drop its concurrent update.
  await withSettingsLock(target.path, () => {
    const settings = readSettingsObject(target.path);
    settings.defaultThinkingLevel = thinkingLevel;
    writeSettingsObject(target.path, settings);
  });
  return { level: thinkingLevel, scope: target.scope, path: target.path };
}

function getCompactionEnabled(settings: Record<string, unknown>): boolean | undefined {
  const compaction = settings.compaction;
  if (!compaction || typeof compaction !== "object" || Array.isArray(compaction)) return undefined;
  const enabled = (compaction as Record<string, unknown>).enabled;
  return typeof enabled === "boolean" ? enabled : undefined;
}

function getDefaultAutoCompaction(scope: unknown, ctx: ConfigContext) {
  const requestedScope = asString(scope) || "global";
  if (requestedScope === "project" || requestedScope === "effective") {
    const project = getProjectSettings(ctx);
    const projectValue = project ? getCompactionEnabled(project.settings) : undefined;
    if (typeof projectValue === "boolean") {
      return { enabled: projectValue, source: "project", path: project?.path };
    }
    if (requestedScope === "project") {
      const writableProject = resolveSettingsPath("project", ctx);
      return { enabled: true, source: "pi_default", path: writableProject.path };
    }
  }
  const globalValue = getCompactionEnabled(readSettingsObject(AGENT_CONFIG_PATH));
  if (typeof globalValue === "boolean") {
    return { enabled: globalValue, source: "global", path: AGENT_CONFIG_PATH };
  }
  return { enabled: true, source: "pi_default", path: AGENT_CONFIG_PATH };
}

async function setDefaultAutoCompaction(enabled: unknown, scope: unknown, ctx: ConfigContext) {
  if (typeof enabled !== "boolean") throw new Error("enabled must be a boolean");
  const target = resolveSettingsPath(scope, ctx);
  await withSettingsLock(target.path, () => {
    const settings = readSettingsObject(target.path);
    const existing = settings.compaction;
    const compaction =
      existing && typeof existing === "object" && !Array.isArray(existing)
        ? { ...(existing as Record<string, unknown>) }
        : {};
    compaction.enabled = enabled;
    settings.compaction = compaction;
    writeSettingsObject(target.path, settings);
  });
  return { enabled, scope: target.scope, path: target.path };
}

// pi 0.99+ enables codemode through the `defaultTools` setting with `+name`
// merge tokens (`docs/cli.md` "Enable codemode"). The toggle manages the
// global default only, merge-style: it never replaces the rest of the list.
const CODEMODE_TOKENS = new Set(["codemode", "+codemode", "-codemode"]);

function getDefaultCodemode() {
  const settings = readSettingsObject(AGENT_CONFIG_PATH);
  const list = settings.defaultTools;
  if (!Array.isArray(list)) {
    return { enabled: false, source: "pi_default", path: AGENT_CONFIG_PATH };
  }
  const tokens = list.filter((token): token is string => typeof token === "string");
  return {
    enabled: tokens.includes("+codemode") || tokens.includes("codemode"),
    source: "global",
    path: AGENT_CONFIG_PATH,
  };
}

async function setDefaultCodemode(enabled: unknown) {
  if (typeof enabled !== "boolean") throw new Error("enabled must be a boolean");
  await withSettingsLock(AGENT_CONFIG_PATH, () => {
    const settings = readSettingsObject(AGENT_CONFIG_PATH);
    const current = Array.isArray(settings.defaultTools)
      ? settings.defaultTools.filter((token) => typeof token === "string")
      : [];
    const withoutCodemode = current.filter((token) => !CODEMODE_TOKENS.has(token));
    const next = enabled ? [...withoutCodemode, "+codemode"] : withoutCodemode;
    if (next.length > 0) settings.defaultTools = next;
    else delete settings.defaultTools;
    writeSettingsObject(AGENT_CONFIG_PATH, settings);
  });
  return { enabled, path: AGENT_CONFIG_PATH };
}

function asString(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function readEnabledModels(settings: Record<string, unknown>): string[] {
  return Array.isArray(settings.enabledModels)
    ? settings.enabledModels.filter(
        (model): model is string => typeof model === "string" && model.trim().length > 0,
      )
    : [];
}

function scopedModelId(pattern: string): string {
  const suffixIndex = pattern.lastIndexOf(":");
  return suffixIndex === -1 ? pattern : pattern.slice(0, suffixIndex);
}

async function setScopedModel(provider: unknown, modelId: unknown, enabled: unknown) {
  const normalizedProvider = asString(provider);
  const normalizedModelId = asString(modelId);
  if (!normalizedProvider || !normalizedModelId)
    throw new Error("provider and modelId are required");
  if (typeof enabled !== "boolean") throw new Error("enabled must be a boolean");
  const reference = `${normalizedProvider}/${normalizedModelId}`;
  let models: string[] = [];
  await withSettingsLock(AGENT_CONFIG_PATH, () => {
    const settings = readSettingsObject(AGENT_CONFIG_PATH);
    const current = readEnabledModels(settings);
    const withoutModel = current.filter((pattern) => scopedModelId(pattern) !== reference);
    models = enabled ? [...withoutModel, reference] : withoutModel;
    if (models.length > 0) settings.enabledModels = models;
    else delete settings.enabledModels;
    writeSettingsObject(AGENT_CONFIG_PATH, settings);
  });
  return {
    provider: normalizedProvider,
    modelId: normalizedModelId,
    enabled,
    modelIds: models.map(scopedModelId),
  };
}

function readAuthConfig(): Record<string, unknown> {
  if (!fs.existsSync(AUTH_CONFIG_PATH)) return {};
  let parsed: unknown;
  try {
    parsed = JSON.parse(fs.readFileSync(AUTH_CONFIG_PATH, "utf8"));
  } catch {
    return {};
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
  return parsed as Record<string, unknown>;
}

function writeAuthConfig(auth: Record<string, unknown>): void {
  fs.mkdirSync(path.dirname(AUTH_CONFIG_PATH), { recursive: true });
  fs.writeFileSync(AUTH_CONFIG_PATH, JSON.stringify(auth, null, 2), {
    encoding: "utf8",
    mode: 0o600,
  });
  try {
    fs.chmodSync(AUTH_CONFIG_PATH, 0o600);
  } catch {
    // chmod is best-effort on platforms/filesystems that do not support POSIX modes.
  }
}

async function setStoredApiKey(
  registry: CatalogRegistry | undefined,
  provider: string,
  apiKey: string,
): Promise<void> {
  const internals = registry as (CatalogRegistry & RegistryInternals) | undefined;
  const credentials = internals?.runtime?.credentials;
  if (credentials?.modify) {
    await credentials.modify(provider, async () => ({ type: "api_key", key: apiKey }));
    return;
  }
  if (internals?.authStorage?.set) {
    await internals.authStorage.set(provider, { type: "api_key", key: apiKey });
    return;
  }
  const auth = readAuthConfig();
  auth[provider] = { type: "api_key", key: apiKey };
  writeAuthConfig(auth);
}

async function removeStoredApiKey(
  registry: CatalogRegistry | undefined,
  provider: string,
): Promise<void> {
  const internals = registry as (CatalogRegistry & RegistryInternals) | undefined;
  const credentials = internals?.runtime?.credentials;
  if (credentials?.delete) {
    await credentials.delete(provider);
    return;
  }
  if (internals?.authStorage?.remove) {
    await internals.authStorage.remove(provider);
    return;
  }
  if (!fs.existsSync(AUTH_CONFIG_PATH)) return;
  const auth = readAuthConfig();
  delete auth[provider];
  writeAuthConfig(auth);
}

// Dispatch a single Configuration operation. `ctx` is the extension command
// context; `ctx.modelRegistry` provides live provider/model/auth access.
// ─── Provider quota surface (spec 2026-09-22) ──────────────────────────────

const quotaProbeCache = createQuotaProbeCache();

function readModelsJsonProviders(): Record<string, { baseUrl?: string; apiKey?: string }> {
  try {
    const parsed = JSON.parse(fs.readFileSync(MODELS_CONFIG_PATH, "utf8")) as {
      providers?: Record<string, Record<string, unknown>>;
    };
    const out: Record<string, { baseUrl?: string; apiKey?: string }> = {};
    for (const [id, entry] of Object.entries(parsed?.providers ?? {})) {
      out[id] = {
        baseUrl: typeof entry?.baseUrl === "string" ? entry.baseUrl : undefined,
        apiKey: typeof entry?.apiKey === "string" ? entry.apiKey : undefined,
      };
    }
    return out;
  } catch {
    return {};
  }
}

/** Provider candidates for quota probes, taken from pi's own provider list.
 * The model catalog is deliberately NOT the source: it only knows providers
 * that expose models with a baseUrl, so providers whose models are described
 * elsewhere (codex / opencode-go / zai-coding-cn) were missing from the probe
 * set while an unconfigured provider that did carry a baseUrl was probed. */
type QuotaProviderRuntime = {
  getProviders?: () => readonly { id?: string; baseUrl?: string }[];
  getRegisteredProviderIds?: () => readonly string[];
  getProvider?: (providerId: string) => { baseUrl?: string } | undefined;
  getModels?: (providerId?: string) => readonly { baseUrl?: string }[];
  hasConfiguredAuth?: (providerId: string) => boolean;
};

function quotaProviderCandidates(runtime: QuotaProviderRuntime): {
  providers: Array<{ providerId: string; baseUrl?: string }>;
} {
  const providerIds = new Set<string>(runtime.getRegisteredProviderIds?.() ?? []);
  for (const provider of runtime.getProviders?.() ?? []) {
    if (provider?.id) providerIds.add(provider.id);
  }
  // No credential filter here: `hasConfiguredAuth` is true for every api-key
  // provider (it reports an auth *mechanism*), so it never excluded anything.
  // Configuration is settled by the probe, which reports `not_configured` when
  // no credential resolves and the UI then hides that provider.
  return {
    providers: [...providerIds].map((providerId) => ({
      providerId,
      baseUrl:
        runtime.getProvider?.(providerId)?.baseUrl ?? runtime.getModels?.(providerId)?.[0]?.baseUrl,
    })),
  };
}

async function resolveQuotaInstance(
  providerId: string,
  baseUrl: string | undefined,
  modelsJsonProviders: Record<string, { apiKey?: string }>,
): Promise<ProviderInstance | null> {
  if (providerId === "openai-codex") {
    // OAuth: pi owns refresh; a failing refresh reports needs_login.
    const runtime = await ModelRuntime.create();
    try {
      const auth = await runtime.getAuth(providerId);
      const header =
        (auth?.auth?.headers as Record<string, string> | undefined)?.Authorization ??
        (auth?.auth?.headers as Record<string, string> | undefined)?.authorization;
      const accessToken =
        (typeof auth?.auth?.apiKey === "string" && auth.auth.apiKey) ||
        (header?.startsWith("Bearer ") ? header.slice(7) : undefined) ||
        undefined;
      if (!accessToken) return null;
      const stored = readStoredCredential(providerId) as
        | { access?: string; accountId?: unknown }
        | undefined;
      const accountId = typeof stored?.accountId === "string" ? stored.accountId : undefined;
      return {
        providerId,
        baseUrl: originOfBaseUrl(baseUrl) || "https://chatgpt.com",
        accessToken,
        accountId,
      };
    } catch {
      return null;
    }
  }
  // api-key providers. pi's own resolver is the authority and is env-aware —
  // a key may live only in the shell environment (the host syncs the login
  // shell into the embedded pi), so auth.json/models.json are the fallbacks,
  // not the only sources.
  let resolvedKey: string | undefined;
  try {
    const runtime = await ModelRuntime.create();
    const auth = await runtime.getAuth(providerId);
    if (typeof auth?.auth?.apiKey === "string" && auth.auth.apiKey) {
      resolvedKey = auth.auth.apiKey;
    }
  } catch {
    // Fall through to the explicit stores below.
  }
  const stored = readStoredCredential(providerId) as { key?: string } | undefined;
  const apiKey =
    resolvedKey ||
    (typeof stored?.key === "string" && stored.key) ||
    modelsJsonProviders[providerId]?.apiKey;
  if (!apiKey) return null;
  // Origin, not the full baseUrl: the specs append their own canonical path.
  return { providerId, baseUrl: originOfBaseUrl(baseUrl), apiKey };
}

export async function handlePicotConfig(
  op: string,
  params: Record<string, unknown>,
  ctx: ConfigContext,
): Promise<PicotConfigResult> {
  const registry = ctx.modelRegistry;
  const preferences = new ModelPreferencesStore();

  const requireRegistry = (): CatalogRegistry => {
    if (!registry) throw new Error("Model registry not ready yet — try again in a moment.");
    return registry;
  };

  try {
    switch (op) {
      case "write_paste_offload": {
        const content = params.content;
        if (typeof content !== "string") throw new Error("content is required");
        if (!ctx.cwd) throw new Error("Active workspace is required");
        const result = writePasteOffloadFile(ctx.cwd, content);
        return { ok: true, data: { path: result.relativePath } };
      }
      case "get_oauth_login_capabilities": {
        const runtime = await ModelRuntime.create();
        const adapter = createPiOAuthLoginAdapter(runtime);
        const capability = await adapter.getCodexCapability();
        if (capability.kind !== "supported") {
          // Unsupported / unavailable providers report an empty list rather
          // than a synthesized capability (baseline protocol rule).
          return { ok: true, data: { providers: [] } };
        }
        // ModelRuntime.checkAuth returns AuthCheck | undefined; an AuthCheck
        // means Pi holds a usable credential regardless of method.
        const configured = Boolean(await runtime.checkAuth("openai-codex"));
        return {
          ok: true,
          data: { providers: [{ providerId: "openai-codex", deviceCode: true, configured }] },
        };
      }

      case "start_oauth_login": {
        const provider = asString(params.provider);
        const method = asString(params.method);
        if (provider !== "openai-codex" || method !== "device_code") {
          throw new Error("Unsupported OAuth provider or method");
        }
        if (!ctx.oauthNotify) throw new Error("OAuth event channel is unavailable");
        const started = oauthLoginManager.start(oauthOwner, 0, "openai-codex");
        const emit = (event: OAuthOperationEvent) => ctx.oauthNotify?.(event);
        // Fire-and-forget: the response resolves immediately with the
        // operation id; device-code/progress/terminal events stream over the
        // config notify channel afterwards.
        void (async () => {
          const runtime = await ModelRuntime.create();
          const adapter = createPiOAuthLoginAdapter(runtime);
          let expiryTimer: ReturnType<typeof setTimeout> | null = null;
          const clearExpiryTimer = () => {
            if (expiryTimer) {
              clearTimeout(expiryTimer);
              expiryTimer = null;
            }
          };
          try {
            await adapter.startCodexDeviceCodeLogin(
              {
                onDeviceCode: (code) => {
                  try {
                    emit(oauthLoginManager.bindDeviceCode(oauthOwner, started.operationId, code));
                    if (code.expiresInSeconds && code.expiresInSeconds > 0) {
                      clearExpiryTimer();
                      expiryTimer = setTimeout(() => {
                        expiryTimer = null;
                        try {
                          emit(oauthLoginManager.expire(oauthOwner, started.operationId));
                        } catch {
                          // Operation already terminal.
                        }
                      }, code.expiresInSeconds * 1000);
                    }
                  } catch {
                    // Operation already terminal; nothing to emit.
                  }
                },
                onProgress: (message) => {
                  try {
                    emit(oauthLoginManager.bindProgress(oauthOwner, started.operationId, message));
                  } catch {
                    // Operation already terminal; nothing to emit.
                  }
                },
              },
              started.signal,
            );
            clearExpiryTimer();
            try {
              emit(oauthLoginManager.complete(oauthOwner, started.operationId));
              await refreshModelRegistry(registry);
            } catch {
              // Already removed (cancelled/expired) — nothing to complete.
            }
          } catch (error) {
            clearExpiryTimer();
            const aborted = (error as Error | null)?.name === "AbortError";
            try {
              const event = aborted
                ? oauthLoginManager.cancel(oauthOwner, started.operationId)
                : oauthLoginManager.fail(oauthOwner, started.operationId, error);
              if (event) emit(event);
            } catch {
              // Already terminal; nothing to emit.
            }
          }
        })().catch((error) => {
          console.warn("[picot-config] OAuth login chain error:", error);
        });
        return {
          ok: true,
          data: { operationId: started.operationId, provider: "openai-codex", state: "starting" },
        };
      }

      case "cancel_oauth_login": {
        const operationId = asString(params.operationId);
        if (!operationId) throw new Error("operationId is required");
        // Unknown ids are a tolerated no-op (map wiped by restart/reload);
        // the UI treats them as expired per design §5.
        const cancelled = oauthLoginManager.cancel(oauthOwner, operationId);
        if (cancelled) ctx.oauthNotify?.(cancelled);
        return { ok: true, data: { operationId } };
      }

      case "get_oauth_login_status": {
        const operationId = asString(params.operationId);
        if (!operationId) throw new Error("operationId is required");
        return { ok: true, data: oauthLoginManager.getStatus(oauthOwner, operationId) };
      }

      case "oauth_logout": {
        const provider = asString(params.provider);
        // Same codex-only whitelist as start_oauth_login (design §3): the
        // op surface never forwards another provider to runtime.logout().
        if (provider !== "openai-codex") throw new Error("Unsupported OAuth provider");
        const runtime = await ModelRuntime.create();
        await runtime.logout(provider);
        await refreshModelRegistry(registry);
        return { ok: true, data: { provider } };
      }

      case "navigate_tree": {
        const targetId = asString(params.targetId);
        if (!targetId) throw new Error("targetId is required");
        if (typeof ctx.navigateTree !== "function") {
          throw new Error("Session tree navigation is unavailable.");
        }
        const result = await ctx.navigateTree(targetId, {
          summarize: params.summarize === true,
          ...(typeof params.customInstructions === "string"
            ? { customInstructions: params.customInstructions }
            : {}),
          ...(typeof params.replaceInstructions === "boolean"
            ? { replaceInstructions: params.replaceInstructions }
            : {}),
          ...(typeof params.label === "string" ? { label: params.label } : {}),
        });
        return { ok: true, data: result ?? { cancelled: false } };
      }
      case "rename_historical_session": {
        const result = await renameHistoricalSession(params.filePath, params.name);
        return { ok: true, data: result };
      }
      case "generate_session_title": {
        const sessionFile = ctx.sessionManager?.getSessionFile();
        if (!sessionFile) throw new Error("The active session has not been saved yet.");
        const modelRuntime = await ModelRuntime.create();
        const title = await generateTitleForSession(sessionFile, {
          model: ctx.model,
          modelRuntime,
        });
        return { ok: true, data: { title } };
      }
      case "list_model_catalog": {
        const catalog = await loadModelCatalog(requireRegistry(), preferences);
        return { ok: true, data: catalog };
      }

      case "provider_quota_report": {
        const force = params.force === true;
        const modelsJsonProviders = readModelsJsonProviders();
        const runtime = await ModelRuntime.create();
        const candidates = quotaProviderCandidates(runtime);
        const picked = providersOfInterest({ ...candidates, modelsJsonProviders });
        const baseUrlById = new Map(
          candidates.providers.map((entry) => [entry.providerId, entry.baseUrl]),
        );
        const reports = await Promise.all(
          picked.map(async ({ providerId, spec }) => {
            const baseUrl = baseUrlById.get(providerId) ?? modelsJsonProviders[providerId]?.baseUrl;
            const instance = await resolveQuotaInstance(providerId, baseUrl, modelsJsonProviders);
            return quotaProbeCache.report(providerId, spec, instance, force);
          }),
        );
        return { ok: true, data: { reports } };
      }

      case "codex_reset_credits_inspect": {
        const instance = await resolveQuotaInstance("openai-codex", "https://chatgpt.com", {});
        if (!instance?.accessToken) {
          return { ok: true, data: { failure: "needs_login", credits: [] } };
        }
        const result = await inspectCodexResetCredits({
          accessToken: instance.accessToken,
          accountId: instance.accountId,
        });
        return { ok: true, data: { credits: result?.credits ?? [] } };
      }

      case "codex_reset_credits_consume": {
        const operationId = asString(params.operationId);
        if (!operationId) throw new Error("operationId is required");
        const instance = await resolveQuotaInstance("openai-codex", "https://chatgpt.com", {});
        if (!instance?.accessToken) {
          return { ok: true, data: { failure: "needs_login" } };
        }
        const result = await consumeCodexResetCredit({
          operationId,
          accessToken: instance.accessToken,
          accountId: instance.accountId,
        });
        if (!result.ok) {
          const failure = (result as { ok: false; error: string }).error;
          return { ok: true, data: { failure } };
        }
        // The consume changed the upstream quota; force a fresh probe.
        quotaProbeCache.invalidate("openai-codex");
        return {
          ok: true,
          data: { code: result.code, availableCount: result.availableCount ?? null },
        };
      }

      case "set_model_visibility": {
        const provider = asString(params.provider);
        const modelId = asString(params.modelId);
        if (!provider || !modelId) throw new Error("provider and modelId are required");
        const visible = params.visible === true;
        preferences.setVisibility(provider, modelId, visible);
        invalidateModelCatalogCache();
        return { ok: true, data: { provider, modelId, visible } };
      }

      case "list_scoped_models": {
        const models = readEnabledModels(readSettingsObject(AGENT_CONFIG_PATH));
        return { ok: true, data: { modelIds: models.map(scopedModelId) } };
      }

      case "set_scoped_model":
        return {
          ok: true,
          data: await setScopedModel(params.provider, params.modelId, params.enabled),
        };

      case "check_model_health": {
        const reg = requireRegistry();
        const provider = asString(params.provider);
        const modelId = asString(params.modelId);
        if (!provider) throw new Error("provider is required");
        const availableKeys = new Set(
          (await reg.getAvailable())
            .filter((model) => model.provider && model.id)
            .map((model) => modelPreferenceKey(model.provider as string, model.id as string)),
        );
        const models = reg.getAll().filter((model) => {
          if (model.provider !== provider || !model.id) return false;
          if (modelId) return model.id === modelId;
          return availableKeys.has(modelPreferenceKey(provider, model.id as string));
        });
        if (models.length === 0) throw new Error("No matching models available for health check");
        // Parallel: N models used to cost N x the probe latency in series.
        const results = await Promise.all(
          models.map((model) => runModelHealthCheck(reg, model, preferences)),
        );
        return { ok: true, data: { results } };
      }

      case "set_api_key": {
        const provider = asString(params.provider);
        const apiKey = asString(params.apiKey);
        if (!provider) throw new Error("provider is required");
        if (!apiKey) throw new Error("apiKey is required");
        await setStoredApiKey(registry, provider, apiKey);
        await refreshModelRegistry(registry);
        return { ok: true, data: { provider } };
      }

      case "remove_api_key": {
        const provider = asString(params.provider);
        if (!provider) throw new Error("provider is required");
        await removeStoredApiKey(registry, provider);
        await refreshModelRegistry(registry);
        return { ok: true, data: { provider } };
      }

      case "list_package_skill_inventory": {
        const scope = parseSkillScope(params.scope);
        return {
          ok: true,
          data: buildPackageSkillInventory(skillInventoryOptions(scope, ctx)),
        };
      }

      case "list_skill_inventory": {
        const scope = parseSkillScope(params.scope);
        return { ok: true, data: buildSkillInventory(skillInventoryOptions(scope, ctx)) };
      }

      case "set_skill_enabled": {
        const mutation = params as SkillInventoryMutation;
        const scope = parseSkillScope(mutation.scope);
        const target = parseSkillTarget(mutation.target);
        if (typeof mutation.enabled !== "boolean") {
          throw new Error("Invalid skill inventory mutation");
        }
        const result = await mutateSkillEnabled({
          ...skillInventoryOptions(scope, ctx),
          target,
          enabled: mutation.enabled,
        });
        return { ok: true, data: result };
      }

      case "set_package_skill_enabled": {
        const mutation = params as PackageSkillMutation;
        const scope = parseSkillScope(mutation.scope);
        const target = parsePackageSkillTarget(mutation.target);
        if (typeof mutation.enabled !== "boolean") {
          throw new Error("Invalid package skill mutation");
        }
        const result = await mutatePackageSkillEnabled({
          ...skillInventoryOptions(scope, ctx),
          ...target,
          enabled: mutation.enabled,
        });
        return { ok: true, data: result };
      }

      case "read_agent_config":
        return { ok: true, data: readConfigFile(AGENT_CONFIG_PATH, "{}") };

      case "write_agent_config": {
        writeConfigFile(AGENT_CONFIG_PATH, params.content);
        return { ok: true, data: { path: AGENT_CONFIG_PATH } };
      }

      // Global agent context / system-prompt append file read/write. AGENTS.md
      // is injected as global context instructions and APPEND_SYSTEM.md is
      // appended to the system prompt without replacing it (pi docs "System
      // Prompt Files"); project-level .pi/ files are workspace files and are
      // edited through the workspace file browser instead. Markdown is plain
      // text, so no JSON validation applies.
      case "read_agents_md":
        return { ok: true, data: readTextFile(AGENTS_MD_PATH) };

      case "write_agents_md": {
        writeTextFile(AGENTS_MD_PATH, params.content);
        return { ok: true, data: { path: AGENTS_MD_PATH } };
      }

      case "read_append_system_md":
        return { ok: true, data: readTextFile(APPEND_SYSTEM_MD_PATH) };

      case "write_append_system_md": {
        writeTextFile(APPEND_SYSTEM_MD_PATH, params.content);
        return { ok: true, data: { path: APPEND_SYSTEM_MD_PATH } };
      }

      case "get_default_thinking_level":
        return { ok: true, data: getDefaultThinkingLevel(params.scope, ctx) };

      case "set_default_thinking_level":
        return { ok: true, data: await setDefaultThinkingLevel(params.level, params.scope, ctx) };

      case "get_default_auto_compaction":
        return { ok: true, data: getDefaultAutoCompaction(params.scope, ctx) };

      case "get_default_codemode":
        return { ok: true, data: getDefaultCodemode() };

      case "mcp_list_servers":
        return { ok: true, data: listMcpServers(mcpContext(ctx)) };

      case "mcp_save_server":
        return { ok: true, data: await saveMcpServer(params, mcpContext(ctx)) };

      case "mcp_delete_server":
        return { ok: true, data: await deleteMcpServer(params, mcpContext(ctx)) };

      case "mcp_toggle_server":
        return { ok: true, data: await toggleMcpServer(params, mcpContext(ctx)) };

      // One batch snapshot of the global file's effective enabled/exposure/
      // toolExposure values. Connection fields and credentials never move.
      case "mcp_import_global_overrides":
        return { ok: true, data: await importGlobalMcpOverrides(mcpContext(ctx)) };

      // User-confirmed legacy mcp.json → mcp-adapter.json copy (the list op
      // only detects; it never writes).
      case "mcp_migrate_adapter_config":
        return {
          ok: true,
          data: await migrateAdapterConfig(params, mcpContext(ctx)),
        };

      case "advisor.config.get":
        return { ok: true, data: await advisorConfigGet(requireRegistry()) };

      case "advisor.config.set":
        return { ok: true, data: advisorConfigSet(params) };

      case "planMode.config.get":
        return { ok: true, data: await planModeConfigGet(requireRegistry()) };

      case "planMode.config.set":
        return { ok: true, data: planModeConfigSet(params) };

      case "safetyGuard.config.get":
        return { ok: true, data: safetyGuardConfigGet() };

      case "safetyGuard.config.set":
        return { ok: true, data: safetyGuardConfigSet(params) };

      case "webaccess.config.get":
        return { ok: true, data: webAccessConfigGet() };

      case "webaccess.config.set":
        return { ok: true, data: webAccessConfigSet(params) };

      case "set_default_auto_compaction":
        return {
          ok: true,
          data: await setDefaultAutoCompaction(params.enabled, params.scope, ctx),
        };

      case "read_models_config":
        return { ok: true, data: readConfigFile(MODELS_CONFIG_PATH, '{\n  "providers": {}\n}\n') };

      case "set_default_codemode":
        return { ok: true, data: await setDefaultCodemode(params.enabled) };

      case "write_models_config": {
        const content = params.content;
        if (typeof content !== "string") throw new Error("content must be a string");
        const parsed = JSON.parse(content);
        if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
          throw new Error("models.json must be a JSON object");
        }
        if (
          "providers" in parsed &&
          (typeof parsed.providers !== "object" || Array.isArray(parsed.providers))
        ) {
          throw new Error("'providers' must be an object");
        }
        // Keep a safety copy of the previous models.json so a bad save can be
        // rolled back; the frontend can restore it if the new content breaks.
        backupConfigFile(MODELS_CONFIG_PATH);
        writeConfigFile(MODELS_CONFIG_PATH, content);
        const refreshed = await refreshRegistryBestEffort(registry);
        return { ok: true, data: { path: MODELS_CONFIG_PATH, refreshed } };
      }

      case "detect_custom_provider": {
        const preferredRaw = asString(params.preferred) || "auto";
        const preferred =
          preferredRaw === "openai-completions" || preferredRaw === "anthropic-messages"
            ? preferredRaw
            : "auto";
        const result = await detectProviderProtocol({
          baseUrl: asString(params.baseUrl),
          apiKey: asString(params.apiKey),
          preferred,
        });
        return { ok: true, data: result };
      }

      case "list_custom_provider_models": {
        const listed = await fetchUpstreamModels({
          baseUrl: asString(params.baseUrl),
          apiKey: asString(params.apiKey),
          protocol: asProviderProtocol(params.protocol),
        });
        return { ok: true, data: listed };
      }

      case "test_custom_provider": {
        const result = await testProviderConnectivity({
          baseUrl: asString(params.baseUrl),
          apiKey: asString(params.apiKey),
          protocol: asProviderProtocol(params.protocol),
          modelId: asString(params.modelId) || undefined,
        });
        return { ok: true, data: result };
      }

      case "save_custom_provider": {
        const protocol = asProviderProtocol(params.protocol);
        const baseUrl = normalizeBaseUrl(asString(params.baseUrl));
        const providerId = resolveProviderId(asString(params.providerId), baseUrl);
        const models = parseProbeModels(params);
        if (models.length === 0) throw new Error("Select at least one model");
        const apiKey = asString(params.apiKey);
        const storeKey = params.storeKey !== false;
        const includeApiKeyInFile = params.includeApiKeyInFile === true;
        if (storeKey && !apiKey) throw new Error("apiKey is required when storeKey is true");
        const entry = buildModelsJsonProviderEntry({
          baseUrl,
          protocol,
          models,
          apiKey,
          includeApiKeyInFile,
        });
        let existing: unknown = { providers: {} };
        if (fs.existsSync(MODELS_CONFIG_PATH)) {
          try {
            existing = JSON.parse(fs.readFileSync(MODELS_CONFIG_PATH, "utf8"));
          } catch {
            existing = { providers: {} };
          }
        }
        const merged = mergeProviderIntoModelsJson(
          existing as ModelsJsonDocument,
          providerId,
          entry,
        );
        writeConfigFile(MODELS_CONFIG_PATH, `${JSON.stringify(merged, null, 2)}\n`);
        let keyStored = false;
        if (storeKey && apiKey) {
          await setStoredApiKey(registry, providerId, apiKey);
          keyStored = true;
        }
        const refreshed = await refreshRegistryBestEffort(registry);
        return {
          ok: true,
          data: {
            providerId,
            baseUrl,
            protocol,
            modelCount: models.length,
            keyStored,
            refreshed,
            path: MODELS_CONFIG_PATH,
          },
        };
      }

      case "read_chat_config":
        return { ok: true, data: readConfigFile(CHAT_CONFIG_PATH, "{}") };

      case "write_chat_config": {
        writeConfigFile(CHAT_CONFIG_PATH, params.content);
        return { ok: true, data: { path: CHAT_CONFIG_PATH } };
      }

      case "open_external": {
        const url = asString(params.url);
        if (!url) throw new Error("url is required");
        openExternal(url);
        return { ok: true };
      }

      default:
        return { ok: false, error: `Unknown configuration operation: ${op}` };
    }
  } catch (e: unknown) {
    return { ok: false, error: errMessage(e) };
  }
}

function openExternal(url: string): void {
  const platform = process.platform;
  const [command, args] =
    platform === "darwin"
      ? ["open", [url]]
      : platform === "win32"
        ? ["cmd", ["/c", "start", "", url]]
        : ["xdg-open", [url]];
  try {
    execFile(command, args, () => {});
  } catch {
    // Best-effort; frontend falls back to window.open.
  }
}
