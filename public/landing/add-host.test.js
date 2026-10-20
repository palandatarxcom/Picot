// ABOUTME: Tests the landing add-remote-host entry (pasted offer + TOFU outcome).
// ABOUTME: Keeps the entry to the host_add/host_confirm_tofu IPC surface with no logging of offers.

import { JSDOM } from "jsdom";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { setupLandingAddHost } from "./add-host.js";

function setupDom() {
  const dom = new JSDOM(`<div class="landing">
    <button type="button" id="landing-add-host-btn"></button>
    <form id="landing-add-host-form" class="hidden">
      <input id="landing-add-host-url">
      <button type="submit" id="landing-add-host-submit"></button>
    </form>
    <div id="landing-add-host-result" class="hidden" role="status"></div>
  </div>`);
  const document = dom.window.document;
  return {
    dom,
    document,
    button: document.getElementById("landing-add-host-btn"),
    form: document.getElementById("landing-add-host-form"),
    input: document.getElementById("landing-add-host-url"),
    result: document.getElementById("landing-add-host-result"),
  };
}

const messages = {
  "landing.addHost": "Add remote host",
  "landing.addHostSubmit": "Add",
  "landing.addHostPending": "New host, key {fingerprint}. Trust it?",
  "landing.addHostConfirm": "Trust and add",
  "landing.addHostAdded": "Host added: {host}",
  "landing.addHostFailed": "Could not add host: {error}",
  "landing.addHostCancelled": "Cancelled",
  "actions.cancel": "Cancel",
};

function t(key, params = {}) {
  const text = messages[key] ?? key;
  return text.replace(/\{(\w+)\}/g, (_, name) => params[name] ?? `{${name}}`);
}

const offer = "picot://connect?offer=eyJzZXJ2ZXJJZCI6InIxIn0";

async function submit(refs, value = offer) {
  refs.button.click();
  expect(refs.form.classList.contains("hidden")).toBe(false);
  refs.input.value = value;
  refs.form.dispatchEvent(new refs.dom.window.Event("submit", { bubbles: true, cancelable: true }));
  await Promise.resolve();
  await Promise.resolve();
}

describe("setupLandingAddHost", () => {
  let refs;

  beforeEach(() => {
    refs = setupDom();
  });

  it("posts the pasted offer to host_add and clears the input", async () => {
    const invoke = vi.fn().mockResolvedValue({ status: "connected", serverId: "r1" });
    setupLandingAddHost({ document: refs.document, invoke, t });

    await submit(refs);

    expect(invoke).toHaveBeenCalledWith("host_add", { offerUrl: offer });
    expect(refs.input.value).toBe("");
    expect(refs.result.classList.contains("hidden")).toBe(false);
    expect(refs.result.textContent).toContain("Host added: r1");
  });

  it("asks for confirmation on a new host and sends the answer back", async () => {
    const invoke = vi
      .fn()
      .mockResolvedValueOnce({
        status: "tofuPending",
        questionId: 7,
        serverId: "r1",
        keyFingerprint: "ABCD 1234",
        kind: "newHost",
        changes: [],
      })
      .mockResolvedValueOnce({ status: "connected", serverId: "r1" });
    setupLandingAddHost({ document: refs.document, invoke, t });

    await submit(refs);

    expect(refs.result.textContent).toContain("New host, key ABCD 1234");

    refs.result.querySelector(".landing-add-host-confirm").click();
    await Promise.resolve();
    await Promise.resolve();

    expect(invoke).toHaveBeenLastCalledWith("host_confirm_tofu", {
      questionId: 7,
      approved: true,
    });
    expect(refs.result.textContent).toContain("Host added: r1");
  });

  it("reports an invalid offer without echoing it", async () => {
    const invoke = vi.fn().mockRejectedValue("Invalid connection offer");
    setupLandingAddHost({ document: refs.document, invoke, t });

    await submit(refs);

    expect(refs.result.textContent).toContain("Could not add host: Invalid connection offer");
    expect(refs.result.textContent).not.toContain("eyJzZXJ2ZXJJZCI6InIxIn0");
  });
});
