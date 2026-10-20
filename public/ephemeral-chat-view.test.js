import { beforeEach, describe, expect, it, vi } from "vitest";
import { EphemeralChatRuntime } from "./ephemeral-chat-runtime.js";
import { EphemeralChatView } from "./ephemeral-chat-view.js";
import { initI18n } from "./i18n.js";

const locale = {
  messages: { copyMessage: "Copy", thinking: "Thinking", attachedImage: "image" },
  app: { welcome: "Welcome", welcomeHint: "Hint", currentWorkspace: "ws:" },
  shortcuts: { focusInput: "focus", abort: "abort" },
  tools: {
    streaming: "streaming",
    complete: "complete",
    error: "error",
    copyOutput: "copy",
    pending: "pending",
  },
  dialogs: {
    cancel: "Cancel",
    no: "No",
    yes: "Yes",
    submit: "Submit",
    save: "Save",
    selectOption: "Select",
    confirm: "Confirm",
    input: "Input",
    editor: "Editor",
  },
  ephemeral: {
    placeholder: "Ask…",
    send: "Send",
    abort: "Abort",
    newChat: "New chat",
    tokens: "tokens",
    sideChat: "Side Chat",
    quickChat: "Quick Chat",
    statusReady: "Ready",
    statusStreaming: "Generating",
    statusError: "Error",
    statusDisconnected: "Disconnected",
    exportUnavailable: "Temporary chats cannot be exported",
  },
  input: {
    switchModel: "Switch model",
    commands: "Commands",
    compact: "Compact",
    exportHtml: "Export HTML",
    sessionStats: "Session stats",
    expandAllTools: "Expand tools",
    collapseAllTools: "Collapse tools",
    compactDesc: "Compact the context",
    exportHtmlDesc: "Export this chat",
    pasteRemoteDisabled: "Remote host: pasted text stays inline",
    sessionStatsDesc: "Show session statistics",
    expandAllToolsDesc: "Expand all tools",
    collapseAllToolsDesc: "Collapse all tools",
  },
  models: { searchPlaceholder: "Search models…", emptyTitle: "No models available" },
  settings: {
    thinkingCompact: "Think {level}",
    thinkingTitle: "Cycle thinking effort",
    thinkingAriaLabel: "Thinking effort: {level}",
    off: "off",
  },
  voice: { voiceInput: "Voice", stopRecording: "Stop" },
  misc: { model: "model" },
  fileMention: { listLabel: "Files" },
};

beforeEach(async () => {
  document.cookie.split(";").forEach((c) => {
    const name = c.split("=")[0].trim();
    if (name) document.cookie = `${name}=; Max-Age=0; Path=/`;
  });
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url) => {
      if (String(url).includes("/locales/en.json")) {
        return { ok: true, status: 200, json: async () => locale };
      }
      return { ok: false, status: 404, json: async () => ({}) };
    }),
  );
  await initI18n();
});

function makeRuntime() {
  return new EphemeralChatRuntime({
    descriptor: { instanceId: "inst-1", generation: 1, kind: "side-chat" },
    transport: { sendEphemeral: vi.fn(() => "ep-1") },
  });
}

