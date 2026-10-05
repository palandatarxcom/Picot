import { readdirSync, readFileSync } from "node:fs";
import { join, relative, resolve } from "node:path";

import { describe, expect, it } from "vitest";

const publicDir = resolve(import.meta.dirname);
const en = JSON.parse(readFileSync(resolve(publicDir, "locales/en.json"), "utf-8"));
const zh = JSON.parse(readFileSync(resolve(publicDir, "locales/zh.json"), "utf-8"));
const ja = JSON.parse(readFileSync(resolve(publicDir, "locales/ja.json"), "utf-8"));
const es = JSON.parse(readFileSync(resolve(publicDir, "locales/es.json"), "utf-8"));

// ── Flatten helpers ───────────────────────────────────────────────────

function flattenKeys(obj, prefix = "") {
  const keys = [];
  for (const [k, v] of Object.entries(obj)) {
    const path = prefix ? `${prefix}.${k}` : k;
    if (v !== null && typeof v === "object" && !Array.isArray(v)) {
      keys.push(...flattenKeys(v, path));
    } else {
      keys.push(path);
    }
  }
  return keys;
}

function lookupValue(obj, dottedKey) {
  const parts = dottedKey.split(".");
  let current = obj;
  for (const part of parts) {
    if (current == null || typeof current !== "object") return undefined;
    current = current[part];
  }
  return typeof current === "string" ? current : undefined;
}

function extractPlaceholders(str) {
  const set = new Set();
  const re = /\{(\w+)\}/g;
  let m;
  while ((m = re.exec(str)) !== null) set.add(m[1]);
  return set;
}

const enKeys = new Set(flattenKeys(en));
const zhKeys = new Set(flattenKeys(zh));
const jaKeys = new Set(flattenKeys(ja));
const esKeys = new Set(flattenKeys(es));

// ── Key parity ────────────────────────────────────────────────────────

describe("locale key parity", () => {
  it("zh contains every en key", () => {
    const missing = [...enKeys].filter((k) => !zhKeys.has(k));
    expect(missing, `zh.json missing keys: ${missing.join(", ")}`).toEqual([]);
  });

  it("en contains every zh key (no extra zh keys)", () => {
    const extra = [...zhKeys].filter((k) => !enKeys.has(k));
    expect(extra, `zh.json has extra keys not in en.json: ${extra.join(", ")}`).toEqual([]);
  });

  // ja/es used to be checked for value shape only, so a key added to en.json
  // and zh.json but forgotten in ja/es shipped as untranslated English.
  for (const [name, keys] of [
    ["ja", jaKeys],
    ["es", esKeys],
  ]) {
    it(`${name} contains every en key`, () => {
      const missing = [...enKeys].filter((k) => !keys.has(k));
      expect(missing, `${name}.json missing keys: ${missing.join(", ")}`).toEqual([]);
    });

    it(`en contains every ${name} key (no extra ${name} keys)`, () => {
      const extra = [...keys].filter((k) => !enKeys.has(k));
      expect(extra, `${name}.json has extra keys not in en.json: ${extra.join(", ")}`).toEqual([]);
    });
  }

  it("every locale value is a non-empty string or nested plain object", () => {
    const checkValues = (obj, path = "") => {
      for (const [k, v] of Object.entries(obj)) {
        const p = path ? `${path}.${k}` : k;
        if (v !== null && typeof v === "object" && !Array.isArray(v)) {
          checkValues(v, p);
        } else if (typeof v === "string") {
          expect(v.length, `empty string at ${p}`).toBeGreaterThan(0);
        } else {
          throw new Error(`non-string, non-object value at ${p}: ${typeof v}`);
        }
      }
    };
    checkValues(en);
    checkValues(zh);
    checkValues(ja);
    checkValues(es);
  });

  it("en and zh have identical {placeholder} sets for every shared key", () => {
    const enFlat = flattenKeys(en).reduce((acc, key) => {
      const val = lookupValue(en, key);
      if (typeof val === "string") acc.set(key, extractPlaceholders(val));
      return acc;
    }, new Map());
    const mismatches = [];
    for (const [key, enPlaceholders] of enFlat) {
      const zhVal = lookupValue(zh, key);
      if (typeof zhVal !== "string") continue;
      const zhPlaceholders = extractPlaceholders(zhVal);
      if (
        enPlaceholders.size !== zhPlaceholders.size ||
        [...enPlaceholders].some((p) => !zhPlaceholders.has(p))
      ) {
        mismatches.push(
          `${key}: en={${[...enPlaceholders].join(",")}} zh={${[...zhPlaceholders].join(",")}}`,
        );
      }
    }
    expect(mismatches, `Placeholder mismatches:\n${mismatches.join("\n")}`).toEqual([]);
  });
});

