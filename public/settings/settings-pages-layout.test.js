// ABOUTME: Asserts the Settings page split between Configuration and Models.
// ABOUTME: Locks navigation, panel placement, activation routing, and locale titles.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { JSDOM } from "jsdom";
import { describe, expect, test } from "vitest";

describe("settings page split", () => {
  const html = readFileSync(join(process.cwd(), "public/index.html"), "utf8");
  const appJs = readFileSync(join(process.cwd(), "public/app.js"), "utf8");

  test("adds a Models navigation entry", () => {
    const dom = new JSDOM(html, { url: "http://localhost" });
    const { document } = dom.window;

    expect(document.querySelector('[data-settings-tab="models"]')).not.toBeNull();
    expect(document.querySelector('[data-settings-tab="models"]').dataset.i18n).toBe(
      "settings.models.title",
    );
    expect(document.querySelector('[data-settings-panel="models"]')).not.toBeNull();
  });

  test("wires the usage sub-tabs for both shells", () => {
    const appJs = readFileSync(join(process.cwd(), "public/app.js"), "utf8");
    const landingJs = readFileSync(join(process.cwd(), "public/landing.js"), "utf8");
    // A cold start renders the landing shell, so wiring only app.js leaves the
    // Usage sub-tabs dead on the page a user actually opens first.
    expect(appJs).toContain("setupUsageTabs(");
    expect(landingJs).toContain("setupUsageTabs(");
  });

  test("keeps Usage cost and provider quota as two tabs of one page", () => {
    const dom = new JSDOM(html, { url: "http://localhost" });
    const { document } = dom.window;

    // One Settings entry, two views inside it.
    expect(document.querySelector('[data-settings-tab="quota"]')).toBeNull();

    const page = document.querySelector('[data-settings-panel="usage"]');
    expect(page).not.toBeNull();
    const tabs = [...page.querySelectorAll("[data-usage-tab]")].map((tab) => tab.dataset.usageTab);
    expect(tabs).toEqual(["quota", "cost"]);
    const panels = [...page.querySelectorAll("[data-usage-panel]")].map(
      (panel) => panel.dataset.usagePanel,
    );
    expect(panels).toEqual(["quota", "cost"]);
    expect(page.querySelector('[data-usage-tab="quota"]').getAttribute("aria-selected")).toBe(
      "true",
    );
    expect(page.querySelector('[data-usage-panel="quota"]').classList.contains("hidden")).toBe(
      false,
    );
    expect(page.querySelector('[data-usage-tab="cost"]').getAttribute("aria-selected")).toBe(
      "false",
    );
    expect(page.querySelector('[data-usage-panel="cost"]').classList.contains("hidden")).toBe(true);
    expect(page.querySelector('[data-usage-panel="cost"] #settings-cost-dashboard')).not.toBeNull();
    expect(
      page.querySelector('[data-usage-panel="quota"] #settings-provider-quota'),
    ).not.toBeNull();

    // Quota is not a cell in the cost infobar next to the models histogram.
    const dashboardJs = readFileSync(join(process.cwd(), "public/cost/dashboard.js"), "utf8");
    expect(dashboardJs).not.toContain("usage-provider-quota");
  });

  test("keeps a dedicated drag region above the Settings overlay controls", () => {
    const dom = new JSDOM(html, { url: "http://localhost" });
    const { document } = dom.window;
    const dragRegion = document.querySelector("#settings-drag-region");
    const css = readFileSync(join(process.cwd(), "public/style.css"), "utf8");

    expect(dragRegion).not.toBeNull();
    expect(dragRegion.getAttribute("aria-hidden")).toBe("true");
    expect(document.querySelector("#settings-drag-region + .settings-nav")).not.toBeNull();
    expect(css).toMatch(/\.settings-drag-region\s*\{[^}]*-webkit-app-region:\s*drag/s);
    expect(appJs).toContain(
      'const settingsDragRegion = document.getElementById("settings-drag-region")',
    );
    expect(appJs).toContain('settingsDragRegion?.addEventListener("mousedown"');
  });

  test("splits Configuration and Models panels by ownership", () => {
    const dom = new JSDOM(html, { url: "http://localhost" });
    const { document } = dom.window;

    const configurationPanel = document.querySelector('[data-settings-panel="configuration"]');
    const modelsPanel = document.querySelector('[data-settings-panel="models"]');

    expect(configurationPanel).not.toBeNull();
    expect(modelsPanel).not.toBeNull();
    expect(configurationPanel.querySelector("#inline-config-textarea")).not.toBeNull();
    expect(configurationPanel.querySelector("#agents-md-textarea")).not.toBeNull();
    expect(configurationPanel.querySelector("#append-system-md-textarea")).not.toBeNull();
    expect(configurationPanel.querySelector("#settings-api-keys")).toBeNull();
    expect(configurationPanel.querySelector("#inline-models-textarea")).toBeNull();
    expect(modelsPanel.querySelector("#settings-api-keys")).not.toBeNull();
    expect(modelsPanel.querySelector("#inline-models-textarea")).not.toBeNull();
  });

  test("removes the non-functional Protection markup", () => {
    const dom = new JSDOM(html, { url: "http://localhost" });
    const { document } = dom.window;

    expect(document.querySelector("#settings-auth-section")).toBeNull();
    expect(document.querySelector("#toggle-auth")).toBeNull();
  });

  test("activates the Models page through app.js routing", () => {
    expect(appJs).toContain('if (targetTabKey === "models")');
    expect(appJs).toContain("modelsPage.activate()");
    expect(appJs).not.toContain('rpcCommand({ type: "get_auth" })');
  });

  test("orders navigation without the retired Agent Inbox entry", () => {
    const dom = new JSDOM(html, { url: "http://localhost" });
    const { document } = dom.window;

    const tabs = [...document.querySelectorAll(".settings-nav-item")].map(
      (item) => item.dataset.settingsTab,
    );
    expect(tabs).toEqual([
      "general",
      "appearance",
      "models",
      "extensions",
      "skills",
      "mcp",
      "subagents",
      // Environment sits at the end of the feature group, before the advanced
      // configuration and usage entries.
      "environment",
      "configuration",
      "usage",
    ]);

    expect(document.querySelector('[data-settings-panel="subagents"]')).not.toBeNull();
    expect(appJs).toContain('if (targetTabKey === "subagents")');
    // Native MCP (Pi 0.99+) ships with the runtime: the nav entry is
    // statically visible, no adapter-detection gate.
    const mcpItem = document.querySelector('[data-settings-tab="mcp"]');
    expect(mcpItem?.classList.contains("hidden")).toBe(false);

    // The Super Agent / Agent Inbox surface was removed with its runtime scope:
    // a disabled placeholder tab is still a dead control, so it must be gone.
    expect(document.querySelector('[data-settings-tab="chat"]')).toBeNull();
    expect(document.querySelector("#super-agent-chat-header")).toBeNull();
    expect(document.querySelector("super-agent-runtime")).toBeNull();
  });

  test("renames Configuration to Advanced Configuration in every locale", () => {
    const dom = new JSDOM(html, { url: "http://localhost" });
    const { document } = dom.window;
    expect(document.querySelector('[data-settings-tab="configuration"]').dataset.i18n).toBe(
      "settings.configuration",
    );

    for (const locale of ["en", "es", "ja", "zh"]) {
      const messages = JSON.parse(
        readFileSync(join(process.cwd(), `public/locales/${locale}.json`), "utf8"),
      );
      expect(messages.settings.configuration).toEqual(expect.any(String));
      expect(messages.settings.configuration.trim()).not.toBe("");
    }
  });

  test("resolves the Models navigation title in every locale", () => {
    for (const locale of ["en", "es", "ja", "zh"]) {
      const messages = JSON.parse(
        readFileSync(join(process.cwd(), `public/locales/${locale}.json`), "utf8"),
      );
      expect(messages.settings.models.title).toEqual(expect.any(String));
      expect(messages.settings.models.title.trim()).not.toBe("");
    }
  });
});
