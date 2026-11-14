// ABOUTME: Files panel — a lazy workspace tree with inline create/rename, one
// ABOUTME: right-click menu per target kind, and drag-to-composer mentions.

import { createFileTypeIcon } from "../file-type-icons.js";
import { onLocaleChange, t } from "../i18n.js";
import { createIcon } from "../icons.js";
import { closeContextMenu, showContextMenu } from "./file-context-menu.js";
import {
  depthOf,
  flattenFileTree,
  isWithinPath,
  normalizeListingEntries,
  parentOf,
  parseExpandedStorage,
  ROOT_PATH,
  trimExpandedForStorage,
} from "./file-tree.js";
import { displayLocalPath, normalizeLocalPath } from "./path-utils.js";

/** Movement that turns a mousedown into a drag rather than a click. */
const DRAG_THRESHOLD_PX = 4;

/**
 * Root listing failures tolerated before this workspace's tree state is
 * discarded. Failures only arrive from explicit actions (open, refresh, expand,
 * retry), so a single transient blip is cleared by the next success; three in a
 * row means the workspace really is gone.
 */
const ROOT_UNREACHABLE_LIMIT = 3;

const EXPANDED_STORAGE_PREFIX = "picot-file-tree-expanded:";

function joinLocalPath(base, relative) {
  const normalizedBase = normalizeLocalPath(base);
  const normalizedRelative = normalizeLocalPath(relative);
  if (!normalizedBase) return normalizedRelative;
  if (!normalizedRelative) return normalizedBase;
  return `${normalizedBase.replace(/\/$/, "")}/${normalizedRelative.replace(/^\//, "")}`;
}

function joinRelativePath(parentPath, name) {
  return parentPath && parentPath !== ROOT_PATH ? `${parentPath}/${name}` : name;
}

function basenameOf(relativePath) {
  const normalized = normalizeLocalPath(relativePath);
  return normalized.slice(normalized.lastIndexOf("/") + 1);
}

function storage() {
  try {
    return globalThis.window?.localStorage ?? null;
  } catch {
    // Storage can be unavailable (private mode, blocked origin). The tree still
    // works; it just forgets which directories were open.
    return null;
  }
}

export class FileBrowser {
  constructor(container, pathEl, messageInput, options = {}) {
    this.container = container;
    this.pathEl = pathEl;
    this.messageInput = messageInput;
    this.onFileSelect = options.onFileSelect || null;
    this.onShowHiddenChange = options.onShowHiddenChange || null;
    // Native reveal/open is a host control, never an HTTP route: an ephemeral or
    // host-origin window has no Pi HTTP server to POST `/api/open` to.
    this.openPath = options.openPath || null;
    this.listFiles = options.listFiles || null;
    this.createEntry = options.createEntry || null;
    this.renameEntry = options.renameEntry || null;
    this.deleteEntry = options.deleteEntry || null;
    // Write availability is a host capability, not a panel preference.
    this.writesAvailable = options.writesAvailable || null;
    this.onMutabilityChange = options.onMutabilityChange || null;

    this.showHidden = false;
    this.workspaceRoot = "";
    this.directoryListings = new Map();
    this.expandedPaths = new Set();
    this.selectedPath = null;
    this.pendingListingPaths = new Set();
    this.pendingEdit = null;
    this.mutationInFlight = false;
    this.rootUnreachableStreak = 0;
    this.staleWorkspace = false;
    this.loadSequence = 0;
    this.fileStatus = null;
    this.fileErrorText = null;

    // Item interactions are delegated to the container — one set of listeners
    // total instead of one per rendered row.
    this.container.addEventListener("click", (e) => this.onItemClick(e));
    this.container.addEventListener("dblclick", (e) => this.onItemDoubleClick(e));
    this.container.addEventListener("mousedown", (e) => this.onItemMouseDown(e));
    this.container.addEventListener("contextmenu", (e) => this.onItemContextMenu(e));
    this.container.addEventListener("keydown", (e) => this.onItemKeyDown(e));

    onLocaleChange(() => {
      closeContextMenu();
      this.render();
    });
  }

  // ── State ──────────────────────────────────────────────────────────────────

  setWorkspaceRoot(path = "") {
    const hadShownHidden = this.showHidden;
    this.showHidden = false;
    if (hadShownHidden) this.notifyShowHiddenChange();
    const normalized = normalizeLocalPath(path);
    const changed = normalized !== this.workspaceRoot;
    this.workspaceRoot = normalized;
    // Invalidate any in-flight listing so a stale reply cannot repopulate the
    // tree we just reset.
    this.loadSequence++;
    this.directoryListings.clear();
    this.expandedPaths.clear();
    this.pendingListingPaths.clear();
    this.selectedPath = null;
    this.pendingEdit = null;
    this.rootUnreachableStreak = 0;
    this.staleWorkspace = false;
    this.fileStatus = null;
    this.fileErrorText = null;
    if (changed) closeContextMenu();
    this.pathEl.textContent = displayLocalPath(normalized);
    this.pathEl.title = normalized;
    this.render();
    this.notifyMutability();
  }

