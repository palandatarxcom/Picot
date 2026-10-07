// ABOUTME: Pure parser, root resolver, bounded recursive walker, and ranker for @-file mentions.
// ABOUTME: Hosted exclusively by the loopback-only GET /api/file-mentions endpoint; never shells out.
import { realpath, stat } from "node:fs/promises";
import { homedir } from "node:os";
import { join as hostJoin, relative as hostRelative } from "node:path";

export type FileMentionCandidate = {
  value: string;
  label: string;
  description: string;
  isDirectory: boolean;
};

export type FileMentionSearchResult = {
  items: FileMentionCandidate[];
  truncated: boolean;
};

/** Thrown for inputs that cannot be parsed into a mention query. Carries no server paths. */
export class InvalidMentionQueryError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "InvalidMentionQueryError";
  }
}

/** Directory basenames that are never surfaced and never recursed into during v1 traversal. */
const IGNORED_DIR_NAMES: ReadonlySet<string> = new Set([
  ".git",
  "node_modules",
  "dist",
  "build",
  "target",
  ".next",
  ".nuxt",
  ".cache",
  "coverage",
  ".venv",
  "venv",
  "__pycache__",
]);

const MAX_ENTRIES = 10_000;
const TIME_BUDGET_MS = 500;
const MAX_CANDIDATES = 20;

type SearchPlatform = "win32" | NodeJS.Platform;

type ReadableDirent = {
  name: string;
  isDirectory(): boolean;
  isSymbolicLink(): boolean;
};

export type SearchOptions = {
  workspaceRoot: string;
  query: string;
  platform?: NodeJS.Platform;
  homeDir?: string;
  now?: () => number;
  readDir?: (dir: string) => Promise<ReadableDirent[]>;
  signal?: AbortSignal;
};

export type ParsedMention = {
  isQuoted: boolean;
  body: string;
  displayBase: string;
  query: string;
  baseDir: string;
  ignored: boolean;
};

function isDriveBase(displayBase: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(displayBase);
}

/**
 * Resolve the filesystem base directory for a parsed display base. Absolute,
 * home, Windows drive, and UNC forms are assigned as-is so their display
 * spelling is preserved; workspace-relative forms are joined to the workspace.
 */
function resolveBaseDir(
  displayBase: string,
  opts: { workspaceRoot: string; homeDir: string; platform: SearchPlatform },
): string {
  if (displayBase === "~/" || displayBase === "~") {
    return opts.homeDir;
  }
  if (displayBase.startsWith("~/")) {
    return hostJoin(opts.homeDir, displayBase.slice(2));
  }
  if (opts.platform === "win32") {
    if (displayBase.startsWith("//")) return displayBase;
    if (isDriveBase(displayBase)) return displayBase;
    if (displayBase.startsWith("/")) {
      throw new InvalidMentionQueryError("Bare absolute path is not supported on Windows");
    }
  }
  if (displayBase.startsWith("/")) return displayBase;
  return hostJoin(opts.workspaceRoot, displayBase);
}

/**
 * Parse a raw mention query (including the leading `@` and any quotes) into its
 * filesystem base, the display prefix to preserve, the fuzzy query segment, and
 * whether the typed path explicitly entered an ignored directory.
 */
export function parseMentionQuery(
  query: string,
  opts: { workspaceRoot: string; homeDir: string; platform?: NodeJS.Platform },
): ParsedMention {
  if (typeof query !== "string" || query.length === 0 || query[0] !== "@") {
    throw new InvalidMentionQueryError("Mention query must begin with @");
  }
  let rest = query.slice(1);
  let isQuoted = false;
  if (rest.startsWith('"')) {
    isQuoted = true;
    rest = rest.length >= 2 && rest.endsWith('"') ? rest.slice(1, -1) : rest.slice(1);
  }
  const body = rest.replace(/\\/g, "/");
  if (body.includes("\0")) {
    throw new InvalidMentionQueryError("Mention query contains a NUL byte");
  }
  const platform: SearchPlatform = opts.platform ?? process.platform;
  if (platform === "win32" && body.startsWith("/") && !body.startsWith("//")) {
    throw new InvalidMentionQueryError("Bare absolute path is not supported on Windows");
  }

  let displayBase: string;
  let fuzzy: string;
  if (body === "" || body === ".") {
    displayBase = "";
    fuzzy = "";
  } else if (body === "~" || body === "~/") {
    displayBase = "~/";
    fuzzy = "";
  } else if (body === "/") {
    displayBase = "/";
    fuzzy = "";
  } else {
    const lastSlash = body.lastIndexOf("/");
    if (lastSlash === -1) {
      displayBase = "";
      fuzzy = body;
    } else {
      displayBase = body.slice(0, lastSlash + 1);
      fuzzy = body.slice(lastSlash + 1);
    }
  }

  const segments = body.split("/").filter((segment) => segment.length > 0 && segment !== ".");
  const ignored = segments.some((segment) => IGNORED_DIR_NAMES.has(segment));

  const baseDir = resolveBaseDir(displayBase, {
    workspaceRoot: opts.workspaceRoot,
    homeDir: opts.homeDir,
    platform,
  });

  return { isQuoted, body, displayBase, query: fuzzy, baseDir, ignored };
}

function posixBasename(path: string): string {
  const parts = path.split("/");
  return parts[parts.length - 1] || path;
}

