// Which OS Ken runs on, read from the webview's user agent, and the few
// strings that differ by OS: shortcut hints (⌘K on macOS, Ctrl+K on
// Windows), how to install Claude Code, and what macOS calls the system
// audio permission.

/** True for a macOS (or iOS) user agent. */
export function isMacUserAgent(ua: string): boolean {
  return /Macintosh|Mac OS X|iPhone|iPad/i.test(ua);
}

/** This machine is a Mac. */
export const isMac: boolean = typeof navigator !== "undefined" && isMacUserAgent(navigator.userAgent ?? "");

const MAC_KEYS: Record<string, string> = { mod: "⌘", shift: "⇧", alt: "⌥", ctrl: "⌃", enter: "↵", esc: "esc" };
const OTHER_KEYS: Record<string, string> = { mod: "Ctrl", shift: "Shift", alt: "Alt", ctrl: "Ctrl", enter: "Enter", esc: "Esc" };

/** A shortcut as this OS writes it: `mod+K` is "⌘K" on macOS and "Ctrl+K"
 *  elsewhere; `shift+enter` is "⇧↵" or "Shift+Enter". */
export function shortcut(keys: string, mac: boolean = isMac): string {
  const names = mac ? MAC_KEYS : OTHER_KEYS;
  const parts = keys
    .split("+")
    .map((p) => p.trim())
    .filter((p) => p.length > 0)
    .map((p) => names[p.toLowerCase()] ?? (p.length === 1 ? p.toUpperCase() : p));
  return parts.join(mac ? "" : "+");
}

/** How to get Claude Code onto this machine, in one line. */
export function claudeInstallHelp(mac: boolean = isMac): string {
  return mac
    ? "Install Claude Code with its native installer (or npm install -g @anthropic-ai/claude-code), then run claude once to sign in."
    : "Install Claude Code with winget install Anthropic.ClaudeCode (or npm install -g @anthropic-ai/claude-code), then run claude once to sign in.";
}

/** The name of the permission system audio needs. macOS files it under
 *  Screen Recording; Windows asks for nothing. */
export function systemAudioLabel(mac: boolean = isMac): string {
  return mac ? "system audio (Screen Recording)" : "system audio";
}

/** Where a file manager opens a file's folder. */
export function fileManagerName(mac: boolean = isMac): string {
  return mac ? "Finder" : "File Explorer";
}
