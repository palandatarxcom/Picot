// ABOUTME: Minimal landing entry for adding a remote host from a pasted connection offer.
// ABOUTME: Runs the host_add / host_confirm_tofu IPC and renders the TOFU outcome inline.

/**
 * Landing "add remote host" entry (relay Phase 2.5 T8). The full picker with the
 * TOFU dialog ships in T9; landing only needs a working paste-and-add path.
 *
 * Secrets discipline: the offer carries the pairing token, so it is never
 * logged, never echoed back in the result line, and the input is cleared as
 * soon as it is handed to the app.
 */
export function setupLandingAddHost({
  document: doc = globalThis.document,
  invoke = tauriInvoke(),
  t = (key) => key,
} = {}) {
  const button = doc?.getElementById("landing-add-host-btn");
  const form = doc?.getElementById("landing-add-host-form");
  const input = doc?.getElementById("landing-add-host-url");
  const result = doc?.getElementById("landing-add-host-result");
  if (!button || !form || !input || !result) return { destroy: () => {} };
  // No app shell (plain browser tab): the IPC entry cannot work, so hide it
  // rather than offering a dead button.
  if (typeof invoke !== "function") {
    button.classList.add("hidden");
    return { destroy: () => {} };
  }

  let busy = false;

  const render = (text, actions = []) => {
    result.replaceChildren(doc.createTextNode(text));
    for (const action of actions) {
      const actionButton = doc.createElement("button");
      actionButton.type = "button";
      actionButton.className = `landing-add-host-action ${action.className}`;
      actionButton.textContent = action.label;
      actionButton.addEventListener("click", action.run);
      result.append(doc.createTextNode(" "), actionButton);
    }
    result.classList.remove("hidden");
  };

  const settle = (reply) => {
    if (reply?.status === "tofuPending") {
      render(t("landing.addHostPending", { fingerprint: reply.keyFingerprint ?? "" }), [
        {
          className: "landing-add-host-confirm",
          label: t("landing.addHostConfirm"),
          run: () => answer(reply.questionId, true),
        },
        {
          className: "landing-add-host-cancel",
          label: t("actions.cancel"),
          run: () => answer(reply.questionId, false),
        },
      ]);
      return;
    }
    if (reply?.status === "cancelled") {
      render(t("landing.addHostCancelled"));
      return;
    }
    render(t("landing.addHostAdded", { host: reply?.label || reply?.serverId || "" }));
  };

  const fail = (error) => {
    render(t("landing.addHostFailed", { error: String(error?.message ?? error) }));
  };

  const answer = (questionId, approved) => {
    if (busy) return;
    busy = true;
    Promise.resolve(invoke("host_confirm_tofu", { questionId, approved }))
      .then(settle, fail)
      .finally(() => {
        busy = false;
      });
  };

  const onSubmit = (event) => {
    event.preventDefault();
    const offerUrl = input.value.trim();
    if (!offerUrl || busy) return;
    busy = true;
    input.value = "";
    result.classList.add("hidden");
    Promise.resolve(invoke("host_add", { offerUrl }))
      .then(settle, fail)
      .finally(() => {
        busy = false;
      });
  };

  const onToggle = () => {
    form.classList.toggle("hidden");
    if (!form.classList.contains("hidden")) input.focus();
  };

  form.addEventListener("submit", onSubmit);
  button.addEventListener("click", onToggle);

  return {
    destroy: () => {
      form.removeEventListener("submit", onSubmit);
      button.removeEventListener("click", onToggle);
    },
  };
}

function tauriInvoke() {
  const core = globalThis.__TAURI__?.core;
  if (typeof core?.invoke !== "function") return null;
  return (command, args) => core.invoke(command, args);
}
