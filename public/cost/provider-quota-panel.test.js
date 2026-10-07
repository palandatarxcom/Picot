// ABOUTME: Provider quota panel tests (spec 2026-09-22) — render contract,
// ABOUTME: empty-state hiding, and the reset-credit double-channel flow.
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { createProviderQuotaPanel } from "./provider-quota-panel.js";

const locale = {
  sectionTitle: "Provider Quota",
  refresh: "Refresh",
  refreshing: "Refreshing…",
  fiveHour: "5-hour limit",
  weekly: "Weekly limit",
  monthly: "30-day limit",
  remaining: "剩余 {n}%",
  needsLogin: "Re-login required",
  unavailable: "Temporarily unavailable",
  justNow: "just now",
  minutesAgo: "{n}m ago",
  hoursAgo: "{n}h ago",
  resetsInHours: "resets in {n}h",
  resetsTomorrow: "resets tomorrow at {time}",
  resetDateFmt: "{m}月{d}日 {hh}:{mm}",
  resetsInDays: "resets in {n}d",
  resetCredits: "Reset quota ({n} left)",
  resetDialogTitle: "Reset quota",
  resetDialogBody: "This spends one reset credit and cannot be undone.",
  creditGranted: "获得 {time}",
  creditDateFmt: "{y}年{m}月{d}日 {hh}:{mm} {ap}",
  creditExpires: "{time} 过期",
  creditUnknown: "Expiry unknown",
  resetsInMinutes: "resets in {n}m",
  resetsAt: "resets {when}",
  resetDialogScope: "OpenAI Codex plan",
  dialogRedeem: "Use 1 credit",
  creditIndexed: "Credit #{n}",
  resetPrefix: "Resets",
  today: "today",
  balance: "Balance",
  resetCreditsAvailable: "You have {count} available reset credits.",
  creditNext: "Up next",
  creditDaysLeft: "（剩余 {days} 天）",
  creditExpired: "(expired)",
  creditNone: "No reset credits available",
  creditEarnHint: "Reset credits are granted by the plan.",
  fifoNote: "The earliest credit is used first.",
  confirmResetDesc: "This spends one of your {count} reset credits and cannot be undone.",
  confirmWhichCredit: "Will spend the credit granted {date}.",
  irreversible: "This cannot be undone.",
  dialogProceed: "Continue",
  dialogConfirm: "Reset now",
  dialogCancel: "Cancel",
  toastUnavailable: "Cannot open the reset operation right now",
  toastUnknown:
    "Result unknown: it may already have applied. Retrying is safe — the same request id is reused, so the server reports if it already applied.",
  toastInFlight: "An operation is already in progress",
  toastNeedsLogin: "Re-login required before resetting",
  toastResetDone: "Quota reset",
  toastNothingToReset: "Nothing to reset",
  toastNoCredit: "No reset credits left",
};

const DAY = 86_400;
const NOW_SEC = Math.floor(Date.now() / 1000);

let container;

/** Drives the two-step reset dialog to its confirm button. Every ledger op is a
 * stub in this suite, so no real reset credit can be spent. */
async function confirmResetDialog() {
  container.querySelector(".quota-reset-chip").click();
  await advance();
  // Single screen: the action button is the only step (screenshot layout).
  document.querySelector(".quota-dialog-action").click();
  await advance();
}

function advance() {
  return new Promise((resolve) => setTimeout(resolve, 20));
}

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
});

afterEach(() => {
  container.remove();
  // A dialog left open would leak into the next case's assertions.
  for (const overlay of document.querySelectorAll(".file-preview-dialog-overlay")) {
    overlay.remove();
  }
});

const SAMPLE_CREDITS = [{ grantedAt: 1_760_000_000, expiresAt: 1_790_000_000 }];

