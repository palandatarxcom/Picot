import { describe, expect, test } from "vitest";
import {
  createFilePreviewFollow,
  isSubagentTool,
  isWriteTool,
  pathFromToolArgs,
  shouldFollowWrite,
  subagentChangedFiles,
  toPreviewPath,
  writePathsFromToolCall,
} from "./file-preview-follow.js";

describe("isWriteTool", () => {
  test("matches write/edit family names case-insensitively", () => {
    expect(isWriteTool("write")).toBe(true);
    expect(isWriteTool("Edit")).toBe(true);
    expect(isWriteTool("apply_patch")).toBe(true);
    expect(isWriteTool("read")).toBe(false);
    expect(isWriteTool("bash")).toBe(false);
  });
});

describe("pathFromToolArgs", () => {
  test("reads path, file_path, or filePath", () => {
    expect(pathFromToolArgs({ path: "src/a.js" })).toBe("src/a.js");
    expect(pathFromToolArgs({ file_path: "src/b.js" })).toBe("src/b.js");
    expect(pathFromToolArgs({ filePath: "src/c.js" })).toBe("src/c.js");
    expect(pathFromToolArgs({ command: "ls" })).toBe("");
  });
});

describe("writePathsFromToolCall", () => {
  const multiFileResult = {
    isError: false,
    details: {
      files: [
        { path: "docs/a.md", kind: "update" },
        { path: "docs/b.md", kind: "create" },
      ],
    },
  };

  test("reads a line-based multi-file edit's result files in order", () => {
    expect(writePathsFromToolCall({ text: "[docs/a.md]\n@REPLACE" }, multiFileResult)).toEqual([
      "docs/a.md",
      "docs/b.md",
    ]);
  });

  test("dedupes result files and drops non-string or blank paths", () => {
    expect(
      writePathsFromToolCall(
        { text: "..." },
        {
          isError: false,
          details: {
            files: [
              { path: "a.md" },
              { path: " a.md " },
              { path: "   " },
              { kind: "x" },
              null,
              { path: 7 },
            ],
          },
        },
      ),
    ).toEqual(["a.md"]);
  });

  test("a failed result reports no files, even when it carries a file list", () => {
    expect(
      writePathsFromToolCall(
        { text: "..." },
        { isError: true, details: { files: [{ path: "a.md" }] } },
      ),
    ).toEqual([]);
  });

  test("falls back to the single args path when the result reports none", () => {
    expect(writePathsFromToolCall({ path: "src/a.js" }, undefined)).toEqual(["src/a.js"]);
    expect(
      writePathsFromToolCall({ file_path: "src/b.js" }, { isError: false, content: [] }),
    ).toEqual(["src/b.js"]);
  });

  test("reports nothing without a path or a file list", () => {
    expect(writePathsFromToolCall({ content: "x" }, undefined)).toEqual([]);
    expect(writePathsFromToolCall(undefined, undefined)).toEqual([]);
  });
});

