// ABOUTME: Sidebar host button and its picker: local plus saved remote hosts,
// ABOUTME: status badges, add-host TOFU question, and §2.5 per-row removal.

import { LOCAL_HOST, resolveHostFromUrl, switchHost } from "./app/host-connection.js";
import { t as i18n, onLocaleChange } from "./i18n.js";
import { replaceButtonGlyph } from "./icons.js";

/**
 * Host picker (relay spec §3.1, Paseo `SidebarHostPicker` 同构). The window's
 * host is its URL (`host-connection.js` contract 4), so selecting a row is a
 * whole-window reload and this module owns no connection state of its own.
 *
 * Secrets discipline: a pasted offer carries the pairing token — it is cleared
 * as soon as it is handed to the app, never echoed into a row, a notice, or a
 * log line.
 */

/** Where the last explicit host choice is kept (contract 4 window default). */
export const LAST_HOST_KEY = "picot-last-host";

/** Badge states the app reports per host (`host_client` status names). */
const BADGES = ["connecting", "online", "offline", "credentialInvalid", "saveFailed"];

function errorMessage(error) {
  return String(error?.message || error || "unknown error");
}

function tauriInvoke(owner) {
  const host = owner || globalThis;
  const invoke = host.__TAURI__?.core?.invoke ?? globalThis.__TAURI__?.core?.invoke;
  if (typeof invoke !== "function") return null;
  return (command, args) => invoke(command, args);
}

/** The badge a host row shows: a host without a credential is invalid, and an
 *  unknown status never claims to be reachable. */
export function hostBadge(host) {
  if (host?.needsPairing === true) return "credentialInvalid";
  const status = host?.status;
  return BADGES.includes(status) ? status : "offline";
}

/**
 * Wire the sidebar host button and its picker.
 *
 * @param {{
 *   root?: Document,
 *   invoke?: ((command: string, args?: object) => Promise<any>) | null,
 *   t?: (key: string, params?: Record<string, string|number>) => string,
 *   env?: any,
 *   flush?: () => void,
 *   navigate?: (href: string) => void,
 * }} [options]
 */
