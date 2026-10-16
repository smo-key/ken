import { describe, expect, it } from "vitest";
import { parseKenAddress, toWorkspaceAddress, unopenableReason, WORKSPACE_HOST } from "./kenAddress";

describe("parseKenAddress", () => {
  it("parses a real member address", () => {
    expect(parseKenAddress("ken://abc-123/notes/todo.md")).toEqual({
      projectId: "abc-123",
      relPath: "notes/todo.md",
    });
  });

  it("parses the workspace pseudo-member's reserved host", () => {
    expect(parseKenAddress("ken://workspace/journal/2026-07-24.md")).toEqual({
      projectId: WORKSPACE_HOST,
      relPath: "journal/2026-07-24.md",
    });
  });

  it("rejects malformed addresses", () => {
    expect(parseKenAddress("not-a-ken-address")).toBeNull();
    expect(parseKenAddress("ken://")).toBeNull();
    expect(parseKenAddress("ken://onlyhost")).toBeNull();
    expect(parseKenAddress("ken://host/")).toBeNull();
  });
});

describe("toWorkspaceAddress", () => {
  it("prefixes a bare workspace-relative path", () => {
    expect(toWorkspaceAddress("journal/2026-07-24.md")).toBe(
      "ken://workspace/journal/2026-07-24.md",
    );
  });

  it("leaves an already-full ken:// address alone", () => {
    expect(toWorkspaceAddress("ken://workspace/memory/style.md")).toBe(
      "ken://workspace/memory/style.md",
    );
  });
});

describe("unopenableReason", () => {
  it("flags invalid addresses", () => {
    expect(unopenableReason("garbage")).toBeTruthy();
  });

  it("flags the workspace pseudo-member as unopenable today", () => {
    expect(unopenableReason("ken://workspace/journal/2026-07-24.md")).toBeTruthy();
  });

  it("allows a real member address", () => {
    expect(unopenableReason("ken://abc-123/notes/todo.md")).toBeNull();
  });
});