describe("subagentChangedFiles", () => {
  // Real shape (session 2026-10-12T02-36-47, subagent call_88dc84bdf): a
  // successful fan-out declares its files only in the structured acceptance
  // report — details.results[0].acceptance.childReport.changedFiles.
  const files = [
    "tmp/fddw_rwe_proposal/D2-研究提案客户沟通版-v0.2.pptx",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/deck.json",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/check_deck.py",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/check_report.json",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/build_pptx.sh",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/gen_notes.py",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/qa_corr.py",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/qa_spill.py",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/corr_report.json",
  ];
  const okResult = {
    isError: false,
    details: {
      results: [{ exitCode: 0, acceptance: { childReport: { changedFiles: files } } }],
    },
  };

  test("reads a successful fan-out's changedFiles in result order", () => {
    expect(subagentChangedFiles(okResult, "/ws", "/ws")).toEqual(files);
  });

  test("drops a failed outer result", () => {
    expect(subagentChangedFiles({ ...okResult, isError: true }, "/ws", "/ws")).toEqual([]);
  });

  test("drops failed child results and keeps the successful ones", () => {
    expect(
      subagentChangedFiles(
        {
          isError: false,
          details: {
            results: [
              { exitCode: 1, acceptance: { childReport: { changedFiles: ["a.md"] } } },
              { exitCode: 0, acceptance: { childReport: { changedFiles: ["b.md"] } } },
            ],
          },
        },
        "/ws",
        "/ws",
      ),
    ).toEqual(["b.md"]);
  });

  test("drops missing and empty reports and blank or non-string paths", () => {
    expect(
      subagentChangedFiles(
        {
          isError: false,
          details: {
            results: [
              { exitCode: 0 },
              { exitCode: 0, acceptance: { childReport: { changedFiles: [] } } },
              {
                exitCode: 0,
                acceptance: { childReport: { changedFiles: ["a.md", " a.md ", "  ", 7, null] } },
              },
            ],
          },
        },
        "/ws",
        "/ws",
      ),
    ).toEqual(["a.md"]);
  });

  test("dedupes repeats across results in first-seen order", () => {
    expect(
      subagentChangedFiles(
        {
          isError: false,
          details: {
            results: [
              { exitCode: 0, acceptance: { childReport: { changedFiles: ["a.md", "b.md"] } } },
              { exitCode: 0, acceptance: { childReport: { changedFiles: ["b.md", "c.md"] } } },
            ],
          },
        },
        "/ws",
        "/ws",
      ),
    ).toEqual(["a.md", "b.md", "c.md"]);
  });

  test("drops relative paths when the fan-out ran in another cwd", () => {
    expect(subagentChangedFiles(okResult, "/elsewhere", "/ws")).toEqual([]);
  });

  test("keeps absolute paths when the cwd differs (containment is enforced later)", () => {
    expect(
      subagentChangedFiles(
        {
          isError: false,
          details: {
            results: [
              {
                exitCode: 0,
                acceptance: { childReport: { changedFiles: ["/ws/x.md", "rel.md"] } },
              },
            ],
          },
        },
        "/elsewhere",
        "/ws",
      ),
    ).toEqual(["/ws/x.md"]);
  });

  test("treats an unknown cwd or workspace as the workspace's own", () => {
    expect(subagentChangedFiles(okResult, undefined, "/ws")).toEqual(files);
    expect(subagentChangedFiles(okResult, "/ws", "")).toEqual(files);
  });

  test("reads nothing from a plain tool result", () => {
    expect(
      subagentChangedFiles(
        { isError: false, details: { files: [{ path: "a.md" }] } },
        "/ws",
        "/ws",
      ),
    ).toEqual([]);
  });
});

describe("isSubagentTool", () => {
  test("matches the subagent tool name case-insensitively", () => {
    expect(isSubagentTool("subagent")).toBe(true);
    expect(isSubagentTool("Subagent")).toBe(true);
    expect(isSubagentTool("edit")).toBe(false);
  });
});

describe("toPreviewPath", () => {
  test("returns the workspace-absolute path (keeps tab identity with browser-open)", () => {
    expect(toPreviewPath("/Users/me/proj/index.html", "/Users/me/proj")).toBe(
      "/Users/me/proj/index.html",
    );
    expect(toPreviewPath("/Users/me/proj/src/app.js", "/Users/me/proj")).toBe(
      "/Users/me/proj/src/app.js",
    );
  });

  test("joins a bare relative path to the workspace root", () => {
    expect(toPreviewPath("src/app.js", "/Users/me/proj")).toBe("/Users/me/proj/src/app.js");
  });

  test("keeps relative paths when no workspace root is known", () => {
    expect(toPreviewPath("src/app.js", "")).toBe("src/app.js");
  });

  test("rejects absolute paths outside the workspace", () => {
    expect(toPreviewPath("/etc/passwd", "/Users/me/proj")).toBe("");
    expect(toPreviewPath("/Users/me/other/a.js", "/Users/me/proj")).toBe("");
    expect(toPreviewPath("/Users/me/proj/index.html", "")).toBe("");
  });
});

