import { describe, expect, it } from "vitest";
import { cardHits, parseToolCard } from "./toolCard";

describe("parseToolCard", () => {
  it("reads a card the engine stored", () => {
    const c = parseToolCard(
      JSON.stringify({ toolUseId: "t1", name: "Grep", summary: "Grep save", status: "error", result: "boom" }),
    );
    expect(c).toEqual({ toolUseId: "t1", name: "Grep", summary: "Grep save", status: "error", result: "boom" });
  });

  it("stops spinning once the chat is no longer working", () => {
    const raw = JSON.stringify({ toolUseId: "t1", name: "Read", summary: "Read a.md", status: "running" });
    expect(parseToolCard(raw, true)?.status).toBe("running");
    expect(parseToolCard(raw, false)?.status).toBe("done");
  });

  it("is null for anything that isn't a card", () => {
    expect(parseToolCard("Read a.md")).toBeNull();
    expect(parseToolCard(JSON.stringify({ name: "Read" }))).toBeNull();
  });
});

describe("cardHits", () => {
  it("lists a search's hits with each code hit's symbol beside its path", () => {
    const result = [
      "Route: every source (2 searched).",
      "Sources: game (searched), wiki (searched)",
      "",
      "1. [code · service · 2026-10-01] game:src/world.ts:3 `saveWorld` (ken://a/src/world.ts#L3) [keyword]",
      "   export function saveWorld(w: string) {",
      "   next: code_usages saveWorld · code_outline src/world.ts · code_imports src/world.ts",
      "2. [decision D-410 · binding · 2026-10-06] [[DECISIONS]] · wiki:Decisions/DECISIONS.md:9 (ken://b/Decisions/DECISIONS.md#L9) [both]",
    ].join("\n");
    expect(cardHits(result)).toEqual([
      { place: "game:src/world.ts:3", symbol: "saveWorld" },
      { place: "[[DECISIONS]] · wiki:Decisions/DECISIONS.md:9", symbol: null },
    ]);
  });

  it("is empty for a result that lists no hits", () => {
    expect(cardHits("Read 40 lines")).toEqual([]);
    expect(cardHits(undefined)).toEqual([]);
  });

  it("finds no hits in the code tools' results", () => {
    const imports = "ken://a/src/game.ts (game)\nImports:\n  - ken://a/src/world.ts\nImported by: (none)\nLibraries and unresolved: (none)";
    expect(cardHits(imports)).toEqual([]);
    const definition = '1 definition of "saveWorld":\n1. function saveWorld — ken://a/src/world.ts#L3 (lines 3-9, game)';
    expect(cardHits(definition)).toEqual([]);
  });
});
