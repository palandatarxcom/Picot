import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { buildFlattenedSessionTree } from "../sidebar/session-tree-model.js";
import {
  cacheSidebarProjects,
  consumeNavState,
  isCompleteSidebarProject,
  readCachedSidebarProjects,
  snapshotNavState,
} from "./nav-state-cache.js";

function clearCookies() {
  document.cookie.split(";").forEach((c) => {
    const name = c.split("=")[0].trim();
    if (name) document.cookie = `${name}=; Max-Age=0; Path=/`;
  });
}

const sampleProjects = [
  {
    workspaceId: "history:proj",
    path: "/work/proj",
    folderName: "proj",
    dirName: "proj",
    isProvisional: false,
    source: "history",
    activityAt: 1000,
    lastActivityAt: 1000,
    sessions: [
      {
        filePath: "/s1.jsonl",
        parentSession: "/parent.jsonl",
        name: "Session 1",
        firstMessage: "hello",
        timestamp: 123,
        mtime: 456,
        ctime: 100,
        tmux: false,
      },
    ],
  },
];

describe("nav-state-cache", () => {
  beforeEach(() => {
    clearCookies();
  });

  afterEach(() => {
    clearCookies();
    // Host namespacing reads the window URL: never leak a param to the next test.
    window.history.replaceState({}, "", "/");
  });

  describe("snapshotNavState / consumeNavState", () => {
    it("round-trips scroll, draft, and sidebar state", () => {
      snapshotNavState({
        messageScroll: 420,
        sidebarScroll: 80,
        inputDraft: "half-typed reply",
        expandedWorkspaces: new Set(["ws-1", "ws-2"]),
        searchQuery: "test",
      });

      const restored = consumeNavState();

      expect(restored).not.toBeNull();
      expect(restored.messageScroll).toBe(420);
      expect(restored.sidebarScroll).toBe(80);
      expect(restored.inputDraft).toBe("half-typed reply");
      expect(restored.expandedWorkspaces).toBeInstanceOf(Set);
      expect([...restored.expandedWorkspaces].sort()).toEqual(["ws-1", "ws-2"]);
      expect(restored.searchQuery).toBe("test");
    });

    it("consuming clears the snapshot so it is not reused", () => {
      snapshotNavState({ messageScroll: 100 });
      expect(consumeNavState()).not.toBeNull();
      expect(consumeNavState()).toBeNull();
    });

    it("returns null when no snapshot exists", () => {
      expect(consumeNavState()).toBeNull();
    });

    it("accepts arrays for expandedWorkspaces", () => {
      snapshotNavState({ expandedWorkspaces: ["a", "b"] });
      const restored = consumeNavState();
      expect(restored).not.toBeNull();
      expect([...restored.expandedWorkspaces].sort()).toEqual(["a", "b"]);
    });

    it("truncates long input drafts to the cookie-safe cap", () => {
      const long = "x".repeat(30000);
      snapshotNavState({ inputDraft: long });
      const restored = consumeNavState();
      expect(restored.inputDraft.length).toBeLessThanOrEqual(1500);
    });
  });

  describe("isCompleteSidebarProject", () => {
    it("accepts projects with identity + sessions array", () => {
      expect(isCompleteSidebarProject(sampleProjects[0])).toBe(true);
    });

    it("rejects projects missing workspaceId, path, or sessions", () => {
      expect(isCompleteSidebarProject({ ...sampleProjects[0], workspaceId: "" })).toBe(false);
      expect(isCompleteSidebarProject({ ...sampleProjects[0], path: "" })).toBe(false);
      expect(isCompleteSidebarProject({ ...sampleProjects[0], sessions: null })).toBe(false);
      expect(isCompleteSidebarProject(null)).toBe(false);
      expect(isCompleteSidebarProject(undefined)).toBe(false);
    });
  });

  describe("cacheSidebarProjects / readCachedSidebarProjects", () => {
    it("round-trips a complete project projection with render fields", () => {
      cacheSidebarProjects(sampleProjects);

      const cached = readCachedSidebarProjects();
      expect(cached).not.toBeNull();
      expect(cached).toHaveLength(1);
      expect(cached[0].path).toBe("/work/proj");
      expect(cached[0].folderName).toBe("proj");
      expect(cached[0].dirName).toBe("proj");
      expect(cached[0].sessions).toHaveLength(1);
      expect(cached[0].sessions[0].name).toBe("Session 1");
      expect(cached[0].sessions[0].firstMessage).toBe("hello");
      expect(cached[0].sessions[0].filePath).toBe("/s1.jsonl");
      expect(cached[0].sessions[0].mtime).toBe(456);
      expect(cached[0].sessions[0].parentSession).toBe("/parent.jsonl");
    });

    it("preserves mtime for recent-first ordering after cache restore", () => {
      const sessions = [
        {
          filePath: "/old.jsonl",
          timestamp: "2026-01-02T00:00:00.000Z",
          mtime: Date.parse("2026-01-01T00:00:00.000Z"),
        },
        {
          filePath: "/recent.jsonl",
          timestamp: "2026-01-01T00:00:00.000Z",
          mtime: Date.parse("2026-01-03T00:00:00.000Z"),
        },
      ];
      cacheSidebarProjects([{ ...sampleProjects[0], sessions }]);

      const cached = readCachedSidebarProjects();
      const rows = buildFlattenedSessionTree(cached[0].sessions);

      expect(rows.map((row) => row.session.filePath)).toEqual(["/recent.jsonl", "/old.jsonl"]);
    });

    it("falls back to session timestamp when cached mtime is unavailable", () => {
      cacheSidebarProjects([
        {
          ...sampleProjects[0],
          sessions: [
            { filePath: "/old.jsonl", timestamp: "2026-01-01T00:00:00.000Z" },
            { filePath: "/recent.jsonl", timestamp: "2026-01-02T00:00:00.000Z" },
          ],
        },
      ]);

      const cached = readCachedSidebarProjects();
      const rows = buildFlattenedSessionTree(cached[0].sessions);

      expect(rows.map((row) => row.session.filePath)).toEqual(["/recent.jsonl", "/old.jsonl"]);
    });

    it("drops sessions without a filePath and incomplete projects", () => {
      cacheSidebarProjects([
        {
          ...sampleProjects[0],
          sessions: [
            ...sampleProjects[0].sessions,
            { name: "no path", firstMessage: "x", timestamp: 1 },
          ],
        },
        { workspaceId: "", path: "", sessions: [] },
      ]);

      const cached = readCachedSidebarProjects();
      expect(cached).not.toBeNull();
      expect(cached).toHaveLength(1);
      expect(cached[0].sessions).toHaveLength(1);
    });

    it("never caches the provisional placeholder row", () => {
      // The placeholder is per-page UI state keyed by the *runtime* session id
      // (not a file). Caching it made the next boot (e.g. the reload after a
      // daemon restart) hydrate an unselectable row: selecting it routes the
      // bare id as `sessionPath`, which the host cannot canonicalize, so the
      // click fails with "Selected session is unavailable".
      cacheSidebarProjects([
        {
          ...sampleProjects[0],
          sessions: [
            {
              filePath: "01a13eab-3d17-7776-8a7e-afbddae6bb59",
              name: "New chat",
              provisional: true,
            },
            ...sampleProjects[0].sessions,
          ],
        },
      ]);

      const cached = readCachedSidebarProjects();
      expect(cached).not.toBeNull();
      expect(cached[0].sessions.map((session) => session.filePath)).toEqual(["/s1.jsonl"]);
      expect(cached[0].sessions.some((session) => session.provisional)).toBe(false);
    });

    it("shards large project trees and reads them back", () => {
      const manySessions = Array.from({ length: 400 }, (_, i) => ({
        filePath: `/s${i}.jsonl`,
        name: `Session ${i}`,
        firstMessage: `m${i}`,
        timestamp: i,
        ctime: i,
        tmux: false,
      }));
      cacheSidebarProjects([{ ...sampleProjects[0], sessions: manySessions }]);

      const cached = readCachedSidebarProjects();
      expect(cached).not.toBeNull();
      expect(cached[0].sessions).toHaveLength(400);
      expect(cached[0].sessions[399].name).toBe("Session 399");
    });

    it("returns null when no cache exists", () => {
      expect(readCachedSidebarProjects()).toBeNull();
    });

    it("ignores non-array input", () => {
      cacheSidebarProjects(null);
      cacheSidebarProjects(undefined);
      cacheSidebarProjects("not-an-array");
      expect(readCachedSidebarProjects()).toBeNull();
    });

    it("namespaces the cache by host so a remote never hydrates the local projection", () => {
      const goTo = (search) => window.history.replaceState({}, "", `/${search}`);
      goTo("?host=r1");
      cacheSidebarProjects(sampleProjects);
      expect(readCachedSidebarProjects()).not.toBeNull();

      // Another host shares this origin and document.cookie, not this cache.
      goTo("?host=r2");
      expect(readCachedSidebarProjects()).toBeNull();

      goTo("?host=r1");
      expect(readCachedSidebarProjects()).not.toBeNull();

      // The local host keeps its own keys, so its cache survives a remote one.
      goTo("");
      expect(readCachedSidebarProjects()).toBeNull();
      cacheSidebarProjects(sampleProjects);
      goTo("?host=r1");
      expect(readCachedSidebarProjects()).not.toBeNull();
      goTo("");
      expect(readCachedSidebarProjects()).not.toBeNull();
      goTo("");
    });
  });
});
