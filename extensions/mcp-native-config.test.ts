// @vitest-environment node

// ABOUTME: Parity tests for the MCP-only native projection module.
// ABOUTME: Expected outcomes mirror upstream core/mcp-servers.ts + extensions/mcp/config.ts.

import { describe, expect, it } from "vitest";
import {
  classifyProjectEntry,
  isMcpOverride,
  parseNativeMcpDocument,
  projectGlobalServers,
  snapshotMcpOverride,
  validateNativeMcpEntry,
} from "./mcp-native-config";

const AGENT_FILE = "/agent/mcp.json";
const PROJECT_FILE = "/work/.pi/mcp.json";

describe("parseNativeMcpDocument", () => {
  it("accepts an object with an mcpServers object and keeps unrelated keys", () => {
    expect(parseNativeMcpDocument('{"autoEnableCodemode":false,"mcpServers":{}}')).toEqual({
      autoEnableCodemode: false,
      mcpServers: {},
    });
  });

  it("accepts strict JSON without comments (no JSONC tolerance)", () => {
    expect(() => parseNativeMcpDocument('{ // c\n "mcpServers": {} }')).toThrow();
  });

  it("rejects non-object documents and non-object mcpServers", () => {
    expect(() => parseNativeMcpDocument("[]")).toThrow(/object/i);
    expect(() => parseNativeMcpDocument('{"mcpServers":[]}')).toThrow(/mcpServers/);
    expect(() => parseNativeMcpDocument("{ not json")).toThrow();
  });
});

describe("validateNativeMcpEntry", () => {
  const ok = (name: string, raw: unknown) => {
    const result = validateNativeMcpEntry(name, raw);
    if (typeof result === "string") throw new Error(`expected valid, got: ${result}`);
    return result;
  };
  const bad = (name: string, raw: unknown) => {
    const result = validateNativeMcpEntry(name, raw);
    if (typeof result !== "string") throw new Error("expected an error string");
    return result;
  };

  it("normalizes the codemode-deferred alias in exposure and toolExposure", () => {
    expect(
      ok("docs", {
        command: "x",
        exposure: "codemode-deferred",
        toolExposure: { a: "codemode-deferred" },
      }),
    ).toEqual({ command: "x", exposure: "codemode", toolExposure: { a: "codemode" } });
  });

  it("rejects invalid names and non-object entries", () => {
    expect(bad("my.server", { command: "x" })).toMatch(/letters, digits/);
    expect(bad("docs", null)).toMatch(/must be an object/);
    expect(bad("docs", ["x"])).toMatch(/must be an object/);
  });

  it("rejects legacy sse and non-http urls", () => {
    expect(bad("docs", { command: "x", type: "sse" })).toMatch(/SSE/);
    expect(bad("docs", { url: "ftp://x.test/mcp" })).toMatch(/http or https/);
  });

  it("rejects a command array (native stdio transport is a single executable)", () => {
    expect(bad("docs", { command: ["npx", "-y", "x"] })).toMatch(/command.*url/);
  });

  it("validates stdio args, env and cwd types", () => {
    expect(
      ok("docs", { command: "npx", args: ["-y", "x"], env: { A: "1" }, cwd: "." }),
    ).toBeTruthy();
    expect(bad("docs", { command: "npx", args: "y" })).toMatch(/args must be an array/);
    expect(bad("docs", { command: "npx", env: { A: 1 } })).toMatch(/env must map/);
    expect(bad("docs", { command: "npx", cwd: 5 })).toMatch(/cwd must be a string/);
  });

  it("validates the shared exposure, enabled, timeout and description fields", () => {
    const exposures = ['"codemode"', '"deferred"', '"direct"', '"hidden"'].join(", ");
    expect(bad("docs", { command: "x", exposure: "loud" })).toContain(exposures);
    expect(bad("docs", { command: "x", enabled: "false" })).toMatch(/enabled must be a boolean/);
    expect(bad("docs", { command: "x", timeout: 0 })).toMatch(/timeout must be a positive/);
    expect(bad("docs", { command: "x", description: 5 })).toMatch(/description must be a string/);
    expect(bad("docs", { command: "x", toolExposure: ["a"] })).toMatch(/toolExposure must map/);
    expect(bad("docs", { command: "x", toolExposure: { delete_all: "hidden2" } })).toMatch(
      /toolExposure "delete_all"/,
    );
  });

  it("requires https (or loopback) for auth.provider", () => {
    expect(ok("docs", { url: "https://x.test/mcp", auth: { provider: "p" } })).toBeTruthy();
    expect(ok("docs", { url: "http://127.0.0.1:9/mcp", auth: { provider: "p" } })).toBeTruthy();
    expect(bad("docs", { url: "http://x.test/mcp", auth: { provider: "p" } })).toMatch(/https/);
    expect(bad("docs", { url: "https://x.test/mcp", auth: {} })).toMatch(/auth.provider/);
  });

  it("mirrors every validateOAuth branch", () => {
    const http = (oauth: unknown) => ({ url: "https://x.test/mcp", oauth });
    expect(ok("docs", http(undefined))).toBeTruthy();
    expect(bad("docs", http([]))).toMatch(/oauth must be an object/);
    expect(bad("docs", http({ clientId: 5 }))).toMatch(/oauth.clientId must be a string/);
    expect(bad("docs", http({ clientSecret: 5 }))).toMatch(/oauth.clientSecret must be a string/);
    expect(bad("docs", http({ callbackPort: 0 }))).toMatch(
      /oauth.callbackPort must be a port number/,
    );
    expect(bad("docs", http({ callbackPort: 65536 }))).toMatch(/oauth.callbackPort/);
    expect(bad("docs", http({ callbackUrl: "https://localhost:8080/callback" }))).toMatch(
      /oauth.callbackUrl must be an http URI/,
    );
    expect(bad("docs", http({ callbackUrl: "http://localhost:8080/callback?x=1" }))).toMatch(
      /oauth.callbackUrl/,
    );
    expect(
      bad("docs", http({ callbackUrl: "http://localhost:8080/oauth/callback", callbackPort: 9 })),
    ).toMatch(/different ports/);
    expect(ok("docs", http({ callbackUrl: "http://[::1]:8080/callback" }))).toBeTruthy();
    expect(bad("docs", http({ scope: 5 }))).toMatch(/oauth.scope must be a string/);
    expect(bad("docs", http({ clientName: "  " }))).toMatch(/oauth.clientName must be a non-empty/);
    expect(bad("docs", http({ clientRegistration: "bogus" }))).toMatch(/oauth.clientRegistration/);
    expect(bad("docs", http({ clientRegistration: "cimd", clientId: "a" }))).toMatch(
      /cannot be combined/,
    );
    expect(
      bad("docs", http({ clientRegistration: "cimd", callbackUrl: "http://localhost/x" })),
    ).toMatch(/requires oauth.callbackUrl/);
    expect(
      ok("docs", http({ clientRegistration: "cimd", callbackUrl: "http://localhost/callback" })),
    ).toBeTruthy();
    expect(
      bad("docs", http({ clientRegistration: "cimd", callbackUrl: "http://[::1]/callback" })),
    ).toMatch(/requires oauth.callbackUrl/);
    expect(bad("docs", http({ authServerMetadataUrl: "http://x.test/meta" }))).toMatch(
      /oauth.authServerMetadataUrl/,
    );
    expect(bad("docs", http({ authServerMetadataUrl: "not a url" }))).toMatch(
      /oauth.authServerMetadataUrl/,
    );
    expect(ok("docs", http({ authServerMetadataUrl: "http://localhost:9/meta" }))).toBeTruthy();
  });

  it("returns an error when neither transport is usable", () => {
    expect(bad("docs", { enabled: false })).toMatch(/either "command".*"url"/);
  });
});

