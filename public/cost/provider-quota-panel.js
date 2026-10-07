import { createIcon, replaceButtonGlyph } from "../icons.js";

// ABOUTME: Settings → Usage "Provider Quota" section (spec 2026-09-22).
// ABOUTME: Renders normalized quota reports; owns the codex reset-credit flow.

const DISPLAY_NAMES = {
  "openai-codex": "OpenAI Codex",
  zai: "Z.ai",
  "zai-coding-cn": "Z.ai (智谱)",
  "opencode-go": "Opencode Go",
  deepseek: "DeepSeek",
  minimax: "MiniMax",
  "minimax-cn": "MiniMax (国内)",
  moonshotai: "Moonshot",
  "moonshotai-cn": "Moonshot (国内)",
  "ollama-cloud": "Ollama Cloud",
};

function providerDisplayName(providerId) {
  return DISPLAY_NAMES[providerId] ?? providerId;
}

function formatRelativeTime(timestamp, locale) {
  if (typeof timestamp !== "number") return "";
  const deltaMs = Date.now() - timestamp;
  const minutes = Math.round(deltaMs / 60000);
  if (minutes < 1) return locale.justNow;
  if (minutes < 60) return locale.minutesAgo.replace("{n}", String(minutes));
  const hours = Math.round(minutes / 60);
  return locale.hoursAgo.replace("{n}", String(hours));
}

/** Credit timestamps arrive as ISO strings or epoch numbers; both must render
 * as one absolute local time (spec: the confirm dialog shows expires_at
 * absolutely, since it is the last check before an irreversible charge). */
function toEpochMs(value) {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value > 1e11 ? value : value * 1000;
  }
  if (typeof value === "string") {
    const parsed = Date.parse(value);
    if (!Number.isNaN(parsed)) return parsed;
  }
  return null;
}

function formatDateOnly(value) {
  const ms = toEpochMs(value);
  return ms === null ? "" : new Date(ms).toLocaleDateString();
}

/** Builds the credit datetime from the locale's own template
 * (creditDateFmt), so zh reads "2026年10月5日 06:41 AM" instead of the
 * WebView-default "October 5, 2026 at 06:41 AM". Placeholder sets are
 * identical across locales (locale-parity test enforces it). */
function formatDateTime(value, locale) {
  const ms = toEpochMs(value);
  if (ms === null) return "";
  const at = new Date(ms);
  const hours = at.getHours();
  const fields = {
    y: at.getFullYear(),
    m: at.getMonth() + 1,
    d: at.getDate(),
    hh: String(hours % 12 || 12).padStart(2, "0"),
    mm: String(at.getMinutes()).padStart(2, "0"),
    ap: hours < 12 ? "AM" : "PM",
  };
  return (locale.creditDateFmt ?? "{y}-{m}-{d} {hh}:{mm}").replace(
    /\{(y|m|d|hh|mm|ap)\}/g,
    (_, field) => String(fields[field] ?? ""),
  );
}

/** Upstream spends the earliest-granted credit first (FIFO), so the list is
 * ordered the way it will be consumed and the first row is the next one. */
function sortCreditsFifo(credits) {
  return [...credits].sort(
    (left, right) => (toEpochMs(left?.grantedAt) ?? 0) - (toEpochMs(right?.grantedAt) ?? 0),
  );
}

function daysUntil(value) {
  const ms = toEpochMs(value);
  if (ms === null) return null;
  return Math.ceil((ms - Date.now()) / 86_400_000);
}

/** Reset stamp bands (2026-09-27 two-line redesign): ≤60min counts minutes,
 * <24h counts hours on the same day or reads "明天 HH:MM 重置" across midnight,
 * farther out is the absolute "M月D日 HH:MM 重置". */
