// Inline-edit state for the Files tree (§12): one edit at a time — a rename of
// an existing row, or a new-document/new-folder row inside a target folder
// ("" = project root). Commit talks to the backend; validation errors surface
// through the tree's existing non-blocking notice (drag.error), and the editor
// stays open so the user can fix the name.
import { api } from "../lib/api";
import { app } from "../lib/app.svelte";
import { drag, parentOf } from "./dnd.svelte";
import { dedupedDocName, siblingNames, validateName } from "./naming";

export type TreeEditMode = "rename" | "new-document" | "new-folder";

class TreeEditState {
  mode = $state<TreeEditMode | null>(null);
  /** rename: the relPath being renamed; creates: the parent folder ("" = root). */
  target = $state("");
  initial = $state("");

  private siblings(folder: string): string[] {
    return siblingNames(
      [...app.files.map((f) => f.relPath), ...app.folders.map((f) => f.relPath)],
      folder,
    );
  }

  beginRename(relPath: string) {
    this.mode = "rename";
    this.target = relPath;
    this.initial = relPath.split("/").pop() ?? relPath;
    drag.error = null;
  }

  beginCreate(mode: "new-document" | "new-folder", folder: string) {
    this.mode = mode;
    this.target = folder;
    this.initial = mode === "new-document" ? dedupedDocName(this.siblings(folder)) : "";
    drag.error = null;
  }

  cancel() {
    this.mode = null;
  }

  async commit(name: string) {
    const mode = this.mode;
    if (!mode) return;
    const trimmed = name.trim();
    const folder = mode === "rename" ? parentOf(this.target) : this.target;
    const currentName = mode === "rename" ? (this.target.split("/").pop() ?? "") : null;
    if (mode === "rename" && trimmed === currentName) {
      this.cancel(); // unchanged — nothing to do
      return;
    }
    const siblings = this.siblings(folder).filter((s) => s !== currentName);
    const invalid = validateName(trimmed, siblings);
    if (invalid) {
      drag.error = invalid; // the tree's non-blocking notice
      return; // keep editing
    }
    const path = folder === "" ? trimmed : `${folder}/${trimmed}`;
    const renameFrom = this.target;
    this.mode = null;
    drag.error = null;
    try {
      if (mode === "rename") {
        await app.moveFile(renameFrom, path);
      } else if (mode === "new-folder") {
        await api.createFolder(path);
        await app.refreshTree();
      } else {
        // The backend may dedupe further (race safety) — open what it made.
        const finalRel = await api.createDocument(path);
        await app.refreshTree();
        app.openTab(finalRel, true);
      }
    } catch (e) {
      drag.error = String(e);
    }
  }
}

export const treeEdit = new TreeEditState();
