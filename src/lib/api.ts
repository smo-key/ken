// Typed wrappers over Tauri commands + events. The only file that talks IPC.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface ProjectInfo {
  id: string;
  name: string;
  root: string;
  excluded: string[];
  ingestRunner: "hidden-tui" | "headless";
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

export type IngestMode = "single" | "collection";
export type IngestRefresh = "on-change" | "manual";

export interface RulesOverride {
  reviewThresholdPct?: number;
  staleDays?: number;
}

export interface ResolvedRules {
  reviewThresholdPct: number;
  staleDays: number;
}

export interface Recipe {
  slug: string;
  name: string;
  description: string;
  sources: string[];
  output: string;
  mode: IngestMode;
  refresh: IngestRefresh;
  rules: RulesOverride | null;
  instruction: string;
}

export type RecipeEntry =
  | { kind: "ok"; recipe: Recipe }
  | { kind: "broken"; error: { slug: string; reason: string } };

export type RunStatus =
  | "running"
  | "fresh"
  | "blocked"
  | "pending_approval"
  | "failed"
  | "discarded"
  | "cancelled";

export interface RunRow {
  id: number;
  slug: string;
  sessionId: string | null;
  startedAt: number;
  finishedAt: number | null;
  status: RunStatus;
  summary: string | null;
  error: string | null;
  changeRatio: number | null;
}

export interface IngestSummary {
  entry: RecipeEntry;
  lastRun: RunRow | null;
  resolvedRules: ResolvedRules | null;
  stale: boolean;
}

export interface IngestDetail {
  recipe: Recipe;
  runs: RunRow[];
  resolvedRules: ResolvedRules;
}

export interface IngestEvent {
  slug: string;
  runId: number;
  status: RunStatus;
  detail: string | null;
}

export interface IngestForm {
  slug?: string;
  name: string;
  description?: string;
  instruction: string;
  sources: string[];
  output: string;
  mode: IngestMode;
  refresh: IngestRefresh;
  rules?: RulesOverride | null;
}

export interface ClaudeDoctor {
  found: boolean;
  path: string | null;
  version: string | null;
  help: string;
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

  listIngests: () => invoke<IngestSummary[]>("list_ingests"),
  getIngest: (slug: string) => invoke<IngestDetail>("get_ingest", { slug }),
  saveIngest: (form: IngestForm) => invoke<Recipe>("save_ingest", { form }),
  deleteIngest: (slug: string) => invoke<void>("delete_ingest", { slug }),
  runIngest: (slug: string, full = true) =>
    invoke<void>("run_ingest", { slug, full }),
  cancelRun: (slug: string) => invoke<void>("cancel_run", { slug }),
  approveRun: (runId: number) => invoke<void>("approve_run", { runId }),
  discardRun: (runId: number) => invoke<void>("discard_run", { runId }),
  pendingApprovals: () => invoke<RunRow[]>("pending_approvals"),
  setIngestRunnerMode: (mode: "hidden-tui" | "headless") =>
    invoke<void>("set_ingest_runner_mode", { mode }),
  claudeDoctor: () => invoke<ClaudeDoctor>("claude_doctor"),

  onIngestRunChanged: (fn: (ev: IngestEvent) => void): Promise<UnlistenFn> =>
    listen<IngestEvent>("ingest-run-changed", (e) => fn(e.payload)),
  onIndexUpdated: (fn: (stats: ScanStats) => void): Promise<UnlistenFn> =>
    listen<ScanStats>("index-updated", (e) => fn(e.payload)),
  onFileSaved: (fn: (relPath: string) => void): Promise<UnlistenFn> =>
    listen<string>("file-saved", (e) => fn(e.payload)),
  onScanError: (fn: (message: string) => void): Promise<UnlistenFn> =>
    listen<string>("scan-error", (e) => fn(e.payload)),
};