describe("shouldFollowWrite", () => {
  test("follows a successful write with a path", () => {
    expect(
      shouldFollowWrite({
        toolName: "write",
        isError: false,
        args: { path: "a.html" },
      }),
    ).toBe(true);
  });

  test("skips errors, reads, and tools without a path", () => {
    expect(shouldFollowWrite({ toolName: "write", isError: true, args: { path: "a.js" } })).toBe(
      false,
    );
    expect(shouldFollowWrite({ toolName: "read", args: { path: "a.js" } })).toBe(false);
    expect(shouldFollowWrite({ toolName: "write", args: { content: "x" } })).toBe(false);
  });

  test("follows when the start event already recorded the path", () => {
    expect(shouldFollowWrite({ toolCallId: "1", isError: false }, "src/a.js")).toBe(true);
  });

  test("follows a line-based multi-file edit whose paths live on the result", () => {
    expect(
      shouldFollowWrite({
        toolName: "edit",
        isError: false,
        args: { text: "[docs/a.md]\n@REPLACE" },
        result: { isError: false, details: { files: [{ path: "docs/a.md" }] } },
      }),
    ).toBe(true);
    // A non-write tool stays unfollowed even when it reports a file list.
    expect(
      shouldFollowWrite({
        toolName: "read",
        isError: false,
        args: {},
        result: { isError: false, details: { files: [{ path: "docs/a.md" }] } },
      }),
    ).toBe(false);
  });
});

describe("createFilePreviewFollow", () => {
  test("opens a relative write path after the tool ends", async () => {
    const revealWrite = async (path) => ({ path });
    const follow = createFilePreviewFollow({
      panel: { revealWrite, workspaceRoot: "/ws" },
      getWorkspacePath: async () => "/ws",
    });
    follow.onToolStart({
      toolCallId: "t1",
      toolName: "write",
      args: { path: "/ws/docs/index.html" },
    });
    const result = await follow.onToolEnd({ toolCallId: "t1", isError: false });
    expect(result).toEqual({ path: "/ws/docs/index.html" });
  });

  test("does not follow a failed write", async () => {
    const revealWrite = async () => {
      throw new Error("should not run");
    };
    const follow = createFilePreviewFollow({ panel: { revealWrite } });
    follow.onToolStart({
      toolCallId: "t1",
      toolName: "write",
      args: { path: "a.js" },
    });
    expect(await follow.onToolEnd({ toolCallId: "t1", isError: true })).toBeNull();
  });

  test("openPath focuses the preview without requiring a write tool", async () => {
    const opened = [];
    const follow = createFilePreviewFollow({
      panel: { openFile: async (path) => opened.push(path) },
      getWorkspacePath: async () => "/ws",
    });
    await follow.openPath("/ws/readme.md");
    expect(opened).toEqual(["/ws/readme.md"]);
  });

  test("notifies onWriteApplied with the raw and preview paths", async () => {
    const applied = [];
    const follow = createFilePreviewFollow({
      panel: { revealWrite: async (path) => ({ filePath: path }) },
      getWorkspacePath: async () => "/ws",
      onWriteApplied: (raw, previewPath) => applied.push([raw, previewPath]),
    });
    follow.onToolStart({ toolCallId: "t1", toolName: "write", args: { path: "/ws/new.js" } });
    await follow.onToolEnd({ toolCallId: "t1" });
    expect(applied).toEqual([["/ws/new.js", "/ws/new.js"]]);
  });

  test("notifies onWriteApplied even without a panel", async () => {
    const applied = [];
    const follow = createFilePreviewFollow({
      getWorkspacePath: async () => "/ws",
      onWriteApplied: (raw, previewPath) => applied.push([raw, previewPath]),
    });
    follow.onToolStart({ toolCallId: "t1", toolName: "edit", args: { file_path: "src/a.ts" } });
    await follow.onToolEnd({ toolCallId: "t1" });
    expect(applied).toEqual([["src/a.ts", "/ws/src/a.ts"]]);
  });

  test("does not notify for failed or out-of-workspace writes", async () => {
    const applied = [];
    const follow = createFilePreviewFollow({
      panel: { revealWrite: async (path) => ({ filePath: path }) },
      getWorkspacePath: async () => "/ws",
      onWriteApplied: (raw, previewPath) => applied.push([raw, previewPath]),
    });
    follow.onToolStart({ toolCallId: "t1", toolName: "write", args: { path: "/ws/a.js" } });
    await follow.onToolEnd({ toolCallId: "t1", isError: true });
    follow.onToolStart({ toolCallId: "t2", toolName: "write", args: { path: "/elsewhere/a.js" } });
    await follow.onToolEnd({ toolCallId: "t2" });
    follow.onToolStart({ toolCallId: "t3", toolName: "read", args: { path: "/ws/b.js" } });
    await follow.onToolEnd({ toolCallId: "t3" });
    expect(applied).toEqual([]);
  });
});

