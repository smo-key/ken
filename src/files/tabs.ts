// Pure reducers for the Files tab strip (VS Code-style preview + pinning).
// State lives reactively in app.svelte.ts; these keep the semantics testable.

export interface FileTab {
  path: string;
  /** Pinned tabs sit leftmost and survive "close others". */
  pinned: boolean;
  /** Preview tabs render italic and are replaced by the next preview open. */
  preview: boolean;
}

export interface TabState {
  tabs: FileTab[];
  active: string | null;
}

/** Stable reorder so pinned tabs are always leftmost. */
function pinnedFirst(tabs: FileTab[]): FileTab[] {
  const pinned = tabs.filter((t) => t.pinned);
  const rest = tabs.filter((t) => !t.pinned);
  return [...pinned, ...rest];
}

/**
 * Open `path`. When `persistent` is false it opens as THE preview tab,
 * replacing any existing preview tab in place; a persistent open (double-click,
 * edit) makes it a normal tab.
 */
export function openTab(
  state: TabState,
  path: string,
  persistent: boolean,
): TabState {
  const existing = state.tabs.find((t) => t.path === path);
  if (existing) {
    const tabs = state.tabs.map((t) =>
      t.path === path && persistent && t.preview ? { ...t, preview: false } : t,
    );
    return { tabs, active: path };
  }
  const newTab: FileTab = { path, pinned: false, preview: !persistent };
  if (!persistent) {
    const idx = state.tabs.findIndex((t) => t.preview && !t.pinned);
    if (idx >= 0) {
      const tabs = state.tabs.slice();
      tabs[idx] = newTab;
      return { tabs, active: path };
    }
  }
  return { tabs: pinnedFirst([...state.tabs, newTab]), active: path };
}

export function closeTab(state: TabState, path: string): TabState {
  const idx = state.tabs.findIndex((t) => t.path === path);
  if (idx < 0) return state;
  const tabs = state.tabs.filter((t) => t.path !== path);
  let active = state.active;
  if (active === path) {
    const neighbor = tabs[idx] ?? tabs[idx - 1] ?? null;
    active = neighbor ? neighbor.path : null;
  }
  return { tabs, active };
}

/** Close everything except `path` and pinned tabs. */
export function closeOthers(state: TabState, path: string): TabState {
  const tabs = state.tabs.filter((t) => t.pinned || t.path === path);
  const active = tabs.some((t) => t.path === path)
    ? path
    : (tabs[0]?.path ?? null);
  return { tabs, active };
}

export function setPinned(
  state: TabState,
  path: string,
  pinned: boolean,
): TabState {
  const tabs = pinnedFirst(
    state.tabs.map((t) =>
      t.path === path
        ? { ...t, pinned, preview: pinned ? false : t.preview }
        : t,
    ),
  );
  return { tabs, active: state.active };
}

/** Promote a preview tab to a persistent one. */
export function makePersistent(state: TabState, path: string): TabState {
  return {
    tabs: state.tabs.map((t) =>
      t.path === path ? { ...t, preview: false } : t,
    ),
    active: state.active,
  };
}

/** Rewrite a tab's path after the file moved on disk. */
export function renameTab(state: TabState, from: string, to: string): TabState {
  return {
    tabs: state.tabs.map((t) => (t.path === from ? { ...t, path: to } : t)),
    active: state.active === from ? to : state.active,
  };
}

/** Rewrite tab paths after a move: an exact file move renames one tab; a folder
 *  move renames every tab under the old prefix. */
export function renameTabsForMove(state: TabState, from: string, to: string): TabState {
  const map = (p: string) =>
    p === from ? to : p.startsWith(from + "/") ? to + p.slice(from.length) : p;
  return {
    tabs: state.tabs.map((t) => ({ ...t, path: map(t.path) })),
    active: state.active === null ? null : map(state.active),
  };
}
