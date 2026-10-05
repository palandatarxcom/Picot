// ABOUTME: DOM contract tests for the project MCP override detail renderer.
// ABOUTME: Proves the three-field control set, draft-preserving validation and the save payload.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { setMessages } from "../i18n.js";
import { renderMcpOverrideDetail } from "./mcp-override-detail.js";

setMessages({
  settings: {
    mcp: {
      enable: "Enable",
      save: "Save",
      sourceLabel: "Source",
      form: {
        exposure: "Tool exposure",
        exposure_codemode: "Default (codemode)",
        exposure_direct: "Direct",
        exposure_deferred: "Deferred",
        exposure_hidden: "Hidden",
      },
      override: {
        sourceHint: "Overrides global server {name}. Connection settings stay global.",
        needsAuthHint: "This server needs sign-in. Use the Global tab.",
        toolExposure: "Tool exposure map (JSON)",
        mapHelp: "Whole-map replacement. {} clears per-tool rules.",
        mapInvalid: "Enter a JSON object, for example {}.",
        mapValueInvalid: 'Tool "{tool}" must map to codemode, direct, deferred or hidden.',
        exposureInvalid: "Choose a tool exposure.",
        remove: "Remove project override",
        removeWarning: "Removes this entry from the project mcp.json only.",
        baseLost: "The global server this override belongs to is gone.",
        staleDraft: "The file changed on disk.",
      },
    },
  },
});

const PROJECT_FILE = "/work/.pi/mcp.json";

function overrideItem(over = {}) {
  return {
    name: "docs",
    sourceFile: PROJECT_FILE,
    kind: "override",
    effective: { enabled: true, exposure: "codemode", toolExposure: { "delete_*": "hidden" } },
    identity: { scope: "global", source: "/agent/mcp.json", override: PROJECT_FILE },
    revision: "abc",
    ...over,
  };
}

function draft(over = {}) {
  return {
    exposure: "codemode",
    toolExposureText: JSON.stringify({ "delete_*": "hidden" }, null, 2),
    ...over,
  };
}

/** Draft mirroring the item's effective map, like the page's draft store. */
function draftFor(item, over = {}) {
  return draft({
    exposure: item.effective?.exposure ?? "codemode",
    toolExposureText: JSON.stringify(item.effective?.toolExposure ?? {}, null, 2),
    ...over,
  });
}

function render(options = {}) {
  const onToggle = vi.fn();
  const onSave = vi.fn();
  const onRemove = vi.fn();
  const onDraftChange = vi.fn();
  const item = overrideItem(options.item);
  const node = renderMcpOverrideDetail({
    item,
    draft: options.draft ? draft(options.draft) : draftFor(item),
    onToggle,
    onSave,
    onRemove,
    onDraftChange,
    status: options.status ?? null,
    pending: options.pending,
  });
  document.body.replaceChildren(node);
  return { node, onToggle, onSave, onRemove, onDraftChange };
}

beforeEach(() => {
  document.body.replaceChildren();
});