// ── Module-local copy tables ──────────────────────────────────────────

describe("settings copy lives in the locale files", () => {
  // A module that ships its own `"settings.x.y": "English"` table escapes every
  // key audit above: the keys never appear in public/locales/*.json, and the
  // module-local English silently wins whenever a locale lacks the key. That is
  // how the Subagents tab shipped 80 keys whose only home was the module.
  const walk = (dir) => {
    const files = [];
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name === "node_modules" || entry.name === "locales") continue;
        files.push(...walk(path));
      } else if (entry.name.endsWith(".js") && !entry.name.endsWith(".test.js")) {
        files.push(path);
      }
    }
    return files;
  };

  // Object keys only: a `t("settings.a.b")` call or a ternary branch is not a
  // table, so the match is anchored to the start of a line and to the `:` that
  // turns the string into a property.
  const COPY_TABLE_KEY = /^[ \t]*(?:"|')(settings\.[A-Za-z0-9_.${}]+)(?:"|')[ \t]*:/;

  it("no frontend module declares a settings.* copy table", () => {
    const violations = [];
    for (const file of walk(publicDir)) {
      const lines = readFileSync(file, "utf-8").split("\n");
      lines.forEach((line, index) => {
        const match = COPY_TABLE_KEY.exec(line);
        if (match) {
          violations.push(
            `${relative(publicDir, file)}:${index + 1} declares module-local copy for ${match[1]}`,
          );
        }
      });
    }
    expect(
      violations,
      `Move these strings into public/locales/*.json:\n${violations.join("\n")}`,
    ).toEqual([]);
  });
});

// ── Settings module key references ────────────────────────────────────

describe("subagents tab key references", () => {
  const source = readFileSync(resolve(publicDir, "settings/subagents-tab.js"), "utf-8");

  // Static `"settings.subagents.x"` strings plus the host-message map, which
  // names its locale keys relative to the namespace. Template references
  // (`settings.subagents.detail.${field}`) are collected too and checked as
  // prefixes; subagents-locale-coverage.test.js walks the concrete values
  // those templates expand to at render time.
  const collectReferences = (text) => {
    const references = new Map();
    const remember = (key, line) => {
      if (!references.has(key)) references.set(key, line);
    };
    text.split("\n").forEach((line, index) => {
      for (const match of line.matchAll(
        /settings\.subagents\.[A-Za-z0-9_.]*(?:\$\{[^}]*\}[A-Za-z0-9_.]*)*/g,
      )) {
        remember(match[0], index + 1);
      }
      for (const match of line.matchAll(/"(?:diagnostics|status)\.[A-Za-z0-9]+"/g)) {
        remember(`settings.subagents.${match[0].slice(1, -1)}`, index + 1);
      }
    });
    return references;
  };

  const references = collectReferences(source);
  const locales = [
    ["en", en],
    ["zh", zh],
    ["ja", ja],
    ["es", es],
  ];

  it("collects every namespace the module renders", () => {
    expect(references.size).toBeGreaterThan(50);
  });

  it("every concrete key the module uses exists in all four locales", () => {
    const violations = [];
    for (const [key, line] of references) {
      if (key.includes("${")) continue;
      for (const [name, messages] of locales) {
        const value = lookupValue(messages, key);
        if (typeof value !== "string" || value.length === 0) {
          violations.push(`subagents-tab.js:${line} ${key} missing from ${name}.json`);
        }
      }
    }
    expect(violations, `Subagents keys absent from a locale:\n${violations.join("\n")}`).toEqual(
      [],
    );
  });

  it("every dynamic key prefix the module uses resolves in all four locales", () => {
    const violations = [];
    for (const [key, line] of references) {
      if (!key.includes("${")) continue;
      const escaped = key.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      const pattern = new RegExp(`^${escaped.replace(/\\\$\\\{[^}]*\\\}/g, "[^.]+")}$`);
      const matched = [...enKeys].filter((candidate) => pattern.test(candidate));
      if (matched.length === 0) {
        violations.push(`subagents-tab.js:${line} ${key} matches no en.json key`);
        continue;
      }
      for (const [name, messages] of locales) {
        for (const candidate of matched) {
          const value = lookupValue(messages, candidate);
          if (typeof value !== "string" || value.length === 0) {
            violations.push(`subagents-tab.js:${line} ${candidate} missing from ${name}.json`);
          }
        }
      }
    }
    expect(violations, `Subagents prefixes unresolved:\n${violations.join("\n")}`).toEqual([]);
  });
});

// ── HTML key references ───────────────────────────────────────────────

