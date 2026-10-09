// ABOUTME: Validates and renders rpiv-todo snapshots in a native mirror panel.
// ABOUTME: Keeps expansion local to the panel and delegates clear to the active runtime.

import { t } from "../i18n.js";

const TODO_TOOL_NAME = "todo";
const EMPTY_STATE = Object.freeze({ tasks: Object.freeze([]), nextId: 1 });
const STATUS_LABELS = {
  pending: "pending",
  in_progress: "in progress",
  completed: "completed",
  deleted: "deleted",
};
const STATUS_GLYPHS = {
  pending: "○",
  in_progress: "◐",
  completed: "✓",
  deleted: "✗",
};

export function isRpivTodoCommandNotify(message) {
  const text = String(message ?? "");
  return (
    text === "No todos yet. Ask the agent to add some!" ||
    /^\d+\/\d+ completed/m.test(text) ||
    text.includes("── Pending ──") ||
    text.includes("── In Progress ──") ||
    text.includes("── Completed ──")
  );
}

export function isRpivTodoDetails(value) {
  if (!value || typeof value !== "object") return false;
  if (!Array.isArray(value.tasks) || typeof value.nextId !== "number") return false;
  return value.tasks.every(isRpivTodoTask);
}

function isRpivTodoTask(value) {
  if (!value || typeof value !== "object") return false;
  return (
    typeof value.id === "number" &&
    typeof value.subject === "string" &&
    ["pending", "in_progress", "completed", "deleted"].includes(value.status)
  );
}

export function replayRpivTodoFromMessages(messages = []) {
  let state = EMPTY_STATE;
  for (const message of messages) {
    if (message?.role !== "toolResult" || message.toolName !== TODO_TOOL_NAME) continue;
    if (isRpivTodoDetails(message.details)) state = cloneTodoState(message.details);
  }
  return state;
}

function cloneTodoState(details) {
  return { tasks: details.tasks.map((task) => ({ ...task })), nextId: details.nextId };
}

export class RpivTodoMirrorPanel {
  #element;
  #state = EMPTY_STATE;
  #expanded = false;
  #onClear;

  constructor({ container, widgetPlacement = "aboveEditor", onClear = null } = {}) {
    this.#onClear = onClear;
    this.#element = document.createElement("section");
    this.#element.className = "rpiv-todo-panel hidden is-collapsed";
    this.#element.setAttribute("aria-label", t("todoMirror.title.todos"));
    this.#insert(container, widgetPlacement);
  }

  get element() {
    return this.#element;
  }

  get hasVisibleTasks() {
    return this.#state.tasks.some((task) => task.status !== "deleted");
  }

  hydrateFromMessages(messages) {
    this.setState(replayRpivTodoFromMessages(messages));
  }

  applyToolResult(result) {
    if (!isRpivTodoDetails(result?.details)) return false;
    this.setState(cloneTodoState(result.details));
    return true;
  }

  setState(state) {
    this.#state = state ?? EMPTY_STATE;
    this.#render();
  }

  clear() {
    this.#expanded = false;
    this.#element.classList.add("is-collapsed");
    this.setState(EMPTY_STATE);
  }

  toggleExpanded() {
    this.#expanded = !this.#expanded;
    this.#render();
  }

