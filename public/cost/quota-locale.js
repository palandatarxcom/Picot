// ABOUTME: Locale bundle for the provider quota panel — one key set, four
// ABOUTME: languages, sourced from the shared i18n table (cost.quota.*).
// The bundle IS the contract: every key the panel consumes must be wired
// here. A key that exists only in the JSON files is invisible to the panel —
// quota-locale.test.js asserts exactly that against the real en.json.
import { t } from "../i18n.js";

export function quotaLocaleBundle() {
  return {
    sectionTitle: t("cost.quota.sectionTitle"),
    refresh: t("cost.quota.refresh"),
    refreshing: t("cost.quota.refreshing"),
    fiveHour: t("cost.quota.fiveHour"),
    weekly: t("cost.quota.weekly"),
    monthly: t("cost.quota.monthly"),
    remaining: t("cost.quota.remaining"),
    needsLogin: t("cost.quota.needsLogin"),
    unavailable: t("cost.quota.unavailable"),
    justNow: t("cost.quota.justNow"),
    minutesAgo: t("cost.quota.minutesAgo"),
    hoursAgo: t("cost.quota.hoursAgo"),
    resetCredits: t("cost.quota.resetCredits"),
    resetsInMinutes: t("cost.quota.resetsInMinutes"),
    resetsAt: t("cost.quota.resetsAt"),
    // Consumed by formatResetStamp's banded stamps (2026-09-27). They were
    // added to the locale files but not wired here, and the panel threw on
    // locale.resetsInHours.replace — killing render() mid-loop, which left
    // the cards on screen without the section head (no Refresh button).
    resetsInHours: t("cost.quota.resetsInHours"),
    resetsTomorrow: t("cost.quota.resetsTomorrow"),
    // The far reset band's {when} value: month/day + HH:MM built from the
    // locale's own template, never a hardcoded 月/日 literal.
    resetDateFmt: t("cost.quota.resetDateFmt"),
    resetDialogTitle: t("cost.quota.resetDialogTitle"),
    resetDialogScope: t("cost.quota.resetDialogScope"),
    dialogRedeem: t("cost.quota.dialogRedeem"),
    creditDateFmt: t("cost.quota.creditDateFmt"),
    creditIndexed: t("cost.quota.creditIndexed"),
    balance: t("cost.quota.balance"),
    resetCreditsAvailable: t("cost.quota.resetCreditsAvailable"),
    creditNext: t("cost.quota.creditNext"),
    creditDaysLeft: t("cost.quota.creditDaysLeft"),
    creditExpired: t("cost.quota.creditExpired"),
    creditNone: t("cost.quota.creditNone"),
    creditEarnHint: t("cost.quota.creditEarnHint"),
    fifoNote: t("cost.quota.fifoNote"),
    confirmResetDesc: t("cost.quota.confirmResetDesc"),
    confirmWhichCredit: t("cost.quota.confirmWhichCredit"),
    irreversible: t("cost.quota.irreversible"),
    dialogProceed: t("cost.quota.dialogProceed"),
    creditGranted: t("cost.quota.creditGranted"),
    creditExpires: t("cost.quota.creditExpires"),
    creditUnknown: t("cost.quota.creditUnknown"),
    dialogConfirm: t("cost.quota.dialogConfirm"),
    dialogCancel: t("cost.quota.dialogCancel"),
    toastUnavailable: t("cost.quota.toastUnavailable"),
    toastUnknown: t("cost.quota.toastUnknown"),
    toastInFlight: t("cost.quota.toastInFlight"),
    toastNeedsLogin: t("cost.quota.toastNeedsLogin"),
    toastResetDone: t("cost.quota.toastResetDone"),
    toastNothingToReset: t("cost.quota.toastNothingToReset"),
    toastNoCredit: t("cost.quota.toastNoCredit"),
  };
}