function makeSeams({ reports = [], consumeResult, openError, inspectData } = {}) {
  const gateway = {
    call: vi.fn(async (op) => {
      // The gateway resolves with the handler payload `{ ok, data }` — the
      // shape extensions/picot-config.ts returns and models-page.js reads.
      if (op === "provider_quota_report") return { ok: true, data: { reports } };
      if (op === "codex_reset_credits_inspect") {
        return { ok: true, data: inspectData ?? { credits: SAMPLE_CREDITS } };
      }
      if (op === "codex_reset_credits_consume") {
        return { ok: true, data: consumeResult ?? { code: "reset" } };
      }
      return {};
    }),
  };
  const dataTransport = {
    resetCreditOpen: vi.fn(async () => {
      if (openError) throw openError;
      return { operationId: "op-uuid-1" };
    }),
    resetCreditSettle: vi.fn(async () => ({})),
  };
  return {
    gateway,
    dataTransport,
    container: () => container,
  };
}

test("renders one card per report with window bars and hides when empty", async () => {
  const seams = makeSeams({
    reports: [
      {
        provider: "openai-codex",
        source: "openai-codex:wham",
        quota: {
          fiveHourPercent: 82,
          weeklyPercent: 40,
          monthlyPercent: 10,
          resetCredits: 2,
          planType: "pro",
          updatedAt: Date.now(),
        },
      },
      {
        provider: "deepseek",
        source: "deepseek:balance",
        quota: { customWindows: [{ label: "CNY 10.50", percent: 0 }], updatedAt: Date.now() },
      },
    ],
  });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  expect(container.classList.contains("hidden")).toBe(false);
  expect(container.querySelectorAll(".quota-card")).toHaveLength(2);
  const bars = container.querySelectorAll(".quota-row");
  expect(bars).toHaveLength(4); // 3 codex windows + 1 balance label
  expect(container.textContent).toContain("OpenAI Codex");
  const chip = container.querySelector(".quota-reset-chip");
  expect(chip?.textContent).toContain("2");
  expect(chip?.querySelector("svg")).not.toBeNull();
  expect(chip?.querySelector("svg")).not.toBeNull();
  expect(container.querySelector(".quota-plan-chip")?.textContent).toBe("pro");
});

test("hides the whole section when no provider reports", async () => {
  const seams = makeSeams({ reports: [] });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  expect(container.classList.contains("hidden")).toBe(true);
  expect(container.querySelectorAll(".quota-card")).toHaveLength(0);
});

test("needs_login renders its note and no reset button without credits", async () => {
  const seams = makeSeams({
    reports: [{ provider: "openai-codex", source: "openai-codex:wham", failure: "needs_login" }],
  });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  expect(container.textContent).toContain("Re-login required");
  expect(container.querySelector(".quota-reset-chip")).toBeNull();
});

test("reset click runs the open→consume→settle ledger flow", async () => {
  const seams = makeSeams({
    reports: [
      {
        provider: "openai-codex",
        source: "openai-codex:wham",
        quota: { fiveHourPercent: 90, resetCredits: 1, updatedAt: Date.now() },
      },
    ],
    consumeResult: { code: "reset" },
  });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  const toasts = [];
  window.addEventListener("picot-toast", (event) => toasts.push(event.detail.message));
  await confirmResetDialog();
  expect(seams.dataTransport.resetCreditOpen).toHaveBeenCalledTimes(1);
  expect(seams.gateway.call).toHaveBeenCalledWith("codex_reset_credits_consume", {
    operationId: "op-uuid-1",
  });
  expect(seams.dataTransport.resetCreditSettle).toHaveBeenCalledWith({
    operationId: "op-uuid-1",
    ambiguous: false,
  });
  expect(toasts).toContain("Quota reset");
});

test("an ambiguous consume settles ambiguous and toasts the unknown result", async () => {
  const seams = makeSeams({
    reports: [
      {
        provider: "openai-codex",
        source: "openai-codex:wham",
        quota: { fiveHourPercent: 90, resetCredits: 1, updatedAt: Date.now() },
      },
    ],
    consumeResult: { failure: "ambiguous" },
  });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  const toasts = [];
  window.addEventListener("picot-toast", (event) => toasts.push(event.detail.message));
  await confirmResetDialog();
  expect(seams.dataTransport.resetCreditSettle).toHaveBeenCalledWith({
    operationId: "op-uuid-1",
    ambiguous: true,
  });
  expect(toasts).toContain(
    "Result unknown: it may already have applied. Retrying is safe — the same request id is reused, so the server reports if it already applied.",
  );
});

