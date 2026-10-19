// @vitest-environment jsdom

// ABOUTME: Verifies the Settings → Daemon → Pair Device block (relay
// ABOUTME: Phase 2, spec §3.2): the two Paseo-style states, the consent→enable
// ABOUTME: call shape, the link-only offer render, copy
// ABOUTME: feedback, regenerate, disable, the keepRunning hint condition and
// ABOUTME: locale coverage.

import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const LOCALES = ["en", "zh", "ja", "es"];

const messagesFor = (locale) => JSON.parse(readFileSync(`public/locales/${locale}.json`, "utf8"));

const lookupValue = (messages, key) => {
  let node = messages;
  for (const part of key.split(".")) {
    if (node == null || typeof node !== "object") return undefined;
    node = node[part];
  }
  return typeof node === "string" ? node : undefined;
};

const leafKeys = (node, prefix) =>
  Object.entries(node).flatMap(([name, value]) =>
    value && typeof value === "object"
      ? leafKeys(value, `${prefix}${name}.`)
      : [`${prefix}${name}`],
  );

// The same i18n contract the daemon-section suite uses: mocked t() records
// every key the renderer asks for and resolves from en.json.
const { requested } = vi.hoisted(() => ({ requested: new Set() }));

vi.mock("../i18n.js", async (importOriginal) => {
  const actual = await importOriginal();
  const { readFileSync: read } = await import("node:fs");
  const en = JSON.parse(read("public/locales/en.json", "utf8"));
  return {
    ...actual,
    t: (key, params = {}) => {
      requested.add(key);
      const value = lookupValue(en, key);
      if (typeof value !== "string") return key;
      return value.replace(/\{(\w+)\}/g, (_, name) =>
        params[name] === undefined ? `{${name}}` : String(params[name]),
      );
    },
  };
});

import { setupPairDeviceSection, tauriRelayTransport } from "./pair-device-section.js";

const EN = messagesFor("en");
const COPY = EN.settings.daemon.pairDevice;
const GROUP_KEYS = "settings.daemon.pairDevice.";

// The block's markup is extracted from the shipped index.html, not re-typed:
// a missing id in the real page must fail these tests, not silently pass
// against a hand-written fixture.
const INDEX = new JSDOM(readFileSync("public/index.html", "utf8")).window.document;
// The block is four consecutive siblings inside the daemon section (the
// collapsed row, the notice, the consent and the offer): walking them from
// the real page keeps a missing or reordered node a test failure, not a
// silent skip.
const BLOCK_NODES = (() => {
  const nodes = [];
  let node = INDEX.getElementById("setting-pair-device");
  while (node) {
    nodes.push(node);
    if (node.id === "pair-device-offer") return nodes;
    node = node.nextElementSibling;
  }
  throw new Error("index.html: the pair-device offer row is missing");
})();
const BLOCK_HTML = BLOCK_NODES.map((node) => node.outerHTML).join("\n");
// The static copy the i18n DOM pass translates (it never travels through t()).
const MARKUP_KEYS = BLOCK_NODES.flatMap((node) => [
  ...node.querySelectorAll("[data-i18n], [data-i18n-aria-label]"),
]).flatMap((node) => [node.dataset.i18n, node.dataset.i18nAriaLabel].filter(Boolean));

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

/** A `relay_status` reply as the daemon's admin op produces it. */
const relayOff = () => ({
  enabled: false,
  state: "offline",
  endpoint: "paseo.021521.xyz:443",
  serverId: null,
});

const relayOn = (state = "online") => ({
  enabled: true,
  state,
  endpoint: "paseo.021521.xyz:443",
  serverId: "srv-1",
});

/** A `daemon_status` reply: the pair block only reads keepRunningAfterQuit. */
const daemonStatus = (keepRunningAfterQuit = true) => ({ running: true, keepRunningAfterQuit });