describe("EphemeralChatView", () => {
  it("keeps export visible but explains temporary chats cannot be exported", () => {
    const view = new EphemeralChatView({
      runtime: makeRuntime(),
      kind: "side-chat",
      toolsEnabled: true,
    });
    const exportCommand = view._sideCommands().find((command) => command.label === "Export HTML");

    expect(exportCommand).toMatchObject({
      disabled: true,
      desc: "Temporary chats cannot be exported",
    });
    view.destroy();
  });

  it("exposes an element and renders snapshot messages", () => {
    const runtime = makeRuntime();
    const view = new EphemeralChatView({ runtime, kind: "side-chat", toolsEnabled: true });
    const el = view.element;
    expect(el).toBeInstanceOf(window.HTMLElement);
    runtime.applySnapshot({
      type: "ephemeral_snapshot",
      instanceId: "inst-1",
      generation: 1,
      runtimeSequenceWatermark: 0,
      messages: [
        {
          role: "user",
          content: [
            { type: "text", text: "hello world" },
            { type: "image", data: "image-data", mimeType: "image/png" },
          ],
        },
      ],
      assistantDraft: null,
      tools: [],
      model: null,
      thinkingLevel: "off",
      isStreaming: false,
      contextUsage: null,
      error: null,
    });
    expect(el.textContent).toContain("hello world");
    expect(el.querySelector(".message-image")?.getAttribute("src")).toBe(
      "data:image/png;base64,image-data",
    );
    view.destroy();
  });

  it("sends a prompt from the composer", () => {
    const runtime = makeRuntime();
    const sendSpy = vi.spyOn(runtime, "sendPrompt");
    const view = new EphemeralChatView({ runtime, kind: "side-chat", toolsEnabled: true });
    const textarea = view.element.querySelector("textarea");
    textarea.value = "what is 2+2";
    view.element.querySelector('[data-role="ephemeral-send"]').click();
    expect(sendSpy).toHaveBeenCalledWith("what is 2+2", []);
    view.destroy();
  });

  it("exposes an attach button that opens the file input and forwards images on submit", () => {
    const runtime = makeRuntime();
    const sendSpy = vi.spyOn(runtime, "sendPrompt");
    const view = new EphemeralChatView({ runtime, kind: "side-chat", toolsEnabled: true });
    const attachBtn = view.element.querySelector('[data-role="ephemeral-attach"]');
    expect(attachBtn).not.toBeNull();
    expect(view.element.querySelector('input[type="file"]')).not.toBeNull();
    expect(view.element.querySelector(".image-previews")).not.toBeNull();
    // Simulate adding a pending image directly via the helper API, then submit.
    view._imageAttachments.getPendingImages().push({ data: "abc", mimeType: "image/png" });
    view._imageAttachments.renderPreviews();
    const textarea = view.element.querySelector("textarea");
    textarea.value = "look at this";
    view.element.querySelector('[data-role="ephemeral-send"]').click();
    expect(sendSpy).toHaveBeenCalledWith("look at this", [
      { type: "image", data: "abc", mimeType: "image/png" },
    ]);
    // Consumed on send.
    expect(view._imageAttachments.getPendingImages()).toHaveLength(0);
    view.destroy();
  });

  it("renders the Side Chat Commands palette with main-chat icons and descriptions", () => {
    const runtime = makeRuntime();
    const view = new EphemeralChatView({ runtime, kind: "side-chat", toolsEnabled: true });
    view.element.querySelector('[data-role="ephemeral-command"]').click();
    const palette = view.element.querySelector(".command-palette");
    expect(palette.classList.contains("hidden")).toBe(false);
    expect(palette.querySelector(".command-palette-header").textContent).toBe("Commands");
    expect(
      Array.from(palette.querySelectorAll(".command-icon svg")).map((icon) =>
        icon.getAttribute("aria-hidden"),
      ),
    ).toHaveLength(5);
    expect(palette.querySelectorAll(".command-icon svg")[0]).not.toBeNull();
    expect(palette.querySelector(".command-desc").textContent).toBe("Compact the context");
    expect(palette.querySelector('[aria-disabled="true"]').textContent).toContain("Export HTML");
    view.destroy();
  });

  it("uses the main composer structure with model and thinking controls", () => {
    const runtime = makeRuntime();
    const view = new EphemeralChatView({ runtime, kind: "side-chat", toolsEnabled: true });
    runtime.applySnapshot({
      type: "ephemeral_snapshot",
      instanceId: "inst-1",
      generation: 1,
      runtimeSequenceWatermark: 0,
      messages: [],
      assistantDraft: null,
      tools: [],
      model: { provider: "anthropic", id: "claude-3" },
      thinkingLevel: "high",
      isStreaming: false,
      contextUsage: null,
      error: null,
    });

    const composer = view.element.querySelector(".composer-card");
    expect(composer).not.toBeNull();
    expect(composer.querySelector('[data-role="ephemeral-model"]')).not.toBeNull();
    expect(composer.querySelector('[data-role="ephemeral-thinking"]')).not.toBeNull();
    expect(composer.querySelector('[data-role="ephemeral-send"] svg')).not.toBeNull();
    const sendIcon = composer.querySelector('[data-role="ephemeral-send"] svg');
    // The send button shows the "arrow-up" icon when not streaming. The icon's
    // exact SVG shape is owned by public/icons.js; assert on the path data so
    // this test does not break every time the icon definition is refactored.
    const arrowPath = sendIcon.querySelector("path");
    expect(arrowPath).not.toBeNull();
    expect(arrowPath.getAttribute("d")).toBe("m5 12 7-7 7 7");
    expect(composer.querySelector('[data-role="ephemeral-send"]').textContent.trim()).toBe("");
    expect(composer.textContent).toContain("claude-3");
    expect(composer.textContent).toContain("Think high");
    view.destroy();
  });

  it("loads models from its own runtime and sends the selected model to that runtime", async () => {
    const runtime = makeRuntime();
    const view = new EphemeralChatView({ runtime, kind: "quick-chat", toolsEnabled: false });
    view.element.querySelector('[data-role="ephemeral-model"]').click();
    // The view first checks the host model cache (async), then falls back to
    // the live runtime query. Wait for the runtime request to be registered
    // before delivering its response, mirroring real async ordering.
    await vi.waitFor(() => {
      expect(runtime.transport.sendEphemeral).toHaveBeenCalledWith(
        "inst-1",
        1,
        expect.objectContaining({ type: "get_available_models" }),
      );
    });
    runtime.applySequencedEvent({
      instanceId: "inst-1",
      generation: 1,
      payload: {
        type: "response",
        id: "ep-1",
        command: "get_available_models",
        success: true,
        data: {
          models: [
            { provider: "anthropic", id: "claude-3" },
            { provider: "openai", id: "gpt-4o" },
          ],
        },
      },
    });
    await vi.waitFor(() => {
      expect(view.element.querySelector(".model-dropdown-item")).not.toBeNull();
    });

    const search = view.element.querySelector(".model-dropdown-search");
    expect(search).not.toBeNull();
    expect(view.element.querySelector(".model-dropdown-items")).not.toBeNull();
    search.value = "gpt";
    search.dispatchEvent(new Event("input"));
    const option = view.element.querySelector(".model-dropdown-item");
    expect(option.tagName).toBe("DIV");
    expect(option.textContent).toContain("gpt-4o");
    expect(view.element.querySelectorAll(".model-dropdown-item")).toHaveLength(1);
    option.click();
    expect(runtime.transport.sendEphemeral).toHaveBeenLastCalledWith("inst-1", 1, {
      type: "set_model",
      provider: "openai",
      modelId: "gpt-4o",
    });
    view.destroy();
  });

  it("renders turn sections: tool cards live inside the turn rail, not a separate block", () => {
    const runtime = makeRuntime();
    const view = new EphemeralChatView({ runtime, kind: "side-chat", toolsEnabled: true });
    runtime.applySnapshot({
      type: "ephemeral_snapshot",
      instanceId: "inst-1",
      generation: 1,
      runtimeSequenceWatermark: 0,
      messages: [
        { role: "user", content: "run ls" },
        { role: "assistant", content: "done" },
      ],
      assistantDraft: null,
      tools: [
        {
          toolCallId: "t1",
          toolName: "bash",
          args: { command: "ls" },
          output: "a.txt",
          status: "complete",
        },
      ],
      turns: [
        {
          user: { role: "user", content: "run ls" },
          tools: [
            {
              toolCallId: "t1",
              toolName: "bash",
              args: { command: "ls" },
              output: "a.txt",
              status: "complete",
            },
          ],
          assistant: { role: "assistant", content: "done" },
          closed: true,
        },
      ],
      model: null,
      thinkingLevel: "off",
      isStreaming: false,
      contextUsage: null,
      error: null,
      cost: 0,
      totalTokens: 0,
    });

    const stream = view.element.querySelector(".messages");
    const turn = stream.querySelector("section.turn");
    expect(turn).not.toBeNull();
    // The bash card is INSIDE the turn's process rail, between user and answer.
    const railCard = turn.querySelector(".process-details-group .tool-card");
    expect(railCard).not.toBeNull();
    expect(railCard.dataset.toolCallId).toBe("t1");
    // No separate tools block under the stream anymore.
    expect(view.element.querySelector(".ephemeral-tools")).toBeNull();
    expect(stream.querySelector(".ephemeral-tools")).toBeNull();
    // User and answer sit inside the turn, in order.
    const userMsg = turn.querySelector(".message.user");
    const answerMsg = turn.querySelector(".message.assistant");
    expect(userMsg?.textContent).toContain("run ls");
    expect(answerMsg?.textContent).toContain("done");
    expect(turn.elementType).toBeUndefined();
    view.destroy();
  });

  it("falls back to the flat transcript when the snapshot carries no turns", () => {
    const runtime = makeRuntime();
    const view = new EphemeralChatView({ runtime, kind: "side-chat", toolsEnabled: true });
    runtime.applySnapshot({
      type: "ephemeral_snapshot",
      instanceId: "inst-1",
      generation: 1,
      runtimeSequenceWatermark: 0,
      messages: [{ role: "user", content: "legacy" }],
      assistantDraft: null,
      tools: [{ toolCallId: "t1", toolName: "bash", args: {}, output: "x", status: "complete" }],
      model: null,
      thinkingLevel: "off",
      isStreaming: false,
      contextUsage: null,
      error: null,
      cost: 0,
      totalTokens: 0,
    });
    expect(view.element.querySelector("section.turn")).toBeNull();
    expect(view.element.querySelector(".message.user")?.textContent).toContain("legacy");
    expect(view.element.querySelector(".tool-card")).not.toBeNull();
    view.destroy();
  });

  it("does not render tool cards when tools are disabled (Quick Chat)", () => {
    const runtime = makeRuntime();
    const view = new EphemeralChatView({ runtime, kind: "quick-chat", toolsEnabled: false });
    runtime.applySnapshot({
      type: "ephemeral_snapshot",
      instanceId: "inst-1",
      generation: 1,
      runtimeSequenceWatermark: 0,
      messages: [],
      assistantDraft: null,
      tools: [{ toolCallId: "tc1", toolName: "bash", args: {}, output: "x", status: "complete" }],
      model: null,
      thinkingLevel: "off",
      isStreaming: false,
      contextUsage: null,
      error: null,
    });
    expect(view.element.querySelector(".tool-card")).toBeNull();
    view.destroy();
  });

  it("renders status, tokens, and cost in the compact usage row", () => {
    const runtime = makeRuntime();
    const view = new EphemeralChatView({ runtime, kind: "side-chat", toolsEnabled: true });
    runtime.applySnapshot({
      type: "ephemeral_snapshot",
      instanceId: "inst-1",
      generation: 1,
      runtimeSequenceWatermark: 0,
      messages: [],
      assistantDraft: null,
      tools: [],
      model: null,
      thinkingLevel: "off",
      isStreaming: true,
      contextUsage: null,
      error: null,
      cost: 0.0123,
      totalTokens: 456,
    });
    const usage = view.element.querySelector(".ephemeral-usage");
    expect(usage.textContent).toContain("Generating");
    expect(usage.textContent).toContain("456");
    expect(usage.textContent).toContain("$0.0123");
    view.destroy();
  });

  it("shows an error status when the runtime has an error", () => {
    const runtime = makeRuntime();
    const view = new EphemeralChatView({ runtime, kind: "side-chat", toolsEnabled: true });
    runtime.applySnapshot({
      type: "ephemeral_snapshot",
      instanceId: "inst-1",
      generation: 1,
      runtimeSequenceWatermark: 0,
      messages: [],
      assistantDraft: null,
      tools: [],
      model: null,
      thinkingLevel: "off",
      isStreaming: false,
      contextUsage: null,
      error: "boom",
      cost: 0,
      totalTokens: 0,
    });
    const usage = view.element.querySelector(".ephemeral-usage");
    expect(usage.textContent).toContain("Error");
    view.destroy();
  });

  it("renders an error from the render state", () => {
    const runtime = makeRuntime();
    const view = new EphemeralChatView({ runtime, kind: "side-chat", toolsEnabled: true });
    runtime.applySnapshot({
      type: "ephemeral_snapshot",
      instanceId: "inst-1",
      generation: 1,
      runtimeSequenceWatermark: 0,
      messages: [],
      assistantDraft: null,
      tools: [],
      model: null,
      thinkingLevel: "off",
      isStreaming: false,
      contextUsage: null,
      error: "something broke",
    });
    expect(view.element.textContent).toContain("something broke");
    view.destroy();
  });

  it("destroy is idempotent and stops reacting to later render state", () => {
    const runtime = makeRuntime();
    const view = new EphemeralChatView({ runtime, kind: "side-chat", toolsEnabled: true });
    expect(() => view.destroy()).not.toThrow();
    // A later snapshot must not throw after destroy.
    expect(() =>
      runtime.applySnapshot({
        type: "ephemeral_snapshot",
        instanceId: "inst-1",
        generation: 1,
        runtimeSequenceWatermark: 0,
        messages: [],
        assistantDraft: null,
        tools: [],
        model: null,
        thinkingLevel: "off",
        isStreaming: false,
        contextUsage: null,
        error: null,
      }),
    ).not.toThrow();
  });

  describe("@-file mention", () => {
    function canned() {
      return vi.fn(async () => ({
        items: [{ value: "@a.ts", label: "a.ts", description: "a.ts", isDirectory: false }],
        truncated: false,
      }));
    }

    it("installs a unique popup per view and Enter selects without submitting", async () => {
      const searchFiles = canned();
      const side = new EphemeralChatView({
        runtime: makeRuntime(),
        kind: "side-chat",
        toolsEnabled: true,
        getWorkspaceRoot: () => "/repo",
        searchFiles,
      });
      const quick = new EphemeralChatView({
        runtime: makeRuntime(),
        kind: "quick-chat",
        toolsEnabled: false,
        getWorkspaceRoot: () => "/repo",
        searchFiles,
      });

      const sideTextarea = side.element.querySelector("textarea");
      const quickTextarea = quick.element.querySelector("textarea");
      sideTextarea.value = "@a";
      sideTextarea.setSelectionRange(2, 2);
      quickTextarea.value = "@a";
      quickTextarea.setSelectionRange(2, 2);
      await side._mentionController.update();
      await quick._mentionController.update();

      const sidePopup = side.element.querySelector(".at-file-mention-menu");
      const quickPopup = quick.element.querySelector(".at-file-mention-menu");
      expect(sidePopup.id).not.toBe(quickPopup.id);
      expect(sidePopup.querySelector(".at-file-mention-option")).not.toBeNull();

      const sendSpy = vi.spyOn(side.runtime, "sendPrompt");
      sideTextarea.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
      expect(sendSpy).not.toHaveBeenCalled();
      expect(sideTextarea.value).toBe("@a.ts ");
      side.destroy();
      quick.destroy();
    });

    it("looks up the owner workspace live on every request", async () => {
      const getWorkspaceRoot = vi.fn(() => "/repo");
      const view = new EphemeralChatView({
        runtime: makeRuntime(),
        kind: "side-chat",
        toolsEnabled: true,
        getWorkspaceRoot,
        searchFiles: canned(),
      });
      const textarea = view.element.querySelector("textarea");
      textarea.value = "@a";
      textarea.setSelectionRange(2, 2);
      await view._mentionController.update();
      textarea.value = "@a";
      textarea.setSelectionRange(2, 2);
      await view._mentionController.update();
      expect(getWorkspaceRoot).toHaveBeenCalledTimes(2);
      view.destroy();
    });

    it("destroy ignores a pending completion response", async () => {
      let resolveSearch;
      const searchFiles = vi.fn(
        () =>
          new Promise((resolve) => {
            resolveSearch = () =>
              resolve({
                items: [{ value: "@a.ts", label: "a.ts", description: "a.ts", isDirectory: false }],
                truncated: false,
              });
          }),
      );
      const view = new EphemeralChatView({
        runtime: makeRuntime(),
        kind: "side-chat",
        toolsEnabled: true,
        getWorkspaceRoot: () => "/repo",
        searchFiles,
      });
      const textarea = view.element.querySelector("textarea");
      textarea.value = "@a";
      textarea.setSelectionRange(2, 2);
      const pending = view._mentionController.update();
      view.destroy();
      resolveSearch();
      await pending;
      expect(view.element.querySelector(".at-file-mention-menu").classList.contains("hidden")).toBe(
        true,
      );
    });
  });
});