  #insert(container, placement) {
    const form = container?.querySelector("form");
    if (!container || !form) return;
    if (placement === "belowEditor") form.insertAdjacentElement("afterend", this.#element);
    else form.insertAdjacentElement("beforebegin", this.#element);
  }

  #render() {
    const visibleTasks = this.#state.tasks.filter((task) => task.status !== "deleted");
    if (visibleTasks.length === 0) {
      this.#element.classList.add("hidden");
      this.#element.replaceChildren();
      return;
    }

    const completed = visibleTasks.filter((task) => task.status === "completed").length;
    const hasActive = visibleTasks.some(
      (task) => task.status === "pending" || task.status === "in_progress",
    );
    const header = document.createElement("div");
    header.className = "rpiv-todo-panel__header";
    const titleGroup = document.createElement("div");
    titleGroup.className = "rpiv-todo-panel__titleGroup";
    const dot = document.createElement("span");
    dot.className = `rpiv-todo-panel__dot ${hasActive ? "is-active" : ""}`;
    dot.textContent = hasActive ? "●" : "○";
    const title = document.createElement("span");
    title.className = "rpiv-todo-panel__title";
    title.textContent = t("todoMirror.title.todos");
    titleGroup.append(dot, title);
    const actions = document.createElement("div");
    actions.className = "rpiv-todo-panel__actions";
    const summary = document.createElement("span");
    summary.className = "rpiv-todo-panel__summary";
    summary.textContent = `${completed}/${visibleTasks.length}`;
    actions.append(summary);
    const clear = this.#createClearButton();
    if (clear && visibleTasks.length <= 5) actions.append(clear);
    header.append(titleGroup, actions);

    const displayTasks = selectDisplayTasks(visibleTasks, this.#expanded);
    const list = document.createElement("ol");
    list.className = "rpiv-todo-panel__list";
    for (const task of displayTasks) list.append(renderTask(task, shouldShowIds(visibleTasks)));
    const children = [header, list];
    if (visibleTasks.length > 5) {
      const footer = document.createElement("div");
      footer.className = "rpiv-todo-panel__footer";
      const more = document.createElement("button");
      more.type = "button";
      more.className = "rpiv-todo-panel__more";
      more.textContent = this.#expanded
        ? t("todoMirror.collapse")
        : t("todoMirror.showAll", { count: visibleTasks.length - 5 });
      more.addEventListener("click", () => this.toggleExpanded());
      footer.append(more);
      if (clear) footer.append(clear);
      children.push(footer);
    }

    this.#element.classList.toggle("is-complete", !hasActive);
    this.#element.replaceChildren(...children);
    this.#element.classList.remove("hidden");
  }

  #createClearButton() {
    if (!this.hasVisibleTasks) return null;
    const clear = document.createElement("button");
    clear.type = "button";
    clear.className = "rpiv-todo-panel__clear";
    clear.textContent = t("todoMirror.clear");
    clear.addEventListener("click", () => this.#onClear?.());
    return clear;
  }
}

function shouldShowIds(tasks) {
  return tasks.some((task) => Array.isArray(task.blockedBy) && task.blockedBy.length > 0);
}

function selectDisplayTasks(tasks, expanded = false) {
  const active = tasks.filter((task) => task.status !== "completed");
  const completed = tasks.filter((task) => task.status === "completed");
  return expanded ? [...active, ...completed] : [...active, ...completed].slice(0, 5);
}

function renderTask(task, showId) {
  const item = document.createElement("li");
  item.className = `rpiv-todo-panel__task is-${task.status.replace("_", "-")}`;
  const glyph = document.createElement("span");
  glyph.className = "rpiv-todo-panel__glyph";
  glyph.textContent = STATUS_GLYPHS[task.status] ?? "•";
  const text = document.createElement("span");
  text.className = "rpiv-todo-panel__text";
  text.textContent = `${showId ? `#${task.id} ` : ""}${task.subject}`;
  item.append(glyph, text);
  if (task.status === "in_progress" && task.activeForm) {
    const active = document.createElement("span");
    active.className = "rpiv-todo-panel__meta";
    active.textContent = `(${task.activeForm})`;
    item.append(active);
  }
  if (Array.isArray(task.blockedBy) && task.blockedBy.length > 0) {
    const blocked = document.createElement("span");
    blocked.className = "rpiv-todo-panel__meta";
    blocked.textContent = `⛓ ${task.blockedBy.map((id) => `#${id}`).join(",")}`;
    item.append(blocked);
  }
  item.title = STATUS_LABELS[task.status] ?? task.status;
  return item;
}
