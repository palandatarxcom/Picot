// ABOUTME: Verifies the DOM confirm modal that replaces window.confirm in the
// ABOUTME: Tauri WebView, where the native confirm bridge does not exist.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { initI18n } from "../i18n.js";
import { showDomConfirm } from "./dom-confirm.js";

beforeEach(async () => {
  document.documentElement.innerHTML = readFileSync(
    join(process.cwd(), "public/index.html"),
    "utf8",
  );
  globalThis.fetch = vi.fn(async () => new Response(JSON.stringify({}), { status: 404 }));
  await initI18n();
});

afterEach(() => {
  document.body.querySelector(".dom-confirm-overlay")?.remove();
  vi.restoreAllMocks();
  delete globalThis.fetch;
});

test("resolves true when the confirm button is clicked", async () => {
  const decision = showDomConfirm("Discard these chats?");
  const confirmBtn = document.querySelector(".sidebar-confirm-yes");
  expect(confirmBtn).not.toBeNull();
  expect(document.querySelector(".sidebar-confirm-message")?.textContent).toBe(
    "Discard these chats?",
  );
  confirmBtn.click();
  await expect(decision).resolves.toBe(true);
  // Settled exactly once: the overlay is gone.
  expect(document.querySelector(".dom-confirm-overlay")).toBeNull();
});

test("resolves false on cancel, Escape, and backdrop click", async () => {
  const viaCancel = showDomConfirm("m");
  document.querySelector(".sidebar-confirm-no").click();
  await expect(viaCancel).resolves.toBe(false);

  const viaEscape = showDomConfirm("m");
  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
  await expect(viaEscape).resolves.toBe(false);

  const viaBackdrop = showDomConfirm("m");
  const overlay = document.querySelector(".dom-confirm-overlay");
  overlay.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  await expect(viaBackdrop).resolves.toBe(false);
});

test("replaces window.confirm, which is silently false in the Tauri WebView", () => {
  // The defect this module exists for: the WebView never shows a native
  // confirm and evaluates it to false, so a confirm-gated flow quietly
  // cancels. The modal must not depend on that bridge at all.
  const original = globalThis.confirm;
  globalThis.confirm = () => {
    throw new Error("window.confirm must not be used");
  };
  try {
    const decision = showDomConfirm("m");
    document.querySelector(".sidebar-confirm-yes").click();
    return expect(decision).resolves.toBe(true);
  } finally {
    globalThis.confirm = original;
  }
});