test("a line-based multi-file edit records every file the result reports", async () => {
  // Real shape (session 2026-10-03T10-21-58, edit call_7ae0f14f): the args
  // carry only the line-editing script, so the written files exist solely on
  // result.details.files. All four must reach the turn's files card, in
  // first-write order; a multi-file result has no single target to reveal.
  const applied = [];
  const files = ["docs/a.md", "docs/b.md", "docs/c.md", "docs/d.md"];
  const follow = createFilePreviewFollow({
    panel: {
      revealWrite: async () => {
        throw new Error("a multi-file result must not reveal a file");
      },
    },
    getWorkspacePath: async () => "/ws",
    onWriteApplied: (raw, previewPath) => applied.push([raw, previewPath]),
  });
  const args = { text: "[docs/a.md]\n@REPLACE\n-x\n+y" };
  follow.onToolStart({ toolCallId: "e1", toolName: "edit", args });
  const revealed = await follow.onToolEnd({
    toolCallId: "e1",
    toolName: "edit",
    args,
    result: { isError: false, details: { files: files.map((path) => ({ path, kind: "update" })) } },
    isError: false,
  });
  expect(applied).toEqual(files.map((path) => [path, `/ws/${path}`]));
  expect(revealed).toBeNull();
});

test("a multi-file result never records or opens a file outside the workspace", async () => {
  const applied = [];
  const follow = createFilePreviewFollow({
    panel: {
      revealWrite: async () => {
        throw new Error("out-of-workspace paths must not reach the panel");
      },
    },
    getWorkspacePath: async () => "/ws",
    onWriteApplied: (raw, previewPath) => applied.push([raw, previewPath]),
  });
  follow.onToolStart({ toolCallId: "e2", toolName: "edit", args: { text: "..." } });
  await follow.onToolEnd({
    toolCallId: "e2",
    toolName: "edit",
    args: { text: "..." },
    result: {
      isError: false,
      details: { files: [{ path: "/etc/passwd" }, { path: "docs/inside.md" }] },
    },
    isError: false,
  });
  expect(applied).toEqual([["docs/inside.md", "/ws/docs/inside.md"]]);
});

test("a failed multi-file edit records nothing", async () => {
  const applied = [];
  const follow = createFilePreviewFollow({
    getWorkspacePath: async () => "/ws",
    onWriteApplied: (raw, previewPath) => applied.push([raw, previewPath]),
  });
  follow.onToolStart({ toolCallId: "e3", toolName: "edit", args: { text: "..." } });
  await follow.onToolEnd({
    toolCallId: "e3",
    toolName: "edit",
    args: { text: "..." },
    result: { isError: true, details: { files: [{ path: "docs/a.md" }] } },
    isError: true,
  });
  expect(applied).toEqual([]);
});

test("records the write even when the panel reveal rejects", async () => {
  // The turn's files card settles on agent_end; the write must be recorded
  // BEFORE the panel reveal. A slow or failing reveal (tab open, native pane
  // attach) must not delay or drop the record, or the card silently misses
  // the file.
  const applied = [];
  const follow = createFilePreviewFollow({
    panel: {
      revealWrite: async () => {
        throw new Error("pane attach failed");
      },
    },
    getWorkspacePath: async () => "/ws",
    onWriteApplied: (raw, previewPath) => applied.push([raw, previewPath]),
  });
  follow.onToolStart({ toolCallId: "w9", toolName: "edit", args: { path: "a.css" } });
  // The caller swallows reveal failures (`.catch(() => {})` in app.js); the
  // record must already have landed before that promise rejects.
  await follow
    .onToolEnd({
      toolCallId: "w9",
      toolName: "edit",
      args: { path: "a.css" },
      result: { content: [] },
      isError: false,
    })
    .catch(() => null);
  expect(applied).toEqual([["a.css", "/ws/a.css"]]);
});

