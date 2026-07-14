// Width of the Files sidebar. Pure clamp helpers + localStorage persistence,
// so the drag handler, the keyboard handler and the tests all agree on what
// counts as a usable width.

export const DEFAULT_SIDEBAR_WIDTH = 264;
/** Below this the tree is all ellipsis and no filename. */
export const MIN_SIDEBAR_WIDTH = 190;
/** Above this the tree is just whitespace, however wide the window is. */
export const MAX_SIDEBAR_WIDTH = 560;

// The nav rail (64px) plus the narrowest editor worth reading — the sidebar
// may never grow into this.
const RESERVED = 64 + 360;

/** The widest the sidebar may get in a window of `windowWidth` px. */
export function maxSidebarWidth(windowWidth: number): number {
  return Math.max(
    MIN_SIDEBAR_WIDTH,
    Math.min(MAX_SIDEBAR_WIDTH, windowWidth - RESERVED),
  );
}

export function clampSidebarWidth(width: number, windowWidth: number): number {
  if (Number.isNaN(width)) return DEFAULT_SIDEBAR_WIDTH;
  const max = maxSidebarWidth(windowWidth);
  return Math.round(Math.min(Math.max(width, MIN_SIDEBAR_WIDTH), max));
}

const KEY = "ken.sidebar.width";

/** The user's preferred width, un-clamped against the current window — a
 *  temporarily narrow window shouldn't overwrite what they picked. */
export function loadSidebarWidth(): number {
  try {
    const raw = Number(localStorage.getItem(KEY));
    if (!raw || Number.isNaN(raw)) return DEFAULT_SIDEBAR_WIDTH;
    return Math.round(
      Math.min(Math.max(raw, MIN_SIDEBAR_WIDTH), MAX_SIDEBAR_WIDTH),
    );
  } catch {
    return DEFAULT_SIDEBAR_WIDTH;
  }
}

export function saveSidebarWidth(width: number) {
  try {
    localStorage.setItem(KEY, String(Math.round(width)));
  } catch {
    /* best-effort */
  }
}