describe("projectGlobalServers", () => {
  it("keeps document order and reports each invalid entry", () => {
    const projection = projectGlobalServers(
      {
        mcpServers: {
          first: { command: "a" },
          broken: { command: ["npx"] },
          second: { url: "https://s.test/mcp" },
        },
      },
      AGENT_FILE,
    );
    expect([...projection.servers.keys()]).toEqual(["first", "second"]);
    expect([...projection.errors.keys()]).toEqual(["broken"]);
    expect(projection.servers.get("first")?.identity).toEqual({
      scope: "global",
      source: AGENT_FILE,
    });
  });

  it("skips a second entry that only differs in - and _ from an earlier winner", () => {
    const projection = projectGlobalServers(
      { mcpServers: { "dev-tools": { command: "a" }, dev_tools: { command: "b" } } },
      AGENT_FILE,
    );
    expect([...projection.servers.keys()]).toEqual(["dev-tools"]);
    expect(projection.errors.get("dev_tools")).toMatch(/conflicts with "dev-tools"/);
  });

  it("flags a non-boolean autoEnableCodemode without dropping servers", () => {
    const projection = projectGlobalServers(
      { mcpServers: { a: { command: "x" } }, autoEnableCodemode: "yes" },
      AGENT_FILE,
    );
    expect([...projection.servers.keys()]).toEqual(["a"]);
    expect([...projection.errors.values()][0]).toMatch(/autoEnableCodemode must be a boolean/);
  });

  it("treats a missing mcpServers key as an empty projection", () => {
    expect([...projectGlobalServers({}, AGENT_FILE).servers.keys()]).toEqual([]);
  });
});

describe("isMcpOverride", () => {
  it("only treats transport-less records as override candidates", () => {
    expect(isMcpOverride({ enabled: false })).toBe(true);
    expect(isMcpOverride({})).toBe(true);
    expect(isMcpOverride({ type: "stdio", enabled: false })).toBe(false);
    expect(isMcpOverride({ command: "x" })).toBe(false);
    expect(isMcpOverride({ url: "https://x.test/mcp" })).toBe(false);
    expect(isMcpOverride(null)).toBe(false);
    expect(isMcpOverride(["enabled"])).toBe(false);
    expect(isMcpOverride("enabled")).toBe(false);
  });
});

