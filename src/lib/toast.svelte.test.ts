import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { errorText, failure, toast, TOAST_MS } from "./toast.svelte";

describe("failure", () => {
  it("joins what failed and why", () => {
    expect(failure("Could not update the wiki", "git is not installed")).toBe(
      "Could not update the wiki: git is not installed",
    );
    expect(failure("Could not save", new Error("disk full"))).toBe("Could not save: disk full");
    expect(failure("Could not save", "")).toBe("Could not save");
  });
  it("drops an Error: prefix", () => {
    expect(errorText("Error: no team")).toBe("no team");
  });
});

describe("toast store", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    toast.items = [];
  });

  it("shows a notice and takes it away after a while", () => {
    toast.show("Sent to Ingest.");
    expect(toast.items.map((t) => t.text)).toEqual(["Sent to Ingest."]);
    vi.advanceTimersByTime(TOAST_MS.info);
    expect(toast.items).toEqual([]);
  });

  it("keeps a failure longer", () => {
    toast.error("Could not write the digest", "Claude Code is not signed in");
    expect(toast.items[0].tone).toBe("error");
    vi.advanceTimersByTime(TOAST_MS.info);
    expect(toast.items).toHaveLength(1);
    vi.advanceTimersByTime(TOAST_MS.error - TOAST_MS.info);
    expect(toast.items).toEqual([]);
  });

  it("dismisses one by id", () => {
    const a = toast.show("a");
    toast.show("b");
    toast.dismiss(a);
    expect(toast.items.map((t) => t.text)).toEqual(["b"]);
  });
});