  /** Load the workspace root, then the directories that were open last time. */
  async load() {
    if (!this.workspaceRoot) {
      this.render();
      return;
    }
    await this.loadDirectory(ROOT_PATH);
    await this.restorePersistedExpanded();
  }

  /** Re-fetch every directory the tree is currently showing. */
  async refresh() {
    if (!this.workspaceRoot) return;
    const loaded = [...this.directoryListings.keys()];
    this.directoryListings.clear();
    this.fileStatus = null;
    this.render();
    await this.loadDirectory(ROOT_PATH);
    const rest = loaded
      .filter((path) => path !== ROOT_PATH)
      .sort((left, right) => depthOf(left) - depthOf(right));
    for (const path of rest) {
      if (!this.expandedPaths.has(path)) continue;
      await this.loadDirectory(path);
    }
  }

  setShowHidden(value) {
    const next = Boolean(value);
    if (next === this.showHidden) return undefined;
    this.showHidden = next;
    this.notifyShowHiddenChange();
    // Filtering is a render concern: the listing already carries dotfiles, so
    // toggling this never costs a round trip.
    this.render();
    return undefined;
  }

  notifyShowHiddenChange() {
    this.onShowHiddenChange?.(this.showHidden);
  }

  async loadDirectory(relativePath) {
    if (!this.workspaceRoot) return;
    if (this.pendingListingPaths.has(relativePath)) return;
    if (typeof this.listFiles !== "function") {
      this.recordListingFailure(relativePath, "file_access_failed");
      return;
    }
    this.pendingListingPaths.add(relativePath);
    const sequence = this.loadSequence;
    this.render();
    try {
      const data = await this.listFiles(relativePath);
      if (sequence !== this.loadSequence) return;
      if (data?.error) {
        this.recordListingFailure(relativePath, data.errorCode || "file_access_failed");
        return;
      }
      this.directoryListings.set(relativePath, {
        entries: normalizeListingEntries(data, this.workspaceRoot),
        loadedAtMs: Date.now(),
        failed: false,
        transient: false,
      });
      this.noteRootReachable();
      if (!this.workspaceRoot && typeof data?.path === "string") {
        this.workspaceRoot = normalizeLocalPath(data.path);
      }
    } catch (error) {
      if (sequence !== this.loadSequence) return;
      this.recordListingFailure(relativePath, error?.code || "file_access_failed");
    } finally {
      this.pendingListingPaths.delete(relativePath);
      this.render();
    }
  }

  recordListingFailure(relativePath, code) {
    // The machine code is the only signal that distinguishes admission
    // rejections (workspace scope) from IO failures; surface it or field
    // debugging has nothing to go on.
    console.error("[Files] listing failed:", { path: relativePath, code });
    const transient = code === "temporarily_unavailable";
    const previous = this.directoryListings.get(relativePath);
    if (relativePath !== ROOT_PATH) {
      this.directoryListings.set(relativePath, {
        entries: previous?.entries ?? [],
        loadedAtMs: previous?.loadedAtMs ?? 0,
        failed: true,
        transient,
      });
      this.render();
      return;
    }
    if (!transient) {
      this.fileStatus = "failed";
      this.fileErrorText = code;
      this.render();
      return;
    }
    // Unreachable is not deleted: keep the cache, the expansion, and the open
    // preview tabs; say "stale" and offer a retry.
    this.staleWorkspace = true;
    this.rootUnreachableStreak += 1;
    if (this.rootUnreachableStreak >= ROOT_UNREACHABLE_LIMIT) {
      this.directoryListings.clear();
      this.expandedPaths.clear();
      this.selectedPath = null;
      this.clearPersistedExpanded();
      this.rootUnreachableStreak = 0;
    }
    this.render();
  }

  noteRootReachable() {
    this.rootUnreachableStreak = 0;
    this.staleWorkspace = false;
    this.fileStatus = null;
    this.fileErrorText = null;
  }

  retryListing(relativePath) {
    this.directoryListings.delete(relativePath);
    if (relativePath === ROOT_PATH) {
      // Show the attempt rather than the banner: a retry that silently keeps
      // the old "stale" message reads as a dead button.
      this.fileStatus = null;
      this.staleWorkspace = false;
    }
    this.render();
    void this.loadDirectory(relativePath);
  }

  async setDirectoryExpanded(relativePath, expanded) {
    if (expanded) {
      this.expandedPaths.add(relativePath);
      this.writePersistedExpanded();
      this.render();
      if (!this.directoryListings.has(relativePath)) await this.loadDirectory(relativePath);
      return;
    }
    for (const path of [...this.expandedPaths]) {
      if (isWithinPath(path, relativePath)) this.expandedPaths.delete(path);
    }
    // The listing cache stays in memory, so re-expanding costs no I/O.
    this.writePersistedExpanded();
    this.render();
  }