function formatResetStamp(resetAt, locale) {
  const ms = toEpochMs(resetAt);
  if (ms === null) return "";
  const minutes = Math.round((ms - Date.now()) / 60_000);
  const hhmm = (at) =>
    `${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;
  if (minutes <= 60) {
    return locale.resetsInMinutes.replace("{n}", String(Math.max(1, minutes)));
  }
  const at = new Date(ms);
  if (minutes < 1440) {
    if (at.toDateString() === new Date().toDateString()) {
      return locale.resetsInHours.replace("{n}", String(Math.round(minutes / 60)));
    }
    return locale.resetsTomorrow.replace("{time}", hhmm(at));
  }
  const when = (locale.resetDateFmt ?? "{m}/{d} {hh}:{mm}").replace(
    /\{(m|d|hh|mm)\}/g,
    (_, field) =>
      String(
        { m: at.getMonth() + 1, d: at.getDate(), hh: hhmm(at).slice(0, 2), mm: hhmm(at).slice(3) }[
          field
        ] ?? "",
      ),
  );
  return locale.resetsAt.replace("{when}", when);
}

/** A custom window whose label is a currency amount is a balance, not a
 * percentage window (deepseek / moonshot shape). */
function isBalanceLabel(label) {
  return /[$¥€]|CNY|USD|EUR|RMB/i.test(String(label ?? ""));
}

/** Mirror of the 2026-09-27 usage bands (≥75% used warning, >90% critical):
 * ≤25% left is warning, under 10% is critical. */
function toneForRemaining(remaining) {
  if (remaining < 10) return "is-critical";
  if (remaining <= 25) return "is-warning";
  return "is-ok";
}

/** Percent rows are two lines (2026-09-27): line 1 = label + tone-tinted
 * remaining badge (left) and the reset stamp (right); line 2 = full-width bar
 * that empties as the quota is consumed — full width makes every bar start at
 * the card's left edge by construction. Probes report used percent; the badge
 * and bar display its complement, 剩余 {n}%. */
function windowRow({ label, percent, resetAt }, locale) {
  const used = Math.max(0, Math.min(100, Math.round(percent)));
  const remaining = 100 - used;
  const tone = toneForRemaining(remaining);
  const row = document.createElement("div");
  row.className = "quota-row";
  const top = document.createElement("div");
  top.className = "quota-row-top";
  const name = document.createElement("span");
  name.className = "quota-row-label";
  name.textContent = label;
  const badge = document.createElement("span");
  badge.className = `quota-pct-badge ${tone}`;
  badge.textContent = locale.remaining.replace("{n}", String(remaining));
  const stamp = document.createElement("span");
  stamp.className = "quota-row-reset";
  stamp.textContent = formatResetStamp(resetAt, locale);
  top.append(name, badge, stamp);
  const bar = document.createElement("div");
  bar.className = "quota-bar";
  bar.setAttribute("role", "presentation");
  const fill = document.createElement("div");
  fill.className = `quota-bar-fill ${tone}`;
  fill.style.width = `${remaining}%`;
  bar.append(fill);
  row.append(top, bar);
  return row;
}

/** Balance providers (deepseek / moonshot) have no percentage: the amount takes
 * the percent slot and the bar stays empty, as opencodex does. */
/** Balance rows have no percent or bar: one line, label left and the amount
 * (the custom window's own label, e.g. "CNY 10.50") right. */
function balanceRow(window, locale) {
  const row = document.createElement("div");
  row.className = "quota-row is-balance";
  const top = document.createElement("div");
  top.className = "quota-row-top";
  const name = document.createElement("span");
  name.className = "quota-row-label";
  name.textContent = locale.balance;
  const value = document.createElement("span");
  value.className = "quota-row-pct is-amount";
  value.textContent = String(window?.label ?? "");
  top.append(name, value);
  row.append(top);
  return row;
}

/**
 * The quota section renderer. `seams.gateway` is the ConfigGateway
 * (provider_quota_report / codex ops); `seams.dataTransport` carries the
 * host-side reset_credit_open / reset_credit_settle ledger ops.
 */
export function createProviderQuotaPanel(seams, { locale }) {
  let reportsById = new Map();
  let loading = false;
  let hasLoaded = false;
  let loadError = false;
  let inFlight = null;

  async function runLoad(force = false) {
    loading = true;
    const previousReports = reportsById;
    reportsById = new Map();
    render();
    try {
      // The gateway resolves with the handler payload `{ ok, data }`, not the
      // handler's own data object — the reports live one level down.
      const payload = await seams.gateway.call("provider_quota_report", { force });
      reportsById = new Map();
      for (const report of payload?.data?.reports ?? []) {
        reportsById.set(report.provider, report);
      }
      hasLoaded = true;
      loadError = false;
    } catch {
      reportsById = previousReports;
      // loadError only shapes the empty-state view; with restored cards the
      // cards stay on screen and the flag waits for the next render.
      loadError = true;
    } finally {
      hasLoaded = true;
      loading = false;
      render();
    }
  }

  function loadReports(force = false) {
    // Serializes loads: a second call while one is in flight (settings entry
    // during the boot-time load, or a refresh click) joins the first instead
    // of snapshotting an empty map as "previous reports" — the poison case
    // that could blank the section on a later failure.
    if (inFlight) return inFlight;
    inFlight = runLoad(force).finally(() => {
      inFlight = null;
    });
    return inFlight;
  }

  async function consumeResetCredit() {
    // Double-channel flow (spec): Rust ledger opens the idempotency-keyed
    // operation, pi consumes it, the ledger settles it.
    let operationId = null;
    try {
      const opened = await seams.dataTransport?.resetCreditOpen();
      operationId = opened?.operationId ?? null;
    } catch {
      return { toast: locale.toastUnavailable };
    }
    if (!operationId) return { toast: locale.toastUnavailable };
    let result;
    try {
      result = await seams.gateway.call("codex_reset_credits_consume", { operationId });
    } catch {
      await seams.dataTransport?.resetCreditSettle({ operationId, ambiguous: true });
      return { toast: locale.toastUnknown };
    }
    const payload = result?.data ?? {};
    const failure = payload.failure;
    if (failure === "ambiguous") {
      await seams.dataTransport?.resetCreditSettle({ operationId, ambiguous: true });
      return { toast: locale.toastUnknown };
    }
    if (failure === "operation_in_flight") return { toast: locale.toastInFlight };
    if (failure === "needs_login") return { toast: locale.toastNeedsLogin };
    await seams.dataTransport?.resetCreditSettle({ operationId, ambiguous: false });
    if (payload.code === "reset") return { toast: locale.toastResetDone };
    if (payload.code === "already_redeemed") return { toast: locale.toastResetDone };
    if (payload.code === "nothing_to_reset") return { toast: locale.toastNothingToReset };
    if (payload.code === "no_credit") return { toast: locale.toastNoCredit };
    return { toast: locale.toastUnknown };
  }

  /** Inspect the reset credits, then ask. Returns {confirmed} or {toast}. */
  async function decideReset() {
    let payload = null;
    try {
      payload = await seams.gateway.call("codex_reset_credits_inspect", {});
    } catch {
      return { confirmed: false, toast: locale.toastUnavailable };
    }
    const data = payload?.data ?? {};
    if (data.failure === "needs_login") return { confirmed: false, toast: locale.toastNeedsLogin };
    const credits = Array.isArray(data.credits) ? data.credits : [];
    return { confirmed: await confirmReset(credits) };
  }

  /** Reset dialog, copied from opencodex's codex reset modal (screenshot):
   * ticket-icon title, "you have N credits", one sub-card per credit with the
   * next one highlighted, a FIFO note, and a single full-width action. */
  function confirmReset(credits) {
    return new Promise((resolve) => {
      const overlay = document.createElement("div");
      overlay.className = "file-preview-dialog-overlay";
      const dialog = document.createElement("div");
      dialog.className = "file-preview-dialog quota-reset-dialog";
      dialog.setAttribute("role", "dialog");
      dialog.setAttribute("aria-modal", "true");
      dialog.setAttribute("aria-label", locale.resetDialogTitle);

      const heading = document.createElement("h3");
      heading.className = "quota-dialog-title";
      const ticket = createIcon("ticket", { size: 18 });
      if (ticket) heading.append(ticket);
      heading.append(document.createTextNode(locale.resetDialogTitle));
      const sub = document.createElement("p");
      sub.className = "quota-dialog-sub";
      sub.textContent = locale.resetDialogScope;

      const ordered = sortCreditsFifo(credits);
      const count = document.createElement("p");
      count.className = "quota-dialog-count";
      const [lead, tail] = locale.resetCreditsAvailable.split("{count}");
      const strong = document.createElement("b");
      strong.textContent = String(ordered.length);
      count.append(
        document.createTextNode(lead ?? ""),
        strong,
        document.createTextNode(tail ?? ""),
      );

      const list = document.createElement("div");
      list.className = "quota-credits";
      if (ordered.length === 0) {
        const empty = document.createElement("div");
        empty.className = "quota-credit-row";
        empty.textContent = locale.creditNone;
        list.append(empty);
      }
      ordered.forEach((credit, index) => {
        const row = document.createElement("div");
        row.className = "quota-credit-row";
        if (index === 0) row.classList.add("is-next");
        const head = document.createElement("div");
        head.className = "quota-credit-head";
        const mark = createIcon("ticket", { size: 14 });
        if (mark) head.append(mark);
        head.append(
          document.createTextNode(
            index === 0
              ? locale.creditNext
              : locale.creditIndexed.replace("{n}", String(index + 1)),
          ),
        );
        if (index === 0) {
          const chip = document.createElement("span");
          chip.className = "quota-credit-chip";
          chip.textContent = "NEXT";
          head.append(chip);
        }
        const meta = document.createElement("div");
        meta.className = "quota-credit-meta";
        const granted = document.createElement("span");
        granted.textContent = locale.creditGranted.replace(
          "{time}",
          formatDateOnly(credit?.grantedAt),
        );
        const expires = document.createElement("span");
        const days = daysUntil(credit?.expiresAt);
        const expiresAt = formatDateTime(credit?.expiresAt, locale);
        if (days === null) {
          expires.textContent = locale.creditUnknown;
        } else if (days <= 0) {
          expires.textContent = `${locale.creditExpires.replace("{time}", expiresAt)} ${locale.creditExpired}`;
        } else {
          expires.textContent = `${locale.creditExpires.replace("{time}", expiresAt)}${locale.creditDaysLeft.replace("{days}", String(days))}`;
        }
        meta.append(granted, expires);
        row.append(head, meta);
        list.append(row);
      });

      const note = document.createElement("p");
      note.className = "quota-dialog-note";
      note.textContent = locale.fifoNote;
      const action = document.createElement("button");
      action.type = "button";
      action.className = "quota-dialog-action";
      action.textContent = locale.dialogRedeem;
      action.disabled = ordered.length === 0;
      action.addEventListener("click", () => finish(true));

      const cancel = document.createElement("button");
      cancel.type = "button";
      cancel.className = "quota-dialog-cancel";
      cancel.textContent = locale.dialogCancel;
      cancel.addEventListener("click", () => finish(false));
      const actions = document.createElement("div");
      actions.className = "quota-dialog-actions";
      actions.append(cancel, action);
      dialog.append(heading, sub, count, list, note, actions);
      overlay.append(dialog);
      document.body.append(overlay);

      let settled = false;
      const finish = (value) => {
        if (settled) return;
        settled = true;
        document.removeEventListener("keydown", onKeyDown, true);
        overlay.remove();
        resolve(value);
      };
      // Capture phase + stopPropagation: the Settings overlay closes on Escape
      // too, so an unguarded key would dismiss the dialog and the whole page.
      const onKeyDown = (event) => {
        if (event.key !== "Escape") return;
        event.preventDefault();
        event.stopPropagation();
        finish(false);
      };
      document.addEventListener("keydown", onKeyDown, true);
      action.focus();
    });
  }

  /** Codex reset control: a rounded chip "ticket icon + N" in the card title,
   * exactly as opencodex renders it. Hidden when no credits are known. */
  function renderResetChip(codexReport) {
    const credits = codexReport?.quota?.resetCredits;
    // Quota without a credit count says nothing about credits — no chip.
    if (typeof credits !== "number") return null;
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "quota-reset-chip";
    chip.setAttribute("aria-label", locale.resetCredits.replace("{n}", String(credits)));
    const icon = createIcon("ticket", { size: 12 });
    if (icon) chip.append(icon);
    chip.append(document.createTextNode(String(credits)));
    chip.addEventListener("click", async () => {
      chip.disabled = true;
      try {
        const decision = await decideReset();
        if (decision.toast) {
          window.dispatchEvent(
            new CustomEvent("picot-toast", { detail: { message: decision.toast } }),
          );
          return;
        }
        if (!decision.confirmed) return;
        const outcome = await consumeResetCredit();
        window.dispatchEvent(
          new CustomEvent("picot-toast", { detail: { message: outcome.toast } }),
        );
        void loadReports(true);
      } finally {
        chip.disabled = false;
      }
    });
    return chip;
  }

  function buildHead(isLoading) {
    const head = document.createElement("div");
    head.className = "quota-section-head";
    const title = document.createElement("h3");
    title.textContent = locale.sectionTitle;
    const refresh = document.createElement("button");
    refresh.type = "button";
    refresh.className = "quota-refresh-btn";
    refresh.textContent = isLoading ? locale.refreshing : locale.refresh;
    refresh.disabled = isLoading;
    // The glyph is decoration on a text button; it spins while the probe is in
    // flight so the button and the skeleton bars carry one busy signal.
    replaceButtonGlyph(refresh, "refresh-cw", { size: 14 });
    if (isLoading) refresh.querySelector("svg")?.classList.add("is-spinning");
    refresh.addEventListener("click", () => void loadReports(true));
    head.append(title, refresh);
    return head;
  }

  function renderSkeletonCard(provider) {
    const card = document.createElement("div");
    card.className = "quota-card is-skeleton";
    card.setAttribute("aria-hidden", "true");
    const name = document.createElement("div");
    name.className = "quota-card-name";
    name.textContent = providerDisplayName(provider);
    const rows = document.createElement("div");
    rows.className = "quota-rows";
    for (let index = 0; index < 2; index += 1) {
      // Two-line shape matching the real rows: an empty top line and a
      // shimmering full-width bar below it.
      const row = document.createElement("div");
      row.className = "quota-row";
      const top = document.createElement("div");
      top.className = "quota-row-top";
      const label = document.createElement("span");
      label.className = "quota-row-label quota-skeleton-label";
      top.append(label);
      const bar = document.createElement("div");
      bar.className = "quota-bar";
      const fill = document.createElement("div");
      fill.className = "quota-bar-fill is-loading";
      bar.append(fill);
      row.append(top, bar);
      rows.append(row);
    }
    card.append(name, rows);
    return card;
  }

  function render() {
    const container = seams.container();
    if (!container) return;
    // `not_configured` = no credential resolved, so the provider is absent
    // rather than shown as unavailable (spec: 未配置的 provider 不显示).
    const reports = [...reportsById.values()].filter(
      (report) => report.failure !== "not_configured",
    );
    const hasContent = reports.length > 0;
    // A successful probe that found no configured providers hides the whole
    // section (spec: no placeholder). A FAILED request must not hide it:
    // the head (with its Refresh) and a failure note stay on screen — hiding
    // them is how the page became a dead white board that could not recover.
    const hideSection = !hasContent && !loading && hasLoaded && !loadError;
    container.classList.toggle("hidden", hideSection);
    // The motion says "in flight" visually; aria-busy says it to assistive
    // tech, which matters because the skeleton bars deliberately keep
    // animating under prefers-reduced-motion.
    container.setAttribute("aria-busy", loading ? "true" : "false");
    if (hideSection) {
      container.replaceChildren();
      return;
    }
    container.replaceChildren();
    const head = buildHead(loading);

    // Data on its way (or the first load not even started yet): skeletons —
    // the section must never render blank while reports are pending.
    if (loading || !hasLoaded) {
      container.append(head, ...Object.keys(DISPLAY_NAMES).map(renderSkeletonCard));
      return;
    }

    if (!hasContent) {
      const note = document.createElement("div");
      note.className = "quota-failure";
      note.textContent = locale.unavailable;
      container.append(head, note);
      return;
    }

    for (const report of reports) {
      const card = document.createElement("div");
      card.className = "quota-card";
      const head = document.createElement("div");
      head.className = "quota-card-title";
      const name = document.createElement("span");
      name.className = "quota-card-name";
      name.textContent = providerDisplayName(report.provider);
      // Codex carries an account plan label (free / plus / pro / …) — a badge
      // next to the name, like opencodex's green badge on the account card.
      const titleLeft = document.createElement("span");
      titleLeft.className = "quota-card-title-left";
      titleLeft.append(name);
      // Codex title chips: plan badge first, then the reset-credit chip right
      // after it (both left-aligned with the name, spec 2026-09-27).
      const planType = report.quota?.planType;
      if (typeof planType === "string" && planType.trim()) {
        const planChip = document.createElement("span");
        planChip.className = "quota-plan-chip";
        planChip.textContent = planType;
        titleLeft.append(planChip);
      }
      if (report.provider === "openai-codex") {
        const chip = renderResetChip(report);
        if (chip) titleLeft.append(chip);
      }
      head.append(titleLeft);
      card.append(head);
      if (report.failure === "needs_login") {
        const note = document.createElement("div");
        note.className = "quota-failure";
        note.textContent = locale.needsLogin;
        card.append(note);
      } else if (report.failure) {
        const note = document.createElement("div");
        note.className = "quota-failure";
        note.textContent = locale.unavailable;
        card.append(note);
      }
      const quota = report.quota;
      if (quota) {
        const rows = [];
        if (typeof quota.fiveHourPercent === "number") {
          rows.push(
            windowRow(
              {
                label: locale.fiveHour,
                percent: quota.fiveHourPercent,
                resetAt: quota.fiveHourResetAt,
              },
              locale,
            ),
          );
        }
        if (typeof quota.weeklyPercent === "number") {
          rows.push(
            windowRow(
              { label: locale.weekly, percent: quota.weeklyPercent, resetAt: quota.weeklyResetAt },
              locale,
            ),
          );
        }
        if (typeof quota.monthlyPercent === "number") {
          rows.push(
            windowRow(
              {
                label: locale.monthly,
                percent: quota.monthlyPercent,
                resetAt: quota.monthlyResetAt,
              },
              locale,
            ),
          );
        }
        for (const custom of quota.customWindows ?? []) {
          rows.push(
            isBalanceLabel(custom?.label) ? balanceRow(custom, locale) : windowRow(custom, locale),
          );
        }
        const list = document.createElement("div");
        list.className = "quota-rows";
        list.append(...rows);
        card.append(list);
        const updated = document.createElement("div");
        updated.className = "quota-updated";
        updated.textContent = formatRelativeTime(quota.updatedAt, locale);
        card.append(updated);
      }

      container.append(card);
    }
    container.prepend(head);
  }

  return {
    render,
    loadReports,
  };
}