export function setupHostSwitcher({
  root = document,
  invoke = tauriInvoke(),
  t = i18n,
  env = globalThis.window,
  flush,
  navigate,
} = {}) {
  const button = root?.getElementById("host-switcher-btn");
  const label = root?.getElementById("host-switcher-label");
  const picker = root?.getElementById("host-picker");
  const list = root?.getElementById("host-picker-list");
  const notice = root?.getElementById("host-picker-notice");
  const addBtn = root?.getElementById("host-add-btn");
  const addForm = root?.getElementById("host-add-form");
  const addUrl = root?.getElementById("host-add-url");
  const tofuBlock = root?.getElementById("host-tofu");
  const tofuTitle = root?.getElementById("host-tofu-title");
  const tofuFingerprint = root?.getElementById("host-tofu-fingerprint");
  const tofuChanges = root?.getElementById("host-tofu-changes");
  const tofuConfirm = root?.getElementById("btn-host-tofu-confirm");
  const tofuCancel = root?.getElementById("btn-host-tofu-cancel");
  const removeBlock = root?.getElementById("host-remove-confirm");
  const removeText = root?.getElementById("host-remove-confirm-text");
  const removeConfirm = root?.getElementById("btn-host-remove-confirm");
  const removeCancel = root?.getElementById("btn-host-remove-cancel");
  const missing = [
    button,
    label,
    picker,
    list,
    notice,
    addBtn,
    addForm,
    addUrl,
    tofuBlock,
    tofuTitle,
    tofuFingerprint,
    tofuChanges,
    tofuConfirm,
    tofuCancel,
    removeBlock,
    removeText,
    removeConfirm,
    removeCancel,
  ].some((node) => !node);
  if (missing) return { destroy: () => {}, open: async () => {}, refresh: async () => {} };
  // No app shell (plain browser tab): there is no host client to ask, so the
  // button is hidden rather than shown broken.
  if (typeof invoke !== "function") {
    button.classList.add("hidden");
    return { destroy: () => {}, open: async () => {}, refresh: async () => {} };
  }

  // The landing shell shares this markup but never wires the picker (spec §7):
  // the button ships hidden and only the app shell reveals it.
  button.classList.remove("hidden");
  replaceButtonGlyph(button, "server", { size: 16, document: root });
  const current = resolveHostFromUrl(env);
  /** Saved remote hosts, local excluded (`host_list` never contains it). */
  let hosts = [];
  /** The pending TOFU question, or null. The app keeps one question at a time. */
  let question = null;
  /** The host a removal is waiting on, or null. */
  let removing = null;
  let open = false;
  let busy = false;
  let noticeMsg = null;

  function setNotice(message) {
    noticeMsg = message || null;
    notice.textContent = noticeMsg ?? "";
    notice.classList.toggle("hidden", !noticeMsg);
  }

  const labelOf = (host) => host?.label || host?.serverId || "—";

  function renderButton() {
    const host = hosts.find((entry) => entry.serverId === current);
    label.textContent = current === LOCAL_HOST || !host ? t("hostPicker.local") : labelOf(host);
    button.dataset.host = current;
  }

  function renderNotice() {
    setNotice(noticeMsg);
  }

  function badge(status) {
    const span = root.createElement("span");
    span.className = "host-badge";
    span.dataset.status = status;
    span.textContent = t(`hostPicker.status.${status}`);
    return span;
  }

  function row({ hostId, text, status, removable }) {
    const wrapper = root.createElement("div");
    wrapper.className = "host-row";
    wrapper.dataset.hostId = hostId;
    if (hostId === current) wrapper.dataset.current = "true";
    const main = root.createElement("button");
    main.type = "button";
    main.className = "host-row-main";
    main.dataset.hostId = hostId;
    const name = root.createElement("span");
    name.className = "host-row-label";
    name.textContent = text;
    main.append(name, badge(status));
    if (hostId === current) main.setAttribute("aria-current", "true");
    wrapper.append(main);
    if (removable) {
      const remove = root.createElement("button");
      remove.type = "button";
      remove.className = "host-row-remove";
      remove.dataset.hostId = hostId;
      remove.textContent = t("hostPicker.remove");
      wrapper.append(remove);
    }
    return wrapper;
  }

  function renderList() {
    list.replaceChildren(
      row({
        hostId: LOCAL_HOST,
        text: t("hostPicker.local"),
        status: "online",
        removable: false,
      }),
      ...hosts.map((host) =>
        row({
          hostId: host.serverId,
          text: labelOf(host),
          status: hostBadge(host),
          removable: true,
        }),
      ),
    );
  }

  function renderRemove() {
    removeBlock.classList.toggle("hidden", removing === null);
    removeText.textContent = removing
      ? t("hostPicker.removeConfirm", { host: labelOf(removing) })
      : "";
    removeConfirm.disabled = busy;
    removeCancel.disabled = busy;
  }

  function renderTofu() {
    tofuBlock.classList.toggle("hidden", question === null);
    if (!question) {
      tofuTitle.textContent = "";
      tofuFingerprint.textContent = "";
      tofuChanges.replaceChildren();
      return;
    }
    const changed = question.kind === "changedConnection";
    tofuTitle.textContent = t(changed ? "hostPicker.tofuTitleChanged" : "hostPicker.tofuTitleNew");
    tofuFingerprint.textContent = t("hostPicker.tofuFingerprint", {
      fingerprint: question.keyFingerprint ?? "—",
    });
    const changes = Array.isArray(question.changes) ? question.changes : [];
    tofuChanges.replaceChildren(
      ...changes.map((change) => {
        const line = root.createElement("p");
        line.className = "settings-help host-tofu-change";
        line.textContent = t(`hostPicker.change.${change.field}`, {
          saved: String(change.saved ?? "—"),
          offered: String(change.offered ?? "—"),
        });
        return line;
      }),
    );
    tofuConfirm.disabled = busy;
    tofuCancel.disabled = busy;
  }

  function render() {
    renderButton();
    renderList();
    renderNotice();
    renderRemove();
    renderTofu();
  }

  async function refresh() {
    try {
      const fresh = await invoke("host_list");
      hosts = Array.isArray(fresh) ? fresh : [];
      render();
    } catch (error) {
      hosts = [];
      setNotice(t("hostPicker.readFailed", { reason: errorMessage(error) }));
      render();
    }
    return hosts;
  }

  function setOpen(next) {
    open = next;
    picker.classList.toggle("hidden", !open);
    button.setAttribute("aria-expanded", String(open));
    if (!open) {
      question = null;
      removing = null;
      addForm.classList.add("hidden");
      setNotice(null);
    }
    render();
  }

  /** One switch, one reload. A refused flush keeps the window where it is. */
  function select(hostId) {
    if (hostId === current) {
      setOpen(false);
      return;
    }
    const url = switchHost(hostId, {
      env,
      flush,
      release: () => invoke("host_release"),
      navigate,
    });
    if (url === null) {
      setNotice(t("hostPicker.switchBlocked"));
      render();
      return;
    }
    try {
      (root.defaultView ?? globalThis).localStorage?.setItem(LAST_HOST_KEY, hostId);
    } catch {
      // shortcut: a refused write only loses the next window's default host.
    }
    setOpen(false);
  }

  /** The add/confirm reply is the same document shape for both commands. */
  function settle(reply) {
    const status = reply?.status;
    if (status === "tofuPending") {
      // The app keeps one question: a new one already superseded the old.
      question = reply;
      removing = null;
      render();
      return;
    }
    question = null;
    if (status === "cancelled") {
      setNotice(t("hostPicker.cancelled"));
      render();
      return;
    }
    if (status === "saveFailed") {
      setNotice(t("hostPicker.saveFailed"));
      render();
      void refresh();
      return;
    }
    setNotice(t("hostPicker.added", { host: reply?.label || reply?.serverId || "—" }));
    render();
    void refresh();
  }

  async function run(job) {
    if (busy) return;
    busy = true;
    render();
    try {
      settle(await job());
    } catch (error) {
      setNotice(t("hostPicker.addFailed", { reason: errorMessage(error) }));
    } finally {
      busy = false;
      render();
    }
  }

  const onButtonClick = () => {
    if (open) {
      setOpen(false);
      return;
    }
    setOpen(true);
    void refresh();
  };

  const onAddClick = () => {
    addForm.classList.toggle("hidden");
    if (!addForm.classList.contains("hidden")) addUrl.focus();
  };

  const onSubmit = (event) => {
    event.preventDefault();
    const offerUrl = addUrl.value.trim();
    if (!offerUrl || busy) return;
    addUrl.value = "";
    addForm.classList.add("hidden");
    void run(() => invoke("host_add", { offerUrl }));
  };

  const onTofuAnswer = (approved) => {
    if (!question || busy) return;
    const questionId = question.questionId;
    void run(() => invoke("host_confirm_tofu", { questionId, approved }));
  };

  const onListClick = (event) => {
    const buttonEl = event.target?.closest?.("button");
    if (!buttonEl || busy) return;
    const { hostId } = buttonEl.dataset;
    if (buttonEl.classList.contains("host-row-remove")) {
      removing = hosts.find((host) => host.serverId === hostId) ?? null;
      addForm.classList.add("hidden");
      render();
      return;
    }
    if (hostId) select(hostId);
  };

  /** Delete the local profile. §2.5: the remote host still trusts this device,
   *  so the copy says its owner has to revoke it there. */
  async function removeHost(host) {
    if (busy) return;
    busy = true;
    render();
    try {
      await invoke("host_remove", { serverId: host.serverId });
      setNotice(t("hostPicker.removed", { host: labelOf(host) }));
    } catch (error) {
      setNotice(t("hostPicker.removeFailed", { reason: errorMessage(error) }));
    } finally {
      busy = false;
    }
    await refresh();
    // A window still pointed at the deleted host has to come home.
    if (host.serverId === current) select(LOCAL_HOST);
  }

  const onRemoveConfirm = () => {
    if (!removing || busy) return;
    const host = removing;
    removing = null;
    void removeHost(host);
  };

  const onRemoveCancel = () => {
    removing = null;
    render();
  };

  button.addEventListener("click", onButtonClick);
  addBtn.addEventListener("click", onAddClick);
  addForm.addEventListener("submit", onSubmit);
  tofuConfirm.addEventListener("click", () => onTofuAnswer(true));
  tofuCancel.addEventListener("click", () => onTofuAnswer(false));
  removeConfirm.addEventListener("click", onRemoveConfirm);
  removeCancel.addEventListener("click", onRemoveCancel);
  list.addEventListener("click", onListClick);

  const stopLocale = onLocaleChange(() => {
    render();
  });

  render();

  return {
    /** Open the picker and read the host list. */
    async open() {
      setOpen(true);
      await refresh();
    },
    refresh,
    /** The picker's own listeners and its one locale subscription. */
    destroy() {
      stopLocale?.();
      button.removeEventListener("click", onButtonClick);
      addBtn.removeEventListener("click", onAddClick);
      addForm.removeEventListener("submit", onSubmit);
      removeConfirm.removeEventListener("click", onRemoveConfirm);
      removeCancel.removeEventListener("click", onRemoveCancel);
      list.removeEventListener("click", onListClick);
    },
  };
}
