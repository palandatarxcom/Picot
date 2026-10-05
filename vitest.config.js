// ABOUTME: Configures Picot's browser and build-script Vitest regression suites.
// ABOUTME: Keeps distribution-asset checks alongside frontend behavior tests.

import { closeSync, openSync } from "node:fs";
import { defineConfig } from "vitest/config";

if (!process.env.PICOT_TEST_SANDBOX_ROOT || !process.env.PICOT_TEST_WRITE_GUARD) {
  throw new Error(
    "Run tests through bun run test or bun run test:focused; direct Vitest is unsafe.",
  );
}
try {
  closeSync(openSync(process.env.PICOT_TEST_WRITE_GUARD, "a"));
  throw new Error("Test write guard is writable: refusing to run without kernel isolation.");
} catch (error) {
  if (error.code !== "EPERM" && error.code !== "EACCES") throw error;
}

export default defineConfig({
  test: {
    environment: "jsdom",
    setupFiles: ["./vitest.setup.js"],
    include: ["public/**/*.test.js", "extensions/**/*.test.ts", "scripts/**/*.test.js"],
    coverage: {
      provider: "istanbul",
      enabled: false,
      reporter: ["text", "html", "lcov"],
      reportsDirectory: "./coverage",
      include: ["public/**/*.js", "extensions/**/*.ts", "scripts/**/*.js"],
      exclude: [
        "**/*.test.{js,ts}",
        "extensions/dist/**",
        // Node CLI scripts have contract tests but do not execute under jsdom.
        "scripts/**",
        "public/**/*-vendor-entry.js",
        "public/vendor/**",
      ],
    },
  },
});