describe("HTML data-i18n key references", () => {
  const htmlFiles = ["index.html"];

  for (const file of htmlFiles) {
    it(`${file} references only keys that exist in en.json`, () => {
      const content = readFileSync(resolve(publicDir, file), "utf-8");
      const attrs = [
        "data-i18n",
        "data-i18n-ph",
        "data-i18n-title",
        "data-i18n-aria-label",
        "data-i18n-alt",
      ];
      const referenced = new Set();

      for (const attr of attrs) {
        const regex = new RegExp(`${attr}="([^"]+)"`, "g");
        let match;

        for (;;) {
          match = regex.exec(content);
          if (!match) break;
          referenced.add(match[1]);
        }
      }

      const missing = [...referenced].filter((k) => !enKeys.has(k));
      expect(missing, `${file} references missing keys: ${missing.join(", ")}`).toEqual([]);
    });
  }
});

// ── JS literal t() key references ─────────────────────────────────────

describe("JS t() literal key references", () => {
  // Phase 1 JS files that should use t()
  const jsFiles = [
    "app.js",
    "ui/context-viz.js",
    "ui/at-file-mention.js",
    "ui/message-renderer.js",
    "ui/markdown.js",
    "ui/tool-card.js",
    "workspace/file-browser.js",
    "ui/dialogs.js",
    "app/updater.js",
    "app/voice-input.js",
    "sidebar/index.js",
    "settings/settings-config.js",
    "settings/models-page.js",
    "settings/models-oauth-login.js",
    "settings/skills-page.js",
    "settings/package-skills-tab.js",
    "settings/toggles.js",
    "settings/save-status.js",
    "packages/install-status.js",
    "workspace/actions.js",
    "session/onboarding.js",
    "cost/dashboard.js",
    "cost/infobar.js",
    "sidebar-workspace-group.js",
    "workspace-projects.js",
    "ephemeral-chat-view.js",
    "side-chat-manager.js",
    "quick-chat-dialog.js",
    "file-preview-panel.js",
    "git-panel.js",
    "git-client.js",
    "git-diff-renderer.js",
  ];

  it("every literal t(\"...\") / t('...') key exists in en.json", () => {
    const referenced = new Set();

    for (const file of jsFiles) {
      let content;
      try {
        content = readFileSync(resolve(publicDir, file), "utf-8");
      } catch (error) {
        throw new Error(`Missing i18n audit file ${file}: ${error.message}`);
      }
      // Match t("key.path") and t('key.path')
      const regex = /\bt\(\s*["']([^"']+)["']/g;
      let match;

      for (;;) {
        match = regex.exec(content);
        if (!match) break;
        referenced.add(match[1]);
      }
    }

    const missing = [...referenced].filter((k) => !enKeys.has(k));
    expect(missing, `t() references missing keys: ${missing.join(", ")}`).toEqual([]);
  });

  it("no raw t() output in innerHTML/insertAdjacentHTML/template without escapeHtml", () => {
    const violations = [];

    for (const file of jsFiles) {
      let content;
      try {
        content = readFileSync(resolve(publicDir, file), "utf-8");
      } catch (error) {
        throw new Error(`Missing i18n audit file ${file}: ${error.message}`);
      }

      // Check for `${t(` in template literals assigned to innerHTML or insertAdjacentHTML
      // We only flag t() in template literals that are directly assigned to innerHTML
      // or passed to insertAdjacentHTML. Using t() in a template literal passed to a
      // function that uses textContent (like renderError) is safe.
      const innerHtmlTemplateRegex = /\.innerHTML\s*=\s*`[^`]*\$\{t\(/g;
      let match;

      for (;;) {
        match = innerHtmlTemplateRegex.exec(content);
        if (!match) break;
        violations.push(`${file}: raw \${t()} in innerHTML template without escapeHtml`);
      }

      // Check for .innerHTML = ...t(...
      const innerHtmlRegex = /\.innerHTML\s*=\s*[^;]*\bt\(/g;

      for (;;) {
        match = innerHtmlRegex.exec(content);
        if (!match) break;
        const segment = content.slice(match.index, match.index + 500);
        if (
          !segment.includes("escapeHtml(t(") &&
          !segment.includes("this.escapeHtml(t(") &&
          !segment.includes("this._escape(t(") &&
          !segment.includes("textContent")
        ) {
          violations.push(`${file}: .innerHTML assignment with raw t() without escapeHtml`);
        }
      }

      // Check for insertAdjacentHTML with t()
      const insertAdjRegex = /insertAdjacentHTML\([^;]*\bt\(/g;

      for (;;) {
        match = insertAdjRegex.exec(content);
        if (!match) break;
        const segment = content.slice(match.index, match.index + 200);
        if (!segment.includes("escapeHtml(t(")) {
          violations.push(`${file}: insertAdjacentHTML with raw t() without escapeHtml`);
        }
      }
    }

    expect(violations, `Raw t() in HTML:\n${violations.join("\n")}`).toEqual([]);
  });
});
