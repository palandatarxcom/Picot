import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeEach, describe, expect, it } from "vitest";
import { setMessages } from "../i18n.js";
import {
  isRpivTodoCommandNotify,
  isRpivTodoDetails,
  RpivTodoMirrorPanel,
  replayRpivTodoFromMessages,
} from "./rpiv-todo-mirror.js";

describe("rpiv todo mirror", () => {
  beforeEach(() => {
    localStorage.clear();
    setMessages({
      todoMirror: {
        title: { todos: "Todos" },
        showAll: "Show all {count} more",
        collapse: "Collapse",
        clear: "Clear",
      },
    });
  });
  it("recognizes rpiv-todo snapshots", () => {
    expect(
      isRpivTodoDetails({
        tasks: [{ id: 1, subject: "Implement sync", status: "in_progress" }],
        nextId: 2,
      }),
    ).toBe(true);
    expect(
      isRpivTodoDetails({ tasks: [{ id: 1, subject: "bad", status: "open" }], nextId: 2 }),
    ).toBe(false);
  });

  it("replays the last todo tool result from messages", () => {
    const state = replayRpivTodoFromMessages([
      {
        role: "toolResult",
        toolName: "todo",
        details: { tasks: [{ id: 1, subject: "Old", status: "pending" }], nextId: 2 },
      },
      {
        role: "toolResult",
        toolName: "bash",
        details: { tasks: [{ id: 99, subject: "Ignored", status: "pending" }], nextId: 100 },
      },
      {
        role: "toolResult",
        toolName: "todo",
        details: { tasks: [{ id: 1, subject: "Done", status: "completed" }], nextId: 2 },
      },
    ]);

    expect(state).toEqual({ tasks: [{ id: 1, subject: "Done", status: "completed" }], nextId: 2 });
  });

  it("detects rpiv-todo command notifications", () => {
    expect(isRpivTodoCommandNotify("3/3 completed\n── Completed ──\n  ✓ #1 Done")).toBe(true);
    expect(isRpivTodoCommandNotify("regular message")).toBe(false);
  });

  it("renders a native panel from tool result details", () => {
    document.body.innerHTML = '<div class="input-area"><form></form></div>';
    const panel = new RpivTodoMirrorPanel({ container: document.querySelector(".input-area") });

    panel.applyToolResult({
      details: {
        tasks: [
          { id: 1, subject: "Build panel", status: "in_progress", activeForm: "building panel" },
          { id: 2, subject: "Deleted", status: "deleted" },
        ],
        nextId: 3,
      },
    });

    const element = document.querySelector(".rpiv-todo-panel");
    expect(element.classList.contains("hidden")).toBe(false);
    // Verify the heading includes the todo identifier (i18n key may be raw).
    expect(element.textContent.toLowerCase()).toContain("todos");
    expect(element.textContent).toContain("0/1");
    expect(element.textContent).toContain("Build panel");
    expect(element.textContent).not.toContain("Deleted");
  });

  it("reports whether anything is mirrored, so /todos is never a silent no-op", () => {
    document.body.innerHTML = '<div class="input-area"><form></form></div>';
    const panel = new RpivTodoMirrorPanel({ container: document.querySelector(".input-area") });

    expect(panel.hasVisibleTasks).toBe(false);

    panel.applyToolResult({
      details: { tasks: [{ id: 1, subject: "Gone", status: "deleted" }], nextId: 2 },
    });
    expect(panel.hasVisibleTasks).toBe(false);

    panel.applyToolResult({
      details: { tasks: [{ id: 1, subject: "Build panel", status: "pending" }], nextId: 2 },
    });
    expect(panel.hasVisibleTasks).toBe(true);
  });

  it("renders hover-expandable collapsed markup", () => {
    document.body.innerHTML = '<div class="input-area"><form></form></div>';
    const panel = new RpivTodoMirrorPanel({ container: document.querySelector(".input-area") });

    panel.applyToolResult({
      details: {
        tasks: [{ id: 1, subject: "Build panel", status: "pending" }],
        nextId: 2,
      },
    });

    const element = document.querySelector(".rpiv-todo-panel");
    expect(element.classList.contains("is-collapsed")).toBe(true);
    expect(element.querySelector(".rpiv-todo-panel__collapseIcon")).toBeNull();
    expect(element.querySelector(".rpiv-todo-panel__list").textContent).toContain("Build panel");
  });

  it("clear() resets expanded state", () => {
    document.body.innerHTML = '<div class="input-area"><form></form></div>';
    const panel = new RpivTodoMirrorPanel({
      container: document.querySelector(".input-area"),
    });
    panel.applyToolResult({
      details: {
        tasks: [{ id: 1, subject: "X", status: "pending" }],
        nextId: 2,
      },
    });
    panel.toggleExpanded();
    expect(panel.element.classList.contains("is-collapsed")).toBe(true);
    panel.clear();
    expect(panel.element.classList.contains("is-collapsed")).toBe(true);
    expect(panel.element.classList.contains("hidden")).toBe(true);
  });

  it("places clear beside the show-more action and hides it while collapsed", () => {
    document.body.innerHTML = '<div class="input-area"><form></form></div>';
    const panel = new RpivTodoMirrorPanel({
      container: document.querySelector(".input-area"),
      onClear: () => {},
    });
    panel.applyToolResult({
      details: {
        tasks: Array.from({ length: 6 }, (_, index) => ({
          id: index + 1,
          subject: `Task ${index + 1}`,
          status: "pending",
        })),
        nextId: 7,
      },
    });
    const element = panel.element;
    const clear = element.querySelector(".rpiv-todo-panel__clear");
    const more = element.querySelector(".rpiv-todo-panel__more");
    expect(clear.parentElement).toBe(more.parentElement);
    expect(element.classList.contains("is-collapsed")).toBe(true);
    panel.toggleExpanded();
    expect(element.classList.contains("is-collapsed")).toBe(true);
    expect(clear.parentElement).toBe(more.parentElement);
  });

  it("toggleExpanded() reveals all tasks", () => {
    document.body.innerHTML = '<div class="input-area"><form></form></div>';
    const panel = new RpivTodoMirrorPanel({ container: document.querySelector(".input-area") });
    panel.applyToolResult({
      details: {
        tasks: Array.from({ length: 6 }, (_, index) => ({
          id: index + 1,
          subject: `Task ${index + 1}`,
          status: "pending",
        })),
        nextId: 7,
      },
    });
    expect(panel.element.querySelectorAll(".rpiv-todo-panel__task")).toHaveLength(5);
    panel.toggleExpanded();
    expect(panel.element.querySelectorAll(".rpiv-todo-panel__task")).toHaveLength(6);
  });

  it("keeps the pill auto-hide rule active after show-more expands the list", () => {
    const style = document.createElement("style");
    style.textContent = readFileSync(resolve("public/ui/rpiv-todo-mirror.css"), "utf8");
    document.head.append(style);
    document.body.innerHTML = '<div class="input-area"><form></form></div>';
    const panel = new RpivTodoMirrorPanel({ container: document.querySelector(".input-area") });
    panel.applyToolResult({
      details: {
        tasks: Array.from({ length: 6 }, (_, index) => ({
          id: index + 1,
          subject: `Task ${index + 1}`,
          status: "pending",
        })),
        nextId: 7,
      },
    });

    // Auto-hide is CSS-only and keyed on .is-collapsed; jsdom cannot simulate
    // :hover, so the test checks the class contract and the pointer-outside state.
    panel.toggleExpanded();
    expect(panel.element.classList.contains("is-collapsed")).toBe(true);
    expect(panel.element.querySelectorAll(".rpiv-todo-panel__task")).toHaveLength(6);
    const list = panel.element.querySelector(".rpiv-todo-panel__list");
    expect(getComputedStyle(list).display).toBe("none");
  });
});