describe("EphemeralChatView safety-guard teardown", () => {
  it("destroys the guard dialog with the view: no answer escapes afterwards", () => {
    const runtime = makeRuntime();
    const respond = vi.spyOn(runtime, "respondToExtensionUi");
    const view = new EphemeralChatView({ runtime, kind: "side-chat", toolsEnabled: true });
    const payload = {
      __safetyGuardBash: 1,
      version: 1,
      sections: [{ label: "Command", body: "rm -rf ./dist" }],
      choices: [{ label: "Block" }, { label: "Allow once" }],
    };
    runtime.dispatchEvent(
      new CustomEvent("extensionuirequest", {
        detail: { request: { id: "sg-1", method: "select", message: JSON.stringify(payload) } },
      }),
    );
    expect(view._dialogContainer.classList.contains("hidden")).toBe(false);

    view.destroy();

    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(respond).not.toHaveBeenCalled();
    expect(view._dialogContainer.classList.contains("hidden")).toBe(true);
  });
});

// T1 inventory row `/v2/paste-offload` (Side/Quick Chat call site): a remote
// host never offers the offload action, and the composer says why.
describe("EphemeralChatView remote paste", () => {
  it("disables paste offload on a remote host and keeps it for the local host", () => {
    const remote = new EphemeralChatView({
      runtime: makeRuntime(),
      kind: "side-chat",
      toolsEnabled: true,
      remoteHost: true,
    });
    const local = new EphemeralChatView({
      runtime: makeRuntime(),
      kind: "side-chat",
      toolsEnabled: true,
    });

    expect(remote._pasteOffload.isDisabled()).toBe(true);
    expect(local._pasteOffload.isDisabled()).toBe(false);
    remote.destroy();
    local.destroy();
  });
});
