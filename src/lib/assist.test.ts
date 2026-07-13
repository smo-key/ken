import { describe, expect, it } from "vitest";
import { digestMarkdown, isQuestionQuery } from "./assist";

describe("isQuestionQuery", () => {
  it("accepts three or more words", () => {
    expect(isQuestionQuery("who owns billing cutover")).toBe(true);
    expect(isQuestionQuery("  when   is   cutover  ")).toBe(true);
  });

  it("accepts a trailing question mark regardless of length", () => {
    expect(isQuestionQuery("cutover?")).toBe(true);
  });

  it("rejects short non-questions and empty input", () => {
    expect(isQuestionQuery("billing cutover")).toBe(false);
    expect(isQuestionQuery("priya")).toBe(false);
    expect(isQuestionQuery("   ")).toBe(false);
    expect(isQuestionQuery("")).toBe(false);
  });
});

describe("digestMarkdown", () => {
  it("renders title, body, and sources", () => {
    const md = digestMarkdown({
      date: "2026-07-12",
      body: "The cutover is now **Sept 12**.",
      sources: ["Standup — Jul 11.md", "People.md"],
    });
    expect(md).toBe(
      "**Ken digest — 2026-07-12**\n\nThe cutover is now **Sept 12**.\n\nSources: Standup — Jul 11.md, People.md",
    );
  });

  it("omits the sources line when there are none", () => {
    const md = digestMarkdown({ date: "2026-07-12", body: "Quiet.", sources: [] });
    expect(md).toBe("**Ken digest — 2026-07-12**\n\nQuiet.");
    expect(md).not.toContain("Sources:");
  });
});
