// ABOUTME: Guards the relay Phase 2.5 HTTP inventory: every fetch/XHR call site under public/
// ABOUTME: must appear as a row with an explicit remote-mode disposition (plan T1 drift guard).
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve } from "node:path";
import { expect, test } from "vitest";

const root = process.cwd();
const inventoryPath =
  "docs/superpowers/specs/not-started/2026-10-19-relay-phase2-5-http-inventory.md";
const DISPOSITIONS = ["既有 WS op", "新受限 op", "远程禁用", "显式保持本机"];
// fetch( / fetchImpl( are direct calls; a literal first argument is a host-relative path.
const CALL_SITE = /(?:^|[^\w.$])(?:fetch|fetchImpl)\(/;
const LITERAL_URL = /(?:^|[^\w.$])(?:fetch|fetchImpl)\(\s*([`"'])([^`"']*)\1/;
const XHR_SITE = /new XMLHttpRequest\(|new Request\(/;

function inventoryRows() {
  const doc = readFileSync(resolve(root, inventoryPath), "utf8");
  const section = doc.split("## 盘点表")[1]?.split("\n## ")[0] ?? "";
  return section
    .split("\n")
    .filter((line) => /^\| `public\/[^`:]+:\d+` \|/.test(line))
    .map((line) => {
      const cells = line
        .split("|")
        .slice(1, -1)
        .map((cell) => cell.trim());
      return {
        file: line.match(/`(public\/[^`:]+):\d+`/)[1],
        disposition: cells[3] ?? "",
        text: line,
      };
    });
}

function scanCallSites() {
  const sites = [];
  const walk = (dir) => {
    for (const name of readdirSync(dir)) {
      const full = join(dir, name);
      if (statSync(full).isDirectory()) {
        if (name !== "vendor") walk(full);
        continue;
      }
      if (!/\.(js|html)$/.test(name) || name.endsWith(".test.js")) continue;
      const file = relative(root, full);
      readFileSync(full, "utf8")
        .split("\n")
        .forEach((line, index) => {
          const url = line.match(LITERAL_URL)?.[2];
          if (CALL_SITE.test(line) || XHR_SITE.test(line)) {
            sites.push({ file, line: index + 1, path: url ? url.split("?")[0] : null });
          }
        });
    }
  };
  walk(join(root, "public"));
  return sites;
}

const rows = inventoryRows();
const sites = scanCallSites();

function countByFile(entries, pick) {
  const counts = new Map();
  for (const entry of entries) {
    const file = pick(entry);
    counts.set(file, (counts.get(file) ?? 0) + 1);
  }
  return counts;
}

test("every fetch/XHR call site under public/ has an inventory row", () => {
  const scanned = countByFile(sites, (site) => site.file);
  const listed = countByFile(rows, (row) => row.file);
  expect([...scanned.keys()].filter((file) => !listed.has(file))).toEqual([]);
  expect([...listed.keys()].filter((file) => !scanned.has(file))).toEqual([]);
  expect([...scanned.entries()].sort()).toEqual([...listed.entries()].sort());
  // A call site whose URL is a literal must have that path quoted in its row.
  for (const site of sites) {
    if (!site.path) continue;
    const fileRows = rows.filter((row) => row.file === site.file);
    expect(
      fileRows.some((row) => row.text.includes(site.path)),
      `${site.file}:${site.line} path ${site.path} missing from inventory`,
    ).toBe(true);
  }
});

test("every inventory row states a remote-mode disposition", () => {
  expect(rows.length).toBeGreaterThan(0);
  for (const row of rows) {
    expect(
      DISPOSITIONS.some((marker) => row.disposition.includes(marker)),
      `${row.file} disposition "${row.disposition}"`,
    ).toBe(true);
  }
});