test("a failed ledger open never reaches consume", async () => {
  const seams = makeSeams({
    reports: [
      {
        provider: "openai-codex",
        source: "openai-codex:wham",
        quota: { fiveHourPercent: 90, resetCredits: 1, updatedAt: Date.now() },
      },
    ],
    openError: new Error("ledger down"),
  });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  const toasts = [];
  window.addEventListener("picot-toast", (event) => toasts.push(event.detail.message));
  await confirmResetDialog();
  // The consume op never ran; the refresh-after-attempt report call is fine.
  const consumeCalls = seams.gateway.call.mock.calls.filter(
    (args) => args[0] === "codex_reset_credits_consume",
  );
  expect(consumeCalls).toHaveLength(0);
  expect(toasts).toContain("Cannot open the reset operation right now");
});

test("reset lists credits oldest-first and highlights the next one", async () => {
  const seams = makeSeams({
    reports: [
      {
        provider: "openai-codex",
        source: "openai-codex:wham",
        quota: { fiveHourPercent: 90, resetCredits: 2, updatedAt: Date.now() },
      },
    ],
    inspectData: {
      credits: [
        { grantedAt: NOW_SEC - 3 * DAY, expiresAt: NOW_SEC + 40 * DAY },
        { grantedAt: NOW_SEC - 30 * DAY, expiresAt: NOW_SEC + 5 * DAY },
      ],
    },
  });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  container.querySelector(".quota-reset-chip").click();
  await advance();

  expect(seams.gateway.call).toHaveBeenCalledWith("codex_reset_credits_inspect", {});
  expect(document.querySelector(".quota-dialog-count")?.textContent).toContain("2");
  const rows = [...document.querySelectorAll(".quota-credit-row")];
  expect(rows).toHaveLength(2);
  const oldestGranted = new Date((NOW_SEC - 30 * DAY) * 1000).toLocaleDateString();
  expect(rows[0].classList.contains("is-next")).toBe(true);
  expect(rows[0].textContent).toContain("Up next");
  expect(rows[0].querySelector(".quota-credit-chip")?.textContent).toBe("NEXT");
  expect(rows[0].textContent).toContain("获得");
  expect(rows[0].textContent).toContain(oldestGranted);
  expect(rows[0].textContent).toContain("过期");
  expect(rows[0].textContent).toContain("剩余");
  expect(rows[1].textContent).toContain("Credit #2");
  expect(document.querySelector(".quota-dialog-note")?.textContent).toContain("earliest");
  expect(seams.dataTransport.resetCreditOpen).not.toHaveBeenCalled();

  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await advance();
  expect(document.querySelector(".file-preview-dialog-overlay")).toBeNull();
  expect(seams.dataTransport.resetCreditOpen).not.toHaveBeenCalled();
});

test("credit expiry renders the locale template date, zh order", async () => {
  vi.useFakeTimers();
  const now = new Date(2026, 8, 27, 15, 0, 0); // 2026-09-27 15:00 local
  vi.setSystemTime(now);
  // Expires 2026-10-05 06:41 local → 8 days out, zh template shape.
  const expiresAt = new Date(2026, 9, 5, 6, 41).getTime();
  const seams = makeSeams({
    reports: [
      {
        provider: "openai-codex",
        source: "openai-codex:wham",
        quota: { fiveHourPercent: 10, resetCredits: 1, updatedAt: Date.now() },
      },
    ],
    inspectData: { credits: [{ grantedAt: 1_760_000_000_000, expiresAt }] },
  });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  container.querySelector(".quota-reset-chip").click();
  // Fake timers freeze advance()'s setTimeout; the dialog open path is pure
  // microtasks (async gateway mock + synchronous DOM build), so flush those.
  for (let i = 0; i < 5; i += 1) await new Promise((resolve) => queueMicrotask(resolve));
  const expires = [...document.querySelectorAll(".quota-credit-meta span")][1];
  expect(expires?.textContent).toBe("2026年10月5日 06:41 AM 过期（剩余 8 天）");
  vi.useRealTimers();
});

