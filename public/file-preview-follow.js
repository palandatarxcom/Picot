// ABOUTME: Opens or refreshes the file preview when the agent writes a file.
// ABOUTME: Maps write-tool arguments onto workspace-relative preview paths.

import { normalizeLocalPath } from "./workspace/path-utils.js";

const WRITE_TOOLS = new Set(["write", "edit", "apply_patch", "str_replace", "search_replace"]);

export function isWriteTool(toolName) {
  return WRITE_TOOLS.has(String(toolName || "").toLowerCase());
}

export function isSubagentTool(toolName) {
  return String(toolName || "").toLowerCase() === "subagent";
}

export function pathFromToolArgs(args) {
  if (!args || typeof args !== "object") return "";
  for (const key of ["path", "file_path", "filePath"]) {
    const value = args[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return "";
}

/**
 * The successful write paths of one tool call. A line-based multi-file `edit`
 * carries only the script in its args — its files live on the result's
 * `details.files` — so the live follow and the history rebuild both read the
 * paths from here instead of guessing from the script text. A failed result
 * reports nothing; blank or non-string paths and duplicates are dropped,
 * keeping first-write order.
 */
export function writePathsFromToolCall(args, result) {
  if (result?.isError) return [];
  const paths = [];
  for (const file of Array.isArray(result?.details?.files) ? result.details.files : []) {
    const path = typeof file?.path === "string" ? file.path.trim() : "";
    if (path && !paths.includes(path)) paths.push(path);
  }
  if (!paths.length) {
    const single = pathFromToolArgs(args);
    if (single) paths.push(single);
  }
  return paths;
}

// A root-anchored path needs no cwd to be attributed; a relative one does.
const ABSOLUTE_PATH = /^(?:[A-Za-z]:[\\/]|\/|\\\\)/;

/**
 * The written paths a successful subagent fan-out declares in its structured
 * acceptance report (`details.results[].acceptance.childReport.changedFiles`).
 * Only exitCode-0 child results contribute; natural-language summaries, the
 * echoed mission and toolCalls are never parsed. A child agent runs in its own
 * cwd: when that differs from the main workspace its relative paths describe
 * another tree, so they are dropped instead of being resolved against — and
 * misattributed to — this workspace. Absolute paths still pass for the
 * existing workspace containment to judge.
 */
export function subagentChangedFiles(result, subagentCwd, workspaceRoot) {
  if (result?.isError) return [];
  const cwd = normalizeLocalPath(subagentCwd);
  const root = normalizeLocalPath(workspaceRoot);
  const elsewhere = Boolean(cwd && root && cwd !== root);
  const paths = [];
  for (const entry of Array.isArray(result?.details?.results) ? result.details.results : []) {
    if (entry?.exitCode !== 0) continue;
    const changed = entry?.acceptance?.childReport?.changedFiles;
    for (const raw of Array.isArray(changed) ? changed : []) {
      const path = typeof raw === "string" ? raw.trim() : "";
      if (!path || (elsewhere && !ABSOLUTE_PATH.test(path))) continue;
      if (!paths.includes(path)) paths.push(path);
    }
  }
  return paths;
}

/**
 * Turn a tool path into the workspace-absolute normalized path that
 * FilePreviewPanel.openFile expects. Returning the absolute form (instead of
 * stripping the workspace root) keeps the follow/reveal path identical to the
 * id a workspace browser-open produces, so a write reloads the live preview
 * tab instead of creating a duplicate relative one. Absolute paths outside the
 * workspace are rejected; a bare relative path is joined to the workspace root.
 */
export function toPreviewPath(rawPath, workspaceRoot = "") {
  const normalized = normalizeLocalPath(rawPath);
  if (!normalized) return "";
  const root = normalizeLocalPath(workspaceRoot);
  if (root) {
    if (normalized === root) return "";
    const prefix = root.endsWith("/") ? root : `${root}/`;
    if (normalized.startsWith(prefix)) return normalized;
    if (
      normalized.startsWith("/") ||
      /^[A-Za-z]:\//.test(normalized) ||
      normalized.startsWith("//")
    ) {
      return "";
    }
    return `${root}/${normalized}`;
  }
  if (
    normalized.startsWith("/") ||
    /^[A-Za-z]:\//.test(normalized) ||
    normalized.startsWith("//")
  ) {
    return "";
  }
  return normalized;
}

export function shouldFollowWrite(event, pendingPath = "") {
  if (!event || event.isError) return false;
  if (!writePathsFromToolCall(event.args, event.result).length && !pendingPath) return false;
  if (event.toolName && !isWriteTool(event.toolName) && !pendingPath) return false;
  return true;
}

export function createFilePreviewFollow({ panel, getWorkspacePath, onWriteApplied } = {}) {
  const pending = new Map();

  // The cwd a fan-out child ran in, so its relative changedFiles are never
  // resolved against — and misattributed to — this workspace.
  const pendingSubagentCwd = new Map();

  async function workspaceRoot() {
    return (
      (typeof getWorkspacePath === "function" ? await getWorkspacePath() : "") ||
      panel?.workspaceRoot ||
      ""
    );
  }

  async function resolvePreviewPath(rawPath) {
    return toPreviewPath(rawPath, await workspaceRoot());
  }

  return {
    onToolStart(event) {
      if (isSubagentTool(event?.toolName)) {
        if (event.toolCallId) pendingSubagentCwd.set(event.toolCallId, event.args?.cwd);
        return;
      }
      if (!isWriteTool(event?.toolName)) return;
      const path = pathFromToolArgs(event.args);
      if (path && event.toolCallId) pending.set(event.toolCallId, path);
    },

    async onToolEnd(event) {
      const remembered = event?.toolCallId ? pending.get(event.toolCallId) : "";
      if (event?.toolCallId) pending.delete(event.toolCallId);
      // The start event is the authoritative fan-out signal (the end event of
      // some transports drops toolName), so a registered cwd also opens it.
      const subagent = isSubagentTool(event?.toolName) || pendingSubagentCwd.has(event?.toolCallId);
      if (subagent) {
        const cwd = event?.toolCallId ? pendingSubagentCwd.get(event.toolCallId) : undefined;
        if (event?.toolCallId) pendingSubagentCwd.delete(event.toolCallId);
        // A failed call is untrusted even when a child report claims success.
        if (event?.isError) return null;
        // A fan-out reports many files and has no single "the" file: record
        // each one (inside the workspace) but never open a preview.
        const files = subagentChangedFiles(event?.result, cwd, await workspaceRoot());
        for (const path of files) {
          const previewPath = await resolvePreviewPath(path);
          if (previewPath && typeof onWriteApplied === "function") {
            onWriteApplied(path, previewPath);
          }
        }
        return null;
      }
      if (!shouldFollowWrite(event, remembered)) return null;
      // Revealing stays the single-path behavior: a line-based multi-file edit
      // reports its files on the result and has no single "the" file to open.
      const revealTarget = pathFromToolArgs(event?.args) || remembered;
      const paths = writePathsFromToolCall(event?.args, event?.result);
      if (!paths.length && revealTarget) paths.push(revealTarget);
      let revealPath = "";
      for (const path of paths) {
        const previewPath = await resolvePreviewPath(path);
        if (!previewPath) continue;
        // Record the write as soon as it is known to be inside the workspace —
        // BEFORE the panel reveal. The reveal can be slow (tab open, native
        // pane attach), and the turn's files card settles on agent_end: a
        // write that only lands after that drain silently loses its card.
        if (typeof onWriteApplied === "function") onWriteApplied(path, previewPath);
        if (path === revealTarget && !revealPath) revealPath = previewPath;
      }
      if (!revealPath) return null;
      return panel ? panel.revealWrite(revealPath) : null;
    },

    async openPath(rawPath) {
      if (!panel) return null;
      const previewPath = await resolvePreviewPath(rawPath);
      if (!previewPath) return null;
      return panel.openFile(previewPath);
    },

    clear() {
      pending.clear();
      pendingSubagentCwd.clear();
    },
  };
}