test("a successful subagent fan-out records every changedFile without revealing one", async () => {
  // Real shape (session 2026-10-12T02-36-47, call_88dc84bdf): the nine written
  // files exist only in details.results[0].acceptance.childReport.changedFiles.
  // A fan-out has no single target to open, so the preview must stay closed.
  const files = [
    "tmp/fddw_rwe_proposal/D2-研究提案客户沟通版-v0.2.pptx",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/deck.json",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/check_deck.py",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/check_report.json",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/build_pptx.sh",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/gen_notes.py",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/qa_corr.py",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/qa_spill.py",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/corr_report.json",
  ];
  const result = {
    isError: false,
    details: {
      results: [{ exitCode: 0, acceptance: { childReport: { changedFiles: files } } }],
    },
  };
  const applied = [];
  const follow = createFilePreviewFollow({
    panel: {
      revealWrite: async () => {
        throw new Error("a fan-out must not reveal a single file");
      },
    },
    getWorkspacePath: async () => "/ws",
    onWriteApplied: (raw, previewPath) => applied.push([raw, previewPath]),
  });
  follow.onToolStart({
    toolCallId: "s1",
    toolName: "subagent",
    args: { agent: "PPTX助手", cwd: "/ws" },
  });
  const revealed = await follow.onToolEnd({
    toolCallId: "s1",
    toolName: "subagent",
    result,
    isError: false,
  });
  expect(applied).toEqual(files.map((path) => [path, `/ws/${path}`]));
  expect(revealed).toBeNull();
});

test("a fan-out rooted in another cwd or a failed fan-out records nothing", async () => {
  const result = {
    isError: false,
    details: {
      results: [{ exitCode: 0, acceptance: { childReport: { changedFiles: ["rel/a.md"] } } }],
    },
  };
  const applied = [];
  const follow = createFilePreviewFollow({
    getWorkspacePath: async () => "/ws",
    onWriteApplied: (raw, previewPath) => applied.push([raw, previewPath]),
  });
  follow.onToolStart({ toolCallId: "s2", toolName: "subagent", args: { cwd: "/elsewhere" } });
  await follow.onToolEnd({ toolCallId: "s2", toolName: "subagent", result, isError: false });
  follow.onToolStart({ toolCallId: "s3", toolName: "subagent", args: { cwd: "/ws" } });
  await follow.onToolEnd({
    toolCallId: "s3",
    toolName: "subagent",
    result: { ...result, isError: true },
    isError: true,
  });
  expect(applied).toEqual([]);
});

test("a failed subagent call records nothing even when a child report looks successful", async () => {
  // Pi's tool_execution_end can carry an outer isError while the result holds
  // a per-result childReport; a failed call must not be trusted for writes.
  const applied = [];
  const follow = createFilePreviewFollow({
    getWorkspacePath: async () => "/ws",
    onWriteApplied: (raw, previewPath) => applied.push([raw, previewPath]),
  });
  follow.onToolStart({ toolCallId: "s4", toolName: "subagent", args: { cwd: "/ws" } });
  const revealed = await follow.onToolEnd({
    toolCallId: "s4",
    toolName: "subagent",
    result: {
      details: {
        results: [{ exitCode: 0, acceptance: { childReport: { changedFiles: ["docs/a.md"] } } }],
      },
    },
    isError: true,
  });
  expect(applied).toEqual([]);
  expect(revealed).toBeNull();
});

test("a failed subagent call still clears its pending cwd", async () => {
  const applied = [];
  const follow = createFilePreviewFollow({
    getWorkspacePath: async () => "/ws",
    onWriteApplied: (raw, previewPath) => applied.push([raw, previewPath]),
  });
  follow.onToolStart({ toolCallId: "s5", toolName: "subagent", args: { cwd: "/ws" } });
  await follow.onToolEnd({
    toolCallId: "s5",
    toolName: "subagent",
    result: {
      details: {
        results: [{ exitCode: 0, acceptance: { childReport: { changedFiles: ["docs/a.md"] } } }],
      },
    },
    isError: true,
  });
  // The id is reused without a fresh start: no stale fan-out receipt may leak.
  await follow.onToolEnd({
    toolCallId: "s5",
    result: {
      details: {
        results: [{ exitCode: 0, acceptance: { childReport: { changedFiles: ["docs/b.md"] } } }],
      },
    },
  });
  expect(applied).toEqual([]);
});
