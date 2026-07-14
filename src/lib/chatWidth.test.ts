import { beforeEach, describe, expect, it } from "vitest";
import {
  DEFAULT_CHAT_WIDTH,
  MAX_CHAT_WIDTH,
  MIN_CHAT_WIDTH,
  clampChatWidth,
  loadChatWidth,
  maxChatWidth,
  saveChatWidth,
} from "./chatWidth";

const ROOMY = 1600;

describe("clampChatWidth", () => {
  it("keeps a comfortable width untouched", () => {
    expect(clampChatWidth(420, ROOMY)).toBe(420);
  });

  it("clamps to the min and max", () => {
    expect(clampChatWidth(10, ROOMY)).toBe(MIN_CHAT_WIDTH);
    expect(clampChatWidth(9999, ROOMY)).toBe(MAX_CHAT_WIDTH);
  });

  it("rounds to whole pixels", () => {
    expect(clampChatWidth(420.6, ROOMY)).toBe(421);
  });

  it("falls back to the default for garbage input", () => {
    expect(clampChatWidth(NaN, ROOMY)).toBe(DEFAULT_CHAT_WIDTH);
    expect(clampChatWidth(Infinity, ROOMY)).toBe(MAX_CHAT_WIDTH);
  });

  it("leaves room for the nav rail and the screen in a narrow window", () => {
    const width = clampChatWidth(MAX_CHAT_WIDTH, 800);
    expect(width).toBeLessThan(MAX_CHAT_WIDTH);
    expect(width).toBe(maxChatWidth(800));
  });

  it("never returns less than the min, however tiny the window", () => {
    expect(clampChatWidth(400, 200)).toBe(MIN_CHAT_WIDTH);
    expect(maxChatWidth(200)).toBe(MIN_CHAT_WIDTH);
  });
});

describe("chat width persistence", () => {
  beforeEach(() => localStorage.clear());

  it("defaults when nothing is stored", () => {
    expect(loadChatWidth()).toBe(DEFAULT_CHAT_WIDTH);
  });

  it("round-trips a saved width", () => {
    saveChatWidth(420);
    expect(loadChatWidth()).toBe(420);
  });

  it("ignores a corrupt stored value", () => {
    localStorage.setItem("ken.chat.width", "as wide as possible");
    expect(loadChatWidth()).toBe(DEFAULT_CHAT_WIDTH);
  });
});
