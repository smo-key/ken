// The Files-tree header filter is a quiet control: it appears only when it can
// do something — there are unread files, or the user is already in the unread
// view (so they can switch back). Same rule the right-pane toolbar used.

export type FilesFilter = "all" | "unread";

export function showUnreadFilter(unreadCount: number, filter: FilesFilter): boolean {
  return unreadCount > 0 || filter === "unread";
}
