import { describe, expect, it } from "vitest";
import { shouldSend } from "./keySend";

describe("composer key handling", () => {
  it("sends on Enter", () => {
    expect(shouldSend({ key: "Enter", shiftKey: false })).toBe(true);
  });
  it("inserts a newline on Shift+Enter", () => {
    expect(shouldSend({ key: "Enter", shiftKey: true })).toBe(false);
  });
  it("ignores other keys", () => {
    expect(shouldSend({ key: "a", shiftKey: false })).toBe(false);
  });
});
