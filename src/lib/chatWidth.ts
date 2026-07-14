// Width of the chat drawer (docked on the right). Pure clamp helpers +
// localStorage persistence, so the drag handler, the keyboard handler and the
// tests all agree on what counts as a usable width. Mirrors sidebar.ts, but
// the drawer is its own preference with its own key and baseline.

/** The drawer's long-standing fixed width — the sensible starting point. */
export const DEFAULT_CHAT_WIDTH = 372;
/** Below this the reply box, tab strip and foot controls get cramped. */
export const MIN_CHAT_WIDTH = 300;
/** Above this the transcript is mostly whitespace, however wide the window. */
export const MAX_CHAT_WIDTH = 640;

// The nav rail (64px) plus the narrowest main content worth reading — the
// drawer may never grow into this and crush the screen behind it.
const RESERVED = 64 + 360;

/** The widest the drawer may get in a window of `windowWidth` px. */
export function maxChatWidth(windowWidth: number): number {
  return Math.max(
    MIN_CHAT_WIDTH,
    Math.min(MAX_CHAT_WIDTH, windowWidth - RESERVED),
  );
}

export function clampChatWidth(width: number, windowWidth: number): number {
  if (Number.isNaN(width)) return DEFAULT_CHAT_WIDTH;
  const max = maxChatWidth(windowWidth);
  return Math.round(Math.min(Math.max(width, MIN_CHAT_WIDTH), max));
}

const KEY = "ken.chat.width";

/** The user's preferred width, un-clamped against the current window — a
 *  temporarily narrow window shouldn't overwrite what they picked. */
export function loadChatWidth(): number {
  try {
    const raw = Number(localStorage.getItem(KEY));
    if (!raw || Number.isNaN(raw)) return DEFAULT_CHAT_WIDTH;
    return Math.round(Math.min(Math.max(raw, MIN_CHAT_WIDTH), MAX_CHAT_WIDTH));
  } catch {
    return DEFAULT_CHAT_WIDTH;
  }
}

export function saveChatWidth(width: number) {
  try {
    localStorage.setItem(KEY, String(Math.round(width)));
  } catch {
    /* best-effort */
  }
}
