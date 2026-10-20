// ABOUTME: Tests the extracted Community package browser without app-level globals.
// ABOUTME: Locks installed-state handling, registry rendering, and desktop-only actions.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { setupPackageBrowse } from "./package-browse.js";

function translations(key, params = {}) {
  const values = {
    "extensions.loadingPackages": "Loading",
    "extensions.failedToLoadPackages": "Failed",
    "extensions.noPackagesMatch": "No matches",
    "extensions.browseCountRange": `${params.start}-${params.end}/${params.total}`,
    "extensions.browseCountZero": `0/${params.total}`,
    "extensions.downloadsPerMonth": `${params.count} downloads`,
    "extensions.desktopOnly": "Desktop only",
    "actions.install": "Install",
    "actions.uninstall": "Uninstall",
    "status.installing": "Installing",
    "status.uninstalling": "Uninstalling",
    "status.removing": "Removing",
    "browse.openExternalPrompt": "Open external link",
    "actions.retry": "Retry",
  };
  return values[key] || key;
}

function createRoot() {
  const root = document.implementation.createHTMLDocument("browse");
  root.body.innerHTML = `
    <input id="pkg-browse-search">
    <div id="pkg-browse-pills"></div>
    <select id="pkg-browse-sort"></select>
    <span id="pkg-browse-count"></span>
    <div id="pkg-browse-list"></div>
  `;
  return root;
}

describe("Community package browser", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("renders all registry results and maps rich installed records by source", async () => {
    const root = createRoot();
    const transport = {
      listPiPackages: vi
        .fn()
        .mockResolvedValue([{ source: "npm:installed", scope: "global", disabled: false }]),
      installPiPackage: vi.fn(),
      removePiPackage: vi.fn(),
      openExternal: vi.fn(),
    };
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({
          version: 1,
          packages: [
            { name: "installed", description: "Installed package", types: ["extensions"] },
            { name: "available", description: "Available package", types: ["skills"] },
          ],
        }),
      }),
    );

    const browser = setupPackageBrowse({
      root,
      transport,
      nativeAvailable: () => true,
      t: translations,
      createIcon: () => document.createElement("span"),
      renderPackageInstallFailure: vi.fn(),
      setExtensionActionButton: (button, label) => {
        button.textContent = label;
      },
      catalogUrl: "https://registry.test/catalog.json",
    });
    await browser.load();

    const rows = root.querySelectorAll(".pkg-browse-row");
    expect(rows).toHaveLength(2);
    expect(rows[0].querySelector(".settings-extension-actions button").textContent).toBe(
      "Uninstall",
    );
    expect(rows[1].querySelector(".settings-extension-actions button").textContent).toBe("Install");
    // Install and uninstall must be visually distinguishable: solid accent for
    // the affirmative action, danger ghost for the destructive one.
    const installedBtn = rows[0].querySelector(".settings-extension-actions button");
    const availableBtn = rows[1].querySelector(".settings-extension-actions button");
    expect(installedBtn.classList.contains("is-danger")).toBe(true);
    expect(installedBtn.classList.contains("is-primary")).toBe(false);
    expect(availableBtn.classList.contains("is-primary")).toBe(true);
    expect(availableBtn.classList.contains("is-danger")).toBe(false);
    expect(root.querySelector("#pkg-browse-installed-only")).toBeNull();
    expect(browser.getInstalledSources()).toEqual(new Set(["npm:installed"]));
  });

  it("disables install actions for non-native clients", async () => {
    const root = createRoot();
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({ packages: [{ name: "available", types: [] }], totalPages: 1 }),
      }),
    );
    const browser = setupPackageBrowse({
      root,
      transport: { listPiPackages: vi.fn() },
      nativeAvailable: () => false,
      t: translations,
      createIcon: () => document.createElement("span"),
      renderPackageInstallFailure: vi.fn(),
      setExtensionActionButton: (button, label) => {
        button.textContent = label;
      },
    });
    await browser.load();

    const button = root.querySelector(".pkg-browse-row .settings-extension-actions button");
    expect(button.disabled).toBe(true);
    expect(button.textContent).toBe("Desktop only");
  });
  it("notifies the Installed list after a successful install but not on failure", async () => {
    const root = createRoot();
    const transport = {
      listPiPackages: vi.fn().mockResolvedValue([]),
      installPiPackage: vi
        .fn()
        .mockImplementation((source) =>
          source === "npm:bad" ? Promise.reject(new Error("offline")) : Promise.resolve({}),
        ),
      removePiPackage: vi.fn(),
      openExternal: vi.fn(),
    };
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({
          packages: [
            { name: "good", description: "Installs", types: [] },
            { name: "bad", description: "Fails", types: [] },
          ],
        }),
      }),
    );
    const onInstalledChanged = vi.fn();
    const browser = setupPackageBrowse({
      root,
      transport,
      nativeAvailable: () => true,
      t: translations,
      createIcon: () => document.createElement("span"),
      renderPackageInstallFailure: vi.fn(),
      setExtensionActionButton: (button, label) => {
        button.textContent = label;
      },
      onInstalledChanged,
      catalogUrl: "https://registry.test/catalog.json",
    });
    await browser.load();

    const buttons = () =>
      root.querySelectorAll(".pkg-browse-row .settings-extension-actions button");
    buttons()[0].click();
    await Promise.resolve();
    await Promise.resolve();
    expect(transport.installPiPackage).toHaveBeenCalledWith("npm:good");
    expect(onInstalledChanged).toHaveBeenCalledTimes(1);
    expect(onInstalledChanged).toHaveBeenCalledWith("npm:good");

    buttons()[1].click();
    await Promise.resolve();
    await Promise.resolve();
    expect(transport.installPiPackage).toHaveBeenCalledWith("npm:bad");
    expect(onInstalledChanged).toHaveBeenCalledTimes(1);
  });
});

// T1 inventory row: the community catalog is a settings-class (Desktop-only)
// surface, so a host that denies the management face must not fetch it at all.
it("never fetches the community catalog without the management face", async () => {
  const root = createRoot();
  const fetchSpy = vi.fn();
  vi.stubGlobal("fetch", fetchSpy);
  const browser = setupPackageBrowse({
    root,
    transport: { listPiPackages: vi.fn() },
    nativeAvailable: () => false,
    managementAvailable: () => false,
    t: translations,
    createIcon: () => document.createElement("span"),
    renderPackageInstallFailure: vi.fn(),
    setExtensionActionButton: (button, label) => {
      button.textContent = label;
    },
    catalogUrl: "https://registry.test/catalog.json",
  });

  await browser.load();

  expect(fetchSpy).not.toHaveBeenCalled();
  expect(root.getElementById("pkg-browse-list").textContent).toContain("Desktop only");
});