function scoreEntry(relPath: string, fuzzy: string, isDirectory: boolean): number {
  if (fuzzy === "") return isDirectory ? 11 : 1;
  const name = posixBasename(relPath).toLowerCase();
  const q = fuzzy.toLowerCase();
  let base: number;
  if (name === q) base = 100;
  else if (name.startsWith(q)) base = 80;
  else if (name.includes(q)) base = 50;
  else if (relPath.toLowerCase().includes(q)) base = 30;
  else return 0;
  return isDirectory ? base + 10 : base;
}

function buildCandidate(
  displayPath: string,
  isDirectory: boolean,
  isQuoted: boolean,
): FileMentionCandidate {
  const valuePath = isDirectory ? `${displayPath}/` : displayPath;
  const name = posixBasename(displayPath);
  const needsQuotes = isQuoted || valuePath.includes(" ");
  const value = needsQuotes ? `@"${valuePath}"` : `@${valuePath}`;
  return {
    value,
    label: `${name}${isDirectory ? "/" : ""}`,
    description: displayPath,
    isDirectory,
  };
}

/**
 * Search a mention query under its resolved base. Traversal is bounded by the
 * fixed entry/time budgets; results are ranked and capped at 20 candidates.
 * Inaccessible or missing bases yield an empty list rather than an error.
 */
export async function searchFileMentions(options: SearchOptions): Promise<FileMentionSearchResult> {
  const platform: SearchPlatform = options.platform ?? process.platform;
  const homeDir = options.homeDir ?? homedir();
  const now = options.now ?? (() => Date.now());
  const readDir =
    options.readDir ??
    (async (dir: string) => {
      const { readdir } = await import("node:fs/promises");
      // SAFETY: readdir(withFileTypes) returns node's Dirent structs; ReadableDirent
      // is this extension's narrower structural view of that same data.
      return (await readdir(dir, { withFileTypes: true })) as unknown as ReadableDirent[];
    });

  const parsed = parseMentionQuery(options.query, {
    workspaceRoot: options.workspaceRoot,
    homeDir,
    platform,
  });
  if (parsed.ignored) return { items: [], truncated: false };

  let realBase: string;
  try {
    realBase = await realpath(parsed.baseDir);
  } catch {
    return { items: [], truncated: false };
  }
  try {
    const info = await stat(realBase);
    if (!info.isDirectory()) return { items: [], truncated: false };
  } catch {
    return { items: [], truncated: false };
  }

  const collected: { rel: string; displayPath: string; isDirectory: boolean; score: number }[] = [];
  let truncated = false;
  let entryCount = 0;
  const startedAt = now();

  const walk = async (dir: string): Promise<void> => {
    if (options.signal?.aborted) return;
    if (now() - startedAt >= TIME_BUDGET_MS) {
      truncated = true;
      return;
    }
    let dirents: ReadableDirent[];
    try {
      dirents = await readDir(dir);
    } catch {
      return;
    }
    for (const entry of dirents) {
      if (options.signal?.aborted) return;
      if (entryCount >= MAX_ENTRIES) {
        truncated = true;
        return;
      }
      entryCount += 1;
      if (now() - startedAt >= TIME_BUDGET_MS) {
        truncated = true;
        return;
      }
      const { name } = entry;
      if (IGNORED_DIR_NAMES.has(name)) continue;
      const isSymlink = entry.isSymbolicLink();
      const isDirectory = entry.isDirectory();
      const fullPath = hostJoin(dir, name);
      // A symlink surfaces as a leaf only when its target is readable; broken
      // or inaccessible links are skipped rather than presented as candidates.
      if (isSymlink) {
        try {
          await stat(fullPath);
        } catch {
          continue;
        }
      }
      const rel = hostRelative(realBase, fullPath).replace(/\\/g, "/");
      const score = scoreEntry(rel, parsed.query, isDirectory);
      if (score > 0) {
        collected.push({ rel, displayPath: parsed.displayBase + rel, isDirectory, score });
      }
      if (isDirectory && !isSymlink) {
        await walk(fullPath);
      }
    }
  };

  await walk(realBase);

  collected.sort((a, b) => b.score - a.score || (a.rel < b.rel ? -1 : a.rel > b.rel ? 1 : 0));
  const items = collected
    .slice(0, MAX_CANDIDATES)
    .map((entry) => buildCandidate(entry.displayPath, entry.isDirectory, parsed.isQuoted));
  return { items, truncated };
}

/** Typed outcome of a `/api/file-mentions` request, mapped before any HTTP write. */
export type FileMentionRouteOutcome =
  | { status: 200; body: FileMentionSearchResult }
  | { status: 400; code: "invalidMentionQuery" }
  | { status: 409; code: "workspaceChanged" }
  | { status: 503; code: "workspaceUnavailable" };

/**
 * Pure policy layer over {@link searchFileMentions}: binds a browser-supplied
 * root to the authoritative main-session workspace and maps errors to status
 * codes. The route adapter canonicalizes `requestedRoot` before calling.
 */
export async function handleFileMentionRequest(input: {
  authoritativeRoot: string | null;
  requestedRoot: string;
  query: string;
  signal?: AbortSignal;
}): Promise<FileMentionRouteOutcome> {
  if (!input.authoritativeRoot) {
    return { status: 503, code: "workspaceUnavailable" };
  }
  if (input.requestedRoot !== input.authoritativeRoot) {
    return { status: 409, code: "workspaceChanged" };
  }
  try {
    const body = await searchFileMentions({
      workspaceRoot: input.authoritativeRoot,
      query: input.query,
      signal: input.signal,
    });
    return { status: 200, body };
  } catch (error) {
    if (error instanceof InvalidMentionQueryError) {
      return { status: 400, code: "invalidMentionQuery" };
    }
    throw error;
  }
}