  setSelected(relativePath) {
    if (this.selectedPath === relativePath) return;
    this.selectedPath = relativePath;
    this.render();
  }

  // ── Persistence ────────────────────────────────────────────────────────────

  storageKey() {
    return `${EXPANDED_STORAGE_PREFIX}${this.workspaceRoot}`;
  }

  readPersistedExpanded() {
    const store = storage();
    if (!store) return [];
    try {
      return parseExpandedStorage(store.getItem(this.storageKey()));
    } catch {
      return [];
    }
  }

  writePersistedExpanded() {
    const store = storage();
    if (!store) return;
    try {
      store.setItem(
        this.storageKey(),
        JSON.stringify(trimExpandedForStorage([...this.expandedPaths])),
      );
    } catch {
      // A full or blocked quota is not worth failing a tree operation over.
    }
  }

  clearPersistedExpanded() {
    const store = storage();
    if (!store) return;
    try {
      store.removeItem(this.storageKey());
    } catch {
      // See writePersistedExpanded.
    }
  }

  /**
   * Re-open the directories this workspace had open, depth-first from the root.
   * A path that no longer exists, is no longer a directory, or is no longer
   * permitted is dropped from storage rather than retried forever.
   */
  async restorePersistedExpanded() {
    const persisted = this.readPersistedExpanded().sort(
      (left, right) => depthOf(left) - depthOf(right),
    );
    if (persisted.length === 0) return;
    for (const path of persisted) {
      const parent = parentOf(path);
      if (parent === null) continue;
      const parentListing = this.directoryListings.get(parent);
      if (!parentListing) continue;
      const entry = parentListing.entries.find((candidate) => candidate.path === path);
      if (!entry?.isDirectory) continue;
      this.expandedPaths.add(path);
      await this.loadDirectory(path);
    }
    this.writePersistedExpanded();
    this.render();
  }

  // ── Rendering ──────────────────────────────────────────────────────────────

  render() {
    const hadFocus = this.container.contains(document.activeElement);
    this.container.textContent = "";

    if (this.fileStatus) {
      this.appendStatus(this.statusText());
      return;
    }
    if (!this.workspaceRoot) return;

    if (this.staleWorkspace) this.container.append(this.buildStaleBanner());

    const rows = flattenFileTree(this.directoryListings, this.expandedPaths, this.showHidden);
    if (rows.length === 0 && !this.pendingEdit) {
      // The banner above already says what is wrong; a spinner on top of it
      // would claim work that is not happening.
      if (this.staleWorkspace) return;
      this.appendStatus(t(this.directoryListings.has(ROOT_PATH) ? "files.empty" : "files.loading"));
      return;
    }

    const fragment = document.createDocumentFragment();
    for (const row of rows) fragment.append(this.buildRow(row));
    this.container.append(fragment);
    this.renderEditRow();
    this.syncRowFocus(hadFocus);
  }

  buildRow({ entry, depth, expanded, loading, failed }) {
    const selected = entry.path === this.selectedPath;
    const el = document.createElement("div");
    el.className = [
      "file-item",
      entry.isDirectory ? "directory" : "",
      selected ? "selected" : "",
      failed ? "failed" : "",
    ]
      .filter(Boolean)
      .join(" ");
    el.setAttribute("role", "treeitem");
    el.setAttribute("aria-level", String(depth + 1));
    el.setAttribute("aria-selected", String(selected));
    el.setAttribute("tabindex", selected ? "0" : "-1");
    el.draggable = false;
    el.dataset.path = entry.path;
    el.dataset.name = entry.name;
    el.dataset.isDirectory = entry.isDirectory ? "true" : "false";
    el.style.setProperty("--file-depth", String(depth));
    if (entry.isDirectory) el.setAttribute("aria-expanded", String(expanded));

    const disclosure = document.createElement("span");
    disclosure.className = ["file-disclosure", expanded ? "expanded" : "", loading ? "loading" : ""]
      .filter(Boolean)
      .join(" ");
    disclosure.setAttribute("aria-hidden", "true");
    if (entry.isDirectory) {
      const glyph = createIcon(loading ? "refresh-cw" : "chevron-right", { size: 12 });
      if (glyph) disclosure.append(glyph);
    }
    el.append(disclosure);

    const iconEl = document.createElement("span");
    iconEl.className = "file-icon";
    iconEl.append(
      createFileTypeIcon({ name: entry.name, isDirectory: entry.isDirectory, expanded }),
    );
    el.append(iconEl);

    const nameEl = document.createElement("span");
    nameEl.className = "file-name";
    nameEl.title = entry.name;
    nameEl.textContent = entry.name;
    el.append(nameEl);

    if (failed) {
      const retry = document.createElement("button");
      retry.type = "button";
      retry.className = "file-row-retry";
      retry.title = t("files.retry");
      retry.setAttribute("aria-label", t("files.retry"));
      const glyph = createIcon("refresh-cw", { size: 12 });
      if (glyph) retry.append(glyph);
      retry.addEventListener("click", (event) => {
        event.stopPropagation();
        this.retryListing(entry.path);
      });
      el.append(retry);
    }
    return el;
  }