function mountBlock() {
  document.body.innerHTML = BLOCK_HTML;
  return {
    row: document.getElementById("setting-pair-device"),
    openBtn: document.getElementById("btn-pair-device-open"),
    notice: document.getElementById("pair-device-notice"),
    consent: document.getElementById("pair-device-consent"),
    consentTitle: document.getElementById("pair-device-consent-title"),
    consentText: document.getElementById("pair-device-consent-text"),
    keepHint: document.getElementById("pair-device-keep-running-hint"),
    cancelBtn: document.getElementById("btn-pair-device-cancel"),
    enableBtn: document.getElementById("btn-pair-device-enable"),
    offer: document.getElementById("pair-device-offer"),
    state: document.getElementById("pair-device-state"),
    qr: document.getElementById("pair-device-qr"),
    url: document.getElementById("pair-device-url"),
    expired: document.getElementById("pair-device-expired"),
    copyBtn: document.getElementById("btn-pair-device-copy"),
    regenerateBtn: document.getElementById("btn-pair-device-regenerate"),
    disableBtn: document.getElementById("btn-pair-device-disable"),
  };
}

/** Mock `__TAURI__.core.invoke` for the three relay commands. */
function mountInvoke({ status = null, setEnabled = null, offer = null } = {}) {
  const calls = [];
  let mints = 0;
  const invoke = vi.fn(async (command, args) => {
    calls.push({ command, args });
    if (command === "relay_status") {
      if (status instanceof Error) throw status;
      return typeof status === "function" ? status() : status;
    }
    if (command === "relay_set_enabled") {
      if (setEnabled instanceof Error) throw setEnabled;
      return typeof setEnabled === "function" ? setEnabled(args) : { enabled: args?.enabled };
    }
    if (command === "relay_offer") {
      mints += 1;
      if (offer instanceof Error) throw offer;
      return typeof offer === "function"
        ? offer(mints)
        : { url: `https://picot.021521.xyz/#offer=t${mints}`, expiresAt: 4000000000 };
    }
    throw new Error(`unexpected command ${command}`);
  });
  window.__TAURI__ = { core: { invoke } };
  return { invoke, calls, commands: () => calls.map((call) => call.command) };
}