test("escaping the reset dialog never touches the ledger", async () => {
  const seams = makeSeams({
    reports: [
      {
        provider: "openai-codex",
        source: "openai-codex:wham",
        quota: { fiveHourPercent: 90, resetCredits: 1, updatedAt: Date.now() },
      },
    ],
  });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  container.querySelector(".quota-reset-chip").click();
  await advance();
  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await advance();
  expect(document.querySelector(".file-preview-dialog-overlay")).toBeNull();
  expect(seams.dataTransport.resetCreditOpen).not.toHaveBeenCalled();
  expect(seams.dataTransport.resetCreditSettle).not.toHaveBeenCalled();
});

test("inspect reporting needs_login shows no dialog and no ledger call", async () => {
  const seams = makeSeams({
    reports: [
      {
        provider: "openai-codex",
        source: "openai-codex:wham",
        quota: { fiveHourPercent: 90, resetCredits: 1, updatedAt: Date.now() },
      },
    ],
    inspectData: { failure: "needs_login", credits: [] },
  });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  const toasts = [];
  window.addEventListener("picot-toast", (event) => toasts.push(event.detail.message));
  container.querySelector(".quota-reset-chip").click();
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(document.querySelector(".file-preview-dialog-overlay")).toBeNull();
  expect(toasts).toContain("Re-login required before resetting");
  expect(seams.dataTransport.resetCreditOpen).not.toHaveBeenCalled();
});

test("an unconfigured provider is absent instead of shown as unavailable", async () => {
  const seams = makeSeams({
    reports: [
      { provider: "openai-codex", source: "openai-codex:wham", failure: "not_configured" },
      { provider: "deepseek", source: "deepseek:balance", failure: "not_configured" },
      {
        provider: "opencode-go",
        source: "opencode-go:usage",
        quota: { fiveHourPercent: 42, updatedAt: Date.now() },
      },
    ],
  });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  const names = [...container.querySelectorAll(".quota-card-name")].map((el) => el.textContent);
  expect(names).toEqual(["Opencode Go"]);
  expect(container.querySelectorAll(".quota-failure")).toHaveLength(0);
});

test("shows the head while the first probe is in flight, hides when settled empty", async () => {
  const seams = makeSeams({ reports: [] });
  let release = () => {};
  seams.gateway.call = vi.fn((op) =>
    op === "provider_quota_report"
      ? new Promise((resolve) => {
          release = () => resolve({ ok: true, data: { reports: [] } });
        })
      : Promise.resolve({ ok: true, data: { credits: [] } }),
  );
  const panel = createProviderQuotaPanel(seams, { locale });
  const pending = panel.loadReports();

  // In flight: the page must not look blank (this is what "配额 page is empty"
  // looked like — the first probe takes seconds).
  expect(container.querySelectorAll(".quota-card.is-skeleton").length).toBeGreaterThan(0);
  expect(container.querySelector(".quota-bar-fill.is-loading")).not.toBeNull();
  expect(container.classList.contains("hidden")).toBe(false);
  expect(container.querySelector(".quota-refresh-btn")?.textContent).toBe("Refreshing…");

  release();
  await pending;
  // Settled empty still hides the whole section (spec: no placeholder).
  expect(container.classList.contains("hidden")).toBe(true);
  expect(container.querySelector(".quota-section-head")).toBeNull();
});

test("refresh shows skeletons immediately and restores previous reports on failure", async () => {
  const reports = [
    {
      provider: "openai-codex",
      source: "openai-codex:wham",
      quota: { fiveHourPercent: 32, updatedAt: Date.now() },
    },
  ];
  const seams = makeSeams({ reports });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  expect(container.querySelector(".quota-card-name")?.textContent).toBe("OpenAI Codex");

  let rejectRefresh;
  seams.gateway.call.mockImplementationOnce(
    () =>
      new Promise((_, reject) => {
        rejectRefresh = reject;
      }),
  );
  const pending = panel.loadReports(true);
  expect(container.querySelectorAll(".quota-card.is-skeleton").length).toBeGreaterThan(0);
  expect(container.querySelectorAll(".quota-card:not(.is-skeleton)")).toHaveLength(0);

  rejectRefresh(new Error("refresh unavailable"));
  await pending;
  expect(container.querySelectorAll(".quota-card.is-skeleton")).toHaveLength(0);
  expect(container.querySelector(".quota-card-name")?.textContent).toBe("OpenAI Codex");
});

