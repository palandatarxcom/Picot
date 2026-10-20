// ABOUTME: Tests the explicit large-paste composer affordance without starting the app.
// ABOUTME: Covers threshold detection, inline dismissal, range replacement, and write failures.

import { JSDOM } from "jsdom";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  PASTE_OFFLOAD_BYTE_THRESHOLD,
  setupComposerPasteOffload,
} from "./composer-paste-offload.js";

function setupDom() {
  const dom = new JSDOM('<div class="composer"><textarea></textarea></div>');
  const document = dom.window.document;
  return {
    dom,
    document,
    container: document.querySelector(".composer"),
    textarea: document.querySelector("textarea"),
  };
}

function paste(dom, textarea, text, start = textarea.value.length, end = start) {
  textarea.setSelectionRange(start, end);
  const event = new dom.window.Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", {
    value: { getData: () => text },
  });
  textarea.dispatchEvent(event);
  textarea.value = `${textarea.value.slice(0, start)}${text}${textarea.value.slice(end)}`;
  textarea.dispatchEvent(new dom.window.Event("input", { bubbles: true }));
}

const translations = {
  "input.pasteDetected": ({ size }) => `Pasted ${size}`,
  "input.pasteAsFile": () => "Attach as file",
  "input.keepInline": () => "Keep inline",
  "input.pasteOffloadFailed": () => "Could not save pasted text",
};

function t(key, params) {
  return translations[key]?.(params) || key;
}

describe("setupComposerPasteOffload", () => {
  let refs;

  beforeEach(() => {
    refs = setupDom();
  });

  it("does not show a prompt below the threshold", async () => {
    const offload = vi.fn();
    setupComposerPasteOffload({ ...refs, offload, t });
    paste(refs.dom, refs.textarea, "x".repeat(PASTE_OFFLOAD_BYTE_THRESHOLD - 1));
    await Promise.resolve();

    expect(refs.container.querySelector(".paste-offload-prompt").classList.contains("hidden")).toBe(
      true,
    );
    expect(offload).not.toHaveBeenCalled();
  });

  it("keeps large pasted text inline when dismissed", async () => {
    const offload = vi.fn();
    setupComposerPasteOffload({ ...refs, offload, t });
    const text = "x".repeat(PASTE_OFFLOAD_BYTE_THRESHOLD);
    paste(refs.dom, refs.textarea, text);
    await Promise.resolve();

    refs.container.querySelector(".paste-offload-keep-inline").click();
    expect(refs.textarea.value).toBe(text);
    expect(refs.container.querySelector(".paste-offload-prompt").classList.contains("hidden")).toBe(
      true,
    );
    expect(offload).not.toHaveBeenCalled();
  });

  it("replaces only the confirmed pasted range with an @ reference", async () => {
    const offload = vi.fn().mockResolvedValue(".pi/tmp/paste-20260821-143205.txt");
    setupComposerPasteOffload({ ...refs, offload, t });
    refs.textarea.value = "before after";
    const text = "x".repeat(PASTE_OFFLOAD_BYTE_THRESHOLD);
    paste(refs.dom, refs.textarea, text, 7, 7);
    await Promise.resolve();

    refs.container.querySelector(".paste-offload-as-file").click();
    await Promise.resolve();

    expect(offload).toHaveBeenCalledWith(text);
    expect(refs.textarea.value).toBe("before \n@.pi/tmp/paste-20260821-143205.txt\nafter");
    expect(refs.container.querySelector(".paste-offload-prompt").classList.contains("hidden")).toBe(
      true,
    );
  });

  it("marks the composer busy while offloading and clears it after success", async () => {
    let resolveOffload;
    const offload = vi.fn(
      () =>
        new Promise((resolve) => {
          resolveOffload = resolve;
        }),
    );
    const controller = setupComposerPasteOffload({ ...refs, offload, t });
    const text = "x".repeat(PASTE_OFFLOAD_BYTE_THRESHOLD);
    paste(refs.dom, refs.textarea, text);
    await Promise.resolve();

    refs.container.querySelector(".paste-offload-as-file").click();
    expect(controller.isBusy()).toBe(true);
    expect(refs.textarea.dataset.pasteOffloadBusy).toBe("true");
    refs.textarea.dispatchEvent(new refs.dom.window.Event("input", { bubbles: true }));
    expect(controller.isBusy()).toBe(true);

    resolveOffload(".pi/tmp/paste-20260821-143205.txt");
    await Promise.resolve();
    expect(controller.isBusy()).toBe(false);
    expect(refs.textarea.dataset.pasteOffloadBusy).toBeUndefined();
  });

  it("leaves the textarea unchanged when offload fails", async () => {
    const offload = vi.fn().mockRejectedValue(new Error("write failed"));
    setupComposerPasteOffload({ ...refs, offload, t });
    const text = "x".repeat(PASTE_OFFLOAD_BYTE_THRESHOLD);
    paste(refs.dom, refs.textarea, text);
    await Promise.resolve();

    refs.container.querySelector(".paste-offload-as-file").click();
    await Promise.resolve();

    expect(refs.textarea.value).toBe(text);
    expect(refs.container.querySelector(".paste-offload-prompt").textContent).toContain(
      "Could not save pasted text",
    );
  });

  it("dismisses a pending offer when the user edits the draft", async () => {
    const offload = vi.fn();
    setupComposerPasteOffload({ ...refs, offload, t });
    const text = "x".repeat(PASTE_OFFLOAD_BYTE_THRESHOLD);
    paste(refs.dom, refs.textarea, text);
    await Promise.resolve();
    refs.textarea.value += " edited";
    refs.textarea.dispatchEvent(new refs.dom.window.Event("input", { bubbles: true }));

    expect(refs.container.querySelector(".paste-offload-prompt").classList.contains("hidden")).toBe(
      true,
    );
  });
});

// T1 inventory row `/v2/paste-offload`: the host has no remote offload face, so
// the pasted text stays inline and the prompt says why — no fetch, no fallback.
it("keeps pasted text inline and explains why a remote host never offloads", async () => {
  const local = setupDom();
  const offload = vi.fn();
  const reason = "Remote host: pasted text stays inline";
  const handle = setupComposerPasteOffload({ ...local, offload, t, disabledReason: reason });
  const text = "x".repeat(PASTE_OFFLOAD_BYTE_THRESHOLD);

  paste(local.dom, local.textarea, text);
  await Promise.resolve();

  const prompt = local.container.querySelector(".paste-offload-prompt");
  expect(prompt.classList.contains("hidden")).toBe(false);
  expect(prompt.querySelector(".paste-offload-label").textContent).toBe(reason);
  expect(prompt.querySelector(".paste-offload-as-file").classList.contains("hidden")).toBe(true);
  expect(handle.isDisabled()).toBe(true);

  prompt.querySelector(".paste-offload-keep-inline").click();
  expect(local.textarea.value).toBe(text);
  expect(offload).not.toHaveBeenCalled();
  expect(prompt.classList.contains("hidden")).toBe(true);
});
