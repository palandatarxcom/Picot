// ABOUTME: Element-scoped chat view for one ephemeral runtime: messages, tools,
// ABOUTME: composer, dialogs, and usage — reusing the shared render helpers.

import { setupVoiceInput } from "./app/voice-input.js";
import { setupComposerCommandMenu } from "./composer-command-menu.js";
import { setupComposerImageAttachments } from "./composer-image-attachments.js";
import { setupComposerPasteOffload } from "./composer-paste-offload.js";
import { onLocaleChange, t } from "./i18n.js";
import { createIcon } from "./icons.js";
import { processImageFile, processImagePayload } from "./image-attachments.js";
import { openModelDropdownMenu } from "./models/model-dropdown.js";
import { filterModelsByCatalogVisibility } from "./models/selection.js";
import { createHostFileMentionSearch, setupAtFileMention } from "./ui/at-file-mention.js";
import { guardComposerArrowInsertion } from "./ui/composer-caret-guard.js";
import { DialogHandler } from "./ui/dialogs.js";
import { MessageRenderer } from "./ui/message-renderer.js";
import { SafetyGuardDialog } from "./ui/safety-guard-dialog.js";
import { ToolCardRenderer } from "./ui/tool-card.js";
import { createTurnSection } from "./ui/turn.js";
import { summarizeTurnRail } from "./ui/turn-model.js";

// Monotonic counter guarantees unique mention-popup ids across ephemeral views.
let ephemeralPopupSeq = 0;

function setRegistryIcon(doc, button, name, options = {}) {
  const icon = createIcon(name, { ...options, document: doc });
  if (icon) button.replaceChildren(icon);
}

/**
 * Builds an isolated DOM tree for one ephemeral chat and projects the runtime's
 * render state into it. Side Chat enables tools; Quick Chat does not. Multiple
 * views can stay alive while only one is visible.
 */