  buildStaleBanner() {
    const banner = document.createElement("div");
    banner.className = "file-stale-banner";
    banner.setAttribute("role", "status");
    const message = document.createElement("span");
    message.className = "file-stale-message";
    message.textContent = t("files.staleWorkspace");
    const retry = document.createElement("button");
    retry.type = "button";
    retry.className = "file-stale-retry";
    retry.textContent = t("files.retry");
    retry.addEventListener("click", () => this.retryListing(ROOT_PATH));
    banner.append(message, retry);
    return banner;
  }

  syncRowFocus(hadFocus) {
    if (this.pendingEdit) return;
    const rows = [...this.container.querySelectorAll(".file-item")];
    if (rows.length === 0) return;
    // Roving tabindex: exactly one row is reachable by Tab, and it is the
    // selected one — or the first row when nothing is selected yet.
    const target = rows.find((row) => row.dataset.path === this.selectedPath) ?? rows[0];
    for (const row of rows) row.setAttribute("tabindex", row === target ? "0" : "-1");
    if (hadFocus) target.focus();
  }

  findRow(relativePath) {
    for (const row of this.container.querySelectorAll(".file-item")) {
      if (row.dataset.path === relativePath) return row;
    }
    return null;
  }

  appendStatus(text) {
    const el = document.createElement("div");
    el.className = "file-loading";
    el.textContent = text;
    if (this.fileErrorText) el.title = this.fileErrorText;
    this.container.append(el);
  }

  showFileStatus(status, errorText = null) {
    this.fileStatus = status;
    this.fileErrorText = errorText;
    this.render();
  }

  statusText() {
    switch (this.fileStatus) {
      case "loading":
        return t("files.loading");
      case "empty":
        return t("files.empty");
      case "failed":
        return t("files.failedLoad");
      case "error":
        return this.fileErrorText ?? "";
      default:
        return "";
    }
  }

  // ── Inline create / rename ─────────────────────────────────────────────────

  /** Where a new entry goes: the selected directory, else the workspace root. */
  defaultCreateParent() {
    if (!this.selectedPath) return ROOT_PATH;
    const row = this.findRow(this.selectedPath);
    if (row?.dataset.isDirectory === "true") return this.selectedPath;
    return parentOf(this.selectedPath) ?? ROOT_PATH;
  }

  beginCreate(kind, parentPath = this.defaultCreateParent()) {
    if (!this.canMutate()) return;
    const parent = parentPath ?? ROOT_PATH;
    if (parent !== ROOT_PATH) {
      this.expandedPaths.add(parent);
      this.writePersistedExpanded();
    }
    this.pendingEdit = {
      kind: kind === "directory" || kind === "folder" ? "create-folder" : "create-file",
      parentPath: parent,
      depth: depthOf(parent),
    };
    this.render();
    if (parent !== ROOT_PATH && !this.directoryListings.has(parent)) {
      void this.loadDirectory(parent);
    }
  }

  beginRename(relativePath) {
    if (!this.canMutate()) return;
    const row = this.findRow(relativePath);
    const name = row?.dataset.name ?? basenameOf(relativePath);
    this.pendingEdit = {
      kind: "rename",
      path: relativePath,
      name,
      isDirectory: row?.dataset.isDirectory === "true",
      depth: Number(row?.style.getPropertyValue("--file-depth") || 0),
    };
    this.render();
  }

  cancelEdit() {
    if (!this.pendingEdit) return;
    this.pendingEdit = null;
    this.render();
  }

