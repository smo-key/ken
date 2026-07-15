// The Files-tree header filter is always present so the user can switch between
// the full tree and the unread-only view at any time — even when nothing is
// currently unread and they want to confirm the list is clear.

export type FilesFilter = "all" | "unread";

export function showUnreadFilter(_unreadCount: number, _filter: FilesFilter): boolean {
  return true;
}

// "Mark all as viewed" is always shown too, but it can only do something when
// there are unread files — otherwise it renders disabled.
export function isMarkAllEnabled(unreadCount: number): boolean {
  return unreadCount > 0;
}