/** Mock the vendor QR renderer and the clipboard. */
function mountShell() {
  const toCanvas = vi.fn(async () => {});
  globalThis.PicotQr = { toCanvas };
  const writeText = vi.fn(async () => {});
  Object.defineProperty(globalThis.navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
  return { toCanvas, writeText };
}

function setup(overrides = {}, daemonKeepRunning = true) {
  const dom = mountBlock();
  const harness = mountInvoke(overrides);
  const shell = mountShell();
  const page = setupPairDeviceSection({ root: document });
  return { ...dom, ...harness, ...shell, page, daemon: daemonStatus(daemonKeepRunning) };
}

beforeEach(() => {
  document.body.innerHTML = "";
  delete window.__TAURI__;
  delete globalThis.PicotQr;
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("relay off", () => {
  it("renders the collapsed row only, and reads the relay state once", async () => {
    const { page, row, consent, offer, commands } = setup({ status: relayOff() });
    await page.activate();
    expect(row.hidden).toBe(false);
    expect(consent.hidden).toBe(true);
    expect(offer.hidden).toBe(true);
    // Disabled means no offer is minted — the poll read is the only traffic.
    expect(commands()).toEqual(["relay_status"]);
  });

  it("expands the consent copy with the relay endpoint, and backs out", async () => {
    const { page, row, consent, consentTitle, consentText, cancelBtn, openBtn, calls } = setup({
      status: relayOff(),
    });
    await page.activate();
    openBtn.click();
    expect(consent.hidden).toBe(false);
    expect(consentTitle.textContent).toBe(COPY.enableTitle);
    // The company relay address is part of the copy the user consents to.
    expect(consentText.textContent).toContain("paseo.021521.xyz:443");
    expect(calls).toHaveLength(1); // expanding sends nothing

    cancelBtn.click();
    expect(consent.hidden).toBe(true);
    expect(row.hidden).toBe(false);
  });

  it("warns about quitting only when keepRunningAfterQuit is false", async () => {
    const hidden = setup({ status: relayOff() }, false);
    await hidden.page.activate(hidden.daemon);
    hidden.openBtn.click();
    expect(hidden.keepHint.hidden).toBe(false);
    expect(hidden.keepHint.textContent).toBe(COPY.keepRunningHint);

    const kept = setup({ status: relayOff() }, true);
    await kept.page.activate(kept.daemon);
    kept.openBtn.click();
    expect(kept.keepHint.hidden).toBe(true);
  });
});

describe("enabling", () => {
  it("sends relay_set_enabled{true}, then lands in the offer state", async () => {
    const { page, openBtn, enableBtn, row, consent, offer, url, state, calls, toCanvas } = setup({
      status: () => (calls.length <= 1 ? relayOff() : relayOn()),
    });
    await page.activate();
    openBtn.click();
    enableBtn.click();
    await flush();
    await flush();
    expect(calls[1]).toEqual({ command: "relay_set_enabled", args: { enabled: true } });
    // The fresh state + a minted offer are what the offer state renders.
    expect(calls.map((call) => call.command)).toEqual([
      "relay_status",
      "relay_set_enabled",
      "relay_status",
      "relay_offer",
    ]);
    expect(row.hidden).toBe(true);
    expect(consent.hidden).toBe(true);
    expect(offer.hidden).toBe(false);
    expect(state.textContent).toBe(`${COPY.state.online} · paseo.021521.xyz:443`);
    expect(url.value).toBe("https://picot.021521.xyz/#offer=t1");
    expect(document.getElementById("pair-device-qr")).toBeNull();
    expect(toCanvas).not.toHaveBeenCalled();
    expect(url.readOnly).toBe(true);
  });

  it("keeps the consent open and names the failure when enabling fails", async () => {
    const { page, openBtn, enableBtn, consent, offer, notice } = setup({
      status: relayOff(),
      setEnabled: new Error("relay enablement failed: metadata lock poisoned"),
    });
    await page.activate();
    openBtn.click();
    enableBtn.click();
    await flush();
    await flush();
    expect(consent.hidden).toBe(false);
    expect(offer.hidden).toBe(true);
    expect(notice.classList.contains("hidden")).toBe(false);
    expect(notice.textContent).toContain("metadata lock poisoned");
  });
});

describe("relay on (offer state)", () => {
  it("renders the live state, endpoint, client note and warning", async () => {
    const { page, offer, state, notice } = setup({ status: relayOn("connecting") });
    await page.activate();
    expect(offer.hidden).toBe(false);
    expect(state.textContent).toBe(`${COPY.state.connecting} · paseo.021521.xyz:443`);
    // The placeholder-host honesty line and the warning are static markup:
    // assert the keys are wired (the i18n DOM pass owns the text, and the
    // locale suite below owns the copy).
    expect(
      offer.querySelector('[data-i18n="settings.daemon.pairDevice.requiresClient"]'),
    ).not.toBeNull();
    expect(
      offer.querySelector('[data-i18n="settings.daemon.pairDevice.securityWarning"]'),
    ).not.toBeNull();
    expect(notice.classList.contains("hidden")).toBe(true);
  });

  it("does not re-mint on every poll: one offer survives refreshes", async () => {
    const { page, commands } = setup({ status: relayOn() });
    await page.activate();
    expect(commands()).toEqual(["relay_status", "relay_offer"]);
    await page.refresh(daemonStatus());
    await page.refresh(daemonStatus());
    expect(commands()).toEqual(["relay_status", "relay_offer", "relay_status", "relay_status"]);
  });

  it("marks the link expired once past expiresAt and regenerates without QR", async () => {
    const now = Math.floor(Date.now() / 1000);
    const { page, qr, url, expired, toCanvas, regenerateBtn } = setup({
      status: relayOn(),
      offer: (mints) => ({
        url: `https://picot.021521.xyz/#offer=t${mints}`,
        // The first mint is already dead; Regenerate is the way out.
        expiresAt: mints === 1 ? now - 60 : now + 300,
      }),
    });
    await page.activate();
    // The selectable URL retains its expired marker until regenerated.
    expect(qr).toBeNull();
    expect(toCanvas).not.toHaveBeenCalled();
    expect(expired.hidden).toBe(false);
    expect(expired.textContent.trim()).toBe(COPY.expired);
    expect(url.value).toBe("https://picot.021521.xyz/#offer=t1");

    regenerateBtn.click();
    await flush();
    expect(url.value).toBe("https://picot.021521.xyz/#offer=t2");
    expect(toCanvas).not.toHaveBeenCalled();
    expect(expired.hidden).toBe(true);
  });

  it("renders only the read-only link while the offer is fresh", async () => {
    const now = Math.floor(Date.now() / 1000);
    const { page, qr, expired, toCanvas } = setup({
      status: relayOn(),
      offer: () => ({ url: "https://picot.021521.xyz/#offer=fresh", expiresAt: now + 300 }),
    });
    await page.activate();
    expect(expired.hidden).toBe(true);
    expect(qr).toBeNull();
    expect(toCanvas).not.toHaveBeenCalled();
  });

  it("copies the link and shows the two-second confirmation", async () => {
    vi.useFakeTimers();
    const { page, copyBtn, writeText } = setup({ status: relayOn() });
    await page.activate();
    copyBtn.click();
    await vi.advanceTimersByTimeAsync(0);
    expect(writeText).toHaveBeenCalledWith("https://picot.021521.xyz/#offer=t1");
    expect(copyBtn.textContent).toBe(COPY.copied);
    await vi.advanceTimersByTimeAsync(2000);
    expect(copyBtn.textContent).toBe(COPY.copy);
  });

  it("regenerates: a new offer URL, and the old code is named dead", async () => {
    const { page, regenerateBtn, url, notice, toCanvas, commands } = setup({
      status: relayOn(),
    });
    await page.activate();
    regenerateBtn.click();
    await flush();
    expect(commands().at(-1)).toBe("relay_offer");
    expect(url.value).toBe("https://picot.021521.xyz/#offer=t2");
    expect(toCanvas).not.toHaveBeenCalled();
    expect(notice.classList.contains("hidden")).toBe(false);
    expect(notice.textContent).toBe(COPY.regenerateNotice);
  });

  it("does not need a QR renderer and keeps LAN QR assets", async () => {
    const { page, qr, notice } = setup({ status: relayOn() });
    globalThis.PicotQr = {
      toCanvas: async () => {
        throw new Error("no renderer");
      },
    };
    await page.activate();
    expect(qr).toBeNull();
    expect(notice.textContent).toBe("");
    expect(INDEX.getElementById("lan-qr-canvas")).not.toBeNull();
  });

  it("disables relay and returns to the collapsed row", async () => {
    let enabled = true;
    const { page, disableBtn, row, offer, calls } = setup({
      status: () => (enabled ? relayOn() : relayOff()),
      setEnabled: (args) => {
        enabled = args.enabled;
      },
    });
    await page.activate();
    disableBtn.click();
    await flush();
    await flush();
    const sent = calls.filter((call) => call.command === "relay_set_enabled");
    expect(sent).toEqual([{ command: "relay_set_enabled", args: { enabled: false } }]);
    expect(row.hidden).toBe(false);
    expect(offer.hidden).toBe(true);
  });

  it("keeps the offer and names the failure when disabling fails", async () => {
    const { page, disableBtn, offer, notice } = setup({
      status: relayOn(),
      setEnabled: new Error("relay enablement failed: lock poisoned"),
    });
    await page.activate();
    disableBtn.click();
    await flush();
    await flush();
    expect(offer.hidden).toBe(false);
    expect(notice.classList.contains("hidden")).toBe(false);
    expect(notice.textContent).toContain("lock poisoned");
  });

  it("stays silent without Tauri IPC instead of leaking the shell error", async () => {
    const dom = mountBlock();
    delete window.__TAURI__;
    const page = setupPairDeviceSection({ root: document });
    await page.activate();
    expect(dom.row.hidden).toBe(false);
    expect(dom.notice.classList.contains("hidden")).toBe(true);
    expect(dom.notice.textContent).toBe("");
  });
});

describe("transport", () => {
  it("reports IPC presence and rejects reads in a plain browser shell", () => {
    delete window.__TAURI__;
    expect(tauriRelayTransport().available()).toBe(false);
    window.__TAURI__ = { core: { invoke: () => Promise.resolve() } };
    expect(tauriRelayTransport().available()).toBe(true);
    delete window.__TAURI__;
    return expect(tauriRelayTransport().status()).rejects.toThrow(/Tauri IPC/);
  });
});

// ── Locale coverage (mirrors the daemon-section suite) ───────────────────────

describe("locale coverage", () => {
  it("requires a client app to open the link in every locale, without scan instructions", () => {
    const expected = {
      en: "You need the Picot client app to open this link.",
      zh: "需要客户端 app 才能打开此链接。",
      ja: "このリンクを開くには Picot クライアントアプリが必要です。",
      es: "Necesitas la app cliente de Picot para abrir este enlace.",
    };
    for (const locale of LOCALES) {
      expect(messagesFor(locale).settings.daemon.pairDevice.requiresClient).toBe(expected[locale]);
    }
  });

  it("resolves every requested key in every locale, with no orphaned copy", async () => {
    await renderEveryBranch();

    expect([...new Set(requested)].filter((key) => !key.startsWith(GROUP_KEYS))).toEqual([]);
    const keys = new Set([
      ...[...requested].filter((key) => key.startsWith(GROUP_KEYS)),
      ...MARKUP_KEYS,
    ]);
    expect(keys.size).toBeGreaterThan(10);
    expect(MARKUP_KEYS.every((key) => key.startsWith(GROUP_KEYS))).toBe(true);
    for (const locale of LOCALES) {
      const messages = messagesFor(locale);
      for (const key of keys) {
        const value = lookupValue(messages, key);
        expect(typeof value, `${locale} is missing ${key}`).toBe("string");
        expect(value.trim(), `${locale} has empty copy for ${key}`).not.toBe("");
      }
    }
    // No unused copy in en.json (the reference locale).
    const declared = leafKeys(EN.settings.daemon.pairDevice, GROUP_KEYS);
    expect([...declared].sort()).toEqual([...keys].sort());
  });
});

async function renderEveryBranch() {
  // The collapsed row, the consent (both hint conditions), and a failed read.
  for (const [relay, keepRunning] of [
    [relayOff(), false],
    [relayOff(), true],
    [new Error("no IPC")],
  ]) {
    mountBlock();
    mountInvoke({ status: relay });
    mountShell();
    const page = setupPairDeviceSection({ root: document });
    await page.activate(daemonStatus(keepRunning));
    document.getElementById("btn-pair-device-open")?.click();
    await flush();
  }
  // The offer state: every connection state, regenerate, copy, and each
  // failure notice.
  for (const state of ["offline", "connecting", "online"]) {
    mountBlock();
    mountInvoke({ status: relayOn(state) });
    mountShell();
    const page = setupPairDeviceSection({ root: document });
    await page.activate();
    document.getElementById("btn-pair-device-copy").click();
    await flush();
    document.getElementById("btn-pair-device-regenerate").click();
    await flush();
  }
  for (const failing of [{ setEnabled: new Error("no") }, { offer: new Error("no") }]) {
    mountBlock();
    mountInvoke({ status: relayOn(), ...failing });
    mountShell();
    const page = setupPairDeviceSection({ root: document });
    await page.activate();
    document.getElementById("btn-pair-device-disable").click();
    await flush();
    await flush();
    document.getElementById("btn-pair-device-regenerate").click();
    await flush();
    await flush();
  }
  // The enable path (its own consent→offer transition).
  mountBlock();
  mountInvoke({ status: () => relayOff() });
  mountShell();
  {
    const page = setupPairDeviceSection({ root: document });
    await page.activate();
    document.getElementById("btn-pair-device-open").click();
    document.getElementById("btn-pair-device-enable").click();
    await flush();
    await flush();
  }
  // The expired-offer branch retains the URL and marks it expired.
  mountBlock();
  mountInvoke({
    status: relayOn(),
    offer: () => ({ url: "https://picot.021521.xyz/#offer=dead", expiresAt: 0 }),
  });
  mountShell();
  {
    const page = setupPairDeviceSection({ root: document });
    await page.activate();
    await flush();
  }
}
