// @vitest-environment jsdom

// ABOUTME: Verifies the Daemon tab's paired-device list and its revoke confirm:
// ABOUTME: safe fields only, one confirm per device, fresh data after a revoke.

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

vi.mock("../i18n.js", async (importOriginal) => {
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

import { setupDeviceManage } from "./device-manage.js";

const INDEX = new JSDOM(readFileSync("public/index.html", "utf8")).window.document;
const SECTION = INDEX.getElementById("setting-remote-section");
const SECTION_HTML = SECTION.outerHTML;
const MARKUP_KEYS = [...SECTION.querySelectorAll("[data-i18n]")].map((node) => node.dataset.i18n);

function fixture() {
  const dom = new JSDOM(`<!doctype html><body>${SECTION_HTML}</body>`, {
    url: "http://127.0.0.1:4321/",
  });
  const { document } = dom.window;
  return {
    dom,
    document,
    list: document.getElementById("device-manage-list"),
    empty: document.getElementById("device-manage-empty"),
    notice: document.getElementById("device-manage-notice"),
    confirm: document.getElementById("device-revoke-confirm"),
    confirmText: document.getElementById("device-revoke-confirm-text"),
    confirmBtn: document.getElementById("btn-device-revoke-confirm"),
  };
}

const devices = [
  { deviceId: "phone-1", pairedAt: 1700000000, owner: "owner-a" },
  { deviceId: "laptop-2", pairedAt: 1700000100, owner: null },
];

function transport({ list = devices } = {}) {
  return {
    available: () => true,
    list: vi.fn(async () => ({ devices: list })),
    revoke: vi.fn(async () => ({ ok: true })),
  };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("setupDeviceManage", () => {
  let refs;

  beforeEach(() => {
    requested.clear();
    refs = fixture();
  });

  it("renders device id, pairing time and owner for every paired device", async () => {
    const ipc = transport();
    const view = setupDeviceManage({ root: refs.document, transport: ipc, locale: () => "en-US" });

    await view.refresh();

    const rows = [...refs.list.querySelectorAll(".device-row")];
    expect(rows.map((row) => row.dataset.deviceId)).toEqual(["phone-1", "laptop-2"]);
    expect(rows[0].querySelector(".device-row-id").textContent).toBe("phone-1");
    expect(rows[0].textContent).toContain("Paired ");
    expect(rows[0].textContent).toContain("Owner owner-a");
    // A pre-Gate-B row has no owner: say so instead of showing "null".
    expect(rows[1].textContent).toContain("Owner unknown");
    expect(rows[1].textContent).not.toContain("null");
    expect(refs.empty.classList.contains("hidden")).toBe(true);
    expect(refs.confirm.hidden).toBe(true);
  });

  it("shows the empty state when no device is paired", async () => {
    const view = setupDeviceManage({ root: refs.document, transport: transport({ list: [] }) });

    await view.refresh();

    expect(refs.list.querySelectorAll(".device-row").length).toBe(0);
    expect(refs.empty.classList.contains("hidden")).toBe(false);
  });

  it("asks once per device and revokes the exact id, then re-reads", async () => {
    const ipc = transport();
    const view = setupDeviceManage({ root: refs.document, transport: ipc });

    await view.refresh();
    refs.list.querySelectorAll("[data-revoke-id]")[0].click();

    expect(refs.confirm.hidden).toBe(false);
    expect(refs.confirmText.textContent).toContain("phone-1");
    expect(refs.confirmText.textContent).toContain("loses access right away");

    refs.confirmBtn.click();
    await tick();

    expect(ipc.revoke).toHaveBeenCalledWith("phone-1");
    // Truth after the action comes from the daemon, never from the click.
    expect(ipc.list).toHaveBeenCalledTimes(2);
    expect(refs.notice.textContent).toContain("Revoked phone-1");
    expect(refs.confirm.hidden).toBe(true);
  });

  it("reports a failed read without showing a stale list", async () => {
    const ipc = {
      available: () => true,
      list: vi.fn(async () => {
        throw new Error("daemon offline");
      }),
      revoke: vi.fn(),
    };
    const view = setupDeviceManage({ root: refs.document, transport: ipc });

    await view.refresh();

    expect(refs.notice.textContent).toContain("Could not read the paired devices: daemon offline");
    expect(refs.list.querySelectorAll(".device-row").length).toBe(0);
  });

  it("drops the pending confirm when the section is left", async () => {
    const ipc = transport();
    const view = setupDeviceManage({ root: refs.document, transport: ipc });

    await view.refresh();
    refs.list.querySelectorAll("[data-revoke-id]")[1].click();
    view.leave();
    await view.refresh();

    expect(refs.confirm.hidden).toBe(true);
  });

  it("asks only for translatable keys, all of which exist", async () => {
    const view = setupDeviceManage({ root: refs.document, transport: transport() });

    await view.refresh();
    refs.list.querySelectorAll("[data-revoke-id]")[0].click();

    const missing = [...requested, ...MARKUP_KEYS].filter(
      (key) => typeof lookupValue(EN, key) !== "string",
    );
    expect(missing).toEqual([]);
    expect(requested).toContain("settings.daemon.devices.revokeConfirm");
  });
});
