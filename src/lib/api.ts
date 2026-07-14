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
  status: "indexed" | "metadata_only" | "failed" | "cloud_only";
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

/** Persisted statuses plus transient live-only ones (never in the DB set). */
export type LiveStatus = RunStatus | "queued" | "waiting";

export interface RunRow {
  id: number;
  slug: string;
  kind: "ingest" | "automation";
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
  kind: "ingest" | "automation";
  slug: string;
  runId: number;
  status: LiveStatus;
  detail: string | null;
  activity?: string | null;
  elapsedSecs?: number | null;
  etaSecs?: number | null;
}

export interface Automation {
  slug: string;
  name: string;
  globs: string[];
  prompt: string;
  autoApply: boolean;
  enabled: boolean;
}

export interface AutomationForm {
  slug?: string;
  name: string;
  globs: string[];
  prompt: string;
  autoApply: boolean;
  enabled: boolean;
}

export interface AutomationDetail {
  automation: Automation;
  runs: RunRow[];
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

export type InboxKind =
  | "approval"
  | "stale"
  | "failed-file"
  | "broken-recipe"
  | "stored"
  | "conflict"
  | "conflict-copy"
  | "automation-proposal";

export interface InboxItem {
  /** Kind-prefixed, stable across refreshes: "run-12", "stale-people", … */
  id: string;
  kind: InboxKind;
  title: string;
  body: string;
  when: number;
  sourceRef: string;
  /** Kind-specific JSON for stored items (conflict versions, copy paths). */
  payload: string | null;
}

export interface ReviewInbox {
  items: InboxItem[];
  done: InboxItem[];
}

/** Parsed payload of a `conflict` inbox item. */
export interface ConflictPayload {
  path: string;
  ours: string;
  theirs: string;
  draft: string | null;
  draftStatus: "pending" | "ready" | "failed";
}

/** Parsed payload of a `conflict-copy` inbox item. */
export interface ConflictCopyPayload {
  copyPath: string;
  originalPath: string | null;
}

export type ConflictResolution =
  | "accept-draft"
  | "keep-mine"
  | "take-theirs"
  | "manual";

export type ConflictCopyResolution = "keep-copy" | "keep-original";

export type SyncStateName = "off" | "synced" | "syncing" | "attention";

export interface SyncStateEvent {
  state: SyncStateName;
  detail: string | null;
}

export interface SyncStatus {
  mode: "git" | "drive";
  auto: boolean;
  /** Whether automatic updates are actually running. */
  active: boolean;
  remote: string | null;
  branch: string | null;
}

export type ChatStatus = "working" | "needs_input" | "done" | "error";

export interface ChatRow {
  id: string;
  title: string;
  kind: "user" | "ingest" | "research";
  pinned: boolean;
  status: ChatStatus;
  createdAt: number;
  lastActiveAt: number;
  archived: boolean;
  /** Stable tier alias (see CHAT_MODELS), or null for the CLI's own default. */
  model: string | null;
}

/** Selectable chat models. Values are the CLI's stable tier aliases, which
 *  auto-resolve to the latest model of each tier — so this never needs version
 *  maintenance. `null` = the CLI's own default (no `--model` forwarded). */
export const CHAT_MODELS: { label: string; value: string | null }[] = [
  { label: "Default", value: null },
  { label: "Haiku", value: "haiku" },
  { label: "Sonnet", value: "sonnet" },
  { label: "Opus", value: "opus" },
  { label: "Fable", value: "fable" },
];

export interface ChatMessage {
  id: number;
  chatId: string;
  role: "user" | "assistant" | "activity" | "divider";
  content: string;
  createdAt: number;
}

export interface PtyChunk {
  chatId: string;
  data: string; // base64
}

export interface McpInfo {
  binaryPath: string | null;
  projectRoot: string;
  addCommand: string;
  jsonConfig: string;
  llmInstruction: string;
}

/** One day's digest, parsed for the Home card. */
export interface DigestDto {
  /** Local calendar day, yyyy-mm-dd. */
  date: string;
  body: string;
  sources: string[];
  generatedAt: number;
}

/** A ⌘K quick answer, tied to the query it answered. */
export interface QuickAnswer {
  query: string;
  body: string;
  sources: string[];
}

/** One streamed chunk of a quick answer, tied to its query. */
export interface QuickAnswerDelta {
  query: string;
  delta: string;
}

/** A knowledge-model entity (Map node). */
export interface EntityRow {
  id: number;
  kind: "person" | "organization" | "topic" | "decision" | "other";
  name: string;
  summary: string;
  /** Project-relative paths this entity is grounded in. */
  sources: string[];
}

/** A relation between two entities (Map edge). */
export interface EntityEdge {
  id: number;
  a: number;
  b: number;
  label: string;
}

/** A knowledge-model event (Timeline entry). */
export interface EventRow {
  id: number;
  /** Best-effort yyyy-mm-dd. */
  date: string;
  category: string;
  text: string;
  /** Project-relative path the event came from. */
  source: string;
}

/** The whole stored knowledge model — small by construction. */
export interface KnowledgeModel {
  entities: EntityRow[];
  edges: EntityEdge[];
  events: EventRow[];
  /** Epoch seconds of the last build; null before the first one. */
  builtAt: number | null;
  /** A manual Deep rebuild is running right now. */
  building: boolean;
  /** Files extracted so far / indexed files — the coverage line. */
  analyzed: number;
  total: number;
  /** `ready` | `notInstalled` | `error`. */
  llmStatus: "ready" | "notInstalled" | "error";
  llmError: string | null;
}

export interface KnowledgeModelState {
  /** `idle` = an automatic build stopped without a model; not an error. */
  state: "building" | "ready" | "error" | "idle";
  detail: string | null;
}

export interface ClaudeDoctor {
  found: boolean;
  path: string | null;
  version: string | null;
  help: string;
}

/** A downloadable on-device model and whether it's installed. */
export interface ModelStatus {
  id: string;
  name: string;
  installed: boolean;
  /** On-disk size when installed, else null. */
  sizeBytes: number | null;
  /** Expected download size, for the pre-download estimate. */
  expectedBytes: number;
  /** The recommended default, pre-selected in the UI. */
  recommended: boolean;
  /** "transcription" | "language" */
  category: "transcription" | "language";
  /** "recommended" | "advanced" */
  tier: "recommended" | "advanced";
  blurb: string;
  /** Whether this is the selected model for its category. */
  selected: boolean;
}

/** Payload of the `model-download-progress` event. */
export interface ModelProgress {
  id: string;
  downloaded: number;
  total: number;
}

/** Payload of the `model-download-error` event. */
export interface ModelDownloadError {
  id: string;
  message: string;
}

/** What `video_transcript` knows about a clip's captions right now. */
export interface VideoTranscript {
  /** WebVTT text, or null while generating / when there is none. */
  vtt: string | null;
  /** The transcript's own project-relative path, when one exists. */
  sourceRel: string | null;
  status: "ready" | "generating" | "none";
}

/** A file staged for import: the copied-in file, previewable but not yet placed. */
export interface ImportDto {
  importId: string;
  fileName: string;
  /** Project-relative path of the staged copy — feed to the preview commands. */
  previewRel: string;
  kind: string;
  size: number;
}

/** The AI's (or default) destination decision for a staged import. */
export interface Placement {
  /** Project-relative folder; empty string = the project root. */
  folder: string;
  /** True when `folder` doesn't exist yet (a proposed new folder). */
  isNew: boolean;
  rationale: string | null;
}

export const api = {
  listProjects: () => invoke<RegistryEntryStatus[]>("list_projects"),
  createProject: (path: string, name: string) =>
    invoke<ProjectInfo>("create_project", { path, name }),
  openProject: (path: string) => invoke<ProjectInfo>("open_project", { path }),
  forgetProject: (id: string) => invoke<void>("forget_project", { id }),
  renameProject: (id: string, name: string) =>
    invoke<ProjectInfo>("rename_project", { id, name }),
  lastProjectId: () => invoke<string | null>("last_project_id"),
  currentProject: () => invoke<ProjectInfo | null>("current_project"),
  setFolderSelection: (excluded: string[]) =>
    invoke<ProjectInfo>("set_folder_selection", { excluded }),
  getTree: () => invoke<TreeData>("get_tree"),
  search: (query: string, limit = 30) =>
    invoke<SearchHit[]>("search", { query, limit }),
  readFile: (relPath: string) => invoke<string>("read_file", { relPath }),
  readFileBytes: (relPath: string) =>
    invoke<ArrayBuffer>("read_file_bytes", { relPath }),
  isCloudOnly: (relPath: string) =>
    invoke<boolean>("is_cloud_only", { relPath }),
  /// Downloads an online-only file from the cloud provider. Slow by nature.
  hydrateFile: (relPath: string) => invoke<void>("hydrate_file", { relPath }),
  saveFile: (relPath: string, content: string) =>
    invoke<number>("save_file", { relPath, content }),
  fileMeta: (relPath: string) => invoke<FileRow | null>("file_meta", { relPath }),
  extractedText: (relPath: string) =>
    invoke<string>("extracted_text", { relPath }),
  reindex: () => invoke<ScanStats>("reindex"),
  moveFile: (fromRel: string, toRel: string) =>
    invoke<void>("move_file", { fromRel, toRel }),
  createFolder: (relPath: string) => invoke<void>("create_folder", { relPath }),
  /** Returns the FINAL rel path (the name may have been deduped). */
  createDocument: (relPath: string) =>
    invoke<string>("create_document", { relPath }),
  openExternal: (relPath: string) => invoke<void>("open_external", { relPath }),

  /// Copy an external file into a staging area so it can be previewed pre-placement.
  importBegin: (srcPath: string) =>
    invoke<ImportDto>("import_begin", { srcPath }),
  /// Ask the AI where the staged file should live. Never errors; defaults to root.
  importClassify: (importId: string) =>
    invoke<Placement>("import_classify", { importId }),
  /// Place the staged file into a folder and index it; returns its final relPath.
  importCommit: (importId: string, destFolderRel: string, createFolder: boolean) =>
    invoke<string>("import_commit", { importId, destFolderRel, createFolder }),
  /// Discard a staged import (dialog cancelled).
  importCancel: (importId: string) =>
    invoke<void>("import_cancel", { importId }),
  fileMtime: (relPath: string) => invoke<number>("file_mtime", { relPath }),

  /// A webview URL for `<video src>` — asset-protocol stream, supports seeking.
  mediaSrc: (relPath: string) => invoke<string>("media_src", { relPath }),
  videoTranscript: (relPath: string) =>
    invoke<VideoTranscript>("video_transcript", { relPath }),
  /// Kicks off on-device Whisper; the .vtt lands via the `index-updated` event.
  generateTranscript: (relPath: string) =>
    invoke<void>("generate_transcript", { relPath }),

  /// Status of the recommended transcription model (cheap, offline file check).
  modelStatus: () => invoke<ModelStatus>("model_status"),
  /// All downloadable models, discovered from the whisper.cpp repo (cached).
  listModels: () => invoke<ModelStatus[]>("list_models"),
  /// Starts a download; progress/completion arrive via `model-download-progress`.
  downloadModel: (id: string) => invoke<void>("download_model", { id }),
  removeModel: (id: string) => invoke<void>("remove_model", { id }),
  setModelSelection: (category: "transcription" | "language", id: string) =>
    invoke<void>("set_model_selection", { category, id }),

  listIngests: () => invoke<IngestSummary[]>("list_ingests"),
  getIngest: (slug: string) => invoke<IngestDetail>("get_ingest", { slug }),
  saveIngest: (form: IngestForm) => invoke<Recipe>("save_ingest", { form }),
  deleteIngest: (slug: string) => invoke<void>("delete_ingest", { slug }),
  runIngest: (slug: string, full = true) =>
    invoke<void>("run_ingest", { slug, full }),
  cancelRun: (slug: string) => invoke<void>("cancel_run", { slug }),
  approveRun: (runId: number) => invoke<void>("approve_run", { runId }),
  discardRun: (runId: number) => invoke<void>("discard_run", { runId }),

  listAutomations: () => invoke<Automation[]>("list_automations"),
  getAutomation: (slug: string) =>
    invoke<AutomationDetail>("get_automation", { slug }),
  saveAutomation: (form: AutomationForm) =>
    invoke<Automation>("save_automation", { form }),
  deleteAutomation: (slug: string) =>
    invoke<void>("delete_automation", { slug }),
  runAutomation: (slug: string) => invoke<void>("run_automation", { slug }),
  approveAutomationProposal: (itemId: number) =>
    invoke<void>("approve_automation_proposal", { itemId }),
  discardAutomationProposal: (itemId: number) =>
    invoke<void>("discard_automation_proposal", { itemId }),
  pendingApprovals: () => invoke<RunRow[]>("pending_approvals"),
  reviewInbox: () => invoke<ReviewInbox>("review_inbox"),
  resolveReviewItem: (id: number) =>
    invoke<void>("resolve_review_item", { id }),
  /// Silence a file's issues for this user only (app-data, never synced).
  ignoreFile: (relPath: string) =>
    invoke<void>("ignore_file", { relPath }),
  unignoreFile: (relPath: string) =>
    invoke<void>("unignore_file", { relPath }),
  listIgnored: () => invoke<string[]>("list_ignored"),
  /// Files changed by someone/something else since the user last looked (nav
  /// dot + the Files "unread" filter). Per-user, app-data, never synced.
  unreadFiles: () => invoke<string[]>("unread_files"),
  /// Record a file as seen at its current version (on open / "Mark as viewed").
  markSeen: (relPath: string) => invoke<void>("mark_seen", { relPath }),
  /// Mark every currently-unread file seen.
  markAllSeen: () => invoke<void>("mark_all_seen"),
  syncStatus: () => invoke<SyncStatus>("sync_status"),
  setSyncAuto: (auto: boolean) =>
    invoke<SyncStatus>("set_sync_auto", { auto }),
  syncNow: () => invoke<void>("sync_now"),
  resolveConflict: (
    itemId: number,
    resolution: ConflictResolution,
    content?: string,
  ) => invoke<string>("resolve_conflict", { itemId, resolution, content }),
  resolveConflictCopy: (itemId: number, resolution: ConflictCopyResolution) =>
    invoke<string>("resolve_conflict_copy", { itemId, resolution }),
  setIngestRunnerMode: (mode: "hidden-tui" | "headless") =>
    invoke<void>("set_ingest_runner_mode", { mode }),
  /// Whether cloud-offline documents are downloaded + indexed in the background.
  getBackgroundIndex: () => invoke<boolean>("get_background_index"),
  setBackgroundIndex: (enabled: boolean) =>
    invoke<void>("set_background_index", { enabled }),
  claudeDoctor: () => invoke<ClaudeDoctor>("claude_doctor"),
  mcpInfo: () => invoke<McpInfo>("mcp_info"),

  currentDigest: () => invoke<DigestDto | null>("current_digest"),
  refreshDigest: () => invoke<void>("refresh_digest"),
  quickAnswer: (query: string) => invoke<boolean>("quick_answer", { query }),
  llmStatus: () => invoke<"ready" | "notInstalled" | "error">("llm_status"),

  knowledgeModel: () => invoke<KnowledgeModel>("knowledge_model"),
  refreshKnowledgeModel: () => invoke<void>("refresh_knowledge_model"),

  listChats: () => invoke<ChatRow[]>("list_chats"),
  chatTranscript: (chatId: string) =>
    invoke<ChatMessage[]>("chat_transcript", { chatId }),
  createChat: () => invoke<ChatRow>("create_chat"),
  sendChatMessage: (
    chatId: string,
    text: string,
    openFiles: string[],
    focusedFile: string | null,
  ) =>
    invoke<void>("send_chat_message", { chatId, text, openFiles, focusedFile }),
  renameChat: (chatId: string, title: string) =>
    invoke<void>("rename_chat", { chatId, title }),
  setChatPinned: (chatId: string, pinned: boolean) =>
    invoke<void>("set_chat_pinned", { chatId, pinned }),
  setChatModel: (chatId: string, model: string | null) =>
    invoke<void>("set_chat_model", { chatId, model }),
  archiveChat: (chatId: string) => invoke<void>("archive_chat", { chatId }),
  enterTerminalMode: (chatId: string) =>
    invoke<void>("enter_terminal_mode", { chatId }),
  leaveTerminalMode: (chatId: string) =>
    invoke<void>("leave_terminal_mode", { chatId }),
  chatPtyInput: (chatId: string, data: string) =>
    invoke<void>("chat_pty_input", { chatId, data }),
  chatPtyResize: (chatId: string, rows: number, cols: number) =>
    invoke<void>("chat_pty_resize", { chatId, rows, cols }),

  startResearch: (question: string, outputDir: string) =>
    invoke<string>("start_research", { question, outputDir }),
  cancelResearch: (chatId: string) =>
    invoke<void>("cancel_research", { chatId }),
  researchOutputOptions: () => invoke<string[]>("research_output_options"),

  onChatUpdated: (fn: (row: ChatRow) => void): Promise<UnlistenFn> =>
    listen<ChatRow>("chat-updated", (e) => fn(e.payload)),
  onChatMessage: (fn: (msg: ChatMessage) => void): Promise<UnlistenFn> =>
    listen<ChatMessage>("chat-message", (e) => fn(e.payload)),
  onChatPtyData: (fn: (chunk: PtyChunk) => void): Promise<UnlistenFn> =>
    listen<PtyChunk>("chat-pty-data", (e) => fn(e.payload)),

  onIngestRunChanged: (fn: (ev: IngestEvent) => void): Promise<UnlistenFn> =>
    listen<IngestEvent>("ingest-run-changed", (e) => fn(e.payload)),
  onIndexUpdated: (fn: (stats: ScanStats) => void): Promise<UnlistenFn> =>
    listen<ScanStats>("index-updated", (e) => fn(e.payload)),
  onFileSaved: (fn: (relPath: string) => void): Promise<UnlistenFn> =>
    listen<string>("file-saved", (e) => fn(e.payload)),
  onSyncState: (fn: (ev: SyncStateEvent) => void): Promise<UnlistenFn> =>
    listen<SyncStateEvent>("sync-state", (e) => fn(e.payload)),
  onReviewChanged: (fn: () => void): Promise<UnlistenFn> =>
    listen<null>("review-changed", () => fn()),
  onScanError: (fn: (message: string) => void): Promise<UnlistenFn> =>
    listen<string>("scan-error", (e) => fn(e.payload)),
  onDigestUpdated: (fn: (digest: DigestDto) => void): Promise<UnlistenFn> =>
    listen<DigestDto>("digest-updated", (e) => fn(e.payload)),
  onDigestGenerating: (fn: () => void): Promise<UnlistenFn> =>
    listen<null>("digest-generating", () => fn()),
  onDigestError: (fn: (message: string) => void): Promise<UnlistenFn> =>
    listen<string>("digest-error", (e) => fn(e.payload)),
  onQuickAnswer: (fn: (answer: QuickAnswer) => void): Promise<UnlistenFn> =>
    listen<QuickAnswer>("quick-answer", (e) => fn(e.payload)),
  onQuickAnswerDelta: (fn: (ev: QuickAnswerDelta) => void): Promise<UnlistenFn> =>
    listen<QuickAnswerDelta>("quick-answer-delta", (e) => fn(e.payload)),
  onKnowledgeModelState: (
    fn: (ev: KnowledgeModelState) => void,
  ): Promise<UnlistenFn> =>
    listen<KnowledgeModelState>("knowledge-model-state", (e) => fn(e.payload)),
  onKnowledgeUpdated: (fn: () => void): Promise<UnlistenFn> =>
    listen<null>("knowledge-updated", () => fn()),
  onModelDownloadProgress: (
    fn: (ev: ModelProgress) => void,
  ): Promise<UnlistenFn> =>
    listen<ModelProgress>("model-download-progress", (e) => fn(e.payload)),
  onModelDownloadError: (
    fn: (ev: ModelDownloadError) => void,
  ): Promise<UnlistenFn> =>
    listen<ModelDownloadError>("model-download-error", (e) => fn(e.payload)),
};