describe("snapshotMcpOverride", () => {
  it("materializes defaults without credentials", () => {
    expect(snapshotMcpOverride({ command: "npx", env: { TOKEN: "secret" } })).toEqual({
      enabled: true,
      exposure: "codemode",
      toolExposure: {},
    });
  });

  it("keeps explicit values and resolves aliases", () => {
    expect(
      snapshotMcpOverride({
        command: "npx",
        enabled: false,
        exposure: "codemode-deferred",
        toolExposure: { "delete_*": "hidden" },
      }),
    ).toEqual({ enabled: false, exposure: "codemode", toolExposure: { "delete_*": "hidden" } });
  });

  it("returns an independent map", () => {
    const source = { toolExposure: { a: "direct" } } as Record<string, unknown>;
    const snapshot = snapshotMcpOverride(source);
    snapshot.toolExposure.a = "hidden";
    expect(source.toolExposure).toEqual({ a: "direct" });
  });
});

describe("classifyProjectEntry", () => {
  const globals = projectGlobalServers(
    {
      mcpServers: {
        context7: { command: "npx", env: { TOKEN: "secret" }, enabled: false, exposure: "direct" },
        docs: { url: "https://example.test/mcp", toolExposure: { "delete_*": "hidden" } },
        "dev-tools": { command: "d" },
      },
    },
    AGENT_FILE,
  );

  it("classifies a full definition and keeps the existing editor", () => {
    const result = classifyProjectEntry("repo", { command: "run" }, globals, PROJECT_FILE);
    expect(result.kind).toBe("definition");
    expect(result.effective).toBeUndefined();
  });

  it("keeps a native-ineligible definition as a definition with a diagnostic", () => {
    const result = classifyProjectEntry("repo", { command: ["npx", "-y"] }, globals, PROJECT_FILE);
    expect(result.kind).toBe("definition");
    expect(result.error).toMatch(/command.*url/);
  });

  it("rejects a project URL definition that sets auth.provider", () => {
    const result = classifyProjectEntry(
      "repo",
      { url: "https://x.test/mcp", auth: { provider: "p" } },
      globals,
      PROJECT_FILE,
    );
    expect(result.kind).toBe("definition");
    expect(result.error).toMatch(/auth is only allowed in the global/);
  });

  it("reports a namespace collision with a global definition", () => {
    const result = classifyProjectEntry("dev_tools", { command: "x" }, globals, PROJECT_FILE);
    expect(result.kind).toBe("definition");
    expect(result.error).toMatch(/conflicts with "dev-tools"/);
  });

  it("does not use a namespace alias as an override base", () => {
    const result = classifyProjectEntry("dev_tools", { enabled: false }, globals, PROJECT_FILE);
    expect(result.kind).toBe("invalid");
    expect(result.error).toMatch(/global|base/i);
  });

  it("materializes an override against the exact-name global base", () => {
    const result = classifyProjectEntry("context7", { exposure: "hidden" }, globals, PROJECT_FILE);
    expect(result.kind).toBe("override");
    expect(result.effective).toEqual({ enabled: false, exposure: "hidden", toolExposure: {} });
    expect(result.identity).toEqual({
      scope: "global",
      source: AGENT_FILE,
      override: PROJECT_FILE,
    });
  });

  it("replaces the tool map rather than merging it", () => {
    expect(
      classifyProjectEntry("docs", { toolExposure: {} }, globals, PROJECT_FILE).effective,
    ).toEqual({ enabled: true, exposure: "codemode", toolExposure: {} });
    expect(
      classifyProjectEntry("docs", { toolExposure: { other: "direct" } }, globals, PROJECT_FILE)
        .effective,
    ).toEqual({ enabled: true, exposure: "codemode", toolExposure: { other: "direct" } });
  });

  it("inherits connection fields from the base without exposing them", () => {
    const result = classifyProjectEntry("context7", { enabled: true }, globals, PROJECT_FILE);
    expect(result.effective).toEqual({ enabled: true, exposure: "direct", toolExposure: {} });
    expect(JSON.stringify(result)).not.toContain("secret");
  });

  it("rejects unknown override keys, bad types and missing base", () => {
    expect(classifyProjectEntry("context7", { command: "x" }, globals, PROJECT_FILE).kind).toBe(
      "definition",
    );
    expect(
      classifyProjectEntry("context7", { enabled: false, url: "u" }, globals, PROJECT_FILE).kind,
    ).toBe("definition");
    expect(
      classifyProjectEntry("context7", { enabled: false, args: [] }, globals, PROJECT_FILE).error,
    ).toMatch(/override can only set/);
    expect(
      classifyProjectEntry("context7", { enabled: "no" }, globals, PROJECT_FILE).error,
    ).toMatch(/enabled must be a boolean/);
    expect(
      classifyProjectEntry("missing", { enabled: false }, globals, PROJECT_FILE).error,
    ).toMatch(/global|base/i);
    expect(classifyProjectEntry("repo", 5, globals, PROJECT_FILE)).toMatchObject({
      kind: "invalid",
    });
  });
});