export class EphemeralChatView {
  constructor({
    runtime,
    kind,
    toolsEnabled,
    getWorkspaceRoot,
    searchFiles,
    loadModelCatalog,
    configGateway = null,
    onOpenModelsSettings = null,
    remoteHost = false,
  }) {
    this.runtime = runtime;
    this.kind = kind || "side-chat";
    this.toolsEnabled = toolsEnabled !== false;
    // Remote host: no paste-offload face (T1 inventory), so the composer says
    // why the paste stays inline instead of offering an action the host refuses.
    this.remoteHost = remoteHost === true;
    this.destroyed = false;
    this._interactionLocked = false;
    // Model list for the shared selector; populated on first menu open.
    this._models = [];
    // Live owner-workspace lookup for @-file mentions. Never cached, never the
    // Quick Chat temporary cwd — supplied by the manager via app.js.
    this._getWorkspaceRoot = getWorkspaceRoot || (() => null);
    // Mention completion comes from the host data plane. An ephemeral Pi has no
    // HTTP origin of its own, so a fetch against the Pi port could never work.
    this._searchFiles = searchFiles || createHostFileMentionSearch(() => this.runtime?.transport);
    this._loadModelCatalog = loadModelCatalog;
    this._configGateway = configGateway;
    this._onOpenModelsSettings = onOpenModelsSettings;

    const doc = globalThis.document;
    this._doc = doc;

    this._root = doc.createElement("div");
    this._root.className = "ephemeral-chat-view";
    this._root.setAttribute("role", "tabpanel");

    this._messagesEl = doc.createElement("div");
    this._messagesEl.className = "messages ephemeral-messages";
    this._root.appendChild(this._messagesEl);

    this._usageEl = doc.createElement("div");
    this._usageEl.className = "ephemeral-usage";
    this._root.appendChild(this._usageEl);

    this._dialogContainer = doc.createElement("div");
    this._dialogContainer.className = "ephemeral-dialog-container hidden";
    this._root.appendChild(this._dialogContainer);

    // Composer mirrors the main chat's card and toolbar hierarchy. The scoped
    // classes avoid duplicate document IDs while deliberately reusing its style.
    this._composer = doc.createElement("div");
    this._composer.className = "composer-card ephemeral-composer";
    this._mentionMenu = doc.createElement("div");
    this._mentionMenu.className = "at-file-mention-menu hidden";
    this._mentionMenu.id = `ephemeral-at-mention-${ephemeralPopupSeq++}`;
    this._textarea = doc.createElement("textarea");
    this._textarea.className = "ephemeral-input";
    this._textarea.placeholder = t("ephemeral.placeholder");
    this._textarea.rows = 2;
    this._imagePreviews = doc.createElement("div");
    this._imagePreviews.className = "image-previews hidden";
    this._imageInput = doc.createElement("input");
    this._imageInput.type = "file";
    this._imageInput.multiple = true;
    this._imageInput.accept = "image/*";
    this._imageInput.classList.add("hidden");
    this._composer.append(this._imagePreviews, this._mentionMenu, this._textarea, this._imageInput);

    const toolbar = doc.createElement("div");
    toolbar.className = "composer-toolbar";
    const toolbarLeft = doc.createElement("div");
    toolbarLeft.className = "composer-toolbar-left";
    toolbar.appendChild(toolbarLeft);
    const toolbarRight = doc.createElement("div");
    toolbarRight.className = "composer-toolbar-right";

    const attachBtn = doc.createElement("button");
    attachBtn.type = "button";
    attachBtn.className = "input-icon-btn ephemeral-attach";
    attachBtn.dataset.role = "ephemeral-attach";
    setRegistryIcon(doc, attachBtn, "plus", { size: 16 });
    this._attachBtn = attachBtn;
    toolbarLeft.appendChild(attachBtn);

    this._commandBtn = doc.createElement("button");
    this._commandBtn.type = "button";
    this._commandBtn.className = "input-icon-btn ephemeral-command";
    this._commandBtn.dataset.role = "ephemeral-command";
    const commandsLabel = t("input.commands");
    this._commandBtn.title = commandsLabel;
    this._commandBtn.setAttribute("aria-label", commandsLabel);
    setRegistryIcon(doc, this._commandBtn, "bot", { size: 16 });
    toolbarLeft.appendChild(this._commandBtn);

    this._modelDropdown = doc.createElement("div");
    this._modelDropdown.className = "model-dropdown";
    this._modelBtn = doc.createElement("button");
    this._modelBtn.type = "button";
    this._modelBtn.className = "model-dropdown-btn";
    this._modelBtn.dataset.role = "ephemeral-model";
    this._modelLabel = doc.createElement("span");
    this._modelLabel.className = "model-dropdown-label";
    this._modelBtn.appendChild(this._modelLabel);
    const chevron = createIcon("chevron-down", { size: 10, document: doc });
    chevron?.classList.add("model-dropdown-chevron");
    if (chevron) this._modelBtn.appendChild(chevron);
    this._modelMenu = doc.createElement("div");
    this._modelMenu.className = "model-dropdown-menu hidden";
    this._modelDropdown.append(this._modelBtn, this._modelMenu);
    toolbarRight.appendChild(this._modelDropdown);

    this._thinkingBtn = doc.createElement("button");
    this._thinkingBtn.type = "button";
    this._thinkingBtn.className = "thinking-tag off";
    this._thinkingBtn.dataset.role = "ephemeral-thinking";
    toolbarRight.appendChild(this._thinkingBtn);

    this._micBtn = doc.createElement("button");
    this._micBtn.type = "button";
    this._micBtn.className = "input-mic-btn ephemeral-mic";
    setRegistryIcon(doc, this._micBtn, "mic", { size: 16 });
    toolbarRight.appendChild(this._micBtn);

    this._sendBtn = doc.createElement("button");
    this._sendBtn.type = "button";
    this._sendBtn.className = "ephemeral-send";
    this._sendBtn.dataset.role = "ephemeral-send";
    toolbarRight.appendChild(this._sendBtn);
    toolbar.appendChild(toolbarRight);
    this._composer.appendChild(toolbar);

    this._commandMenu = doc.createElement("div");
    this._commandMenu.className = "command-palette hidden";
    this._commandHeader = doc.createElement("div");
    this._commandHeader.className = "command-palette-header";
    this._commandHeader.textContent = t("input.commands");
    this._commandList = doc.createElement("div");
    this._commandList.className = "command-list";
    this._commandMenu.append(this._commandHeader, this._commandList);
    this._root.append(this._commandMenu, this._composer);

    // Shared render helpers, each scoped to this view's containers.
    this.messageRenderer = new MessageRenderer(this._messagesEl);
    this.toolCardRenderer = this.toolsEnabled ? new ToolCardRenderer(this._messagesEl) : null;
    this.dialogHandler = new DialogHandler({
      container: this._dialogContainer,
      notificationContainer: this._messagesEl,
      send: (message) => {
        // The response routes back through the runtime's owner-scoped transport.
        this.runtime.respondToExtensionUi(message.id, message);
      },
    });
    this.safetyGuardDialog = new SafetyGuardDialog({
      container: this._dialogContainer,
      send: (message) => {
        this.runtime.respondToExtensionUi(message.id, message);
      },
    });

    this._destroyVoice = setupVoiceInput({ micBtn: this._micBtn, messageInput: this._textarea });
    this._detachCaretGuard = guardComposerArrowInsertion(this._textarea);

    this._pasteOffload = setupComposerPasteOffload({
      textarea: this._textarea,
      container: this._composer,
      document: doc,
      offload: async (content) => {
        const response = await fetch("/v2/paste-offload", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ content }),
        });
        const result = await response.json().catch(() => null);
        if (!response.ok)
          throw new Error(
            result?.error?.code || result?.error || `Paste offload failed: ${response.status}`,
          );
        return result?.path;
      },
      disabledReason: this.remoteHost ? t("input.pasteRemoteDisabled") : null,
      t,
    });

    // Image attachments: attach button, native picker, paste/drop, previews.
    // Spec §Frontend Module Boundaries: any reused setup helper must return
    // an explicit cleanup function for the owning view.
    this._imageAttachments = setupComposerImageAttachments({
      doc,
      composerCard: this._composer,
      textarea: this._textarea,
      attachBtn: this._attachBtn,
      imageInput: this._imageInput,
      imagePreviews: this._imagePreviews,
      processImageFile,
      processImagePayload,
      pickImageFiles: this.runtime?.transport?.pickImageFiles?.bind(this.runtime.transport),
      getWorkspacePath: undefined,
      isNativeAvailable: () => Boolean(this.runtime?.transport?.capabilities?.native),
      t,
    });
    this._commandMenuController = setupComposerCommandMenu({
      button: this._commandBtn,
      menu: this._commandMenu,
      list: this._commandList,
      getCommands: () => this._sideCommands(),
      document: this._doc,
      createIcon,
    });

    this._onRenderState = (event) => this._render(event.detail);
    this._onExtensionUi = (event) => this._showExtensionDialog(event.detail.request);
    this._onKeyDown = (event) => this._handleKeyDown(event);
    this.runtime.addEventListener("renderstate", this._onRenderState);
    this.runtime.addEventListener("extensionuirequest", this._onExtensionUi);
    // Mention completion is wired before the submit keydown so its listener can
    // intercept Enter/Tab/Escape first; teardown happens in destroy().
    this._mentionController = setupAtFileMention({
      input: this._textarea,
      container: this._mentionMenu,
      getWorkspaceRoot: () => this._getWorkspaceRoot(),
      searchFiles: this._searchFiles,
      document: this._doc,
    });
    this._textarea.addEventListener("keydown", this._onKeyDown);
    this._onSendClick = () => {
      if (this.runtime.isStreaming) this.runtime.abort();
      else this._submit();
    };
    this._sendBtn.addEventListener("click", this._onSendClick);
    this._onModelClick = () => void this._toggleModelMenu();
    this._onThinkingClick = () => this._cycleThinkingLevel();
    this._onDocumentClick = (event) => {
      if (!this._modelDropdown.contains(event.target)) this._closeModelMenu();
    };
    this._modelBtn.addEventListener("click", this._onModelClick);
    this._thinkingBtn.addEventListener("click", this._onThinkingClick);
    this._doc.addEventListener("click", this._onDocumentClick);
    this._unsubscribeLocale = onLocaleChange(() => {
      this._commandHeader.textContent = t("input.commands");
      this._renderComposerState({
        model: this.runtime.model,
        thinkingLevel: this.runtime.thinkingLevel,
        thinkingLevels: this.runtime.thinkingLevels,
        isStreaming: this.runtime.isStreaming,
      });
    });
    this._renderComposerState({
      model: this.runtime.model,
      thinkingLevel: this.runtime.thinkingLevel,
      thinkingLevels: this.runtime.thinkingLevels,
      isStreaming: this.runtime.isStreaming,
    });
  }

  get element() {
    return this._root;
  }

  activate() {
    this.runtime.acknowledgeVisible();
  }

  deactivate() {
    // Hiding the view does not destroy it.
  }

  focusLastMeaningfulControl() {
    if (!this.destroyed) this._textarea.focus();
  }

  setInteractionLocked(locked) {
    this._interactionLocked = Boolean(locked);
    this._textarea.disabled = this._interactionLocked;
    this._sendBtn.disabled = this._interactionLocked;
    this._micBtn.disabled = this._interactionLocked;
    this._modelBtn.disabled = this._interactionLocked;
    this._thinkingBtn.disabled = this._interactionLocked;
    this._attachBtn.disabled = this._interactionLocked;
    this._commandBtn.disabled = this._interactionLocked;
  }

  destroy() {
    if (this.destroyed) return;
    this.destroyed = true;
    this.runtime.removeEventListener("renderstate", this._onRenderState);
    this.runtime.removeEventListener("extensionuirequest", this._onExtensionUi);
    this._textarea.removeEventListener("keydown", this._onKeyDown);
    this._sendBtn.removeEventListener("click", this._onSendClick);
    this._modelBtn.removeEventListener("click", this._onModelClick);
    this._thinkingBtn.removeEventListener("click", this._onThinkingClick);
    this._commandMenuController?.destroy();
    this._mentionController?.destroy();
    this._doc.removeEventListener("click", this._onDocumentClick);
    this._unsubscribeLocale?.();
    this.messageRenderer?.destroy();
    this.toolCardRenderer?.destroy();
    this.dialogHandler?.destroy();
    this.safetyGuardDialog?.destroy();
    this._destroyVoice?.();
    this._detachCaretGuard?.();
    this._pasteOffload?.destroy();
    this._imageAttachments?.destroy();
    this._root.remove();
  }

  // ── Internals ───────────────────────────────────────────────────────────────

  _submit() {
    if (this.destroyed || this._interactionLocked || this._pasteOffload?.isBusy()) return;
    if (this.runtime.isStreaming) {
      this.runtime.abort();
      return;
    }
    const text = this._textarea.value.trim();
    if (!text) return;
    const images = this._imageAttachments?.consumePendingImages() || [];
    this.runtime.sendPrompt(text, images);
    this._textarea.value = "";
  }

  _handleKeyDown(event) {
    if (this.destroyed) return;
    if (event.key === "Escape" && this.runtime.isStreaming) {
      event.preventDefault();
      this.runtime.abort();
    } else if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      this._submit();
    }
  }

  _render(state) {
    if (this.destroyed || !state) return;
    this.messageRenderer.clear();
    // Main-chat parity: turn-grouped rendering with in-stream process rails
    // (tool cards live inside their turn, between the user message and the
    // answer — never in a separate block under the stream).
    if (Array.isArray(state.turns)) {
      this._renderTurns(state);
    } else {
      this._renderFlat(state);
    }
    if (state.error) {
      this.messageRenderer.renderError(state.error);
    }
    this._renderUsage(state);
    this._renderComposerState(state);
  }

  _renderTurns(state) {
    let openAnswerHost = null;
    for (const turn of state.turns) {
      const section = createTurnSection({ withStatus: false });
      if (turn.user) {
        // Render into a detached holder, then move the bubble into the turn's
        // user slot (the slot's only public API is claimUserElement).
        const holder = this._doc.createElement("div");
        const userEl = this.messageRenderer.renderUserMessage(turn.user, true, holder);
        section.claimUserElement(userEl);
      }
      if (this.toolCardRenderer && turn.tools.length > 0) {
        for (const tool of turn.tools) {
          this.toolCardRenderer.createToolCard(tool, section.rail.host);
          this.toolCardRenderer.updateToolCard(tool);
        }
        if (turn.closed || !state.isStreaming) {
          section.rail.setLabel(summarizeTurnRail(0, turn.tools.length));
          section.rail.setDisclosure(false);
        } else {
          // Live turn: keep the rail expanded so streaming tool output reads
          // in place, exactly like the main chat's live process details.
          section.rail.setDisclosure(true);
        }
      }
      if (turn.assistant) {
        this.messageRenderer.renderAssistantMessage(
          turn.assistant,
          false,
          true,
          section.answer.host,
          false,
        );
      } else if (!turn.closed) {
        openAnswerHost = section.answer.host;
      }
      this._messagesEl.appendChild(section.element);
    }
    if (state.assistantDraft) {
      // The draft belongs to the open turn's answer slot; if every turn is
      // already closed (edge ordering: agent still streaming after turn_end),
      // fall back to the stream tail so streaming text is never lost.
      const host = openAnswerHost ?? this._messagesEl;
      const streamingEl = this.messageRenderer.renderAssistantMessage(
        { content: state.assistantDraft.text || "" },
        true,
        false,
        host,
      );
      if (state.assistantDraft.thinking) {
        this.messageRenderer.updateStreamingThinking(streamingEl, state.assistantDraft.thinking);
      }
    }
  }

  /** Legacy hosts without a turns-bearing snapshot: flat transcript fallback. */
  _renderFlat(state) {
    for (const message of state.messages || []) {
      if (message.role === "user") {
        this.messageRenderer.renderUserMessage(message);
      } else if (message.role === "assistant") {
        this.messageRenderer.renderAssistantMessage(message);
      }
    }
    if (state.assistantDraft) {
      const streamingEl = this.messageRenderer.renderAssistantMessage(
        { content: state.assistantDraft.text || "" },
        true,
      );
      if (state.assistantDraft.thinking) {
        this.messageRenderer.updateStreamingThinking(streamingEl, state.assistantDraft.thinking);
      }
    }
    if (this.toolCardRenderer) {
      this.toolCardRenderer.clear();
      for (const tool of state.tools || []) {
        this.toolCardRenderer.createToolCard(tool);
        this.toolCardRenderer.updateToolCard(tool);
      }
    }
  }

  _sideCommands() {
    return [
      {
        icon: "text-collapse",
        label: t("input.compact"),
        desc: t("input.compactDesc"),
        action: () => this.runtime.runCommand("compact"),
      },
      {
        icon: "clipboard",
        label: t("input.exportHtml"),
        desc: t("ephemeral.exportUnavailable"),
        action: () => {},
        disabled: true,
      },
      {
        icon: "bar-chart",
        label: t("input.sessionStats"),
        desc: t("input.sessionStatsDesc"),
        action: async () => {
          const stats = await this.runtime.runCommand("get_session_stats");
          if (!stats) return;
          this.messageRenderer.renderSystemMessage(
            t("status.sessionStatsMessages", {
              total: stats.totalMessages,
              user: stats.userMessages,
              assistant: stats.assistantMessages,
            }),
          );
        },
      },
      {
        icon: "chevrons-down",
        label: t("input.expandAllTools"),
        desc: t("input.expandAllToolsDesc"),
        action: () => this.toolCardRenderer?.expandAll(),
      },
      {
        icon: "chevrons-up",
        label: t("input.collapseAllTools"),
        desc: t("input.collapseAllToolsDesc"),
        action: () => this.toolCardRenderer?.collapseAll(),
      },
    ];
  }
  async _toggleModelMenu() {
    if (this._interactionLocked) return;
    if (!this._modelMenu.classList.contains("hidden")) {
      this._closeModelMenu();
      return;
    }
    // The shared main-composer selector renders once models resolve; the
    // menu opens empty (with the shared search box) in the meantime.
    openModelDropdownMenu({
      doc: this._doc,
      dropdown: this._modelDropdown,
      menu: this._modelMenu,
      loadModels: () => this._models,
      isSelected: (m) => {
        const activeId = this.runtime.model?.id ?? this.runtime.model?.modelId;
        return Boolean(activeId) && m.id === activeId;
      },
      onPick: (m) => {
        void this.runtime.setModel(m.provider, m.id).catch(() => {});
        this._closeModelMenu();
      },
      configGateway: this._configGateway,
      onOpenSettingsClick: this._onOpenModelsSettings
        ? () => {
            this._closeModelMenu();
            this._onOpenModelsSettings();
          }
        : null,
      close: () => this._closeModelMenu(),
      t,
    });
    try {
      // Live query against this ephemeral Pi: there is no host-side model
      // cache (the landing cold start removed it — no runtime to warm it).
      let models = await this.runtime.getAvailableModels();
      if (this._loadModelCatalog) {
        try {
          models = filterModelsByCatalogVisibility(models, await this._loadModelCatalog());
        } catch {
          // Keep the runtime list if the shared configuration bridge is unavailable.
        }
      }
      this._models = models;
    } catch {
      this._models = [];
    }
    if (this.destroyed || this._modelMenu.classList.contains("hidden")) return;
    // Re-render through the shared selector now that models are available.
    openModelDropdownMenu({
      doc: this._doc,
      dropdown: this._modelDropdown,
      menu: this._modelMenu,
      loadModels: () => this._models,
      isSelected: (m) => {
        const activeId = this.runtime.model?.id ?? this.runtime.model?.modelId;
        return Boolean(activeId) && m.id === activeId;
      },
      onPick: (m) => {
        void this.runtime.setModel(m.provider, m.id).catch(() => {});
        this._closeModelMenu();
      },
      configGateway: this._configGateway,
      onOpenSettingsClick: this._onOpenModelsSettings
        ? () => {
            this._closeModelMenu();
            this._onOpenModelsSettings();
          }
        : null,
      close: () => this._closeModelMenu(),
      t,
    });
  }

  _closeModelMenu() {
    this._modelMenu.classList.add("hidden");
    this._modelDropdown.classList.remove("open");
  }

  async _cycleThinkingLevel() {
    if (this._interactionLocked) return;
    try {
      await this.runtime.cycleThinkingLevel();
    } catch {
      // The runtime reports command failures through its normal failure event.
    }
  }

  _renderComposerState(state) {
    const model = state?.model;
    const modelId =
      typeof model === "string" ? model : model?.id || model?.modelId || t("misc.model");
    this._modelLabel.textContent = modelId;
    this._modelBtn.title = t("input.switchModel");
    this._modelBtn.setAttribute("aria-label", t("input.switchModel"));

    const thinkingLevel = state?.thinkingLevel || "off";
    const thinkingLevels = Array.isArray(state?.thinkingLevels) ? state.thinkingLevels : ["off"];
    this._thinkingBtn.textContent = t("settings.thinkingCompact", { level: thinkingLevel });
    this._thinkingBtn.title = t("settings.thinkingTitle");
    this._thinkingBtn.setAttribute(
      "aria-label",
      t("settings.thinkingAriaLabel", { level: thinkingLevel }),
    );
    this._thinkingBtn.classList.toggle("off", thinkingLevel === "off");
    this._thinkingBtn.dataset.availableLevels = thinkingLevels.join(",");

    const sendLabel = state?.isStreaming ? t("ephemeral.abort") : t("ephemeral.send");
    this._sendBtn.title = sendLabel;
    this._sendBtn.setAttribute("aria-label", sendLabel);
    if (state?.isStreaming) {
      setRegistryIcon(this._doc, this._sendBtn, "square", { size: 16, filled: true });
    } else {
      setRegistryIcon(this._doc, this._sendBtn, "arrow-up", { size: 16 });
    }
    const voiceLabel = t("voice.voiceInput");
    this._micBtn.title = voiceLabel;
    this._micBtn.setAttribute("aria-label", voiceLabel);
  }

  _renderUsage(state) {
    if (!this._usageEl) return;
    const parts = [];
    const status = this._connectionStatus(state);
    if (status) parts.push(status);
    const tokens = state?.totalTokens ?? state?.contextUsage?.used ?? state?.contextUsage?.tokens;
    if (typeof tokens === "number" && tokens > 0) {
      parts.push(`${tokens} ${t("ephemeral.tokens")}`);
    }
    const cost = state?.cost;
    if (typeof cost === "number" && cost > 0) {
      parts.push(`$${cost.toFixed(4)}`);
    }
    this._usageEl.textContent = parts.join(" · ");
  }

  _connectionStatus(state) {
    if (state?.error) return t("ephemeral.statusError");
    if (state?.isStreaming) return t("ephemeral.statusStreaming");
    return t("ephemeral.statusReady");
  }

  _showExtensionDialog(request) {
    if (this.destroyed || !request) return;
    switch (request.method) {
      case "select":
        if (this.safetyGuardDialog.handleExtensionUIRequest(request)) break;
        this.dialogHandler.showSelect(request);
        break;
      case "confirm":
        this.dialogHandler.showConfirm(request);
        break;
      case "input":
        this.dialogHandler.showInput(request);
        break;
      case "editor":
        this.dialogHandler.showEditor(request);
        break;
      default:
        break;
    }
  }
}
