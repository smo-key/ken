// Import staging shared by the Files tree header and the ImportDialog host.
// Picking a file copies it into staging and opens the placement dialog (the
// AI's folder decision and preview resolve inside the dialog).
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { api, type ImportDto } from "./api";

class ImportsStore {
  staged = $state<ImportDto | null>(null);
  importing = $state(false);
  importError = $state<string | null>(null);

  async startImport() {
    if (this.importing || this.staged) return;
    this.importing = true;
    this.importError = null;
    try {
      const chosen = await openDialog({ directory: false });
      if (typeof chosen !== "string") return; // cancelled
      this.staged = await api.importBegin(chosen);
    } catch (e) {
      this.importError = String(e);
    } finally {
      this.importing = false;
    }
  }

  close() {
    this.staged = null;
  }
}

export const imports = new ImportsStore();
