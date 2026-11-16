// ABOUTME: Client runtime platform sniffing for display labels and formatting.
// ABOUTME: Display-only — filesystem semantics must never branch on these helpers.

/**
 * Runtime platform probes for the WebView client. They mirror the boot-time
 * macOS detection in index.html (navigator.platform starts with "Mac") and
 * detect Windows hosts (WebView2 reports "Win32"; Windows browsers always
 * carry "Windows NT" in the user agent). Tests inject the explicit platform
 * at the API boundary instead of stubbing navigator.
 */
function navigatorValue(key) {
  if (typeof navigator === "undefined") return "";
  const value = navigator[key];
  return typeof value === "string" ? value : "";
}

export function isMacOSRuntime() {
  return (
    navigatorValue("platform").startsWith("Mac") || navigatorValue("userAgent").includes("Mac OS")
  );
}

export function isWindowsRuntime() {
  const platform = navigatorValue("platform");
  const ua = navigatorValue("userAgent");
  return platform.startsWith("Win") || ua.includes("Windows NT");
}