test("a load in flight spins the Refresh glyph, and stops once it settles", async () => {
  const reports = [
    {
      provider: "opencode-go",
      source: "opencode-go:usage",
      quota: { weeklyPercent: 12, updatedAt: Date.now() },
    },
  ];
  const seams = makeSeams({ reports });
  let release = () => {};
  seams.gateway.call = vi.fn((op) =>
    op === "provider_quota_report"
      ? new Promise((resolve) => {
          release = () => resolve({ ok: true, data: { reports } });
        })
      : Promise.resolve({ ok: true, data: { credits: [] } }),
  );
  const panel = createProviderQuotaPanel(seams, { locale });
  const pending = panel.loadReports(true);

  // One busy signal, not two: the skeleton bar slides and the button's own
  // glyph spins while the same probe is in flight.
  expect(container.querySelector(".quota-bar-fill.is-loading")).not.toBeNull();
  expect(container.querySelector(".quota-refresh-btn svg.is-spinning")).not.toBeNull();
  expect(container.getAttribute("aria-busy")).toBe("true");

  release();
  await pending;
  // The glyph stays (it is the button's icon) but the animation means
  // "data is in flight" and nothing else.
  const settled = container.querySelector(".quota-refresh-btn svg");
  expect(settled).not.toBeNull();
  expect(settled.classList.contains("is-spinning")).toBe(false);
  expect(container.getAttribute("aria-busy")).toBe("false");
});

test("a failed first load keeps the section visible with a failure note", async () => {
  const seams = makeSeams({ reports: [] });
  seams.gateway.call = vi.fn((op) =>
    op === "provider_quota_report"
      ? Promise.reject(new Error("runtime not ready"))
      : Promise.resolve({ ok: true, data: { credits: [] } }),
  );
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  // Hiding the section here was the dead white board: the Refresh button
  // vanished with it, so the page could never recover without a restart.
  expect(container.classList.contains("hidden")).toBe(false);
  expect(container.querySelector(".quota-failure")?.textContent).toBe("Temporarily unavailable");
  expect(container.querySelector(".quota-refresh-btn")).not.toBeNull();
  expect(container.querySelectorAll(".quota-card.is-skeleton")).toHaveLength(0);
});

test("a load started while another is in flight joins it instead of racing", async () => {
  const seams = makeSeams({
    reports: [
      {
        provider: "zai",
        source: "zai:quota-limit",
        quota: { weeklyPercent: 7, updatedAt: Date.now() },
      },
    ],
  });
  let release = () => {};
  seams.gateway.call = vi.fn((op) =>
    op === "provider_quota_report"
      ? new Promise((resolve) => {
          release = () =>
            resolve({
              ok: true,
              data: {
                reports: [
                  {
                    provider: "zai",
                    source: "zai:quota-limit",
                    quota: { weeklyPercent: 7, updatedAt: Date.now() },
                  },
                ],
              },
            });
        })
      : Promise.resolve({ ok: true, data: { credits: [] } }),
  );
  const panel = createProviderQuotaPanel(seams, { locale });
  const first = panel.loadReports();
  const second = panel.loadReports(true);
  expect(seams.gateway.call).toHaveBeenCalledTimes(1);

  release();
  await Promise.all([first, second]);
  expect(container.querySelectorAll(".quota-card.is-skeleton")).toHaveLength(0);
  expect(container.querySelector(".quota-card-name")?.textContent).toBe("Z.ai");
});

