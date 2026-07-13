// Ephemeral drag-and-drop state for moving files in the tree, plus the last
// move error (shown as a non-blocking notice near the tree).

class DragState {
  /** relPath of the file currently being dragged, or null. */
  from = $state<string | null>(null);
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

/** Whether the dragged file may drop into `folder` ("" = root). */
export function canDrop(folder: string): boolean {
  return drag.from !== null && parentOf(drag.from) !== folder;
}
