// Typed wrappers over Tauri commands + events. The only file that talks IPC.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface ProjectInfo {
  id: string;
  name: string;
  root: string;
  excluded: string[];
}

export interface RegistryEntryStatus {
  id: string;
  name: string;
  path: string;
  available: boolean;
}

export interface FileRow {
  relPath: string;
  kind: string;
  size: number;
  mtime: number;
  status: "indexed" | "metadata_only" | "failed";
  error: string | null;
}

export interface SearchHit {
  relPath: string;
  kind: string;
  status: string;
  snippet: string;
  rank: number;
}

export interface ScanStats {
  added: number;
  updated: number;
  removed: number;
  failed: number;
  unchanged: number;
}

export interface FolderInfo {
  relPath: string;
  excluded: boolean;
}

export interface TreeData {
  files: FileRow[];
  folders: FolderInfo[];
}

export const api = {
  listProjects: () => invoke<RegistryEntryStatus[]>("list_projects"),
  createProject: (path: string, name: string) =>
    invoke<ProjectInfo>("create_project", { path, name }),
  openProject: (path: string) => invoke<ProjectInfo>("open_project", { path }),
  forgetProject: (id: string) => invoke<void>("forget_project", { id }),
  currentProject: () => invoke<ProjectInfo | null>("current_project"),
  setFolderSelection: (excluded: string[]) =>
    invoke<ProjectInfo>("set_folder_selection", { excluded }),
  getTree: () => invoke<TreeData>("get_tree"),
  search: (query: string, limit = 30) =>
    invoke<SearchHit[]>("search", { query, limit }),
  readFile: (relPath: string) => invoke<string>("read_file", { relPath }),
  readFileBytes: (relPath: string) =>
    invoke<ArrayBuffer>("read_file_bytes", { relPath }),
  saveFile: (relPath: string, content: string) =>
    invoke<number>("save_file", { relPath, content }),
  fileMeta: (relPath: string) => invoke<FileRow | null>("file_meta", { relPath }),
  extractedText: (relPath: string) =>
    invoke<string>("extracted_text", { relPath }),
  reindex: () => invoke<ScanStats>("reindex"),
  openExternal: (relPath: string) => invoke<void>("open_external", { relPath }),
  fileMtime: (relPath: string) => invoke<number>("file_mtime", { relPath }),

  onIndexUpdated: (fn: (stats: ScanStats) => void): Promise<UnlistenFn> =>
    listen<ScanStats>("index-updated", (e) => fn(e.payload)),
  onFileSaved: (fn: (relPath: string) => void): Promise<UnlistenFn> =>
    listen<string>("file-saved", (e) => fn(e.payload)),
  onScanError: (fn: (message: string) => void): Promise<UnlistenFn> =>
    listen<string>("scan-error", (e) => fn(e.payload)),
};
