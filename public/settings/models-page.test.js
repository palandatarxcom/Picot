// ABOUTME: Covers the Models page: provider catalog, API keys, and models.json editing.
// ABOUTME: Verifies model mutations refresh the active model catalog through the callback.

import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { setupModelsPage } from "./models-page.js";

describe("models provider editor", () => {
  let dom;
  let call;
  let oauthCommand;
  let oauthGateway;

  beforeEach(() => {
    dom = new JSDOM(`
      <div id="settings-api-keys"></div>
      <span id="inline-models-path"></span>
      <textarea id="inline-models-textarea"></textarea>
      <div id="inline-models-error" class="hidden"></div>
      <button id="inline-models-save">Save</button>
      <button id="inline-models-insert-example">Example</button>
      <a id="models-config-docs-link"></a>
    `);
    globalThis.window = dom.window;
    globalThis.document = dom.window.document;
    globalThis.confirm = vi.fn(() => true);
    call = vi.fn(async (operation) => {
      if (operation === "read_models_config") {
        return {
          ok: true,
          data: {
            path: "/home/.pi/agent/models.json",
            content: JSON.stringify({
              providers: {
                gateway: {
                  baseUrl: "https://gateway.example/v1",
                  api: "openai-completions",
                  models: [{ id: "gpt-5.5" }],
                },
                local: {
                  baseUrl: "http://localhost:11434/v1",
                  api: "openai-completions",
                  models: [{ id: "qwen" }],
                },
              },
            }),
          },
        };
      }
      if (operation === "write_models_config") return { ok: true };
      if (operation === "list_model_catalog") {
        return {
          ok: true,
          data: {
            providers: [
              {
                provider: "anthropic",
                displayName: "Anthropic",
                configured: true,
                source: "stored",
                models: [
                  {
                    provider: "anthropic",
                    id: "claude-sonnet",
                    available: true,
                    visible: true,
                    health: { status: "healthy" },
                  },
                ],
              },
              {
                provider: "openai",
                displayName: "OpenAI",
                configured: true,
                source: "stored",
                models: [
                  {
                    provider: "openai",
                    id: "gpt-4.1",
                    available: true,
                    visible: true,
                    health: { status: "healthy" },
                  },
                ],
              },
            ],
          },
        };
      }
      throw new Error(`Unexpected operation: ${operation}`);
    });
    // OAuth capability/login/logout now ride the oauth gateway's v3 command
    // shape ({ success, data }) over /picot-config, not the { ok, data } call.
    oauthCommand = vi.fn(async (frame) => {
      if (frame?.type === "get_oauth_login_capabilities") {
        return {
          success: true,
          data: {
            providers: [{ providerId: "openai-codex", deviceCode: true, configured: false }],
          },
        };
      }
      throw new Error(`Unexpected OAuth command: ${frame?.type}`);
    });
    oauthGateway = { command: oauthCommand, subscribe: () => () => {} };
  });

  afterEach(() => {
    vi.restoreAllMocks();
    dom.window.close();
    delete globalThis.window;
    delete globalThis.document;
    delete globalThis.confirm;
  });

  test("activation loads both the provider catalog and models.json", async () => {
    const page = setupModelsPage({ configGateway: { call }, oauthGateway });
    await page.activate();

    expect(call.mock.calls.map(([operation]) => operation)).toContain("list_model_catalog");
    expect(call.mock.calls.map(([operation]) => operation)).toContain("read_models_config");
  });

  test("marks a partially enabled provider with an indeterminate master toggle", async () => {
    const partialCall = vi.fn(async (operation) => {
      if (operation === "list_model_catalog") {
        return {
          ok: true,
          data: {
            providers: [
              {
                provider: "partial",
                displayName: "Partial",
                configured: true,
                source: "stored",
                models: [
                  { provider: "partial", id: "one", visible: true, available: true },
                  { provider: "partial", id: "two", visible: false, available: true },
                  { provider: "partial", id: "three", available: true },
                ],
              },
            ],
          },
        };
      }
      return call(operation);
    });
    const page = setupModelsPage({ configGateway: { call: partialCall }, oauthGateway });
    await page.loadApiKeysPanel();

    const toggle = document.querySelector(".api-model-select-all-toggle");
    expect(toggle.checked).toBe(false);
    expect(toggle.indeterminate).toBe(true);
    expect(toggle.getAttribute("aria-checked")).toBe("mixed");
  });

  test("switches providers without removing siblings from models.json", async () => {
    const page = setupModelsPage({ configGateway: { call }, oauthGateway });
    await page.loadInlineModelsEditor();

    const providerButtons = document.querySelectorAll(".models-provider-item");
    expect(providerButtons).toHaveLength(2);
    providerButtons[1].click();

    const baseUrl = document.querySelector(
      '.models-config-field input[placeholder="https://api.example.com/v1"]',
    );
    expect(baseUrl.value).toBe("http://localhost:11434/v1");
    baseUrl.value = "http://127.0.0.1:11434/v1";
    baseUrl.dispatchEvent(new window.Event("input", { bubbles: true }));

    const config = JSON.parse(document.getElementById("inline-models-textarea").value);
    expect(Object.keys(config.providers)).toEqual(["gateway", "local"]);
    expect(config.providers.local.baseUrl).toBe("http://127.0.0.1:11434/v1");
  });

  test("renders the Codex OAuth entry only when capability reports it", async () => {
    const page = setupModelsPage({ configGateway: { call }, oauthGateway });
    await page.activate();

    expect(oauthCommand.mock.calls.map(([frame]) => frame?.type)).toContain(
      "get_oauth_login_capabilities",
    );
    const oauthItem = document.querySelector(".models-oauth-provider-item");
    expect(oauthItem).not.toBeNull();
    // The sidebar keeps the provider name (same as the logged-in state), not
    // the sign-in action label.
    expect(oauthItem.textContent.trim()).toBe("OpenAI Codex");
    oauthItem.click();
    expect(
      document.querySelector("#settings-api-keys .models-config-main .provider-manager-card"),
    ).not.toBeNull();
  });

  test("hides the Codex OAuth entry when capability is absent", async () => {
    const noCapabilityOauth = vi.fn(async () => ({
      success: true,
      data: { providers: [] },
    }));
    const noCapabilityCall = vi.fn(async (operation) => call(operation));
    const page = setupModelsPage({
      configGateway: { call: noCapabilityCall },
      oauthGateway: { command: noCapabilityOauth },
    });
    await page.activate();
    expect(document.querySelector(".models-oauth-provider-item")).toBeNull();
  });

  test("keeps configured Codex models visible alongside connected OAuth state", async () => {
    const configuredOauth = vi.fn(async () => ({
      success: true,
      data: { providers: [{ providerId: "openai-codex", deviceCode: true, configured: true }] },
    }));
    const configuredCall = vi.fn(async (operation) => {
      if (operation === "list_model_catalog") {
        return {
          ok: true,
          data: {
            providers: [
              {
                provider: "openai-codex",
                displayName: "OpenAI Codex",
                configured: true,
                source: "stored",
                models: [
                  {
                    provider: "openai-codex",
                    id: "gpt-5.5",
                    name: "GPT-5.5",
                    available: true,
                    visible: true,
                    health: { status: "healthy" },
                  },
                ],
              },
            ],
          },
        };
      }
      return call(operation);
    });
    const page = setupModelsPage({
      configGateway: { call: configuredCall },
      oauthGateway: { command: configuredOauth },
    });
    await page.activate();

    // Configured Codex keeps its model list and the connected badge; the
    // OAuth sidebar entry and the API-key remove button are not shown.
    expect(document.querySelector(".models-oauth-provider-item")).toBeNull();
    expect(document.querySelector(".api-model-name")?.textContent).toBe("GPT-5.5");
    expect(document.querySelector(".models-oauth-connected-status")).not.toBeNull();
    const actions = document.querySelector(".api-key-row-actions");
    expect(actions).not.toBeNull();
    expect(actions.querySelector(".api-model-check-visible")).not.toBeNull();
    expect(actions.querySelector(".api-key-row-oauth-action")).not.toBeNull();
    expect(actions.querySelector(".danger")).toBeNull();
  });

  test("logout of configured Codex calls Pi logout and flips back to sign-in", async () => {
    let capabilityCalls = 0;
    const logoutOauth = vi.fn(async (frame) => {
      if (frame?.type === "get_oauth_login_capabilities") {
        capabilityCalls += 1;
        return {
          success: true,
          data: {
            providers: [
              { providerId: "openai-codex", deviceCode: true, configured: capabilityCalls === 1 },
            ],
          },
        };
      }
      if (frame?.type === "oauth_logout") return { success: true };
      throw new Error(`Unexpected OAuth command: ${frame?.type}`);
    });
    const logoutCall = vi.fn(async (operation) => {
      if (operation === "list_model_catalog") {
        return {
          ok: true,
          data: {
            providers: [
              {
                provider: "openai-codex",
                displayName: "OpenAI Codex",
                configured: true,
                source: "stored",
                models: [
                  {
                    provider: "openai-codex",
                    id: "gpt-5.5",
                    name: "GPT-5.5",
                    available: true,
                    visible: true,
                    health: { status: "healthy" },
                  },
                ],
              },
            ],
          },
        };
      }
      return call(operation);
    });
    const page = setupModelsPage({
      configGateway: { call: logoutCall },
      oauthGateway: { command: logoutOauth },
    });
    await page.activate();

    const logoutBtn = document.querySelector(".api-key-row-oauth-action");
    expect(logoutBtn).not.toBeNull();
    logoutBtn.click();
    // The confirm is now a DOM modal (the Tauri WebView has no native
    // confirm bridge); answer it to release the logout call.
    document.querySelector(".sidebar-confirm-yes")?.click();
    // Logout rides the oauth gateway's v3 command surface (`oauth_logout`).
    await vi.waitFor(() =>
      expect(logoutOauth.mock.calls.some(([frame]) => frame?.type === "oauth_logout")).toBe(true),
    );
    // Capability re-query reports unconfigured; the OAuth sign-in entry returns.
    await vi.waitFor(() =>
      expect(document.querySelector(".models-oauth-provider-item")).not.toBeNull(),
    );
  });

  test("after a successful login the card flips to the configured Codex row", async () => {
    let loggedIn = false;
    const loginOauth = vi.fn(async () => ({
      success: true,
      data: { providers: [{ providerId: "openai-codex", deviceCode: true, configured: loggedIn }] },
    }));
    const loginCall = vi.fn(async (operation) => {
      if (operation === "list_model_catalog") {
        return {
          ok: true,
          data: {
            providers: [
              {
                provider: "openai-codex",
                displayName: "OpenAI Codex",
                configured: loggedIn,
                source: "stored",
                models: [
                  {
                    provider: "openai-codex",
                    id: "gpt-5.5",
                    name: "GPT-5.5",
                    available: true,
                    visible: true,
                    health: { status: "healthy" },
                  },
                ],
              },
            ],
          },
        };
      }
      return call(operation);
    });
    const page = setupModelsPage({
      configGateway: { call: loginCall },
      oauthGateway: { command: loginOauth },
    });
    await page.activate();

    // Initially unconfigured: select the sidebar entry, then the sign-in card
    // is shown.
    const oauthItem = document.querySelector(".models-oauth-provider-item");
    expect(oauthItem).not.toBeNull();
    oauthItem.click();
    const loginCta = document.querySelector(".oauth-login-cta");
    expect(loginCta).not.toBeNull();
    expect(document.querySelector(".api-key-row-oauth-action")).toBeNull();

    // The user completes authorization in the browser; Pi now reports the
    // credential. The onSuccess refresh sequence re-queries both surfaces.
    loggedIn = true;
    await page.loadApiKeysPanel();
    await page.loadOAuthCapability();

    // The card now renders the configured row (model list + logout), and the
    // sign-in CTA is gone.
    expect(document.querySelector(".oauth-login-cta")).toBeNull();
    expect(document.querySelector(".api-key-row-oauth-action")).not.toBeNull();
    expect(document.querySelector(".api-model-name")?.textContent).toBe("GPT-5.5");
    expect(document.querySelector(".models-oauth-connected-status")).not.toBeNull();
  });

  test("splits API-key providers and custom providers into separate panels", async () => {
    const page = setupModelsPage({ configGateway: { call }, oauthGateway });
    await page.loadInlineModelsEditor();
    await page.loadApiKeysPanel();

    // API-key (auth) providers keep the master-detail layout: a sidebar list
    // plus the selected provider's model card on the right.
    expect(document.querySelector("#settings-api-keys .models-auth-provider-item")).not.toBeNull();
    expect(
      document.querySelector("#settings-api-keys .models-config-main .api-key-row-name")
        .textContent,
    ).toBe("Anthropic");
    expect(
      document.querySelector("#settings-api-keys .models-config-main .api-model-name").textContent,
    ).toBe("claude-sonnet");

    // Custom providers render only in the models.json sidebar, never in auth.
    const labels = [
      ...document.querySelectorAll("#models-config-layout .models-provider-item"),
    ].map((item) => item.textContent.trim());
    expect(labels).toEqual(["gateway", "local"]);
  });

  test("switches the selected auth provider in the master-detail panel", async () => {
    const page = setupModelsPage({ configGateway: { call }, oauthGateway });
    await page.loadApiKeysPanel();

    const sidebarItems = [
      ...document.querySelectorAll("#settings-api-keys .models-auth-provider-item"),
    ];
    expect(sidebarItems.map((item) => item.textContent.trim())).toEqual(["Anthropic", "OpenAI"]);
    expect(
      document.querySelector("#settings-api-keys .models-config-main .api-key-row-name")
        .textContent,
    ).toBe("Anthropic");

    sidebarItems[1].click();
    expect(
      document.querySelector("#settings-api-keys .models-config-main .api-key-row-name")
        .textContent,
    ).toBe("OpenAI");
    expect(
      document.querySelector("#settings-api-keys .models-config-main .api-model-name").textContent,
    ).toBe("gpt-4.1");
    const selectedAfter = document.querySelector(
      "#settings-api-keys .models-auth-provider-item.selected",
    );
    expect(selectedAfter?.textContent.trim()).toBe("OpenAI");
  });

  test("renders authenticated providers while models.json is still loading", async () => {
    let resolveModelsConfig;
    const pendingModelsConfig = new Promise((resolve) => {
      resolveModelsConfig = resolve;
    });
    const delayedCall = vi.fn(async (operation) => {
      if (operation === "read_models_config") return pendingModelsConfig;
      return call(operation);
    });
    const page = setupModelsPage({ configGateway: { call: delayedCall }, oauthGateway });
    const modelsLoad = page.loadInlineModelsEditor();

    await page.loadApiKeysPanel();
    expect(
      document.querySelector("#settings-api-keys .models-config-main .api-key-row-name")
        ?.textContent,
    ).toBe("Anthropic");
    // The custom-provider sidebar is not blocked by the pending models.json read.
    expect(document.querySelector("#models-config-layout")).not.toBeNull();

    resolveModelsConfig({
      ok: true,
      data: { path: "/home/.pi/agent/models.json", content: '{"providers":{}}' },
    });
    await modelsLoad;
  });

  test("adds a model and saves the complete provider configuration", async () => {
    const onModelConfigurationChanged = vi.fn();
    const page = setupModelsPage({
      configGateway: { call },
      oauthGateway,
      onModelConfigurationChanged,
    });
    await page.loadInlineModelsEditor();

    document.querySelector(".models-model-add").click();
    const modelId = document.querySelector('.models-config-field input[placeholder="model-name"]');
    modelId.value = "claude-sonnet";
    modelId.dispatchEvent(new window.Event("input", { bubbles: true }));
    document.getElementById("inline-models-save").click();
    await vi.waitFor(() => expect(onModelConfigurationChanged).toHaveBeenCalledOnce());

    const write = call.mock.calls.find(([operation]) => operation === "write_models_config");
    const saved = JSON.parse(write[1].content);
    expect(saved.providers.gateway.models.map((model) => model.id)).toEqual([
      "gpt-5.5",
      "claude-sonnet",
    ]);
    expect(saved.providers.local.models[0].id).toBe("qwen");
  });

  test("visual models form exposes a save button for edited fields", async () => {
    const onModelConfigurationChanged = vi.fn();
    const page = setupModelsPage({
      configGateway: { call },
      oauthGateway,
      onModelConfigurationChanged,
    });
    await page.loadInlineModelsEditor();

    const toolbarSave = document.querySelector(".models-config-toolbar-save");
    expect(toolbarSave).not.toBeNull();

    document.querySelectorAll(".models-provider-item")[1].click();
    const baseUrl = document.querySelector(
      '.models-config-field input[placeholder="https://api.example.com/v1"]',
    );
    baseUrl.value = "http://127.0.0.1:11434/v1";
    baseUrl.dispatchEvent(new window.Event("input", { bubbles: true }));
    toolbarSave.click();

    await vi.waitFor(() => expect(onModelConfigurationChanged).toHaveBeenCalledOnce());
    const write = call.mock.calls.find(([operation]) => operation === "write_models_config");
    expect(JSON.parse(write[1].content).providers.local.baseUrl).toBe("http://127.0.0.1:11434/v1");
  });

  test("does not overwrite an existing provider when adding a custom provider", async () => {
    const page = setupModelsPage({ configGateway: { call }, oauthGateway });
    await page.loadInlineModelsEditor();

    document.querySelector(".models-provider-add").click();
    document.querySelector(".provider-picker-card").click();
    const inputs = [...document.querySelectorAll(".provider-setup-form input")];
    inputs[0].value = "gateway";
    inputs[1].value = "https://replacement.example/v1";
    inputs[3].value = "replacement-model";
    document.querySelector(".provider-setup-actions .ui-button--primary").click();

    await vi.waitFor(() => expect(document.querySelector(".provider-setup-dialog")).not.toBeNull());
    expect(
      call.mock.calls.filter(([operation]) => operation === "write_models_config"),
    ).toHaveLength(0);
    expect(document.querySelector(".api-key-editor-error, .provider-setup-dialog")).not.toBeNull();
  });

  test("inserts the example at the caret without clearing existing content", async () => {
    const page = setupModelsPage({ configGateway: { call }, oauthGateway });
    await page.loadInlineModelsEditor();

    const textarea = document.getElementById("inline-models-textarea");
    textarea.value = '{\n  "providers": { "gateway": {} }\n}';
    textarea.setSelectionRange(0, 0);
    document.getElementById("inline-models-insert-example").click();

    // 原内容保留，示例插入到光标处。
    expect(textarea.value).toContain('"gateway": {}');
    expect(textarea.value.startsWith("{"));
    expect(textarea.value).toContain('"ollama"');
  });

  test("refreshes the model catalog after saving a custom provider", async () => {
    const onModelConfigurationChanged = vi.fn();
    const page = setupModelsPage({
      configGateway: { call },
      oauthGateway,
      onModelConfigurationChanged,
    });
    await page.loadInlineModelsEditor();

    document.querySelector(".models-provider-add").click();
    document.querySelector(".provider-picker-card").click();
    const inputs = [...document.querySelectorAll(".provider-setup-form input")];
    inputs[0].value = "newprovider";
    inputs[1].value = "https://newprovider.example/v1";
    inputs[3].value = "new-model";
    document.querySelector(".provider-setup-actions .ui-button--primary").click();

    await vi.waitFor(() => expect(onModelConfigurationChanged).toHaveBeenCalledOnce());
    expect(
      call.mock.calls.filter(([operation]) => operation === "write_models_config"),
    ).toHaveLength(1);
  });
});

