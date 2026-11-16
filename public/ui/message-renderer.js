// ABOUTME: Renders user and assistant chat messages for the Picot WebView.
// ABOUTME: Preserves renderer behavior while exposing user elements for navigation.
/**
 * Message Renderer - Renders chat messages with markdown support
 */

import { onLocaleChange, t } from "../i18n.js";
import { createIcon, setButtonIcon } from "../icons.js";
import { createScrollOwner } from "../session/scroll-ownership.js";
import { displayNativePath } from "../workspace/path-utils.js";
import { renderMarkdown, renderStreamingMarkdown, renderUserMarkdown } from "./markdown.js";
import { sanitizeMarkup } from "./sanitize-markup.js";

const USER_MESSAGE_COLLAPSE_CHAR_THRESHOLD = 400;
const USER_MESSAGE_COLLAPSE_NEWLINE_THRESHOLD = 8;

export function shouldCollapseUserMessage(text) {
  if (typeof text !== "string" || text.length === 0) return false;
  return (
    text.length >= USER_MESSAGE_COLLAPSE_CHAR_THRESHOLD ||
    (text.match(/\n/g)?.length ?? 0) >= USER_MESSAGE_COLLAPSE_NEWLINE_THRESHOLD
  );
}

/**
 * Format a message timestamp for a chat log.
 *
 * Same calendar day (local time) → "HH:MM". A different day → "MM/DD HH:MM"
 * (no i18n — the numeric form reads the same across locales). Invalid or
 * missing input → "" so callers can render unconditionally and omit the
 * span when there is nothing to show. Intentionally does NOT reuse the
 * sidebar's relative-time `formatSessionTime`; chat logs want absolute
 * clock times, not "2h ago".
 */
export function formatMessageTime(timestampMs) {
  // null / undefined must short-circuit before Number(): Number(null) === 0
  // is a finite value and would otherwise render the epoch as a real time.
  if (timestampMs == null) return "";
  const ms = Number(timestampMs);
  if (!Number.isFinite(ms)) return "";
  const date = new Date(ms);
  // Number.isFinite(1e20) passes, but new Date(1e20) is invalid (getTime → NaN).
  if (Number.isNaN(date.getTime())) return "";
  const now = new Date();
  const sameDay =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();
  const pad = (n) => String(n).padStart(2, "0");
  const hhmm = `${pad(date.getHours())}:${pad(date.getMinutes())}`;
  if (sameDay) return hhmm;
  return `${pad(date.getMonth() + 1)}/${pad(date.getDate())} ${hhmm}`;
}

/**
 * Format a response duration (ms) for the message footer: "3.2s" under a
 * minute, "1m 05s" above it. Picot times generation client-side (streaming
 * element created -> message finalized); pi's runtime events carry no duration
 * of their own. Returns "" for missing/invalid input so callers can render
 * unconditionally.
 */
