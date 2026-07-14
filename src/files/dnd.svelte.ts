// Ephemeral drag-and-drop state for moving files in the tree, plus the last
// move error (shown as a non-blocking notice near the tree).

class DragState {
  /** relPath of the file currently being dragged, or null. */
  from = $state<string | null>(null);
  /** What's being dragged — folders get the into-own-subtree drop guard. */
  fromKind = $state<"file" | "folder">("file");
  /** relPath of the folder currently hovered as a drop target ("" = root). */
  over = $state<string | null>(null);
  /** Last move failure, surfaced near the tree until the next drag. */
  error = $state<string | null>(null);

  reset() {
    this.from = null;
    this.over = null;
  }
}

export const drag = new DragState();

/** Parent folder of a rel path ("" for a top-level entry). */
export function parentOf(relPath: string): string {
  const i = relPath.lastIndexOf("/");
  return i >= 0 ? relPath.slice(0, i) : "";
}

/** Whether the dragged entry may drop into `folder` ("" = root). A dragged
 *  folder may never drop into itself or its own subtree. */
export function canDrop(folder: string): boolean {
  if (drag.from === null) return false;
  if (parentOf(drag.from) === folder) return false;
  if (
    drag.fromKind === "folder" &&
    (folder === drag.from || folder.startsWith(drag.from + "/"))
  ) {
    return false;
  }
  return true;
}