  renderEditRow() {
    const edit = this.pendingEdit;
    if (!edit) return;
    const placeholder = t(
      edit.kind === "rename" ? "files.renamePlaceholder" : "files.newNamePlaceholder",
    );
    const input = document.createElement("input");
    input.type = "text";
    input.className = "file-edit-input";
    input.autocomplete = "off";
    input.spellcheck = false;
    input.value = edit.kind === "rename" ? edit.name : "";
    input.placeholder = placeholder;
    input.setAttribute("aria-label", placeholder);

    let settled = false;
    const submit = () => {
      if (settled) return;
      settled = true;
      void this.commitEdit(input.value);
    };
    input.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        submit();
      } else if (event.key === "Escape") {
        event.preventDefault();
        settled = true;
        this.cancelEdit();
      }
    });
    // Blur commits, so clicking away does not silently discard a typed name.
    input.addEventListener("blur", submit);
    input.addEventListener("mousedown", (event) => event.stopPropagation());
    input.addEventListener("click", (event) => event.stopPropagation());

    if (edit.kind === "rename") {
      const row = this.findRow(edit.path);
      if (!row) {
        this.pendingEdit = null;
        return;
      }
      const nameEl = row.querySelector(".file-name");
      if (nameEl) nameEl.replaceWith(input);
      else row.append(input);
    } else {
      const row = document.createElement("div");
      row.className = "file-item file-edit-row";
      row.style.setProperty("--file-depth", String(edit.depth));
      const icon = document.createElement("span");
      icon.className = "file-icon";
      icon.append(
        createFileTypeIcon({
          name: edit.kind === "create-folder" ? "folder" : "file",
          isDirectory: edit.kind === "create-folder",
        }),
      );
      row.append(icon, input);
      const parentRow = edit.parentPath === ROOT_PATH ? null : this.findRow(edit.parentPath);
      if (parentRow) parentRow.after(row);
      else this.container.prepend(row);
    }
    input.focus();
    input.select();
  }

  async commitEdit(rawName) {
    const edit = this.pendingEdit;
    if (!edit) return;
    // Clearing first makes the blur that follows an Enter a no-op.
    this.pendingEdit = null;
    const name = typeof rawName === "string" ? rawName.trim() : "";
    if (!name || (edit.kind === "rename" && name === edit.name)) {
      this.render();
      return;
    }

    this.setMutationInFlight(true);
    try {
      if (edit.kind === "rename") {
        const result = await this.renameEntry(edit.path, name);
        const newPath =
          typeof result?.path === "string"
            ? result.path
            : joinRelativePath(parentOf(edit.path) ?? ROOT_PATH, name);
        await this.applyRename(edit.path, newPath, edit.isDirectory);
      } else {
        const kind = edit.kind === "create-folder" ? "directory" : "file";
        const result = await this.createEntry(edit.parentPath, name, kind);
        const newPath =
          typeof result?.path === "string" ? result.path : joinRelativePath(edit.parentPath, name);
        await this.applyCreate(edit.parentPath, newPath, kind === "directory");
      }
    } catch (error) {
      this.notifyMutationFailure(error);
      this.render();
    } finally {
      this.setMutationInFlight(false);
    }
  }

  async applyCreate(parentPath, newPath, isDirectory) {
    this.directoryListings.delete(parentPath);
    await this.loadDirectory(parentPath);
    this.selectedPath = newPath;
    if (isDirectory) {
      // Keep the parent open so the new folder is visible in place.
      this.expandedPaths.add(parentPath);
      this.writePersistedExpanded();
    }
    this.render();
  }

  async applyRename(oldPath, newPath, isDirectory) {
    const parentPath = parentOf(oldPath) ?? ROOT_PATH;
    if (isDirectory) {
      // A renamed directory is a different path: its cached listing, its
      // expansion and any selection inside it describe a path that is gone.
      for (const key of [...this.directoryListings.keys()]) {
        if (key !== parentPath && isWithinPath(key, oldPath)) this.directoryListings.delete(key);
      }
      for (const key of [...this.expandedPaths]) {
        if (isWithinPath(key, oldPath)) this.expandedPaths.delete(key);
      }
      this.writePersistedExpanded();
      if (this.selectedPath && isWithinPath(this.selectedPath, oldPath)) this.selectedPath = null;
    } else if (this.selectedPath === oldPath) {
      this.selectedPath = newPath;
    }
    this.directoryListings.delete(parentPath);
    await this.loadDirectory(parentPath);
    this.render();
  }

  // ── Delete ─────────────────────────────────────────────────────────────────

  async deletePath(relativePath, isDirectory) {
    if (!this.canMutate()) return;
    const name = basenameOf(relativePath);
    if (!globalThis.confirm(t("files.deleteConfirm", { name }))) return;
    this.setMutationInFlight(true);
    try {
      await this.deleteEntry(relativePath);
      const parentPath = parentOf(relativePath) ?? ROOT_PATH;
      if (isDirectory) {
        for (const key of [...this.directoryListings.keys()]) {
          if (key !== parentPath && isWithinPath(key, relativePath)) {
            this.directoryListings.delete(key);
          }
        }
        for (const key of [...this.expandedPaths]) {
          if (isWithinPath(key, relativePath)) this.expandedPaths.delete(key);
        }
        this.writePersistedExpanded();
      }
      if (this.selectedPath && isWithinPath(this.selectedPath, relativePath)) {
        this.selectedPath = null;
      }
      this.directoryListings.delete(parentPath);
      await this.loadDirectory(parentPath);
    } catch (error) {
      this.notifyMutationFailure(error);
    } finally {
      this.setMutationInFlight(false);
      this.render();
    }
  }

  // ── Mutability ─────────────────────────────────────────────────────────────

  canMutate() {
    if (!this.workspaceRoot) return false;
    if (this.mutationInFlight) return false;
    if (this.writesAvailable && !this.writesAvailable()) return false;
    return (
      typeof this.createEntry === "function" &&
      typeof this.renameEntry === "function" &&
      typeof this.deleteEntry === "function"
    );
  }

  setMutationInFlight(value) {
    this.mutationInFlight = Boolean(value);
    this.notifyMutability();
  }

  notifyMutability() {
    this.onMutabilityChange?.(this.canMutate());
  }

  /**
   * Show a localized reason for a failed mutation. The host's machine code
   * picks the sentence; the code itself is never spliced into the UI, and an
   * unrecognized code falls back to the generic line rather than leaking.
   */
  notifyMutationFailure(error) {
    const known = {
      already_exists: "files.errorAlreadyExists",
      invalid_name: "files.errorInvalidName",
      invalid_path: "files.errorInvalidName",
      file_not_found: "files.errorNotFound",
      not_found: "files.errorNotFound",
      directory_not_empty: "files.errorDirectoryNotEmpty",
      is_directory: "files.errorDirectoryNotEmpty",
      permission_denied: "files.errorPermissionDenied",
      path_outside_workspace: "files.errorPermissionDenied",
      temporarily_unavailable: "files.errorUnavailable",
    };
    this.notify(known[error?.code] ?? "files.errorGeneric");
  }

  notify(key) {
    globalThis.window?.dispatchEvent(
      new CustomEvent("picot-toast", { detail: { message: t(key) } }),
    );
  }

  // ── Row interaction ────────────────────────────────────────────────────────

  itemFromEvent(event) {
    return event.target?.closest?.(".file-item") || null;
  }

  /** Absolute path of a row's workspace-relative path. */
  absolutePath(relativePath) {
    if (relativePath === ROOT_PATH) return this.workspaceRoot;
    return joinLocalPath(this.workspaceRoot, relativePath);
  }

  /** True once the root listing has been fetched — "the tree is showing". */
  hasListing() {
    return this.directoryListings.has(ROOT_PATH);
  }

  /**
   * Where "reveal in file manager" should land: the selected directory, the
   * selected file's directory, or the workspace root.
   */
  getRevealTarget() {
    if (!this.workspaceRoot) return "";
    if (!this.selectedPath) return this.workspaceRoot;
    const row = this.findRow(this.selectedPath);
    if (row?.dataset.isDirectory === "true") return this.absolutePath(this.selectedPath);
    return this.absolutePath(parentOf(this.selectedPath) ?? ROOT_PATH);
  }

  onItemClick(event) {
    if (event.target?.closest?.(".file-row-retry")) return;
    const item = this.itemFromEvent(event);
    if (!item) {
      this.setSelected(null);
      return;
    }
    const { path, name, isDirectory } = item.dataset;
    this.selectedPath = path;
    if (isDirectory === "true") {
      void this.setDirectoryExpanded(path, !this.expandedPaths.has(path));
      return;
    }
    this.render();
    // Single-click on a file → trigger onFileSelect callback for preview.
    this.onFileSelect?.(this.absolutePath(path), { name, path: this.absolutePath(path) });
  }

  onItemDoubleClick(event) {
    const item = this.itemFromEvent(event);
    if (!item || item.dataset.isDirectory === "true") return;
    event.preventDefault();
    this.openNatively(this.absolutePath(item.dataset.path));
  }

  onItemContextMenu(event) {
    event.preventDefault();
    closeContextMenu();
    const item = this.itemFromEvent(event);
    if (item) {
      this.selectedPath = item.dataset.path;
      this.render();
    }
    const target = item ? this.rowTarget(item) : { kind: "blank" };
    showContextMenu({
      clientX: event.clientX,
      clientY: event.clientY,
      label: t("files.title"),
      items: this.contextMenuItems(target),
    });
  }

  /** Menu target for a row: which kind it is, and whether it is open. */
  rowTarget(item) {
    const isDirectory = item.dataset.isDirectory === "true";
    return {
      kind: isDirectory ? "directory" : "file",
      path: item.dataset.path,
      name: item.dataset.name,
      expanded: this.expandedPaths.has(item.dataset.path),
    };
  }

  contextMenuItems(target) {
    if (target.kind === "blank") {
      return [
        { label: t("files.newFile"), onSelect: () => this.beginCreate("file", ROOT_PATH) },
        { label: t("files.newFolder"), onSelect: () => this.beginCreate("directory", ROOT_PATH) },
        null,
        { label: t("files.refreshDirectory"), onSelect: () => void this.refresh() },
      ];
    }
    const absolute = this.absolutePath(target.path);
    const isDirectory = target.kind === "directory";
    const canWrite = this.canMutate();
    const items = [];
    if (!isDirectory) {
      items.push(
        {
          label: t("files.preview.title"),
          onSelect: () => this.onFileSelect?.(absolute, { name: target.name, path: absolute }),
        },
        { label: t("files.openInApp"), onSelect: () => this.openNatively(absolute) },
      );
    } else {
      items.push(
        {
          label: t(target.expanded ? "files.collapseDirectory" : "files.expandDirectory"),
          onSelect: () => void this.setDirectoryExpanded(target.path, !target.expanded),
        },
        { label: t("files.revealInFileManager"), onSelect: () => this.openNatively(absolute) },
      );
    }
    items.push({
      label: t("files.addToComposer"),
      onSelect: () => this.insertFileMention(absolute, { isDirectory }),
    });
    items.push(null, {
      label: t("files.copyRelativePath"),
      onSelect: () => this.copyText(target.path),
    });
    items.push({
      label: t("files.copyAbsolutePath"),
      onSelect: () => this.copyText(absolute),
    });
    if (isDirectory) {
      items.push(null, {
        label: t("files.newFile"),
        disabled: !canWrite,
        onSelect: () => this.beginCreate("file", target.path),
      });
      items.push({
        label: t("files.newFolder"),
        disabled: !canWrite,
        onSelect: () => this.beginCreate("directory", target.path),
      });
    }
    items.push(
      null,
      {
        label: t("files.rename"),
        disabled: !canWrite,
        onSelect: () => this.beginRename(target.path),
      },
      {
        label: t("files.delete"),
        disabled: !canWrite,
        onSelect: () => void this.deletePath(target.path, isDirectory),
      },
    );
    return items;
  }

  copyText(text) {
    const clipboard = globalThis.navigator?.clipboard;
    if (!clipboard?.writeText) {
      this.notify("files.copyFailed");
      return;
    }
    clipboard.writeText(text).then(
      () => this.notify("files.copiedPath"),
      () => this.notify("files.copyFailed"),
    );
  }

  visibleRows() {
    return [...this.container.querySelectorAll(".file-item:not(.file-edit-row)")];
  }

  onItemKeyDown(event) {
    const item = this.itemFromEvent(event);
    if (!item || item.classList.contains("file-edit-row")) return;
    const { path, isDirectory } = item.dataset;
    const rows = this.visibleRows();
    const index = rows.indexOf(item);
    const isDir = isDirectory === "true";
    const expanded = this.expandedPaths.has(path);
    const move = (next) => {
      if (!next) return;
      event.preventDefault();
      this.selectedPath = next.dataset.path;
      this.render();
      // render() rebuilt the rows: the element we navigated from is detached.
      this.findRow(next.dataset.path)?.focus();
    };
    switch (event.key) {
      case "ArrowDown":
        move(rows[index + 1]);
        break;
      case "ArrowUp":
        move(rows[index - 1]);
        break;
      case "ArrowRight":
        if (isDir && !expanded) void this.setDirectoryExpanded(path, true);
        else if (isDir) move(rows[index + 1]);
        break;
      case "ArrowLeft":
        if (isDir && expanded) {
          void this.setDirectoryExpanded(path, false);
        } else {
          move(rows.find((row) => row.dataset.path === parentOf(path)));
        }
        break;
      case "Enter":
      case " ":
        if (isDir) {
          event.preventDefault();
          void this.setDirectoryExpanded(path, !expanded);
        } else if (event.key === "Enter") {
          event.preventDefault();
          this.selectedPath = path;
          this.render();
          this.onFileSelect?.(this.absolutePath(path), {
            name: item.dataset.name,
            path: this.absolutePath(path),
          });
        }
        break;
      default:
        break;
    }
  }

  /**
   * Custom drag-to-chat via mouse events. WKWebView does not fire
   * dragover/dragend/drop — only dragstart — making HTML5 DnD unusable.
   * We listen for mousedown on rows, start a custom drag after a small
   * movement threshold, and detect the drop target with elementFromPoint.
   */
  onItemMouseDown(event) {
    if (event.button !== 0) return;
    const item = this.itemFromEvent(event);
    if (!item || item.classList.contains("file-edit-row")) return;
    if (event.target?.closest?.(".file-row-retry")) return;
    event.preventDefault();

    const filePath = this.absolutePath(item.dataset.path);
    const isDirectory = item.dataset.isDirectory === "true";
    const startX = event.clientX;
    const startY = event.clientY;
    let dragging = false;
    let composerFocused = false;
    let ghost = null;
    const input = this.messageInput;
    const card = input.closest("#composer-card");

    const onMove = (e) => {
      if (!dragging) {
        if (
          Math.abs(e.clientX - startX) < DRAG_THRESHOLD_PX &&
          Math.abs(e.clientY - startY) < DRAG_THRESHOLD_PX
        ) {
          return;
        }
        dragging = true;
        item.classList.add("dragging");
        document.body.classList.add("file-dragging");
        ghost = document.createElement("div");
        ghost.className = "file-drag-ghost";
        ghost.textContent = item.dataset.name;
        document.body.append(ghost);
      }
      if (ghost) {
        ghost.style.left = `${e.clientX}px`;
        ghost.style.top = `${e.clientY}px`;
      }
      if (card) {
        const el = document.elementFromPoint(e.clientX, e.clientY);
        const overComposer = !!el && (el === card || card.contains(el));
        card.classList.toggle("file-drop-hover", overComposer);
        if (overComposer && !composerFocused) {
          composerFocused = true;
          input.focus();
        }
      }
    };

    const onUp = (e) => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      item.classList.remove("dragging");
      document.body.classList.remove("file-dragging");
      if (ghost) ghost.remove();
      if (card) card.classList.remove("file-drop-hover");

      // Below the threshold this is a click: the container's click handler
      // already selected the row and expanded or previewed it.
      if (!dragging) return;

      const el = document.elementFromPoint(e.clientX, e.clientY);
      if (!el || !card) return;
      if (el !== card && !card.contains(el)) return;

      // Keep focus inside the trusted mouse gesture. WKWebView may reject
      // focus requests deferred beyond mouseup, leaving the mention inserted
      // without an active composer.
      e.preventDefault();
      this.insertFileMention(filePath, { isDirectory });
    };

    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  }

  async openNatively(filePath) {
    try {
      if (!this.openPath) throw new Error("Host open control unavailable");
      await this.openPath(filePath);
    } catch (err) {
      console.error("[FileBrowser] Failed to open:", err);
    }
  }

  /**
   * Compute a workspace-relative mention token (`@<posix-relpath>`) for an
   * absolute file path, or null when no workspace root is known or the file
   * is not representable as a relative path. Segment-based (not startsWith)
   * so trailing/double slashes fold cleanly, sibling prefixes are
   * disambiguated, and different Windows drives are rejected.
   *
   * A directory keeps a trailing `/`, so the mention reads as a directory to
   * Pi's own completion instead of as a file that happens to share its name.
   */
  toMentionPath(filePath, { isDirectory = false } = {}) {
    if (!this.workspaceRoot || typeof filePath !== "string" || filePath === "") return null;

    const segs = (p) =>
      p
        .replace(/\\/g, "/")
        .split("/")
        .filter((s) => s !== "" && s !== ".");
    const rootSegs = segs(this.workspaceRoot);
    const fileSegs = segs(filePath);

    if (rootSegs.length === 0) return null;
    if (fileSegs.includes("..") || rootSegs.includes("..")) return null;

    // Reject cross-drive Windows paths (e.g. root C: vs file D:)
    const isDriveSeg = (s) => /^[A-Za-z]:$/.test(s);
    const normalizedRootPath = this.workspaceRoot.replace(/\\/g, "/");
    const normalizedFilePath = filePath.replace(/\\/g, "/");
    const isUncRoot = normalizedRootPath.startsWith("//");
    const isUncFile = normalizedFilePath.startsWith("//");
    const isWindowsPath =
      isDriveSeg(rootSegs[0]) || isDriveSeg(fileSegs[0]) || isUncRoot || isUncFile;
    const sameSegment = (left, right) =>
      isWindowsPath ? left.toLowerCase() === right.toLowerCase() : left === right;
    // A UNC server/share pair is its volume root. Relative paths cannot cross
    // shares even when both paths have the same server prefix.
    if (
      isUncRoot &&
      isUncFile &&
      (!sameSegment(rootSegs[0], fileSegs[0]) || !sameSegment(rootSegs[1], fileSegs[1]))
    )
      return null;
    if (
      !sameSegment(rootSegs[0], fileSegs[0]) &&
      (isDriveSeg(rootSegs[0]) || isDriveSeg(fileSegs[0]))
    )
      return null;

    // Find common prefix length
    let i = 0;
    while (i < rootSegs.length && i < fileSegs.length && sameSegment(rootSegs[i], fileSegs[i])) {
      i++;
    }

    const upCount = rootSegs.length - i;
    const remaining = fileSegs.slice(i);
    if (remaining.length === 0) return null; // file is an ancestor dir of root

    const parts = [];
    for (let j = 0; j < upCount; j++) parts.push("..");
    parts.push(...remaining);
    return `@${parts.join("/")}${isDirectory ? "/" : ""}`;
  }

  /**
   * Insert a file or directory mention (`@<relative-path>`) at the textarea
   * selection. Called by the composer-card drop handler in app.js. Returns true
   * when a mention was inserted, false when the path is not representable.
   */
  insertFileMention(filePath, { isDirectory = false } = {}) {
    if (!filePath) return false;
    const mention = this.toMentionPath(filePath, { isDirectory });
    if (!mention) return false;

    const input = this.messageInput;
    input.focus();
    // setRangeText triggers WKWebView's native text-edit repaint,
    // unlike direct .value assignment which may not visually update.
    const start = input.selectionStart ?? 0;
    const end = input.selectionEnd ?? 0;
    try {
      input.setRangeText(mention, start, end, "end");
    } catch {
      // Fallback for environments without setRangeText
      const before = input.value.slice(0, start);
      const after = input.value.slice(end);
      input.value = before + mention + after;
      input.selectionStart = input.selectionEnd = start + mention.length;
    }
    input.dispatchEvent(new Event("input"));
    return true;
  }
}