export function formatDurationLabel(durationMs) {
  // null / undefined must short-circuit before Number(): Number(null) === 0 is
  // a finite value and would otherwise render a fake "0.0s" duration.
  if (durationMs == null) return "";
  const ms = Number(durationMs);
  if (!Number.isFinite(ms) || ms < 0) return "";
  // Round to one decimal before branching: on the unrounded value 59.96s would
  // render as "60.0s" instead of the minute it rounds into, and rounding the
  // remainder alone turns 119.6s into "1m 60s".
  const totalSeconds = Math.round(ms / 100) / 10;
  if (totalSeconds < 60) return `${totalSeconds.toFixed(1)}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = Math.round(totalSeconds - minutes * 60);
  if (seconds === 60) return `${minutes + 1}m 00s`;
  return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
}

/** Full timestamp for the hover `title` (screen-reader / exact reference). */
function fullTimestampTitle(timestampMs) {
  const ms = Number(timestampMs);
  if (!Number.isFinite(ms)) return "";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(
    d.getHours(),
  )}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export class MessageRenderer {
  /**
   * @param {Element} container
   * @param {{ sessionTreeActions?: boolean }} [options] `sessionTreeActions`
   *   enables the Fork/Edit user-message buttons. Only the persisted main
   *   chat (app.js) wires the `messagefork`/`messageedit` transport handlers,
   *   so ephemeral surfaces (Quick/Side Chat) and the native runtime keep the
   *   default `false` and render no inert controls.
   */
  constructor(container, { sessionTreeActions = false, scrollOwner = null } = {}) {
    this.container = container;
    this.sessionTreeActions = sessionTreeActions;
    // Scroll ownership (spec P3): one authority decides whether renders may
    // follow the bottom. A caller may share one owner with the tool-card
    // renderer; without one the view owns its own.
    this.scrollOwner = scrollOwner ?? createScrollOwner({ container });
    this._ownsScrollOwner = !scrollOwner;
    this.lastWelcomeOptions = null;
    this._destroyed = false;

    // Update already-rendered DOM when the locale changes without re-rendering
    // streaming content.
    this.unsubscribeLocaleChange = onLocaleChange(() => {
      if (!this.container) return;
      this.container.querySelectorAll(".message-copy-btn").forEach((btn) => {
        btn.setAttribute("aria-label", t("messages.copyMessage"));
        btn.title = t("messages.copyMessage");
      });
      this.container.querySelectorAll(".thinking-label-text").forEach((el) => {
        el.textContent = t("messages.thinking");
      });
      this.container.querySelectorAll(".message-user-collapse-toggle").forEach((button) => {
        button.textContent =
          button.getAttribute("aria-expanded") === "true"
            ? t("messages.collapse")
            : t("messages.expand");
      });
      if (this.container.querySelector(".welcome")) {
        this.renderWelcome(this.lastWelcomeOptions || {});
      }
    });
  }

  clear() {
    if (!this.container) return;
    this.container.replaceChildren();
    // Session switches reuse the same renderer instance. If the previous session
    // left the viewport away from bottom, keep new renders from inheriting that
    // stale anchor state (which can suppress auto-scroll until the user scrolls).
    this.scrollOwner?.followBottom();
  }

  clearSearchHighlights() {
    const marks = this.container.querySelectorAll("mark[data-search-highlight='true']");
    marks.forEach((mark) => {
      const text = document.createTextNode(mark.textContent || "");
      mark.replaceWith(text);
      text.parentNode?.normalize();
    });
  }

  highlightSearchQuery(query, { scrollToFirst = true } = {}) {
    this.clearSearchHighlights();

    const normalizedQuery = typeof query === "string" ? query.trim() : "";
    if (!normalizedQuery) return 0;

    const pattern = new RegExp(this.escapeRegExp(normalizedQuery), "gi");
    let matchCount = 0;
    let firstMatch = null;

    this.container.querySelectorAll(".message-content").forEach((content) => {
      const walker = document.createTreeWalker(content, NodeFilter.SHOW_TEXT, {
        acceptNode: (node) => {
          if (!node.textContent?.trim()) return NodeFilter.FILTER_REJECT;
          if (node.parentElement?.closest("mark[data-search-highlight='true']")) {
            return NodeFilter.FILTER_REJECT;
          }
          return NodeFilter.FILTER_ACCEPT;
        },
      });

      const textNodes = [];
      let currentNode = walker.nextNode();
      while (currentNode) {
        textNodes.push(currentNode);
        currentNode = walker.nextNode();
      }

      textNodes.forEach((node) => {
        const count = this.highlightTextNode(node, pattern, (mark) => {
          if (!firstMatch) firstMatch = mark;
        });
        matchCount += count;
      });
    });

    if (scrollToFirst && firstMatch && typeof firstMatch.scrollIntoView === "function") {
      firstMatch.scrollIntoView({ block: "center", behavior: "smooth" });
    }

    return matchCount;
  }

  renderWelcome({ workspacePath } = {}) {
    this.lastWelcomeOptions = { workspacePath };
    const welcome = document.createElement("div");
    welcome.className = "welcome";

    const icon = document.createElement("div");
    icon.className = "welcome-icon";
    const logo = document.createElement("img");
    logo.src = "icons/logo-dark.svg";
    logo.alt = "Picot logo";
    logo.className = "tau-icon-welcome";
    icon.appendChild(logo);
    welcome.appendChild(icon);

    welcome.appendChild(this._textElement("p", t("app.welcome")));
    welcome.appendChild(this._textElement("p", t("app.welcomeHint"), "hint"));
    if (workspacePath) {
      const workspace = document.createElement("p");
      workspace.className = "hint welcome-workspace";
      workspace.appendChild(document.createTextNode(`${t("app.currentWorkspace")} `));
      const code = document.createElement("code");
      code.textContent = displayNativePath(workspacePath);
      workspace.appendChild(code);
      welcome.appendChild(workspace);
    }

    const shortcuts = document.createElement("div");
    shortcuts.className = "shortcuts-hint";
    shortcuts.appendChild(this._textElement("span", `/ ${t("shortcuts.focusInput")}`));
    shortcuts.appendChild(this._textElement("span", `Esc ${t("shortcuts.abort")}`));
    welcome.appendChild(shortcuts);
    this.container.replaceChildren(welcome);
  }

  renderUserMessage(message, isHistory = false, targetContainer = null) {
    // Remove welcome message if present
    const welcome = this.container.querySelector(".welcome");
    if (welcome) welcome.remove();

    const div = document.createElement("div");
    div.className = `message user${isHistory ? " history" : ""}`;

    const content = document.createElement("div");
    content.className = "message-content";
    if (message.images?.length > 0) {
      const images = document.createElement("div");
      images.className = "message-images";
      for (const image of message.images) {
        const imageElement = document.createElement("img");
        imageElement.className = "message-image";
        imageElement.src = this._imageSource(image);
        imageElement.alt = t("messages.attachedImage");
        images.appendChild(imageElement);
      }
      content.appendChild(images);
    }
    this._appendMarkup(content, renderUserMarkdown(message.content));
    div.appendChild(content);
    const expandToggle = this._createUserCollapseToggle(content, message.content);
    const actions = this._createActionsBar({
      order: "user",
      timestamp: message.timestamp,
      copyable: Boolean((content.textContent || "").trim()),
      expandToggle,
      actionText: typeof message.content === "string" ? message.content : "",
    });
    if (actions) {
      div.appendChild(actions);
    }
    // P2: the history fold gate mounts older turns into a fragment host
    // before the batch control; default appends stay on the container.
    const host = targetContainer ?? this.container;
    host.appendChild(div);
    this._setupCodeCopyButtons(div);
    this._setupCopyBtn(div);
    if (!isHistory) this.scrollToBottom();
    return div;
  }

  renderAssistantMessage(
    message,
    isStreaming = false,
    isHistory = false,
    targetContainer = null,
    suppressToolbar = false,
  ) {
    // Remove welcome message if present
    const welcome = this.container.querySelector(".welcome");
    if (welcome) welcome.remove();

    const div = document.createElement("div");
    div.className = `message assistant${isHistory ? " history" : ""}`;
    div.dataset.messageId = message.id || "streaming";

    let contentHtml = "";
    let rawStreamingText = "";

    if (typeof message.content === "string") {
      rawStreamingText = message.content;
      contentHtml = isStreaming
        ? renderStreamingMarkdown(message.content)
        : renderMarkdown(message.content);
    } else if (Array.isArray(message.content)) {
      for (const block of message.content) {
        if (block.type === "text") {
          rawStreamingText += block.text;
          contentHtml += isStreaming
            ? renderStreamingMarkdown(block.text)
            : renderMarkdown(block.text);
        } else if (block.type === "thinking") {
          contentHtml += this.renderThinkingBlock(block.thinking);
        }
      }
    }
    // Markdown is rendered live during streaming, so the raw text (with its
    // syntax markers) can't be recovered from the DOM at finalize time.
    if (isStreaming) {
      div._streamingRawText = rawStreamingText;
    }

    const streamingClass = isStreaming ? " streaming" : "";

    const markup = `
      <div class="message-content${streamingClass}">${contentHtml}</div>
    `;
    this._replaceMarkup(div, markup);

    this._setupThinkingToggles(div);
    this._setupCodeCopyButtons(div);
    // The toolbar (copy + time + usage) attaches only once streaming has
    // finished and the row is not a folded process-details render. Streaming
    // rows and suppressed process rows get the toolbar later/at never.
    if (!isStreaming && !suppressToolbar) {
      const actions = this._createActionsBar({
        order: "assistant",
        timestamp: message.timestamp,
        usage: message.usage,
        copyable: Boolean((div.querySelector(".message-content")?.textContent || "").trim()),
      });
      if (actions) {
        div.appendChild(actions);
        this._setupCopyBtn(div);
      }
    }
    // History turns fold process content (thinking + tool calls) into a
    // collapsible "Process details" group; callers pass that group's body as
    // targetContainer so intermediate steps land inside it instead of the
    // main messages flow.
    const host = targetContainer ?? this.container;
    host.appendChild(div);
    if (!isHistory) this.scrollToBottom();

    return div;
  }

  renderThinkingBlock(thinking) {
    return `<div class="thinking-block">
<div class="thinking-toggle" data-thinking-toggle="true" role="button" tabindex="0">
<span class="chevron"><svg width="8" height="8" viewBox="0 0 8 8" fill="currentColor"><path d="M2 1l4 3-4 3z"/></svg></span>
<span class="thinking-label"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="vertical-align:-1px"><path d="M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z"/><path d="M12 5a3 3 0 1 1 5.997.125 4 4 0 0 1 2.526 5.77 4 4 0 0 1-.556 6.588A4 4 0 1 1 12 18Z"/><path d="M12 5v13"/><path d="M6.5 9h11"/><path d="M7 13h10"/></svg> <span class="thinking-label-text">${this.escapeHtml(t("messages.thinking"))}</span></span>
</div>
<div class="thinking-content">${this.escapeHtml(thinking)}</div>
</div>`;
  }

  _setupThinkingToggles(root) {
    root.querySelectorAll(".thinking-label-text").forEach((label) => {
      label.textContent = t("messages.thinking");
    });
    root.querySelectorAll("[data-thinking-toggle]").forEach((toggle) => {
      if (toggle.dataset.bound === "true") return;
      const toggleThinking = () => {
        toggle.nextElementSibling?.classList.toggle("expanded");
        toggle.classList.toggle("expanded");
      };
      toggle.addEventListener("click", toggleThinking);
      toggle.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          toggleThinking();
        }
      });
      toggle.dataset.bound = "true";
    });
  }

  /**
   * P1 rail-hosted streaming thinking: one .thinking-block element kept in
   * `host` (the turn rail), updated in place. Unlike the legacy in-content
   * variant, the element is standalone and never prefixes .message-content.
   */
  renderStreamingThinkingInto(host, thinking) {
    if (!host) return;
    let thinkingDiv = host.querySelector(":scope > .thinking-block.streaming-thinking");
    if (!thinkingDiv) {
      thinkingDiv = document.createElement("div");
      thinkingDiv.className = "thinking-block streaming-thinking";
      const markup = `
        <div class="thinking-toggle expanded" data-thinking-toggle="true" role="button" tabindex="0">
          <span class="chevron"><svg width="8" height="8" viewBox="0 0 8 8" fill="currentColor"><path d="M2 1l4 3-4 3z"/></svg></span>
          <span class="thinking-label"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="vertical-align:-1px"><path d="M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z"/><path d="M12 5a3 3 0 1 1 5.997.125 4 4 0 0 1 2.526 5.77 4 4 0 0 1-.556 6.588A4 4 0 1 1 12 18Z"/><path d="M12 5v13"/><path d="M6.5 9h11"/><path d="M7 13h10"/></svg> <span class="thinking-label-text"></span></span>
        </div>
        <div class="thinking-content expanded"></div>`;
      this._appendMarkup(thinkingDiv, markup);
      host.appendChild(thinkingDiv);
      this._setupThinkingToggles(thinkingDiv);
    }
    const contentEl = thinkingDiv.querySelector(".thinking-content");
    if (contentEl) {
      contentEl.textContent = thinking;
      this.scrollToBottom();
    }
  }

  updateStreamingThinking(messageElement, thinking) {
    let thinkingDiv = messageElement.querySelector(".streaming-thinking");
    if (!thinkingDiv) {
      const contentDiv = messageElement.querySelector(".message-content");
      if (!contentDiv) return;
      thinkingDiv = document.createElement("div");
      thinkingDiv.className = "thinking-block streaming-thinking";
      const markup = `
        <div class="thinking-toggle expanded" data-thinking-toggle="true" role="button" tabindex="0">
          <span class="chevron"><svg width="8" height="8" viewBox="0 0 8 8" fill="currentColor"><path d="M2 1l4 3-4 3z"/></svg></span>
          <span class="thinking-label"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="vertical-align:-1px"><path d="M12 5a3 3 0 1 0-5.997.125 4 4 0 0 0-2.526 5.77 4 4 0 0 0 .556 6.588A4 4 0 1 0 12 18Z"/><path d="M12 5a3 3 0 1 1 5.997.125 4 4 0 0 0 2.526 5.77 4 4 0 0 0-.556 6.588A4 4 0 1 1 12 18Z"/><path d="M12 5v13"/><path d="M6.5 9h11"/><path d="M7 13h10"/></svg> <span class="thinking-label-text"></span></span>
        </div>
        <div class="thinking-content expanded"></div>`;
      this._appendMarkup(thinkingDiv, markup);
      contentDiv.prepend(thinkingDiv);
      this._setupThinkingToggles(thinkingDiv);
    }
    const contentEl = thinkingDiv.querySelector(".thinking-content");
    if (contentEl) {
      contentEl.textContent = thinking;
      this.scrollToBottom();
    }
  }

  updateStreamingMessage(messageElement, content) {
    const contentDiv = messageElement.querySelector(".message-content");
    if (contentDiv) {
      messageElement._streamingRawText = content;
      // Keep any thinking block, update only the text part
      const thinkingBlock = contentDiv.querySelector(".streaming-thinking");
      const rendered = renderStreamingMarkdown(content);
      if (thinkingBlock) {
        // Remove everything after the thinking block and re-add text
        let textNode = contentDiv.querySelector(".streaming-text");
        if (!textNode) {
          textNode = document.createElement("div");
          textNode.className = "streaming-text";
          contentDiv.appendChild(textNode);
        }
        this._replaceMarkup(textNode, rendered);
      } else {
        this._replaceMarkup(contentDiv, rendered);
      }
      this._setupCodeCopyButtons(contentDiv);
      this.scrollToBottom();
    }
  }

  finalizeStreamingMessage(messageElement, usage = null, thinking = "", durationMs = null) {
    const contentDiv = messageElement.querySelector(".message-content");
    if (contentDiv) {
      contentDiv.classList.remove("streaming");
      // Prefer the raw text stashed during streaming — the DOM now holds
      // rendered markdown, so textContent has lost the syntax markers.
      const streamingText = contentDiv.querySelector(".streaming-text");
      const domText = streamingText ? streamingText.textContent : contentDiv.textContent;
      const rawText =
        typeof messageElement._streamingRawText === "string"
          ? messageElement._streamingRawText
          : domText;
      messageElement._streamingRawText = null;

      // Rebuild with thinking block (if any) + markdown text
      let html = "";
      if (thinking) {
        html += this.renderThinkingBlock(thinking);
      }
      html += renderMarkdown(rawText);
      this._replaceMarkup(contentDiv, html);
      this._setupThinkingToggles(contentDiv);
      this._setupCodeCopyButtons(contentDiv);
    }

    // Attach the toolbar (copy + finalize timestamp + usage) once streaming
    // finishes. The finalize timestamp is Date.now() — the completion moment
    // — matching the real-time assistant contract in the message-toolbar spec.
    if (
      !messageElement.dataset.turnDemoted &&
      this._copyableText(messageElement).trim() &&
      !messageElement.querySelector(".message-actions")
    ) {
      const actions = this._createActionsBar({
        order: "assistant",
        timestamp: Date.now(),
        usage,
        durationMs,
        copyable: true,
      });
      if (actions) {
        messageElement.appendChild(actions);
        this._setupCopyBtn(messageElement);
      }
    }
  }

  renderSystemMessage(text) {
    const div = document.createElement("div");
    div.className = "system-message";
    div.textContent = text;
    this.container.appendChild(div);
    this.scrollToBottom();
  }

  renderError(errorMessage) {
    const div = document.createElement("div");
    div.className = "error-message";
    div.textContent = `⚠️ ${errorMessage}`;
    this.container.appendChild(div);
    this.scrollToBottom();
  }

  /**
   * Returns the user-visible text of a message, excluding private thinking
   * blocks so copying an assistant message keeps only the answer.
   */
  _copyableText(messageEl) {
    const content = messageEl.querySelector(".message-content");
    if (!content) return "";
    const clone = content.cloneNode(true);
    clone.querySelectorAll(".thinking-block, .streaming-thinking").forEach((el) => {
      el.remove();
    });
    return clone.textContent || "";
  }

  _setupCopyBtn(messageEl) {
    const btn = messageEl.querySelector(".message-copy-btn");
    if (!btn) return;
    setButtonIcon(btn, "copy", { size: 12 });
    btn.addEventListener("click", () => {
      const text = this._copyableText(messageEl);
      if (!text) return;
      // Fallback for non-HTTPS (LAN access)
      const copyText = (t) => {
        if (navigator.clipboard) return navigator.clipboard.writeText(t);
        const ta = document.createElement("textarea");
        ta.value = t;
        ta.style.cssText = "position:fixed;left:-9999px";
        document.body.appendChild(ta);
        ta.select();
        document.execCommand("copy");
        document.body.removeChild(ta);
        return Promise.resolve();
      };
      copyText(text).then(() => {
        btn.classList.add("copied");
        setTimeout(() => {
          btn.classList.remove("copied");
        }, 1500);
      });
    });
  }

  _textElement(tagName, text, className = "") {
    const element = document.createElement(tagName);
    if (className) element.className = className;
    element.textContent = text;
    return element;
  }

  /**
   * Build the .message-actions toolbar row. `order` selects the slot layout:
   *   - "user":      [expand], time, copy
   *   - "assistant": copy, time, usage (usage only when cost > 0)
   * The user slot order prepends the long-message expand toggle when one is
   * supplied. The time span is omitted when `formatMessageTime(timestamp)`
   * returns "". The copy button is omitted when there is no copyable text
   * (e.g. an image-only user message). The toolbar row itself is always
   * returned (never null) so future actions still have a slot — per the
   * design spec every rendered user message keeps its toolbar.
   */
  _createActionsBar({
    order,
    timestamp = null,
    usage = null,
    durationMs = null,
    copyable = true,
    expandToggle = null,
    actionText = "",
  }) {
    const actions = document.createElement("div");
    actions.className = "message-actions";
    const timeLabel = formatMessageTime(timestamp);
    const timeSpan = () => {
      const span = document.createElement("span");
      span.className = "message-time";
      span.textContent = timeLabel;
      const title = fullTimestampTitle(timestamp);
      if (title) span.title = title;
      return span;
    };
    const copyBtn = copyable ? this._createCopyButton() : null;
    if (order === "user") {
      // Fixed user order (2026-08-21 design): Expand → Fork → Edit → Copy →
      // Timestamp. Expand/Collapse only exists for collapsed long prompts;
      // Fork/Edit always sit directly after it and never reorder. Fork/Edit
      // are session-tree actions gated to persisted sessions — ephemeral
      // chats have no tree to navigate, so the slots vanish there and Copy
      // shifts forward.
      if (expandToggle) actions.appendChild(expandToggle);
      if (this.sessionTreeActions) {
        actions.appendChild(this._createUserActionButton("fork", actionText));
        actions.appendChild(this._createUserActionButton("edit", actionText));
      }
      if (copyBtn) actions.appendChild(copyBtn);
      if (timeLabel) actions.appendChild(timeSpan());
    } else {
      if (copyBtn) actions.appendChild(copyBtn);
      if (timeLabel) actions.appendChild(timeSpan());
      const durationLabel = formatDurationLabel(durationMs);
      if (durationLabel) {
        const durationSpan = document.createElement("span");
        durationSpan.className = "message-duration";
        durationSpan.textContent = durationLabel;
        durationSpan.title = t("messages.responseTime");
        actions.appendChild(durationSpan);
      }
      if (usage?.cost && usage.cost.total > 0) {
        const usageSpan = document.createElement("span");
        usageSpan.className = "message-usage";
        usageSpan.textContent = `$${usage.cost.total.toFixed(4)}`;
        actions.appendChild(usageSpan);
      }
    }
    return actions;
  }

  /**
   * Create the expand/collapse toggle for a collapsible long user message,
   * or return null when the message is short enough to stay inline. The
   * caller places the toggle inside the .message-actions toolbar, before
   * the timestamp; click handling toggles `.expanded` on the content.
   */
  _createUserCollapseToggle(contentEl, rawContent) {
    if (!shouldCollapseUserMessage(rawContent)) return null;

    contentEl.classList.add("user-message-collapsible");
    const button = document.createElement("button");
    button.type = "button";
    button.className = "message-user-collapse-toggle";
    button.textContent = t("messages.expand");
    button.setAttribute("aria-expanded", "false");
    button.addEventListener("click", () => {
      const expanded = contentEl.classList.toggle("expanded");
      button.setAttribute("aria-expanded", String(expanded));
      button.textContent = expanded ? t("messages.collapse") : t("messages.expand");
    });
    return button;
  }

  /**
   * Fork / Edit action for user messages (Pi-native /fork and /tree-select
   * entry points). The buttons dispatch bubbling `messagefork` /
   * `messageedit` CustomEvents carrying the Pi entry id (resolved from the
   * nearest [data-entry-id] anchor at click time) and the original prompt
   * text. App.js owns the transport calls; the renderer stays presentational.
   */
  _createUserActionButton(kind, actionText) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `message-action message-${kind}-btn`;
    button.setAttribute(
      "aria-label",
      t(kind === "fork" ? "messages.forkSession" : "messages.editMessage"),
    );
    button.title = t(kind === "fork" ? "messages.forkSession" : "messages.editMessage");
    const icon = createIcon(kind === "fork" ? "git-branch" : "pencil", { size: 12 });
    if (icon) button.appendChild(icon);
    button.addEventListener("click", (event) => {
      event.stopPropagation();
      const entryId = button.closest("[data-entry-id]")?.dataset.entryId || null;
      button.dispatchEvent(
        new CustomEvent(kind === "fork" ? "messagefork" : "messageedit", {
          bubbles: true,
          detail: { entryId, text: actionText },
        }),
      );
    });
    return button;
  }

  _createCopyButton() {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "message-copy-btn";
    button.setAttribute("aria-label", t("messages.copyMessage"));
    const icon = createIcon("copy", { size: 12 });
    if (icon) button.appendChild(icon);
    return button;
  }

  _imageSource(image) {
    const data = typeof image?.data === "string" ? image.data : "";
    if (/^data:image\/(?:png|jpe?g|gif|webp);base64,/i.test(data)) return data;
    const mime = /^image\/(?:png|jpe?g|gif|webp)$/i.test(image?.mimeType || "")
      ? image.mimeType
      : "image/png";
    return `data:${mime};base64,${data}`;
  }

  _appendMarkup(parent, markup) {
    const parsed = new DOMParser().parseFromString(String(markup || ""), "text/html");
    sanitizeMarkup(parsed.body);
    parent.append(...Array.from(parsed.body.childNodes));
  }

  _replaceMarkup(parent, markup) {
    parent.replaceChildren();
    this._appendMarkup(parent, markup);
  }

  _setupCodeCopyButtons(root) {
    root.querySelectorAll(".copy-btn").forEach((button) => {
      if (button.dataset.bound === "true") return;
      button.addEventListener("click", () => {
        const code = button.closest(".code-block-wrapper")?.querySelector("code");
        if (!code) return;
        const copy = (text) => {
          if (navigator.clipboard) return navigator.clipboard.writeText(text);
          const textarea = document.createElement("textarea");
          textarea.value = text;
          textarea.style.cssText = "position:fixed;left:-9999px";
          document.body.appendChild(textarea);
          textarea.select();
          document.execCommand("copy");
          textarea.remove();
          return Promise.resolve();
        };
        copy(code.textContent || "").then(() => {
          button.textContent = t("messages.copied");
          button.classList.add("copied");
          setTimeout(() => {
            button.textContent = t("messages.copy");
            button.classList.remove("copied");
          }, 2000);
        });
      });
      button.dataset.bound = "true";
    });
  }

  highlightTextNode(node, pattern, onMatch) {
    const text = node.textContent || "";
    const regex = new RegExp(pattern.source, pattern.flags);
    let lastIndex = 0;
    let matchCount = 0;
    let match = regex.exec(text);
    if (!match) return 0;

    const fragment = document.createDocumentFragment();
    while (match) {
      const [matchedText] = match;
      const start = match.index;
      if (start > lastIndex) {
        fragment.appendChild(document.createTextNode(text.slice(lastIndex, start)));
      }

      const mark = document.createElement("mark");
      mark.dataset.searchHighlight = "true";
      mark.textContent = matchedText;
      fragment.appendChild(mark);
      if (typeof onMatch === "function") onMatch(mark);

      matchCount += 1;
      lastIndex = start + matchedText.length;
      match = regex.exec(text);
    }

    if (lastIndex < text.length) {
      fragment.appendChild(document.createTextNode(text.slice(lastIndex)));
    }

    node.replaceWith(fragment);
    return matchCount;
  }

  escapeRegExp(text) {
    return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  }

  escapeHtml(text) {
    return String(text ?? "").replace(/[&<>"']/g, (character) => {
      const entities = {
        "&": "&amp;",
        "<": "&lt;",
        ">": "&gt;",
        '"': "&quot;",
        "'": "&#39;",
      };
      return entities[character];
    });
  }

  scrollToBottom() {
    this.scrollOwner?.scrollToBottom();
  }

  // Tear down listeners and release the container reference. Idempotent:
  // hiding/deactivating a view must NOT destroy it; only an explicit destroy()
  // removes the scroll + locale listeners so a recreated view stays clean.
  destroy() {
    if (this._destroyed) return;
    this._destroyed = true;
    if (this._ownsScrollOwner && this.scrollOwner) {
      this.scrollOwner.destroy();
    }
    this.scrollOwner = null;
    if (typeof this.unsubscribeLocaleChange === "function") {
      this.unsubscribeLocaleChange();
      this.unsubscribeLocaleChange = null;
    }
    this.container = null;
  }
}
