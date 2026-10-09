import { describe, expect, it } from "vitest";
import { parseToolCard } from "./toolCard";

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
