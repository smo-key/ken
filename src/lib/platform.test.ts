import { describe, expect, it } from "vitest";
import { claudeInstallHelp, isMacUserAgent, shortcut, systemAudioLabel } from "./platform";

describe("isMacUserAgent", () => {
  it("tells macOS from Windows", () => {
    expect(isMacUserAgent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15")).toBe(true);
    expect(isMacUserAgent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Edg/129.0")).toBe(false);
    expect(isMacUserAgent("")).toBe(false);
  });
});

describe("shortcut", () => {
  it("writes the Mac symbols on macOS", () => {
    expect(shortcut("mod+K", true)).toBe("⌘K");
    expect(shortcut("mod+enter", true)).toBe("⌘↵");
    expect(shortcut("shift+enter", true)).toBe("⇧↵");
    expect(shortcut("mod+f", true)).toBe("⌘F");
  });
  it("writes Ctrl and words elsewhere", () => {
    expect(shortcut("mod+K", false)).toBe("Ctrl+K");
    expect(shortcut("mod+enter", false)).toBe("Ctrl+Enter");
    expect(shortcut("shift+enter", false)).toBe("Shift+Enter");
    expect(shortcut("mod+f", false)).toBe("Ctrl+F");
  });
});

describe("per-OS wording", () => {
  it("names winget on Windows and the native installer on macOS", () => {
    expect(claudeInstallHelp(false)).toContain("winget install Anthropic.ClaudeCode");
    expect(claudeInstallHelp(true)).toContain("native installer");
  });
  it("names Screen Recording only on macOS", () => {
    expect(systemAudioLabel(true)).toContain("Screen Recording");
    expect(systemAudioLabel(false)).toBe("system audio");
  });
});
