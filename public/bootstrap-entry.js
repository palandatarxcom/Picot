// A bare dynamic import() has no error path: if module linking fails (e.g. a
// transient mismatch between cached and fresh files during an app update),
// the rejection goes unhandled and the app is left on a blank screen with
// nothing but a console error. Reload once to pick up a consistent set of
// files; a second failure means it's a real bug, so we stop retrying.
const RELOAD_GUARD_KEY = "picot:bootstrap-reload-attempted";

import { applyLastHostDefault } from "./app/host-connection.js";
import { readInjectedCapability } from "./app/host-origin.js";

// Route discriminator, decided BEFORE any app module loads: app.js constructs
// the chat object graph (MessageRenderer, ConfigGateway, terminal, …) at
// import time and cannot run on a route without a workspace session. Native
// `/` is the landing route and boots landing.js instead; every canonical
// workspace route and all non-native/browser routes keep app.js. The
// capability global is only read here, never consumed — the WebSocket client
// consumes it later for the authenticated hello.
const native = Boolean(readInjectedCapability());
const pathname = globalThis.location?.pathname || "";
const landingEntry = "./landing.js";
const appEntry = "./app.js";
const landing = native && (pathname === "/" || pathname === "");
// Contract 4 window default, and only here: the landing route is what a new
// window opens, while every canonical workspace route already names its host
// on the URL (a workspace route without one is a local workspace, never a
// default). A stale or unreadable last host leaves the window local.
if (landing) await applyLastHostDefault();
const entry = landing ? landingEntry : appEntry;

import(entry)
  .then(() => sessionStorage.removeItem(RELOAD_GUARD_KEY))
  .catch((error) => {
    console.error(`[bootstrap] failed to load ${entry}`, error);
    if (sessionStorage.getItem(RELOAD_GUARD_KEY)) return;
    sessionStorage.setItem(RELOAD_GUARD_KEY, "1");
    location.reload();
  });
