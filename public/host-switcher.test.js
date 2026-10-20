// @vitest-environment jsdom

// ABOUTME: Verifies the sidebar host picker: local-first list with status
// ABOUTME: badges and current highlight, switch persistence, the add-host TOFU
// ABOUTME: question, §2.5 removal, and full locale coverage of its copy.

import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";
import { beforeEach, describe, expect, it, vi } from "vitest";

const EN = JSON.parse(readFileSync("public/locales/en.json", "utf8"));

const lookupValue = (messages, key) => {
  let node = messages;
  for (const part of key.split(".")) {
    if (node == null || typeof node !== "object") return undefined;
    node = node[part];
  }
  return typeof node === "string" ? node : undefined;
};

const { requested } = vi.hoisted(() => ({ requested: new Set() }));

vi.mock("./i18n.js", async (importOriginal) => {
  const actual = await importOriginal();
  return {
    ...actual,
    t: (key, params = {}) => {
      requested.add(key);
      const value = lookupValue(EN, key);
      if (typeof value !== "string") return key;
      return value.replace(/\{(\w+)\}/g, (_, name) =>
        params[name] === undefined ? `{${name}}` : String(params[name]),
      );
    },
  };
});

import { hostBadge, LAST_HOST_KEY, setupHostSwitcher } from "./host-switcher.js";

// The shipped markup, not a re-typed fixture: a missing id in index.html must
// fail here rather than pass against a hand-written copy.
const INDEX = new JSDOM(readFileSync("public/index.html", "utf8")).window.document;
const BUTTON_HTML = INDEX.getElementById("host-switcher-btn").outerHTML;
const PICKER_HTML = INDEX.getElementById("host-picker").outerHTML;
const MARKUP_KEYS = [...INDEX.getElementById("host-picker").querySelectorAll("[data-i18n]")]
  .map((node) => node.dataset.i18n)
  .concat("hostPicker.title");

const offer = "picot://connect?offer=eyJzZXJ2ZXJJZCI6InIxIn0";