test("keeps provider keyboard focus inside the clipped sidebar", () => {
  const css = readFileSync("public/settings/settings-config.css", "utf8");
  const focusRule = css.match(
    /\.models-provider-item:focus-visible[^{]*\{(?<declarations>[^}]*)\}/,
  );

  expect(focusRule?.groups?.declarations).toMatch(/outline:\s*none/);
  expect(focusRule?.groups?.declarations).toMatch(/background:\s*var\(--bg-glass-hover\)/);
});

test("does not draw a curved inset border on the selected provider or model", () => {
  const css = readFileSync("public/settings/settings-config.css", "utf8");
  const selectedRules = [...css.matchAll(/[^{}]*\.selected[^{}]*\{(?<declarations>[^}]*)\}/g)];
  const providerSelectionRules = selectedRules.filter((rule) =>
    /\.models-(?:provider|model)-item\.selected/.test(rule[0]),
  );

  expect(providerSelectionRules).not.toHaveLength(0);
  for (const rule of providerSelectionRules) {
    expect(rule.groups.declarations).not.toMatch(/box-shadow/);
  }
});

test("bounds the master-detail grid so its panels scroll internally", () => {
  const css = readFileSync("public/settings/settings-config.css", "utf8");
  const layoutRule = css.match(/\.models-config-layout\s*\{(?<declarations>[^}]*)\}/)?.groups
    ?.declarations;
  const sidebarRule = css.match(/\.models-config-sidebar\s*\{(?<declarations>[^}]*)\}/)?.groups
    ?.declarations;
  const mainRule = css.match(/\.models-config-main\s*\{(?<declarations>[^}]*)\}/)?.groups
    ?.declarations;
  const listRule = css.match(/\.models-provider-list\s*\{(?<declarations>[^}]*)\}/)?.groups
    ?.declarations;

  // The grid row must be height-constrained, otherwise max-height only clips
  // and the overflow-y auto panels have no bounded height to scroll within.
  expect(layoutRule).toMatch(/grid-template-rows:\s*minmax\(0,\s*1fr\)/);
  expect(layoutRule).toMatch(/max-height:/);
  expect(sidebarRule).toMatch(/min-height:\s*0/);
  expect(mainRule).toMatch(/overflow-y:\s*auto/);
  expect(listRule).toMatch(/overflow-y:\s*auto/);
});
