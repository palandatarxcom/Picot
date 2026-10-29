// ABOUTME: Verifies cross-platform local filesystem path normalization for browser consumers.
// ABOUTME: Covers POSIX, Windows drive, UNC, basename, and parent-path behavior.
import { expect, test } from "vitest";
import {
  basenameLocalPath,
  displayLocalPath,
  normalizeLocalPath,
  parentLocalPath,
  relativeLocalPath,
} from "./path-utils.js";

test("normalizes Windows separators while preserving a drive root", () => {
  expect(normalizeLocalPath("C:\\Users\\Lin\\repo\\..\\picot\\")).toBe("C:/Users/Lin/picot");
  expect(parentLocalPath("C:\\Users\\Lin\\picot")).toBe("C:/Users/Lin");
  expect(basenameLocalPath("C:\\Users\\Lin\\picot\\README.md")).toBe("README.md");
});

test("normalizes UNC paths without losing the share root", () => {
  expect(normalizeLocalPath("\\\\server\\share\\repo\\src\\..\\")).toBe("//server/share/repo");
  expect(parentLocalPath("\\\\server\\share\\repo")).toBe("//server/share");
  expect(basenameLocalPath("\\\\server\\share\\repo\\file.ts")).toBe("file.ts");
});

test("strips the Windows verbatim prefix before normalizing", () => {
  expect(normalizeLocalPath("\\\\?\\D:\\work\\repo")).toBe("D:/work/repo");
  expect(normalizeLocalPath("\\\\?\\C:\\Users\\Lin\\repo\\..\\picot")).toBe("C:/Users/Lin/picot");
  expect(normalizeLocalPath("\\\\?\\D:\\")).toBe("D:/");
});

test("maps the verbatim UNC prefix back to the share root", () => {
  expect(normalizeLocalPath("\\\\?\\UNC\\server\\share\\repo")).toBe("//server/share/repo");
});

test("formats relative paths with a leading slash for sidebar display", () => {
  expect(displayLocalPath("Users/Lin/project")).toBe("/Users/Lin/project");
  expect(displayLocalPath("/Users/Lin/project")).toBe("/Users/Lin/project");
  expect(displayLocalPath("C:/Users/Lin/project")).toBe("C:/Users/Lin/project");
});

test("keeps POSIX root semantics", () => {
  expect(normalizeLocalPath("/Users//Lin/../repo/")).toBe("/Users/repo");
  expect(parentLocalPath("/")).toBe("/");
  expect(basenameLocalPath("/Users/Lin/file.ts")).toBe("file.ts");
});

test("preserves relative parent traversal and normalizes empty dot segments", () => {
  expect(normalizeLocalPath("..")).toBe("..");
  expect(normalizeLocalPath("")).toBe("");
  expect(normalizeLocalPath(".")).toBe("");
  expect(normalizeLocalPath("a/./b/../c")).toBe("a/c");
  expect(parentLocalPath(".")).toBe("");
});

test("returns a relative path only when the file belongs to the canonical workspace", () => {
  expect(relativeLocalPath("/work/picot/src/app.js", "/work/picot")).toBe("src/app.js");
  expect(relativeLocalPath("/work/picot/README.md", "/work/picot")).toBe("README.md");
  expect(relativeLocalPath("C:\\work\\picot\\src\\app.ts", "C:/work/picot")).toBe("src/app.ts");
  expect(relativeLocalPath("\\\\host\\share\\repo\\src\\app.rs", "//host/share/repo")).toBe(
    "src/app.rs",
  );
  expect(relativeLocalPath("/work/other/app.js", "/work/picot")).toBeNull();
  expect(relativeLocalPath("D:/work/picot/app.ts", "C:/work/picot")).toBeNull();
});