function fixture(url = "http://127.0.0.1:4321/") {
  const dom = new JSDOM(
    `<!doctype html><body><div class="sidebar-footer">${BUTTON_HTML}${PICKER_HTML}</div></body>`,
    {
      url,
    },
  );
  const { document } = dom.window;
  return {
    dom,
    document,
    button: document.getElementById("host-switcher-btn"),
    label: document.getElementById("host-switcher-label"),
    picker: document.getElementById("host-picker"),
    list: document.getElementById("host-picker-list"),
    notice: document.getElementById("host-picker-notice"),
    addBtn: document.getElementById("host-add-btn"),
    form: document.getElementById("host-add-form"),
    input: document.getElementById("host-add-url"),
    tofu: document.getElementById("host-tofu"),
    tofuTitle: document.getElementById("host-tofu-title"),
    tofuFingerprint: document.getElementById("host-tofu-fingerprint"),
    tofuChanges: document.getElementById("host-tofu-changes"),
    tofuConfirm: document.getElementById("btn-host-tofu-confirm"),
    remove: document.getElementById("host-remove-confirm"),
    removeText: document.getElementById("host-remove-confirm-text"),
    removeConfirm: document.getElementById("btn-host-remove-confirm"),
  };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

const hosts = [
  { serverId: "r1", label: "desk", status: "online", needsPairing: false },
  { serverId: "r2", label: "laptop", status: "connecting", needsPairing: false },
  { serverId: "r3", label: "old", status: "offline", needsPairing: true },
];

function setup(refs, options = {}) {
  const invoke =
    options.invoke ??
    vi.fn(async (command) => {
      if (command === "host_list") return hosts;
      if (command === "host_remove") return { ok: true };
      return { status: "connected", serverId: "r1" };
    });
  const navigate = options.navigate ?? vi.fn();
  const picker = setupHostSwitcher({
    root: refs.document,
    invoke,
    env: refs.dom.window,
    navigate,
    flush: options.flush,
  });
  return { picker, invoke, navigate };
}

async function openPicker(refs) {
  refs.button.click();
  await tick();
}

describe("setupHostSwitcher", () => {
  let refs;

  beforeEach(() => {
    requested.clear();
    refs = fixture();
  });

  it("lists the local host first, then saved hosts with badges and the current one marked", async () => {
    refs = fixture("http://127.0.0.1:4321/?host=r1");
    setup(refs);
    await openPicker(refs);

    const rows = [...refs.list.querySelectorAll(".host-row")];
    expect(rows.map((row) => row.dataset.hostId)).toEqual(["local", "r1", "r2", "r3"]);
    expect(rows[0].querySelector(".host-row-label").textContent).toBe("This Mac");
    expect(rows[0].querySelector(".host-row-remove")).toBeNull();
    expect(rows[1].querySelector(".host-badge").dataset.status).toBe("online");
    expect(rows[2].querySelector(".host-badge").dataset.status).toBe("connecting");
    // A host with no stored credential is invalid, whatever the bridge says.
    expect(rows[3].querySelector(".host-badge").dataset.status).toBe("credentialInvalid");
    expect(rows[1].dataset.current).toBe("true");
    expect(rows[1].querySelector(".host-row-main").getAttribute("aria-current")).toBe("true");
    expect(rows[0].dataset.current).toBeUndefined();
    // The button label names the current host.
    expect(refs.label.textContent).toBe("desk");
  });

  it("switches to a picked host through the URL reload path and remembers it", async () => {
    const { invoke, navigate } = setup(refs);
    await openPicker(refs);

    refs.list.querySelectorAll(".host-row-main")[1].click();
    await tick();

    expect(invoke.mock.calls.map((call) => call[0])).toContain("host_release");
    expect(navigate).toHaveBeenCalledTimes(1);
    expect(navigate.mock.calls[0][0]).toBe("http://127.0.0.1:4321/?host=r1");
    expect(refs.dom.window.localStorage.getItem(LAST_HOST_KEY)).toBe("r1");
    expect(refs.picker.classList.contains("hidden")).toBe(true);
  });

  it("stays on the current host when the draft flush refuses", async () => {
    const { navigate } = setup(refs, {
      flush: () => {
        throw new Error("draft save failed");
      },
    });
    await openPicker(refs);

    refs.list.querySelectorAll(".host-row-main")[1].click();
    await tick();

    expect(navigate).not.toHaveBeenCalled();
    expect(refs.notice.textContent).toContain("Could not save your draft");
    expect(refs.dom.window.localStorage.getItem(LAST_HOST_KEY)).toBeNull();
  });

  it("stays put when the flush reports the draft is not durable", async () => {
    const { navigate } = setup(refs, { flush: async () => false });
    await openPicker(refs);

    refs.list.querySelectorAll(".host-row-main")[1].click();
    await tick();

    expect(navigate).not.toHaveBeenCalled();
    expect(refs.notice.textContent).toContain("Could not save your draft");
    expect(refs.dom.window.localStorage.getItem(LAST_HOST_KEY)).toBeNull();
  });

  it("posts the pasted offer, clears the input, and shows the new-host question", async () => {
    const invoke = vi.fn(async (command) => {
      if (command === "host_list") return hosts;
      if (command === "host_add")
        return {
          status: "tofuPending",
          questionId: 4,
          serverId: "r4",
          keyFingerprint: "ABCD 1234 EF56 7890",
          kind: "newHost",
          changes: [],
        };
      return { status: "connected", serverId: "r4" };
    });
    setup(refs, { invoke });
    await openPicker(refs);

    refs.addBtn.click();
    expect(refs.form.classList.contains("hidden")).toBe(false);
    refs.input.value = offer;
    refs.form.dispatchEvent(
      new refs.dom.window.Event("submit", { bubbles: true, cancelable: true }),
    );
    await tick();

    expect(invoke).toHaveBeenCalledWith("host_add", { offerUrl: offer });
    expect(refs.input.value).toBe("");
    expect(refs.tofu.classList.contains("hidden")).toBe(false);
    expect(refs.tofuTitle.textContent).toBe("Trust this new host?");
    expect(refs.tofuFingerprint.textContent).toContain("ABCD 1234 EF56 7890");

    refs.tofuConfirm.click();
    await tick();

    expect(invoke).toHaveBeenCalledWith("host_confirm_tofu", { questionId: 4, approved: true });
    expect(refs.tofu.classList.contains("hidden")).toBe(true);
    expect(refs.notice.textContent).toContain("Connected to r4");
  });

  it("names the added host by its saved label, not a missing status field", async () => {
    const invoke = vi.fn(async (command) => {
      if (command === "host_list") return hosts;
      // status_payload carries serverId and status only — no label (F-05).
      return { status: "connected", serverId: "r1" };
    });
    setup(refs, { invoke });
    await openPicker(refs);

    refs.addBtn.click();
    refs.input.value = offer;
    refs.form.dispatchEvent(
      new refs.dom.window.Event("submit", { bubbles: true, cancelable: true }),
    );
    await tick();

    expect(refs.notice.textContent).toBe("Connected to desk.");
    expect(refs.notice.textContent).not.toContain("undefined");
  });

  it("keeps the question across close and reopen, and answers it afterwards", async () => {
    const invoke = vi.fn(async (command) => {
      if (command === "host_list") return hosts;
      if (command === "host_add")
        return {
          status: "tofuPending",
          questionId: 7,
          serverId: "r4",
          keyFingerprint: "FFFF 0000",
          kind: "newHost",
          changes: [],
        };
      return { status: "connected", serverId: "r4" };
    });
    setup(refs, { invoke });
    await openPicker(refs);

    refs.addBtn.click();
    refs.input.value = offer;
    refs.form.dispatchEvent(
      new refs.dom.window.Event("submit", { bubbles: true, cancelable: true }),
    );
    await tick();
    expect(refs.tofu.classList.contains("hidden")).toBe(false);

    refs.button.click(); // close
    await tick();
    expect(refs.picker.classList.contains("hidden")).toBe(true);

    refs.button.click(); // reopen: the app still binds this question to this window
    await tick();
    expect(refs.tofu.classList.contains("hidden")).toBe(false);
    expect(refs.tofuFingerprint.textContent).toContain("FFFF 0000");

    refs.tofuConfirm.click();
    await tick();
    expect(invoke).toHaveBeenCalledWith("host_confirm_tofu", { questionId: 7, approved: true });
    expect(refs.tofu.classList.contains("hidden")).toBe(true);
  });

  it("shows the changed-connection question with its diff lines", async () => {
    const invoke = vi.fn(async (command) => {
      if (command === "host_list") return hosts;
      return {
        status: "tofuPending",
        questionId: 9,
        serverId: "r1",
        keyFingerprint: "AAAA 1111",
        kind: "changedConnection",
        changes: [
          { field: "relayEndpoint", saved: "relay.old:443", offered: "relay.new:443" },
          { field: "useTls", saved: true, offered: false },
        ],
      };
    });
    setup(refs, { invoke });
    await openPicker(refs);

    refs.addBtn.click();
    refs.input.value = offer;
    refs.form.dispatchEvent(
      new refs.dom.window.Event("submit", { bubbles: true, cancelable: true }),
    );
    await tick();

    expect(refs.tofuTitle.textContent).toBe("This host's connection changed. Trust it again?");
    const diffs = [...refs.tofuChanges.querySelectorAll(".host-tofu-change")].map(
      (node) => node.textContent,
    );
    expect(diffs).toEqual(["Relay: relay.old:443 → relay.new:443", "TLS: true → false"]);
  });

  it("keeps one question at a time and answers only the newest", async () => {
    let round = 0;
    const invoke = vi.fn(async (command) => {
      if (command === "host_list") return hosts;
      if (command === "host_add") {
        round += 1;
        return {
          status: "tofuPending",
          questionId: round,
          serverId: `r${round}`,
          keyFingerprint: `KEY${round}`,
          kind: "newHost",
          changes: [],
        };
      }
      return { status: "connected", serverId: "r2" };
    });
    setup(refs, { invoke });
    await openPicker(refs);

    for (const value of ["picot://connect?offer=a", "picot://connect?offer=b"]) {
      refs.addBtn.click();
      refs.input.value = value;
      refs.form.dispatchEvent(
        new refs.dom.window.Event("submit", { bubbles: true, cancelable: true }),
      );
      await tick();
    }

    expect(refs.document.querySelectorAll("#host-tofu").length).toBe(1);
    expect(refs.tofuFingerprint.textContent).toContain("KEY2");

    refs.tofuConfirm.click();
    await tick();

    expect(invoke).toHaveBeenCalledWith("host_confirm_tofu", { questionId: 2, approved: true });
  });

  it("removes a host with §2.5 copy that says the remote still trusts this device", async () => {
    const { invoke } = setup(refs);
    await openPicker(refs);

    refs.list.querySelectorAll(".host-row-remove")[1].click();
    await tick();

    expect(refs.remove.classList.contains("hidden")).toBe(false);
    expect(refs.removeText.textContent).toContain("laptop");
    expect(refs.removeText.textContent).toContain("still trusts this device");

    refs.removeConfirm.click();
    await tick();

    expect(invoke).toHaveBeenCalledWith("host_remove", { serverId: "r2" });
    expect(refs.notice.textContent).toContain("Removed laptop");
  });

  it("comes home to the local host after deleting the host it is on", async () => {
    refs = fixture("http://127.0.0.1:4321/?host=r2");
    const { navigate } = setup(refs);
    await openPicker(refs);

    // Remove buttons exist for remote rows only: r1, r2, r3 — r2 is this window.
    refs.list.querySelectorAll(".host-row-remove")[1].click();
    await tick();
    refs.removeConfirm.click();
    await tick();

    expect(navigate).toHaveBeenCalled();
    expect(navigate.mock.calls[0][0]).toBe("http://127.0.0.1:4321/");
  });

  it("hides the button when the shell has no app IPC", () => {
    setupHostSwitcher({ root: refs.document, invoke: null, env: refs.dom.window });
    expect(refs.button.classList.contains("hidden")).toBe(true);
  });

  it("badge mapping never claims an unknown status is reachable", () => {
    expect(hostBadge({ status: "online" })).toBe("online");
    expect(hostBadge({ status: "credentialInvalid" })).toBe("credentialInvalid");
    expect(hostBadge({ status: "stopped" })).toBe("offline");
    expect(hostBadge({ status: "offline", needsPairing: true })).toBe("credentialInvalid");
    expect(hostBadge(undefined)).toBe("offline");
  });

  it("asks only for translatable keys, all of which exist", async () => {
    const invoke = vi.fn(async (command) => {
      if (command === "host_list") return hosts;
      if (command === "host_add")
        return {
          status: "tofuPending",
          questionId: 1,
          kind: "changedConnection",
          keyFingerprint: "K",
          changes: [{ field: "daemonKey", saved: "a", offered: "b" }],
        };
      return { status: "cancelled" };
    });
    setup(refs, { invoke });
    await openPicker(refs);
    refs.addBtn.click();
    refs.input.value = offer;
    refs.form.dispatchEvent(
      new refs.dom.window.Event("submit", { bubbles: true, cancelable: true }),
    );
    await tick();

    const missing = [...requested, ...MARKUP_KEYS].filter(
      (key) => typeof lookupValue(EN, key) !== "string",
    );
    expect(missing).toEqual([]);
    // Every key the picker can reach has four-locale parity (i18n parity suite);
    // this asserts the ones its own render paths produce are in English copy.
    expect(requested).toContain("hostPicker.status.credentialInvalid");
    expect(requested).toContain("hostPicker.change.daemonKey");
    expect(requested).toContain("hostPicker.tofuTitleChanged");
  });
});