test("bar tone bands switch at 75% and 90%", async () => {
  const seams = makeSeams({
    reports: [
      {
        provider: "ollama-cloud",
        source: "ollama-cloud:usage",
        quota: {
          customWindows: [
            { label: "w74", percent: 74 },
            { label: "w75", percent: 75 },
            { label: "w90", percent: 90 },
            { label: "w91", percent: 91 },
          ],
          updatedAt: Date.now(),
        },
      },
    ],
  });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  const toneByLabel = {};
  const badgeByLabel = {};
  for (const row of container.querySelectorAll(".quota-row")) {
    const label = row.querySelector(".quota-row-label")?.textContent;
    toneByLabel[label] = row.querySelector(".quota-bar-fill")?.className.split(" ").pop();
    const badge = row.querySelector(".quota-pct-badge");
    badgeByLabel[label] = badge ? `${badge.textContent}/${badge.className.split(" ").pop()}` : null;
  }
  expect(toneByLabel).toEqual({
    w74: "is-ok",
    w75: "is-warning",
    w90: "is-warning",
    w91: "is-critical",
  });
  // The badge rides the bar's tone family and shows the remaining percent.
  expect(badgeByLabel).toEqual({
    w74: "剩余 26%/is-ok",
    w75: "剩余 25%/is-warning",
    w90: "剩余 10%/is-warning",
    w91: "剩余 9%/is-critical",
  });
});

test("reset stamps pick the minute/hours/tomorrow/absolute band", async () => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date(2026, 8, 27, 10, 0, 0)); // 2026-09-27 10:00 local
  const at = (h, m) => new Date(2026, 8, 27, h, m).getTime();
  const resetAt = {
    minutes: at(10, 40),
    hours: at(15, 0), // same day, 5h away
    tomorrow: at(23, 30) + 3 * 3600_000, // crosses midnight
    absolute: at(10, 0) + 3 * 86_400_000, // 3 days out
  };
  vi.useRealTimers();
  const seams = makeSeams({
    reports: [
      {
        provider: "ollama-cloud",
        source: "ollama-cloud:usage",
        quota: {
          customWindows: [
            { label: "m", percent: 1, resetAt: resetAt.minutes },
            { label: "h", percent: 2, resetAt: resetAt.hours },
            { label: "t", percent: 3, resetAt: resetAt.tomorrow },
            { label: "a", percent: 4, resetAt: resetAt.absolute },
          ],
          updatedAt: Date.now(),
        },
      },
    ],
  });
  vi.useFakeTimers();
  vi.setSystemTime(new Date(2026, 8, 27, 10, 0, 0));
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  const stamp = (label) =>
    [...container.querySelectorAll(".quota-row")]
      .find((r) => r.querySelector(".quota-row-label")?.textContent === label)
      ?.querySelector(".quota-row-reset")?.textContent;
  expect(stamp("m")).toBe("resets in 40m");
  expect(stamp("h")).toBe("resets in 5h");
  expect(stamp("t")).toBe("resets tomorrow at 02:30");
  expect(stamp("a")).toBe("resets 9月30日 10:00");
  vi.useRealTimers();
});

test("balance rows are a single line with the amount, no bar", async () => {
  const seams = makeSeams({
    reports: [
      {
        provider: "deepseek",
        source: "deepseek:balance",
        quota: { customWindows: [{ label: "CNY 10.50", percent: 0 }], updatedAt: Date.now() },
      },
    ],
  });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  const balance = container.querySelector(".quota-row.is-balance");
  expect(balance?.querySelector(".quota-bar")).toBeNull();
  expect(balance?.querySelector(".quota-row-top")?.textContent).toContain("CNY 10.50");
});

test("codex reset chip sits directly after the plan chip", async () => {
  const seams = makeSeams({
    reports: [
      {
        provider: "openai-codex",
        source: "openai-codex:wham",
        quota: { fiveHourPercent: 10, planType: "pro", resetCredits: 2, updatedAt: Date.now() },
      },
    ],
  });
  const panel = createProviderQuotaPanel(seams, { locale });
  await panel.loadReports();
  const left = container.querySelector(".quota-card-title-left");
  const children = [...left.children].map((el) => el.className);
  expect(children).toEqual(["quota-card-name", "quota-plan-chip", "quota-reset-chip"]);
});