describe("renderMcpOverrideDetail", () => {
  it("shows only the three override controls plus Save and Remove", () => {
    const { node } = render();
    expect(node.querySelectorAll('[role="switch"]')).toHaveLength(1);
    const selects = node.querySelectorAll("select");
    expect(selects).toHaveLength(1);
    expect([...selects[0].options].map((o) => o.value)).toEqual([
      "codemode",
      "direct",
      "deferred",
      "hidden",
    ]);
    expect(node.querySelectorAll("textarea")).toHaveLength(1);
    expect(node.querySelectorAll("input")).toHaveLength(0);
    expect(
      [...node.querySelectorAll(".mcp-form-actions button")].map((b) => b.textContent),
    ).toEqual(["Save", "Remove project override"]);
    // No sign-in affordance and no "inherit" escape hatch.
    expect(node.querySelector(".mcp-login")).toBeNull();
    expect([...selects[0].options].some((o) => /inherit/i.test(o.textContent))).toBe(false);
  });

  it("starts the map textarea from the effective map of a partial override", () => {
    const { node } = render({
      item: {
        effective: { enabled: false, exposure: "direct", toolExposure: { other: "hidden" } },
      },
    });
    expect(node.querySelector("textarea").value).toBe(JSON.stringify({ other: "hidden" }, null, 2));
    expect(node.querySelector("select").value).toBe("direct");
    expect(node.querySelector('[role="switch"]').getAttribute("aria-checked")).toBe("false");
  });

  it("saves the explicit three-field payload", () => {
    const { node, onSave, onDraftChange } = render();
    const textarea = node.querySelector("textarea");
    textarea.value = '{ "delete_exact": "direct" }';
    textarea.dispatchEvent(new Event("input"));
    expect(onDraftChange).toHaveBeenCalledWith({
      exposure: "codemode",
      toolExposureText: '{ "delete_exact": "direct" }',
    });

    node.querySelector("form").dispatchEvent(new Event("submit"));
    expect(onSave).toHaveBeenCalledWith({
      enabled: true,
      exposure: "codemode",
      toolExposure: { delete_exact: "direct" },
    });
  });

  it("treats empty map text as invalid instead of an implicit empty map", () => {
    const { node, onSave } = render();
    node.querySelector("textarea").value = "";
    node.querySelector("form").dispatchEvent(new Event("submit"));
    expect(onSave).not.toHaveBeenCalled();
    expect(node.querySelector(".mcp-field-error").hidden).toBe(false);
    expect(node.querySelector("textarea").value).toBe("");
  });

  it("rejects malformed JSON and bad exposure values while keeping the draft", () => {
    const { node, onSave } = render();
    const textarea = node.querySelector("textarea");
    textarea.value = "{ not json";
    node.querySelector("form").dispatchEvent(new Event("submit"));
    expect(onSave).not.toHaveBeenCalled();
    expect(node.querySelector(".mcp-field-error").hidden).toBe(false);

    textarea.value = '{"read": "loud"}';
    node.querySelector("form").dispatchEvent(new Event("submit"));
    expect(onSave).not.toHaveBeenCalled();
    expect(textarea.value).toBe('{"read": "loud"}');
  });

  it("removes the override without offering connection editing", () => {
    const { node, onRemove } = render();
    node.querySelector(".mcp-btn-danger").click();
    expect(onRemove).toHaveBeenCalledTimes(1);
  });

  it("disables the switch and Save for an invalid override but still allows removal", () => {
    const { node, onToggle, onSave } = render({
      item: {
        kind: "invalid",
        validationError: 'server "docs" needs a global server to override',
        effective: undefined,
      },
    });
    expect(node.querySelector('[role="switch"]').disabled).toBe(true);
    expect(node.querySelector("form")).toBeNull();
    expect(node.querySelector(".mcp-group-error").textContent).toMatch(/needs a global server/);
    const remove = node.querySelector(".mcp-btn-danger");
    expect(remove.disabled).toBe(false);
    remove.click();
    expect(node.querySelectorAll("form")).toHaveLength(0);
    expect(onToggle).not.toHaveBeenCalled();
    expect(onSave).not.toHaveBeenCalled();
  });

  it("points a needs-auth override at the global tab without an OAuth control", () => {
    const { node } = render({ status: { state: "needs-auth" } });
    const hint = node.querySelector('[data-hint="needs-auth"]');
    expect(hint).not.toBeNull();
    expect(hint.textContent).toContain("Global tab");
    expect(node.querySelector(".mcp-login")).toBeNull();
    expect(node.querySelector('[data-action="mcp-login"]')).toBeNull();
    expect(node.querySelector('[data-action="mcp-logout"]')).toBeNull();
  });

  it("keeps Save disabled while a toggle acknowledgement is pending", () => {
    const { node } = render({ pending: true });
    expect(node.querySelector('[role="switch"]').disabled).toBe(true);
    expect(node.querySelector(".mcp-btn-primary").disabled).toBe(true);
  });
});
