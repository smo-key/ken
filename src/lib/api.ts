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
  /** Whether the background hydration worker would download & index this file
   *  (see Rust `wants_background_index`). Only meaningful for `cloud_only`
   *  rows — false for everything already local or otherwise ineligible. */
  backgroundEligible: boolean;
}

export interface SearchHit {
  relPath: string;
  kind: string;
  status: string;
  snippet: string;
  rank: number;
}

/** A hit from `hybrid_search` (keyword FTS + semantic, merged). `source` is
 *  `"keyword"`, `"semantic"`, or `"both"`. `tier` mirrors `chunks.tier`:
 *  `0` = full, `1` = search-only, `null` = unexpected lookup failure (treat
 *  as no badge, same as `0`). */
export interface HybridHit {
  path: string;
  chunkId: number;
  snippet: string;
  source: string;
  tier: number | null;
}

/** Mirrors the Rust `SemanticIndexStateEvent` internally-tagged enum
 *  (`#[serde(tag = "state", rename_all = "camelCase")]`) exactly — each
 *  variant carries only the fields that enum case has. `project_id` is set
 *  when the event was emitted for a specific project/member (S9 step 5). */
export type SemanticIndexState =
  | { state: "building"; done: number; total: number; project_id?: string }
  | { state: "ready"; project_id?: string }
  | { state: "unavailable"; reason: string; project_id?: string }
  | { state: "warning"; reason: string; project_id?: string };

/** Frontend view of `ken_core::profiler::ProjectProfile`, mirroring the Rust
 *  `ProfileDto` (`#[serde(rename_all = "camelCase")]`) — project-profiler
 *  task 3.1. `chunking` and the internal `generated_hash` aren't exposed;
 *  `handEdited` is `ProjectProfile::is_hand_edited()`, true when the on-disk
 *  file has diverged from the hash Ken stamped on its own last write (design
 *  D2) — `profile_project` refuses to overwrite such a profile rather than
 *  clobbering the user's edits. */
export interface ProjectProfile {
  kind: "code" | "docs" | "mixed" | "media";
  summary: string;
  languages: string[];
  excludes: string[];
  focusHints: string[];
  handEdited: boolean;
}

/** Mirrors the Rust `ProfileStateEvent` internally-tagged enum
 *  (`#[serde(tag = "state", rename_all = "camelCase")]`), same shape as
 *  `SemanticIndexState` above. Delivered two ways (project-profiler task
 *  2.1/2.2): scoped to an open member via `emit_member` (`project_id` set,
 *  `path` absent) for `profile_project`, or scoped to a workspace-creation
 *  candidate folder that isn't a project yet (`path` set, `project_id`
 *  absent) for `profile_candidates` — callers tell the two apart by which
 *  key accompanies the event. */
export type ProfileState =
  | { state: "scanning"; project_id?: string; path?: string }
  | { state: "refining"; project_id?: string; path?: string }
  | {
      state: "ready";
      profile: ProjectProfile;
      project_id?: string;
      path?: string;
    }
  | { state: "error"; reason: string; project_id?: string; path?: string };

export interface ScanStats {
  added: number;
  updated: number;
  removed: number;
  failed: number;
  unchanged: number;
  /** Set when `index-updated` was emitted for a specific project/member
   *  (S9 step 5 event envelope); absent on app-global emits. */
  project_id?: string;
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
  /** Set on member-scoped emits of `ingest-run-changed` (S9 step 5). */
  project_id?: string;
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
  /** Set on member-scoped emits of `sync-state` (S9 step 5). */
  project_id?: string;
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
  /** Projects this chat asks about, bound on its first message (schema
   *  v13). null = this project only; "all" = every member; otherwise a
   *  group name. Widens reading, not writing. */
  scope?: string | null;
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
  /** Set on member-scoped emits of `chat-updated` (S9 step 5). */
  project_id?: string;
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
  /** Set on member-scoped emits of `chat-message` (S9 step 5). */
  project_id?: string;
}

export interface PtyChunk {
  chatId: string;
  data: string; // base64
  /** Set on member-scoped emits of `chat-pty-data` (S9 step 5). */
  project_id?: string;
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
  /** Set on member-scoped emits of `digest-updated` (S9 step 5). */
  project_id?: string;
}

/** A ⌘K quick answer, tied to the query it answered. */
export interface QuickAnswer {
  query: string;
  body: string;
  sources: string[];
  /** Set on member-scoped emits of `quick-answer` (S9 step 5). */
  project_id?: string;
}

/** One streamed chunk of a quick answer, tied to its query. */
export interface QuickAnswerDelta {
  query: string;
  delta: string;
  /** Set on member-scoped emits of `quick-answer-delta` (S9 step 5). */
  project_id?: string;
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

/**
 * One OCR text region for the Cmd+F highlight overlay (Phase 3). `bbox` is
 * `[x, y, w, h]`, each normalized to `0..1` with a top-left origin (x grows
 * right, y grows down) — treat it like a CSS/image rectangle. `page` is the
 * 0-based page index (always 0 for a single image).
 */
export interface OcrRegion {
  page: number;
  text: string;
  /** `[x, y, w, h]`, normalized, top-left origin. */
  bbox: [number, number, number, number];
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
  /** Files whose extraction terminally failed (retry budget exhausted). */
  failed: number;
  /** `ready` | `notInstalled` | `error`. */
  llmStatus: "ready" | "notInstalled" | "error";
  llmError: string | null;
}

export interface KnowledgeModelState {
  /** `idle` = an automatic build stopped without a model; not an error. */
  state: "building" | "ready" | "error" | "idle";
  detail: string | null;
  /** Set on member-scoped emits of `knowledge-model-state` (S9 step 5). */
  project_id?: string;
}

/** Entity kinds shared by per-project (`EntityRow`) and workspace-KG global
 *  entities — the proposal's own words: workspace-KG "kinds reuse the
 *  per-project set". Duplicated as a literal union here (rather than
 *  imported from `./knowledge`) to avoid a circular import back into this
 *  module — federated-kg task 3.1. */
type EntityKind = "person" | "organization" | "topic" | "decision" | "other";

/** Mirrors `WorkspaceKgMemberStatusDto` (federated-kg task 2.2) — one open
 *  member's staleness in `workspace_kg_overview`. */
export interface WorkspaceKgMemberStatus {
  projectId: string;
  name: string;
  /** This member's current `knowledge_model_built_at` watermark; `null` =
   *  no knowledge model built yet. */
  currentWatermark: number | null;
  /** True when the workspace-KG's cached snapshot for this member is
   *  missing or behind `currentWatermark` — the next build will re-read it
   *  (spec: "unchanged members are skipped"). */
  stale: boolean;
}

/** Mirrors `WorkspaceKgOverviewDto` (federated-kg task 2.2) — counts +
 *  per-member staleness. */
export interface WorkspaceKgOverview {
  /** `null` before the first build ever completes. */
  builtAt: number | null;
  llmPasses: boolean;
  globalEntities: number;
  entityLinks: number;
  edges: number;
  /** Only currently-open members — no workspace manifest exists yet to
   *  enumerate members that aren't open (see Rust doc comment on
   *  `WorkspaceKgOverviewDto`). */
  members: WorkspaceKgMemberStatus[];
}

/** Mirrors `WorkspaceKgEdgeDto` — one edge in a `workspace_kg_entity` wiki
 *  page, an out-link or a back-link depending which list it's in.
 *  `otherId`/`otherName` always name the OTHER endpoint (design D4:
 *  back-links are `global_edges` queried in reverse, not a separate table). */
export interface WorkspaceKgEdge {
  id: number;
  otherId: number;
  otherName: string;
  relation: string;
  weight: number;
  /** `"imported"` | `"cooccur"` | `"llm"`. */
  provenance: string;
}

/** Mirrors `WorkspaceKgPointerDto` — a `doc_pointers` row: a "mentioned in"
 *  entry pointing into one member's file. */
export interface WorkspaceKgPointer {
  projectId: string;
  relPath: string;
  snippet: string;
  /** `ken://<project-id>/<rel-path>` (design D4 addressing). */
  uri: string;
  /** True if this pointer's file is confirmed missing on disk. Only
   *  checkable for a currently-open member; a pointer into a closed member
   *  is never flagged stale by this field alone (never a crash either way). */
  stale: boolean;
}

/** Mirrors `WorkspaceKgEntityDto` — the full wiki-page payload for one
 *  global entity in a single call (federated-kg task 2.2 / design D4: "the
 *  frontend never joins") — summary, both edge directions, and per-project
 *  doc pointers. */
export interface WorkspaceKgEntity {
  id: number;
  kind: EntityKind;
  name: string;
  summary: string;
  updatedAt: number;
  /** `kg://<id>` (design D4 addressing). */
  uri: string;
  outLinks: WorkspaceKgEdge[];
  backLinks: WorkspaceKgEdge[];
  pointers: WorkspaceKgPointer[];
}

/** Mirrors `WorkspaceKgSearchHitDto` — one `workspace_kg_search` hit. */
export interface WorkspaceKgSearchHit {
  id: number;
  kind: EntityKind;
  name: string;
  summary: string;
  uri: string;
}

/** Mirrors the Rust `WorkspaceKgStateEvent` internally-tagged enum
 *  (`#[serde(tag = "state", rename_all = "camelCase")]`), same shape
 *  convention as `SemanticIndexState` above. `building` is only ever
 *  emitted once up front (`done: 0`) — the ken-core build has no per-member
 *  progress callback yet (see Rust doc comment on `WorkspaceKgStateEvent`),
 *  so `total` (the real member count) is the only progress signal today. */
export type WorkspaceKgState =
  | { state: "building"; done: number; total: number }
  | {
      state: "ready";
      globalEntities: number;
      entityLinks: number;
      importedEdges: number;
      cooccurEdges: number;
      llmEdges: number;
      llmPasses: boolean;
    }
  | { state: "unavailable"; reason: string };

/** Mirrors ken-core's `Memory` (ken-memory task 1.1/2.2) — no `rename_all`
 *  on the Rust struct, so field names pass through unchanged. `extra`
 *  (unknown frontmatter keys, preserved on rewrite) is `#[serde(skip)]` on
 *  the Rust side, so it never reaches the frontend. */
export interface Memory {
  slug: string;
  description: string;
  projects: string[];
  created: string;
  updated: string;
  body: string;
}

/** Mirrors `JournalDayDto` — one day's journal content as returned by
 *  `read_journal`, most-recent-first. */
export interface JournalDay {
  date: string;
  content: string;
}

/** Mirrors ken-core's `DistillCandidate` (design D6) — one proposed
 *  long-term memory awaiting approval. Always workspace-scope (the journal
 *  has no per-project home); `sources` are journal-relative paths (e.g.
 *  `journal/2026-07-24.md`) per `compose_distill_prompt`'s own output
 *  contract, not full `ken://` addresses. */
export interface DistillCandidate {
  slug: string;
  description: string;
  body: string;
  sources: string[];
}

/** Mirrors the Rust `MemoryStateEvent` internally-tagged enum
 *  (`#[serde(tag = "state", rename_all = "camelCase")]`), same shape
 *  convention as `WorkspaceKgState`/`SemanticIndexState` above. App-global
 *  (a distillation run reads the whole workspace journal, no owning
 *  project) — fired by `distill_journal`. */
export type MemoryStateEvent =
  | { state: "planning" }
  | { state: "distilling" }
  | { state: "ready"; candidates: DistillCandidate[] }
  | { state: "error"; reason: string };

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

/** Payload of the `transcript-progress` event. */
export interface TranscriptProgress {
  relPath: string;
  phase: "extracting" | "transcribing";
  /** 0–100; present only while transcribing. */
  pct: number | null;
  /** Set when the emitting project was known (S9 step 5); absent for the
   *  recording-finish path, which falls back to unscoped. */
  project_id?: string;
}

/** Payload of the `hydration-progress` event. */
export interface HydrationProgress {
  relPath: string;
  downloaded: number;
  total: number;
  /** Set on member-scoped emits of `hydration-progress` (S9 step 5). */
  project_id?: string;
}

// ---- Record (on-device meeting recorder) ----

export type RecordPhase = "idle" | "recording" | "paused" | "stopped";
export type PermissionStatus =
  | "granted"
  | "denied"
  | "notDetermined"
  | "unsupported";
export type RecordSourceName = "mic" | "system";
export type RecordStorage = "transcript" | "audio" | "both";

export interface AudioDevice {
  id: string;
  name: string;
}

export interface RecordPermissions {
  mic: PermissionStatus;
  screen: PermissionStatus;
  micSettingsUrl: string;
  screenSettingsUrl: string;
}

export interface RecordLevelEvent {
  source: RecordSourceName;
  rms: number;
}

export interface RecordStateEvent {
  phase: RecordPhase;
  elapsedMs: number;
  mic: boolean;
  system: boolean;
}

export interface RecordSavedEvent {
  relPath: string;
}

export interface RecordErrorEvent {
  message: string;
  canRetry: boolean;
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

/** A registered feature flag with its resolved values, as returned by
 *  `listFeatures`. `projectOverride` is `null` when no project is given, the
 *  flag isn't project-scoped, or the project hasn't set an override. */
export interface FeatureInfo {
  name: string;
  scope: "global" | "project" | "workspace";
  description: string;
  global: boolean;
  projectOverride: boolean | null;
  effective: boolean;
}

/** Payload of the `kenignore-warning` event: 1-based line numbers a `.kenignore`
 *  edit left malformed and skipped. */
export interface KenignoreWarning {
  malformedLines: number[];
  /** Set on member-scoped emits (S9 step 5). */
  project_id?: string;
}

// ---- Workspace (workspace change, task 4.1) ----

/** Mirrors the Rust `Candidate` (`ken_core::workspace`, camelCase) — one
 *  immediate subfolder of a prospective workspace parent, as surfaced by
 *  `discover_workspace_candidates` for the creation checklist. */
export interface Candidate {
  name: string;
  existing: boolean;
  fileCount: number;
  markers: string[];
}

export type WorkspaceMemberStatus = "active" | "dormant" | "missing" | "invalid";

/** Mirrors `WorkspaceMemberDto` — one member's row in `workspace_overview`.
 *  `projectId`/`fileCount` are `null` for `missing`/`invalid` members (no
 *  resolvable `ProjectHandle`); `reason` carries the parse error only for
 *  `invalid`. */
export interface WorkspaceMember {
  /** The manifest key: parent-relative path, so a member inside a group
   *  folder reads `SR/ShatteredRealms`. Identity everywhere — display
   *  through {@link memberLeaf}. */
  name: string;
  projectId: string | null;
  status: WorkspaceMemberStatus;
  reason: string | null;
  fileCount: number | null;
}

/** A member's display name: the last segment of its manifest key.
 *  `SR/ShatteredRealms` → `ShatteredRealms`; a flat member is unchanged. */
export function memberLeaf(name: string): string {
  const slash = name.lastIndexOf("/");
  return slash === -1 ? name : name.slice(slash + 1);
}

/** The group folder a nested member lives in (`SR/ShatteredRealms` →
 *  `SR`), or null for a direct child of the workspace root. */
export function memberGroup(name: string): string | null {
  const slash = name.lastIndexOf("/");
  return slash === -1 ? null : name.slice(0, slash);
}

/** Mirrors `WorkspaceOverviewDto` — the open workspace's manifest header
 *  plus the full member roster, as returned by `open_workspace`,
 *  `create_workspace`, and `workspace_overview`. */
export interface WorkspaceOverview {
  id: string;
  name: string;
  root: string;
  focused: string | null;
  members: WorkspaceMember[];
}

/** Mirrors the Rust `WorkspaceStateEvent` internally-tagged enum
 *  (`#[serde(tag = "state", rename_all = "camelCase")]`), app-global (plain
 *  `app.emit`, no member envelope) — a workspace lifecycle spans every
 *  member at once. */
export type WorkspaceStateEvent =
  | { state: "opening"; name: string }
  | { state: "open"; id: string; name: string }
  | { state: "focus"; projectId: string }
  | { state: "closed" };

/** Mirrors the Rust `MemberStatusEvent` internally-tagged enum
 *  (`#[serde(tag = "status", rename_all = "camelCase")]`), delivered
 *  through `emit_member` — the envelope adds `project_id` (snake_case, same
 *  convention every other member-scoped event in this file already uses,
 *  e.g. `ScanStats.project_id`). Only the two *runtime* transitions a
 *  resolvable member goes through fire this event; `missing`/`invalid`
 *  members are reported through `workspace_overview` instead. */
export type MemberStatusEvent =
  | { status: "active"; name: string; project_id: string }
  | { status: "dormant"; name: string; project_id: string };

/** One `search_all_projects` hit (`AllProjectsHitDto`, task 3.3) — a plain
 *  `SearchHit` labeled with its owning member. */
export interface AllProjectsHit extends SearchHit {
  projectId: string;
  memberName: string;
}

export type AllProjectsMemberSearchStatus =
  | "searched"
  | "dormant"
  | "missing"
  | "invalid";

/** One member's outcome in `search_all_projects` — honest per-member
 *  coverage, mirroring `RouteSearchResult`'s `memberStatus`. */
export interface AllProjectsMemberStatus {
  projectId: string | null;
  memberName: string;
  status: AllProjectsMemberSearchStatus;
}

/** Mirrors `SearchAllProjectsDto` — the all-projects keyword fan-out's
 *  return shape. */
export interface SearchAllProjectsResult {
  results: AllProjectsHit[];
  memberStatus: AllProjectsMemberStatus[];
}

// ---- kg-routing (kg-routing change, task 4.1) ----

/** Mirrors the Rust `RouteReasonDto` internally-tagged enum
 *  (`#[serde(tag = "type", rename_all = "camelCase")]`) — why `plan_route`
 *  picked its targets. */
export type RouteReason =
  | { type: "named" }
  | { type: "kgEntities"; entityIds: number[] }
  | { type: "broadcast" };

/** Mirrors `RoutePlanDto` — `targets` are stringified project ids. */
export interface RoutePlan {
  targets: string[];
  reason: RouteReason;
}

/** Mirrors `RoutedHitDto` — one merged, cited hit from `route_search`.
 *  `source` is `"keyword"` | `"semantic"` | `"both"` (same vocabulary as
 *  `HybridHit.source`); `kgBreadcrumbs` is `kg://<entity-id>` per entity
 *  that selected the plan (plan-level, not per-hit — see the Rust doc
 *  comment on `RoutedHitDto`), empty unless the plan's reason was
 *  `kgEntities`. */
export interface RoutedHit {
  path: string;
  chunkId: number;
  snippet: string;
  source: string;
  projectId: string;
  memberName: string;
  /** `ken://<project-id>/<rel-path>`. */
  address: string;
  kgBreadcrumbs: string[];
}

export type RouteMemberStatus = "searched" | "index-building" | "unavailable";

/** Mirrors `MemberStatusEntryDto` — one member's outcome in a `route_search`
 *  call. */
export interface RouteMemberStatusEntry {
  projectId: string;
  memberName: string;
  status: RouteMemberStatus;
}

/** Mirrors `RouteSearchDto` — `route_search`'s return shape: the plan, the
 *  cross-member RRF-merged cited results, and per-member coverage. */
export interface RouteSearchResult {
  plan: RoutePlan;
  results: RoutedHit[];
  memberStatus: RouteMemberStatusEntry[];
}

/** Mirrors `CandidateDto` — a sibling folder not yet in the workspace.
 *  `existing` means it already has `.ken/project.json` and will be
 *  adopted rather than created fresh. */
export interface WorkspaceCandidate {
  name: string;
  existing: boolean;
  fileCount: number;
  markers: string[];
}

/** Mirrors `ProjectGroupDto` — a named set of members that belong
 *  together despite being separate repos. `members` are parent-relative
 *  folder names; `projectIds` are the resolvable ones a scoped search
 *  actually targets (shorter than `members` when one can't resolve). */
export interface ProjectGroup {
  name: string;
  members: string[];
  projectIds: string[];
}

/** One member's stored digest for the day, parsed. Mirrors
 *  `MemberDigestDto`. */
export interface MemberDigest {
  projectId: string;
  name: string;
  body: string;
  sources: string[];
}

/** A member with no digest stored for the day — named rather than
 *  dropped, so Home never silently omits a project. */
export interface MemberAwaitingDigest {
  projectId: string;
  name: string;
}

/** Mirrors `WorkspaceDigestDto`. `board` is null when `kenPipeline` is
 *  off. `hasContent` is false when nothing has been written and the board
 *  is empty — `awaiting` alone is something to explain, not to render. */
export interface WorkspaceDigest {
  date: string;
  members: MemberDigest[];
  awaiting: MemberAwaitingDigest[];
  hasContent: boolean;
  board: PipelineDigestDto | null;
}

/** Mirrors `MemberOverviewDto`. `status` is `ok` | `missing` | `invalid`;
 *  `missing`/`invalid` members carry no project id or counts, and are
 *  surfaced nowhere else in the app. */
export interface MemberOverview {
  folder: string;
  status: "ok" | "missing" | "invalid";
  detail: string | null;
  projectId: string | null;
  name: string | null;
  resident: boolean;
  indexReady: boolean;
  fileCount: number;
  failedFiles: number;
  unread: number;
}

/** Mirrors the Rust `RoutedSearchStateEvent` internally-tagged enum
 *  (`#[serde(tag = "state", rename_all = "camelCase")]`), app-global like
 *  `WorkspaceStateEvent` — a routed search spans every planned member at
 *  once, no single owning project. */
export type RoutedSearchStateEvent =
  | { state: "planning" }
  | { state: "searching"; done: number; total: number }
  | { state: "done" };

// ---- ken-tasks (ken-tasks change, task 4.1) ----

export type TaskStatus = "backlog" | "todo" | "doing" | "review" | "done";
export type TaskKind = "human" | "ai";
export type BoardKind = "main" | "daily";
export type GoalStatus = "active" | "done" | "dropped";
export type TaskHomeKind = "workspace" | "project" | "family";

/** Mirrors the Rust `AssigneeFilter` enum — no explicit `tag`/`content` on
 *  the Rust side, so serde's default *externally tagged* representation
 *  applies: the unit variant `Unassigned` serializes as the bare string
 *  `"unassigned"`; the newtype variant `Named(String)` as `{ named: "..." }`. */
export type AssigneeFilter = "unassigned" | { named: string };

/** Mirrors `ken_core::tasks::TaskFilter` (camelCase, every field optional —
 *  absent means "don't filter on this"). Shared shape for `task_list` and
 *  the Tasks tab's own client-side filtering (`tasks.svelte.ts`). */
export interface TaskFilter {
  status?: TaskStatus;
  project?: string;
  tag?: string;
  assignee?: AssigneeFilter;
  kind?: TaskKind;
  goal?: string;
  board?: BoardKind;
}

/** Mirrors `ken_core::tasks::TaskPatch` — every field absent means "leave
 *  alone"; there is no "remove key" variant (clearing a value means setting
 *  it to `""`). Drag-drop sends only `{ status }` (S6 byte-fidelity: a
 *  patch must never touch a key it didn't mean to change). */
export interface TaskPatch {
  title?: string;
  status?: TaskStatus;
  kind?: TaskKind;
  assignee?: string;
  project?: string;
  tags?: string[];
  due?: string;
  goal?: string;
  board?: BoardKind;
}

/** Mirrors `ken_core::tasks::Task` (camelCase). `path`/`homeDir` are
 *  absolute OS paths (Rust `PathBuf`) — never rendered raw in the UI, only
 *  used to derive a project-relative path for opening the file (see
 *  `tasks.svelte.ts`'s `openFile`). `statusRaw`/`kindRaw`/`boardRaw` carry
 *  the on-disk string verbatim even when it's out of vocabulary — what
 *  drives the needs-attention tray. */
export interface Task {
  id: string;
  title: string;
  status: TaskStatus | null;
  statusRaw: string;
  /** ken-pipeline D2: the resolved lane id, or `null` for a ticket with no
   *  `pipeline:` key (the classic path is untouched). */
  lane: string | null;
  kind: TaskKind;
  kindRaw: string;
  assignee: string;
  project: string;
  tags: string[];
  due: string | null;
  goal: string | null;
  board: BoardKind;
  boardRaw: string;
  created: string;
  updated: string;
  body: string;
  path: string;
  home: TaskHomeKind;
  homeDir: string;
}

/** Mirrors the Rust `AttentionReason` enum (`#[serde(tag = "reason",
 *  content = "value", rename_all = "camelCase")]`). */
export type AttentionReason =
  | { reason: "invalidStatus"; value: string }
  | { reason: "invalidKind"; value: string }
  | { reason: "invalidBoard"; value: string }
  | { reason: "unknownGoal"; value: string }
  // ken-pipeline task 1.4/4.9: `pipeline:` names a definition that isn't
  // loaded; the ticket's `status` matches no lane in its pipeline; a
  // `blocked_by` id matching no ticket; the ticket is in the blocked lane
  // but its `return_lane` is missing/orphaned (empty string = missing).
  | { reason: "unknownPipeline"; value: string }
  | { reason: "unknownLane"; value: string }
  | { reason: "unknownBlocker"; value: string }
  | { reason: "unknownReturnLane"; value: string };

/** Mirrors `ken_core::tasks::NeedsAttention`. */
export interface NeedsAttention {
  id: string;
  title: string;
  path: string;
  reasons: AttentionReason[];
}

/** Mirrors `ken_core::tasks::Goal` — no assignee, no claim lifecycle. */
export interface Goal {
  id: string;
  title: string;
  status: GoalStatus | null;
  statusRaw: string;
  created: string;
  updated: string;
  body: string;
  path: string;
}

/** Mirrors `ken_core::tasks::GoalPatch`. */
export interface GoalPatch {
  title?: string;
  status?: GoalStatus;
}

/** Mirrors `ken_core::tasks::Progress` — derived goal progress (done/total),
 *  never stored in any file. */
export interface Progress {
  done: number;
  total: number;
}

// ---- ken-pipeline (ken-pipeline change, task 4.1) ----
//
// Mirrors `crates/ken-core/src/pipeline.rs` (camelCase throughout — every
// type there derives `#[serde(rename_all = "camelCase")]`) plus the
// `src-tauri/src/lib.rs` command DTOs built on top of it. `pipeline_board`/
// `board_get`/`board-state` all return the SAME `BoardStateDto` shape (the
// pipeline fields are simply empty when `kenPipeline` is off or a board has
// no pipeline tickets), so `tasksStore`'s existing `board-state` stream is
// the live source for `pipelines`/`pipelineFields`/`pipelineLaneCounts`/
// `blocked` too — `pipeline.svelte.ts` reads `tasksStore.board` rather than
// keeping a second copy in sync.

export type PipelineKickoff = "manual" | "confirm" | "auto";
export type PipelineRunner = "mcp" | "command";
export type PipelineTarget = "web" | "tauri" | "none";

/** Mirrors the Rust `Lane` struct. */
export interface PipelineLane {
  id: string;
  name: string;
  mapsTo: TaskStatus;
  mapsToRaw: string;
  agent: string | null;
  model: string | null;
  kickoff: PipelineKickoff;
  kickoffRaw: string;
  onPass: string | null;
  onFail: string | null;
  writesCode: boolean;
  human: boolean;
  terminal: boolean;
  generative: boolean;
  blocked: boolean;
  runner: PipelineRunner;
}

/** Mirrors the Rust `Pipeline` struct — one loaded definition file. Lane
 *  order (`lanes`) IS column order (D1); the frontend never re-sorts it. */
export interface Pipeline {
  id: string;
  name: string;
  auto: boolean;
  concurrencyCap: number;
  bounceCap: number;
  lanes: PipelineLane[];
  body: string;
  path: string;
}

/** Mirrors the Rust `PipelineIssue` enum (`tag = "issue"`) —
 *  `validate_pipeline`'s findings, surfaced by `pipeline_list_defs`. */
export type PipelineIssue =
  | { issue: "noLanes" }
  | { issue: "laneMissingId"; index: number }
  | { issue: "duplicateLaneId"; id: string }
  | { issue: "multipleBlockedLanes"; first: string; second: string }
  | { issue: "multipleHumanLanes"; first: string; second: string }
  | { issue: "invalidMapsTo"; lane: string; value: string }
  | { issue: "invalidKickoff"; lane: string; value: string }
  | { issue: "unknownTransition"; lane: string; edge: string; target: string };

/** Mirrors `PipelineDefDto` (`#[serde(flatten)] def: Pipeline` + `issues`) —
 *  `pipeline_list_defs`'s return shape. */
export interface PipelineDefDto extends Pipeline {
  issues: PipelineIssue[];
}

/** Mirrors the Rust `TicketFields` struct — the ken-pipeline half of a
 *  ticket's frontmatter, keyed by ticket id in `BoardStateDto.pipelineFields`
 *  (only pipeline tickets — `task.lane != null` — get an entry; `Task`
 *  itself carries no `scope`/`verify`/`model`/etc., only `lane`). */
export interface TicketFields {
  pipeline: string | null;
  model: string | null;
  agent: string | null;
  scope: string[];
  verify: string | null;
  bounces: number;
  returnLane: string | null;
  blockedBy: string[];
  blockReason: string | null;
  blockedAt: string | null;
  parent: string | null;
  spawnedBy: string | null;
  origin: string | null;
  projects: string[];
  target: PipelineTarget;
  targetRaw: string;
}

/** Mirrors `BlockedSummaryDto` — one row of `BoardStateDto.blocked`.
 *  `rootBlockers` is `pipeline::root_blockers` — the root of the dependency
 *  chain, not the nearest link (D5's "the subtle part"). */
export interface BlockedSummaryDto {
  ticketId: string;
  title: string;
  project: string;
  returnLane: string | null;
  blockedBy: string[];
  blockReason: string | null;
  blockedAt: string | null;
  rootBlockers: string[];
}

export type PipelineRunOutcome = "queued" | "running" | "pass" | "fail" | "blocked" | "cancelled";

/** Mirrors the Rust `RunRecord` — one append-only ledger entry
 *  (`.ken-workspace/runs/YYYY-MM/<ulid>.md`, D13). */
export interface PipelineRunRecord {
  id: string;
  ticket: string;
  pipeline: string;
  lane: string;
  agent: string;
  model: string;
  scope: string[];
  verify: string;
  started: string;
  ended: string;
  outcome: PipelineRunOutcome | null;
  outcomeRaw: string;
  artifacts: string[];
  report: string;
}

/** Mirrors `RunQueue` — the `pipeline_runs` command's return shape and the
 *  `pipeline-runs` event payload. `waitingHuman` is ticket ids sitting at a
 *  confirmation gate (derived from `admit`, not ledger data). */
export interface PipelineRunQueue {
  running: PipelineRunRecord[];
  queued: PipelineRunRecord[];
  blocked: PipelineRunRecord[];
  stale: PipelineRunRecord[];
  waitingHuman: string[];
}

/** Mirrors `PipelineBlockersDto` — `pipeline_blockers`'s return shape.
 *  `direct` is the ticket's own `blocked_by`; `chain` is every id between
 *  the ticket and its root blocker(s), root first. */
export interface PipelineBlockersDto {
  ticketId: string;
  direct: string[];
  chain: string[];
}

export interface DigestAwaitingReviewDto {
  ticketId: string;
  title: string;
  updated: string;
  runCount: number;
}
export interface DigestUnblockedDto {
  ticketId: string;
  title: string;
  returnLane: string;
}
export interface DigestBlockedDto {
  ticketId: string;
  title: string;
  blockedAt: string | null;
  rootBlockers: string[];
  blockReason: string | null;
  runCount: number;
}
export interface DigestMovedDto {
  ticketId: string;
  title: string;
  lane: string;
  runCount: number;
}
export interface DigestIdeaDto {
  ticketId: string;
  title: string;
  spawnedBy: string | null;
}

/** Mirrors `PipelineDigestDto` — `pipeline_digest`'s return shape. Group
 *  order matches the spec: awaiting review, then newly unblocked, then
 *  blocked (oldest-first), then moved-today, new ideas, stale runs.
 *  `markdown` is `render_digest_markdown`'s output — the one renderer
 *  chat/MCP/`journal_append` all use, included so the frontend never
 *  re-derives its own markdown from the structured groups. */
export interface PipelineDigestDto {
  awaitingReview: DigestAwaitingReviewDto[];
  newlyUnblocked: DigestUnblockedDto[];
  blocked: DigestBlockedDto[];
  movedToday: DigestMovedDto[];
  newIdeas: DigestIdeaDto[];
  staleRuns: PipelineRunRecord[];
  markdown: string;
}

/** Mirrors `ConfirmReason` (`tag = "reason"`) — why `pipeline_kickoff`
 *  returned `needsConfirm` rather than starting immediately. */
export type PipelineConfirmReason =
  | { reason: "laneGate" }
  | { reason: "manualKickoff" }
  | { reason: "missingBoundary"; scope: boolean; verify: boolean }
  | { reason: "unblocked" }
  | { reason: "autoDisabled" };

/** Mirrors `RefusalReason` (`tag = "reason"`) — why work was refused
 *  outright. NOTE: `humanLane`/`noAgent`/`manualLane` are Rust newtype
 *  variants (`HumanLane(String)`) under a *bare* `tag = "reason"` (no
 *  `content`) — serde's internally-tagged representation only supports
 *  struct-shaped variant content, so these three may fail to serialize at
 *  all (a live IPC error, not a typed payload) rather than arriving as the
 *  shape below. Flagged as an observation for the ken-core owner, not fixed
 *  here (crates/** is out of this session's touch scope) — every call site
 *  in this build treats a `pipelineKickoff` rejection as an opaque string
 *  (`String(err)`), so this is not a hard blocker for the UI. */
export type PipelineRefusalReason =
  | { reason: "blocked"; returnLane: string | null; blockedBy: string[]; blockReason: string | null }
  | { reason: "humanLane"; lane: string }
  | { reason: "noAgent"; lane: string }
  | { reason: "manualLane"; lane: string };

/** Mirrors `PipelineKickoffOutcome` (`tag = "kind"`) — `pipeline_kickoff`'s
 *  verdict. `needsConfirm` is the confirmation gate's own payload (D3: "a
 *  dialog showing lane, agent, model, scope, and verify command"); calling
 *  `pipelineKickoff(id, true)` again after the human accepts proceeds. */
export type PipelineKickoffOutcome =
  | { kind: "queued"; runId: string; ready: boolean; running: number; cap: number }
  | {
      kind: "needsConfirm";
      reason: PipelineConfirmReason;
      lane: string;
      agent: string | null;
      model: string | null;
      scope: string[];
      verify: string | null;
    }
  | { kind: "refused"; reason: PipelineRefusalReason };

export type PipelineAdvanceOutcome = "pass" | "fail";

/** Mirrors the Rust `TransitionRefusal` enum (`tag = "refusal"`).
 *  `unknownLane` is a `String` newtype variant under a bare `tag =
 *  "refusal"` — the same internally-tagged-newtype serialization risk
 *  flagged on `PipelineRefusalReason` above; shaped defensively. */
export type PipelineTransitionRefusal =
  | ({ refusal: "unknownLane" } & Record<string, unknown>)
  | { refusal: "noEdge"; lane: string; outcome: PipelineAdvanceOutcome }
  | { refusal: "unknownTarget"; lane: string; target: string }
  | { refusal: "noBlockedLane" };

/** Mirrors the Rust `Transition` enum (`tag = "kind"`) — `pipeline_advance`'s
 *  resolved move. `blocked` is D4's retry-cap escalation (there is no
 *  separate "halted" state) — `wouldHaveEntered` is the lane it was
 *  bouncing to, which becomes `return_lane`. `patch` (the raw `TaskPatch`
 *  written) is present on the wire but untyped here — the UI reads `task`
 *  from `PipelineAdvanceDto` for the post-transition ticket instead of
 *  re-deriving anything from the patch. */
export type PipelineTransition =
  | { kind: "moved"; from: string; to: string; backward: boolean; bounces: number; patch: unknown; log: string }
  | {
      kind: "blocked";
      from: string;
      wouldHaveEntered: string;
      block: { returnLane: string; blockedBy: string[]; reason: string | null; blockedAt: string };
      patch: unknown;
      log: string;
    }
  | { kind: "refused"; reason: PipelineTransitionRefusal };

/** Mirrors `PipelineAdvanceDto` — `pipeline_advance`'s return shape. */
export interface PipelineAdvanceDto {
  task: Task;
  transition: PipelineTransition;
}

/** Mirrors the Rust `BlockRequest` (every field optional/defaulted;
 *  `now` is always server-stamped — see `pipeline_block`'s doc comment —
 *  so the frontend never sends it). */
export interface PipelineBlockRequest {
  blockedBy?: string[];
  reason?: string;
}

/** Mirrors the Rust `UnblockRequest` — clearing dependencies and the reason
 *  are independent (D5: clearing one must not unblock a ticket the other
 *  still applies to). */
export interface PipelineUnblockRequest {
  clearDeps?: boolean;
  clearReason?: boolean;
}

export type PipelineSignoffDecision = "accept" | "acceptWithComments" | "reject";

/** Mirrors `PipelineSignoffDto` — `child` is `Some` only for
 *  `acceptWithComments`. */
export interface PipelineSignoffDto {
  parent: Task;
  child: Task | null;
}

/** Mirrors `PipelineIdeaOutcome` (`tag = "kind"`) — `pipeline_propose_idea`'s
 *  verdict. */
export type PipelineIdeaOutcome = { kind: "landed"; task: Task } | { kind: "nearDuplicate"; ticketId: string; score: number };

/** Mirrors `ArtifactManifestDto` (`#[serde(flatten)] manifest:
 *  ArtifactManifest` + `expired`) — `pipeline_artifacts`'s return shape.
 *  `durable` is always `false` server-side (D9: "there is no field to set
 *  it any other way"). */
export interface PipelineArtifactManifestDto {
  ticket: string;
  created: string;
  expires: string;
  files: string[];
  expired: boolean;
}

/** Mirrors the Rust `BoardStateDto` — `board_get`/`pipeline_board`'s return
 *  shape and the `board-state` event payload. `progress` is keyed by goal
 *  id. The four `pipeline*`/`blocked` fields are always present (never
 *  `undefined`) but empty when `kenPipeline` is off or the board has no
 *  pipeline tickets — a `kenTasks`-only build simply never reads them. */
export interface BoardStateDto {
  tasks: Task[];
  goals: Goal[];
  needsAttention: NeedsAttention[];
  progress: Record<string, Progress>;
  pipelines: Pipeline[];
  pipelineFields: Record<string, TicketFields>;
  pipelineLaneCounts: Record<string, Record<string, number>>;
  blocked: BlockedSummaryDto[];
}

/** Mirrors the Rust `Rollover` enum (`#[serde(rename_all = "camelCase")]`)
 *  — the three per-task daily-rollover resolutions (design D5), never
 *  auto-applied. */
export type Rollover = "roll" | "promote" | "archive";

/** Mirrors the Rust `DailyCandidate` — one drafted daily-board item awaiting
 *  approval (not yet a file); `key` is a run-local handle for
 *  `resolveDailyCandidate`. */
export interface DailyCandidate {
  key: string;
  title: string;
  body: string;
  project: string | null;
  tags: string[];
}

/** Mirrors the Rust `DailyPlanStateEvent` internally-tagged enum
 *  (`#[serde(tag = "state", rename_all = "camelCase")]`), same shape
 *  convention as `MemoryStateEvent`. App-global — a planning run reads the
 *  whole workspace journal, no single owning project. */
export type DailyPlanStateEvent =
  | { state: "planning" }
  | { state: "ready"; candidates: DailyCandidate[] }
  | { state: "error"; reason: string };

// ---- ken-families (ken-families change, task 4.1/4.2/4.3) ----

/** Mirrors the Rust `FamilyConnection` (camelCase) — one saved connection's
 *  settings half; live sync status comes separately in
 *  `FamilyConnectionDto.state`. */
export interface FamilyConnection {
  familyId: string;
  name: string;
  remoteUrl: string;
  memberId: string;
  liveSync: boolean;
  pollIntervalSecs: number;
  attachedWorkspaceId: string | null;
}

/** Mirrors `ken_core::family_sync::ConnectionState` (`#[serde(tag = "state",
 *  rename_all = "camelCase")]`) — externally tagged on a `state` field, with
 *  each non-unit variant's own fields sitting alongside it. `Conflict` and
 *  `Unavailable` are terminal-until-a-human-acts (see design.md D1/D7):
 *  `Conflict` offers `familyResolveConflict`, `Unavailable` offers nothing. */
export type FamilyConnectionState =
  | { state: "idle" }
  | { state: "syncing" }
  | { state: "conflict"; detail: string }
  | { state: "error"; detail: string }
  | { state: "unavailable"; reason: string };

/** Mirrors `FamilyConnectionDto` — one connection's settings plus its live
 *  `SyncEngine` state, as `familyList`/`familyCreate`/`familyJoin` return it. */
export interface FamilyConnectionDto {
  connection: FamilyConnection;
  state: FamilyConnectionState;
}

/** Mirrors `ken_core::family_sync::IntegrateOutcome`
 *  (`#[serde(tag = "outcome", rename_all = "camelCase")]`). */
export type FamilyIntegrateOutcome =
  | { outcome: "upToDate" }
  | { outcome: "fastForward"; commits: number }
  | { outcome: "rebased"; commits: number }
  | { outcome: "conflict"; detail: string };

/** Mirrors `ken_core::family_sync::PushOutcome` (same tagging convention). */
export type FamilyPushOutcome =
  | { outcome: "upToDate" }
  | { outcome: "pushed"; commits: number }
  | { outcome: "nonFastForward"; detail: string }
  | { outcome: "failed"; detail: string };

/** Mirrors `ken_core::family_sync::SyncReport` — one poll/sync-now cycle's
 *  outcome, the payload behind the tray badge and the settings page's "last
 *  sync" line. */
export interface FamilySyncReport {
  state: FamilyConnectionState;
  ran: boolean;
  integrated: FamilyIntegrateOutcome | null;
  pushed: FamilyPushOutcome | null;
  pushRetried: boolean;
}

/** The `family-sync` app event — emitted after every poll tick and every
 *  on-demand command that touches a connection's transport. `unreadInboxCount`
 *  is THIS device's own inbox count for that family (task 2.3's diff-able
 *  count, not a stateful server-side delta). */
export interface FamilySyncEvent {
  familyId: string;
  report: FamilySyncReport;
  unreadInboxCount: number;
}

/** Mirrors `ken_core::family::FamilyMember`. */
export interface FamilyMember {
  id: string;
  name: string;
}

/** Mirrors `ken_core::family::FamilyManifest` (plain field names, no
 *  camelCase rename needed — every field is already a single lowercase
 *  word). */
export interface FamilyManifest {
  id: string;
  name: string;
  template: number;
  members: FamilyMember[];
}

export type FamilyInboxKind = "task" | "message" | "notification";
export type FamilyInboxStatus = "unread" | "seen" | "accepted" | "archived";

/** Mirrors `ken_core::family::InboxTaskPayload` (`#[serde(default)]`, plain
 *  field names). */
export interface FamilyInboxTaskPayload {
  title: string;
  project: string;
  tags: string[];
  due: string;
  kind: string;
}

/** Mirrors `ken_core::family::InboxItem` (camelCase). `kind`/`status` are
 *  `null` when the file's raw value is outside the vocabulary —
 *  `kindRaw`/`statusRaw` always carry what was actually on disk, and
 *  `malformed` marks a file whose frontmatter block couldn't be parsed at
 *  all (D4: "shown raw in the tray, never crash, never be rewritten"). */
export interface FamilyInboxItem {
  id: string;
  kind: FamilyInboxKind | null;
  kindRaw: string;
  from: string;
  status: FamilyInboxStatus | null;
  statusRaw: string;
  created: string;
  updated: string;
  title: string;
  task: FamilyInboxTaskPayload | null;
  body: string;
  malformed: boolean;
  fileName: string;
}

export const api = {
  listProjects: () => invoke<RegistryEntryStatus[]>("list_projects"),
  createProject: (path: string, name: string) =>
    invoke<ProjectInfo>("create_project", { path, name }),
  openProject: (path: string) => invoke<ProjectInfo>("open_project", { path }),
  /** Open an additional project alongside whatever is already open, without
   *  closing it. Requires the `workspace` feature flag — rejects with
   *  `"workspace disabled"` when it's off. */
  openMember: (path: string) => invoke<ProjectInfo>("open_member", { path }),
  /** Close one workspace member, leaving the others open. Same `workspace`
   *  flag gate as `openMember`. */
  closeMember: (projectId: string) =>
    invoke<void>("close_member", { projectId }),

  // ---- Workspace (workspace change, task 4.1) ----
  /** Open an existing `.ken-workspace/` manifest at `parent`. Flag-gated on
   *  `workspace`; activates every resolvable member up to the resident cap
   *  and restores the last-focused member. Progress rides `workspace-state`
   *  + `member-status`. */
  openWorkspace: (parent: string) =>
    invoke<WorkspaceOverview>("open_workspace", { parent }),
  /** Create a new workspace manifest over `parent` from the selected member
   *  folder names, then open it (same activation path as `openWorkspace`). */
  createWorkspace: (parent: string, name: string, members: string[]) =>
    invoke<WorkspaceOverview>("create_workspace", { parent, name, members }),
  /** Members + per-member status + counts for the currently open workspace. */
  workspaceOverview: () => invoke<WorkspaceOverview>("workspace_overview"),
  /** Switch focus to `id`, activating a dormant member (LRU-evicting past
   *  the resident cap) without closing any other member. Emits
   *  `workspace-state`'s `focus` variant. */
  focusProject: (id: string) => invoke<void>("focus_project", { id }),
  /** Immediate subfolder candidates under `parent` for the workspace-creation
   *  checklist — one level deep, tagged `existing`/`new` + file count +
   *  repo markers. */
  discoverWorkspaceCandidates: (parent: string) =>
    invoke<Candidate[]>("discover_workspace_candidates", { parent }),
  /** Close the open workspace, tearing down every member's runtime. */
  closeWorkspace: () => invoke<void>("close_workspace"),
  /** Keyword FTS fan-out over every ACTIVE workspace member, merged by
   *  round-robin rank-position interleave and labeled per hit (design D6 —
   *  BM25 scores aren't comparable across corpora). Requires `workspace`;
   *  upgrades to `routeSearch` when `kgRouting` is also on (kg-routing
   *  proposal: "same UI slot, richer results"). */
  searchAllProjects: (query: string, limit = 30) =>
    invoke<SearchAllProjectsResult>("search_all_projects", { query, limit }),
  forgetProject: (id: string) => invoke<void>("forget_project", { id }),
  renameProject: (id: string, name: string) =>
    invoke<ProjectInfo>("rename_project", { id, name }),
  lastProjectId: () => invoke<string | null>("last_project_id"),
  currentProject: () => invoke<ProjectInfo | null>("current_project"),
  setFolderSelection: (excluded: string[]) =>
    invoke<ProjectInfo>("set_folder_selection", { excluded }),
  getTree: () => invoke<TreeData>("get_tree"),
  /** The whole workspace as one tree: every member's files and folders,
   *  each path prefixed with the member's folder name. Same shape as
   *  getTree, so FileTree renders it unchanged. */
  getTreeAll: () => invoke<TreeData>("get_tree_all"),
  search: (query: string, limit = 30) =>
    invoke<SearchHit[]>("search", { query, limit }),
  /** Keyword FTS merged with semantic (when the `semanticIndex` feature is
   *  on for the project); transparently degrades to FTS-only results when
   *  it's off, so callers can always route through this instead of `search`. */
  hybridSearch: (query: string, limit = 30) =>
    invoke<HybridHit[]>("hybrid_search", { query, limit }),
  /** Route `query` across every open workspace member (kg-routing task 4.1):
   *  plan (Named/KG-guided/Broadcast), fan out hybrid search over the
   *  targets, merge with cross-member RRF, and return cited `ken://`/
   *  `kg://` addresses. Requires `kgRouting`. Progress rides
   *  `routed-search-state`. */
  routeSearch: (
    query: string,
    limit = 30,
    scope?: string | null,
    group?: string | null,
  ) =>
    invoke<RouteSearchResult>("route_search", {
      query,
      limit,
      scope: scope ?? null,
      group: group ?? null,
    }),
  /** Named groups of members, stored in the workspace manifest. */
  workspaceGroups: () => invoke<ProjectGroup[]>("workspace_groups"),
  workspaceSetGroup: (name: string, members: string[]) =>
    invoke<ProjectGroup[]>("workspace_set_group", { name, members }),
  workspaceRemoveGroup: (name: string) =>
    invoke<ProjectGroup[]>("workspace_remove_group", { name }),
  /** Sibling folders under the workspace root that aren't members yet. */
  workspaceCandidates: () => invoke<WorkspaceCandidate[]>("workspace_candidates"),
  /** Join an existing sibling folder to the open workspace. It lands
   *  dormant and opens on first focus. */
  workspaceAddMember: (folder: string) =>
    invoke<MemberOverview[]>("workspace_add_member", { folder }),
  /** Folders dismissed as "not a project" (world data, vendored source). */
  workspaceIgnored: () => invoke<string[]>("workspace_ignored"),
  workspaceIgnoreCandidate: (folder: string) =>
    invoke<string[]>("workspace_ignore_candidate", { folder }),
  workspaceUnignoreCandidate: (folder: string) =>
    invoke<string[]>("workspace_unignore_candidate", { folder }),
  /** Every manifest member's already-stored digest for the day plus the
   *  pipeline board summary. Composes only — never generates, schedules,
   *  or refreshes a member's digest. Requires `workspace`. */
  workspaceDigest: (day?: string) =>
    invoke<WorkspaceDigest>("workspace_digest", { day: day ?? null }),
  /** Per-member state for Home's members strip, covering EVERY manifest
   *  member including unresolvable ones. Requires `workspace`. */
  workspaceMembersOverview: () =>
    invoke<MemberOverview[]>("workspace_members_overview"),
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
  /**
   * Stored OCR regions for a file (images / scanned PDFs), in reading order —
   * the input to the Cmd+F highlight overlay. OCR runs in the background, so a
   * freshly added image may return `[]` until the worker finishes it; the
   * `index-updated` event fires when new OCR text lands.
   */
  getOcrRegions: (relPath: string) =>
    invoke<OcrRegion[]>("get_ocr_regions", { relPath }),
  reindex: () => invoke<ScanStats>("reindex"),
  moveFile: (fromRel: string, toRel: string) =>
    invoke<void>("move_file", { fromRel, toRel }),
  /// Move a file OR folder to the OS trash (recoverable — not a permanent delete).
  deleteFile: (relPath: string) => invoke<void>("delete_file", { relPath }),
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
  cancelRun: (slug: string, kind: "ingest" | "automation" = "ingest") =>
    invoke<void>("cancel_run", { slug, kind }),
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
  /// Whether the semantic (meaning-based) index is enabled for the active project.
  getSemanticIndex: () => invoke<boolean>("get_semantic_index"),
  /** Toggle a project-scoped feature flag on the active project. Paired
   *  with `getSemanticIndex` above for `semanticIndex`, mirroring the
   *  `getBackgroundIndex`/`setBackgroundIndex` pair (see
   *  `onSemanticIndexState` for build/availability updates). */
  setProjectFeature: (flag: string, value: boolean) =>
    invoke<void>("set_project_feature", { flag, value }),
  /** Set a feature flag's global default (`settings.json`). Project-scoped
   *  flags fall back to this value when a project has no override. */
  setGlobalFeature: (flag: string, value: boolean) =>
    invoke<void>("set_global_feature", { flag, value }),
  /** All registered feature flags, resolved for `projectId` (or with no
   *  project context when omitted — used by onboarding before a project
   *  exists). One call feeds both the onboarding disclosure and Settings. */
  listFeatures: (projectId?: string) =>
    invoke<FeatureInfo[]>("list_features", { projectId }),
  /** Profile one open member (deterministic scan, then optional
   *  Background-priority local-LLM refinement), written to
   *  `.ken/index-profile.json`. Gated server-side on the project-scoped
   *  `profiler` flag and rejects while a profile of this project is already
   *  running; `projectId` omitted profiles the focused member. Returns as
   *  soon as the background pass starts — progress arrives via
   *  `onProfileState` events keyed by `project_id` (project-profiler task
   *  2.1). */
  profileProject: (projectId?: string) =>
    invoke<void>("profile_project", { projectId }),
  /** Profile workspace-creation candidate folders that aren't projects yet
   *  (concurrency 2, per-candidate failures never block the others) —
   *  project-profiler task 2.2. Progress arrives via `onProfileState`
   *  events keyed by `path` instead of `project_id`. */
  profileCandidates: (paths: string[]) =>
    invoke<void>("profile_candidates", { paths }),
  /// Whether videos are auto-transcribed on-device (Whisper) during indexing.
  getTranscribeOnIndex: () => invoke<boolean>("get_transcribe_on_index"),
  setTranscribeOnIndex: (enabled: boolean) =>
    invoke<void>("set_transcribe_on_index", { enabled }),
  claudeDoctor: () => invoke<ClaudeDoctor>("claude_doctor"),
  mcpInfo: () => invoke<McpInfo>("mcp_info"),

  currentDigest: () => invoke<DigestDto | null>("current_digest"),
  refreshDigest: () => invoke<void>("refresh_digest"),
  quickAnswer: (query: string) => invoke<boolean>("quick_answer", { query }),
  llmStatus: () => invoke<"ready" | "notInstalled" | "error">("llm_status"),
  /// Fire-and-forget: warm the on-device model (⌘K open) so the first answer
  /// streams without paying the load. No-op when no local model is installed.
  warmLlm: () => invoke<void>("warm_llm"),

  knowledgeModel: () => invoke<KnowledgeModel>("knowledge_model"),
  refreshKnowledgeModel: () => invoke<void>("refresh_knowledge_model"),

  /** Manually rebuild the workspace knowledge graph now (federated-kg task
   *  3.1). Returns as soon as the build thread is spawned; progress and
   *  outcome arrive via `onWorkspaceKgState` events. Rejects if the
   *  `federatedKg` flag is off, or a build is already running. */
  rebuildWorkspaceKg: () => invoke<void>("rebuild_workspace_kg"),
  /** Workspace-KG summary: counts + per-member staleness. Rejects with the
   *  flag-off error before `kg.sqlite` is ever opened. */
  workspaceKgOverview: () =>
    invoke<WorkspaceKgOverview>("workspace_kg_overview"),
  /** The full wiki-page payload for one global entity — summary, out-links,
   *  back-links, and per-project doc pointers in one call. */
  workspaceKgEntity: (id: number) =>
    invoke<WorkspaceKgEntity>("workspace_kg_entity", { id }),
  /** Case-insensitive substring search over global entity names + summaries
   *  (name matches ranked above summary-only matches), capped at 50 hits.
   *  An empty/whitespace-only query always returns `[]` (no "list all"). */
  workspaceKgSearch: (query: string) =>
    invoke<WorkspaceKgSearchHit[]>("workspace_kg_search", { query }),

  // ---- Memory (ken-memory task 4.1) ----
  /** Create (`"create"`, slug must not exist) or replace (`"replace"`, slug
   *  must exist) a memory at workspace scope (`.ken-workspace/memory/`) or
   *  project scope (the focused member's `.ken/memory/`). Flag-gated on
   *  `kenMemory`; rejects with a friendly message when the flag is off. */
  memoryWrite: (
    scope: "workspace" | "project",
    slug: string,
    content: string,
    mode: "create" | "replace",
  ) => invoke<Memory>("memory_write", { scope, slug, content, mode }),
  /** Append a `## HH:MM` entry to today's journal file, creating it (and
   *  `journal/`) if absent — how agent-desktop reports task findings back
   *  via `ken-mcp`'s twin tool. */
  journalAppend: (text: string, project?: string, tags?: string[]) =>
    invoke<void>("journal_append", { text, project, tags }),
  /** Recent journal days, most-recent-first, checking `journal/archive/` too
   *  (spec: "archived journal stays findable"). `daysBack` omitted = today
   *  only; a day with no file on either side is skipped, not an error. */
  readJournal: (daysBack?: number) =>
    invoke<JournalDay[]>("read_journal", { daysBack }),
  /** Kick off a distillation pass over the current journal window. Returns
   *  as soon as the background thread starts; progress/outcome arrive via
   *  `onMemoryState` (`planning` → `distilling` → `ready`/`error`).
   *  Rejects if a run is already in progress. */
  distillJournal: () => invoke<void>("distill_journal"),
  /** Approve (writes the candidate via `memoryWrite` in create mode, always
   *  workspace scope) or dismiss (records the slug so it isn't re-proposed)
   *  a distillation candidate by slug — looked up from the last
   *  `distillJournal` run's server-side cache. */
  resolveDistillCandidate: (slug: string, approve: boolean) =>
    invoke<void>("resolve_distill_candidate", { slug, approve }),
  /** `memory-state`: `planning` → `distilling` → `ready` (candidates) |
   *  `error` (reason). App-global like `onWorkspaceKgState`. */
  onMemoryState: (fn: (ev: MemoryStateEvent) => void): Promise<UnlistenFn> =>
    listen<MemoryStateEvent>("memory-state", (e) => fn(e.payload)),

  listChats: () => invoke<ChatRow[]>("list_chats"),
  chatTranscript: (chatId: string) =>
    invoke<ChatMessage[]>("chat_transcript", { chatId }),
  createChat: () => invoke<ChatRow>("create_chat"),
  /** `scope`: null = this project only; `"all"` = every workspace member;
   *  anything else = a group name. Widens what the session may READ —
   *  edits stay pinned to the focused project. */
  sendChatMessage: (
    chatId: string,
    text: string,
    openFiles: string[],
    focusedFile: string | null,
    scope?: string | null,
  ) =>
    invoke<void>("send_chat_message", {
      chatId,
      text,
      openFiles,
      focusedFile,
      scope: scope ?? null,
    }),
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
  onSemanticIndexState: (
    fn: (ev: SemanticIndexState) => void,
  ): Promise<UnlistenFn> =>
    listen<SemanticIndexState>("semantic-index-state", (e) => fn(e.payload)),
  /** Fires for both `profile_project` (project_id-keyed) and
   *  `profile_candidates` (path-keyed) — see `ProfileState`. */
  onProfileState: (fn: (ev: ProfileState) => void): Promise<UnlistenFn> =>
    listen<ProfileState>("profile-state", (e) => fn(e.payload)),
  /** Fires when a `.kenignore` edit has malformed lines. */
  onKenignoreWarning: (
    fn: (ev: KenignoreWarning) => void,
  ): Promise<UnlistenFn> =>
    listen<KenignoreWarning>("kenignore-warning", (e) => fn(e.payload)),
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
  /** `workspace-kg-state`: `building` (once, up front) → `ready` |
   *  `unavailable` (federated-kg task 3.1). App-global — a workspace-KG
   *  build spans every open member, so unlike `onKnowledgeModelState` there
   *  is no `project_id` to filter on. */
  onWorkspaceKgState: (
    fn: (ev: WorkspaceKgState) => void,
  ): Promise<UnlistenFn> =>
    listen<WorkspaceKgState>("workspace-kg-state", (e) => fn(e.payload)),
  /** `workspace-state`: `opening` → `open` | `focus` | `closed`
   *  (workspace task 4.1). App-global — a workspace lifecycle spans every
   *  member at once, so there's no `project_id` to filter on. */
  onWorkspaceState: (
    fn: (ev: WorkspaceStateEvent) => void,
  ): Promise<UnlistenFn> =>
    listen<WorkspaceStateEvent>("workspace-state", (e) => fn(e.payload)),
  /** `member-status`: one resolvable member going `active` (gained a live
   *  runtime) or `dormant` (evicted / lazy-deferred). Envelope carries
   *  `project_id` like every other member-scoped event in this file. */
  onMemberStatus: (fn: (ev: MemberStatusEvent) => void): Promise<UnlistenFn> =>
    listen<MemberStatusEvent>("member-status", (e) => fn(e.payload)),
  /** `routed-search-state`: `planning` → `searching m/n` → `done`
   *  (kg-routing task 4.1). App-global like `onWorkspaceState`. */
  onRoutedSearchState: (
    fn: (ev: RoutedSearchStateEvent) => void,
  ): Promise<UnlistenFn> =>
    listen<RoutedSearchStateEvent>("routed-search-state", (e) => fn(e.payload)),
  onModelDownloadProgress: (
    fn: (ev: ModelProgress) => void,
  ): Promise<UnlistenFn> =>
    listen<ModelProgress>("model-download-progress", (e) => fn(e.payload)),
  onModelDownloadError: (
    fn: (ev: ModelDownloadError) => void,
  ): Promise<UnlistenFn> =>
    listen<ModelDownloadError>("model-download-error", (e) => fn(e.payload)),
  onTranscriptProgress: (
    fn: (ev: TranscriptProgress) => void,
  ): Promise<UnlistenFn> =>
    listen<TranscriptProgress>("transcript-progress", (e) => fn(e.payload)),
  onHydrationProgress: (
    fn: (ev: HydrationProgress) => void,
  ): Promise<UnlistenFn> =>
    listen<HydrationProgress>("hydration-progress", (e) => fn(e.payload)),

  // ---- Record ----
  recordInputDevices: () => invoke<AudioDevice[]>("record_input_devices"),
  recordPermissions: () => invoke<RecordPermissions>("record_permissions"),
  /**
   * Ask for a permission, then re-read the current status. The mic prompt is
   * async (fire-and-forget on the Rust side: its completion block does nothing),
   * so the returned snapshot may still be `notDetermined` right after — callers
   * should also re-poll `recordPermissions()` on window focus.
   */
  recordRequestPermission: async (
    kind: "mic" | "screen",
  ): Promise<RecordPermissions> => {
    await invoke<void>("record_request_permission", { kind });
    return invoke<RecordPermissions>("record_permissions");
  },
  recordStart: (mic: boolean, system: boolean, deviceId: string | null) =>
    invoke<void>("record_start", { mic, system, deviceId }),
  recordPause: () => invoke<void>("record_pause"),
  recordResume: () => invoke<void>("record_resume"),
  recordStop: (storage: RecordStorage) =>
    invoke<void>("record_stop", { storage }),
  recordCancel: () => invoke<void>("record_cancel"),
  /**
   * Open a macOS System Settings privacy deep link. Routed through Rust because
   * the frontend opener capability scope forbids the `x-apple.systempreferences:`
   * scheme.
   */
  openSettingsUrl: (url: string) =>
    invoke<void>("record_open_settings", { url }),

  onRecordLevel: (
    fn: (ev: RecordLevelEvent) => void,
  ): Promise<UnlistenFn> =>
    listen<RecordLevelEvent>("record-level", (e) => fn(e.payload)),
  onRecordState: (
    fn: (ev: RecordStateEvent) => void,
  ): Promise<UnlistenFn> =>
    listen<RecordStateEvent>("record-state", (e) => fn(e.payload)),
  onRecordTranscribing: (fn: () => void): Promise<UnlistenFn> =>
    listen<null>("record-transcribing", () => fn()),
  onRecordSaved: (
    fn: (ev: RecordSavedEvent) => void,
  ): Promise<UnlistenFn> =>
    listen<RecordSavedEvent>("record-saved", (e) => fn(e.payload)),
  onRecordError: (
    fn: (ev: RecordErrorEvent) => void,
  ): Promise<UnlistenFn> =>
    listen<RecordErrorEvent>("record-error", (e) => fn(e.payload)),

  // ---- ken-tasks (ken-tasks change, task 4.1) ----
  /** Create a task file — workspace home by default, or `projectId`'s
   *  `<project>/.ken/tasks/` when given. `fields.status` omitted defaults to
   *  `backlog`, the intake column. Flag-gated on `kenTasks`. */
  taskCreate: (title: string, body?: string, fields?: TaskPatch, projectId?: string) =>
    invoke<Task>("task_create", { title, body, fields, projectId }),
  /** List tasks across every home, optionally filtered — the same
   *  `TaskFilter` shape the board UI mirrors client-side. */
  taskList: (filter?: TaskFilter) => invoke<Task[]>("task_list", { filter }),
  /** Patch a task by id. Drag-drop sends only `{ status }` — the S6
   *  byte-fidelity contract depends on patches naming only the keys that
   *  actually changed; never send a whole-task patch. */
  taskUpdate: (id: string, patch: TaskPatch) => invoke<Task>("task_update", { id, patch }),
  /** Set `done`, append `report` under `## Log`, and (when `kenMemory` is
   *  on) write a one-line journal summary. This is the agent/MCP-facing
   *  completion path; the board's own drag-to-done interaction uses
   *  `taskUpdate({ status: "done" })` instead, per task 4.3. */
  taskComplete: (id: string, report: string) =>
    invoke<Task>("task_complete", { id, report }),
  /** Move a task file to `<its own home>/archive/YYYY-MM/`. */
  taskArchive: (id: string) => invoke<Task>("task_archive", { id }),
  /** The whole board on demand — the Tasks tab's initial load and manual
   *  refresh; live updates after that arrive via `onBoardState`. */
  boardGet: () => invoke<BoardStateDto>("board_get"),
  /** Create a goal (workspace home only — goals have no per-repo home). */
  goalCreate: (title: string, body?: string, status?: GoalStatus) =>
    invoke<Goal>("goal_create", { title, body, status }),
  /** Patch a goal by id — title and/or status only. */
  goalUpdate: (id: string, patch: GoalPatch) => invoke<Goal>("goal_update", { id, patch }),
  /** Every goal in the workspace home. */
  goalList: () => invoke<Goal[]>("goal_list"),
  /** Draft daily-board candidates from recent journal content + activity
   *  ("plan my day" — on request only, never autonomous). Returns as soon
   *  as the background pass starts; progress/outcome arrive via
   *  `onDailyPlanState`. Rejects if a run is already in progress. */
  planDailyTasks: () => invoke<void>("plan_daily_tasks"),
  /** Approve (creates the `board: daily` task file, workspace home unless
   *  `projectId` names a member) or dismiss a drafted candidate by `key`,
   *  looked up from the last `planDailyTasks` run's server-side cache. */
  resolveDailyCandidate: (key: string, approve: boolean, projectId?: string) =>
    invoke<Task | null>("resolve_daily_candidate", { key, approve, projectId }),
  /** Daily tasks eligible for the new-day rollover prompt (`board: daily`,
   *  not done, `updated` before today). Purely derived — safe to call
   *  repeatedly (e.g. after each per-task resolution). */
  dailyRolloverCandidates: () => invoke<Task[]>("daily_rollover_candidates"),
  /** Apply one task's rollover resolution: roll forward (bump `updated`
   *  only), promote to the main board, or archive. Never auto-called — one
   *  explicit choice per task. */
  resolveDailyRollover: (id: string, choice: Rollover) =>
    invoke<Task>("resolve_daily_rollover", { id, choice }),

  /** `board-state`: the whole board, re-emitted after every mutating
   *  command and on every watcher-detected external change (agent writes,
   *  hand edits, `git pull`). App-global — the board aggregates every home,
   *  with no single owning project. */
  onBoardState: (fn: (dto: BoardStateDto) => void): Promise<UnlistenFn> =>
    listen<BoardStateDto>("board-state", (e) => fn(e.payload)),
  /** `daily-plan-state`: `planning` → `ready` (candidates) | `error`
   *  (reason), driven by `planDailyTasks`. */
  onDailyPlanState: (fn: (ev: DailyPlanStateEvent) => void): Promise<UnlistenFn> =>
    listen<DailyPlanStateEvent>("daily-plan-state", (e) => fn(e.payload)),

  // ---- ken-families (ken-families change, task 4.1/4.2/4.3) ----
  /** Scaffold + commit a brand-new family repo whose remote is `remoteUrl`
   *  (an empty repo the user already created on their git host); the
   *  creator becomes the family's first — and owner — member. Rejects with
   *  a friendly message when `kenFamilies` is off or `git` is unavailable. */
  familyCreate: (name: string, memberName: string, remoteUrl: string) =>
    invoke<FamilyConnectionDto>("family_create", { name, memberName, remoteUrl }),
  /** Clone an existing family. Pass `existingMemberId` when you're already
   *  in the manifest, or `newMemberName` to be appended (join appends,
   *  never rewrites — D2). */
  familyJoin: (remoteUrl: string, existingMemberId?: string, newMemberName?: string) =>
    invoke<FamilyConnectionDto>("family_join", { remoteUrl, existingMemberId, newMemberName }),
  /** Every saved connection with its live sync state. */
  familyList: () => invoke<FamilyConnectionDto[]>("family_list"),
  /** The full manifest (member roster, owner, template version) for one
   *  connection — a settings-page convenience read. */
  familyManifestGet: (familyId: string) => invoke<FamilyManifest>("family_manifest_get", { familyId }),
  /** Forget a connection (stops its poller, drops the cached engine,
   *  detaches the pseudo-member). Never deletes the on-disk clone. */
  familyRemove: (familyId: string) => invoke<void>("family_remove", { familyId }),
  /** Toggle live sync for one connection; starts/stops its poller. */
  familySetLiveSync: (familyId: string, liveSync: boolean) =>
    invoke<void>("family_set_live_sync", { familyId, liveSync }),
  /** Change one connection's poll interval in seconds (clamped server-side
   *  to design.md D1's 30s–30min bounds). */
  familySetPollInterval: (familyId: string, secs: number) =>
    invoke<void>("family_set_poll_interval", { familyId, secs }),
  /** Run the fetch → rebase-integrate → push cycle on demand ("Sync now"). */
  familySyncNow: (familyId: string) => invoke<FamilySyncReport>("family_sync_now", { familyId }),
  /** Clear a `Conflict` state after the user has resolved the clone by
   *  hand (D1: "never auto-resolve"). No-op on any other state. */
  familyResolveConflict: (familyId: string) => invoke<void>("family_resolve_conflict", { familyId }),
  /** Attach a connection to a workspace — the clone joins search as a
   *  `kind: family` member once that workspace is open. */
  familyAttachWorkspace: (familyId: string, workspaceId: string) =>
    invoke<void>("family_attach_workspace", { familyId, workspaceId }),
  /** Detach a connection from its workspace; drops the pseudo-member if
   *  it's currently resident. */
  familyDetachWorkspace: (familyId: string) => invoke<void>("family_detach_workspace", { familyId }),
  /** This device's own inbox for one family (`members/<me>/inbox/`). */
  familyInboxList: (familyId: string) => invoke<FamilyInboxItem[]>("family_inbox_list", { familyId }),
  /** Patch one inbox item's status — `seen`/`archived` only;
   *  `accepted` is reserved for `familyAcceptTask`. */
  familySetItemStatus: (familyId: string, itemId: string, status: FamilyInboxStatus) =>
    invoke<FamilyInboxItem>("family_set_item_status", { familyId, itemId, status }),
  /** Accept a `task` inbox item (D4's acceptance gate): mints a new board
   *  task in `members/<me>/board/` and marks the inbox item `accepted`, in
   *  one commit. The ONLY way an incoming task can ever enter the board —
   *  there is no auto-accept path anywhere in this API. */
  familyAcceptTask: (familyId: string, itemId: string) =>
    invoke<Task>("family_accept_task", { familyId, itemId }),
  /** Push back on an inbox item: creates a new message item in the
   *  SENDER's inbox (lane rule 2) and leaves the original item's status
   *  untouched — call `familySetItemStatus` separately if you also want to
   *  mark the original seen/archived. */
  familyPushBack: (familyId: string, itemId: string, note: string) =>
    invoke<void>("family_push_back", { familyId, itemId, note }),
  /** `family-sync`: emitted after every poll tick and every on-demand
   *  command that touches a connection's transport. App-global, like
   *  `board-state` — a family connection has no single owning project. */
  onFamilySync: (fn: (ev: FamilySyncEvent) => void): Promise<UnlistenFn> =>
    listen<FamilySyncEvent>("family-sync", (e) => fn(e.payload)),

  // ---- ken-pipeline (ken-pipeline change, task 4.1) ----
  /** Every loaded pipeline definition plus its `validate_pipeline` findings
   *  — the natural place to discover a bad definition file. */
  pipelineListDefs: () => invoke<PipelineDefDto[]>("pipeline_list_defs"),
  /** Lane-ordered board state on demand — same `BoardStateDto` shape
   *  `board_get`/`board-state` give, gated specifically on `kenPipeline`
   *  (not just `kenTasks`). Most UI reads should prefer `tasksStore.board`
   *  (already live via `board-state`) over calling this directly. */
  pipelineBoard: () => invoke<BoardStateDto>("pipeline_board"),
  /** The run ledger's derived queue view on demand; live updates arrive via
   *  `onPipelineRuns`. */
  pipelineRuns: () => invoke<PipelineRunQueue>("pipeline_runs"),
  /** The resolved blocker chain for one ticket, root first. */
  pipelineBlockers: (ticketId: string) => invoke<PipelineBlockersDto>("pipeline_blockers", { ticketId }),
  /** The daily update — grouped, markdown-rendered, and (when `kenMemory`
   *  is on) journaled server-side on every call. `day` defaults to today. */
  pipelineDigest: (day?: string) => invoke<PipelineDigestDto>("pipeline_digest", { day }),
  /** The confirmation gate (D3). First call (or `confirmed: false`) with a
   *  `Confirm` verdict returns `needsConfirm` and writes nothing; call again
   *  with `confirmed: true` once the human accepts to actually queue the
   *  run. `Start`/`Queued`/an already-accepted `Confirm` all write a run
   *  record with `outcome: queued` (D14: Ken never itself distinguishes
   *  "start" from "queue" — `ready` says whether the cap is free). */
  pipelineKickoff: (ticketId: string, confirmed: boolean) =>
    invoke<PipelineKickoffOutcome>("pipeline_kickoff", { ticketId, confirmed }),
  /** Resolve `on_pass`/`on_fail`, apply bounce accounting (a cap breach
   *  blocks the ticket instead — D4), append `report` to the ticket's
   *  `## Log`, and close whichever run is currently open for the ticket.
   *  `artifacts` names which QA-lane outputs landed under
   *  `.ken-workspace/artifacts/<ticket-id>/` for this run (D9). */
  pipelineAdvance: (ticketId: string, outcome: PipelineAdvanceOutcome, report: string, artifacts?: string[]) =>
    invoke<PipelineAdvanceDto>("pipeline_advance", { ticketId, outcome, report, artifacts }),
  /** Cancel a still-open (`queued`/`running`) run; refused once the run is
   *  already closed (the ledger is append-only history). */
  pipelineCancelRun: (runId: string) => invoke<void>("pipeline_cancel_run", { runId }),
  /** Block a ticket — routes through `pipeline::block` server-side, so
   *  cycle detection and `return_lane` capture (captured from the ticket's
   *  CURRENT lane, never caller-supplied) cannot be bypassed. A cycle
   *  refusal rejects with the offending path in the error message. */
  pipelineBlock: (ticketId: string, request: PipelineBlockRequest) =>
    invoke<Task>("pipeline_block", { ticketId, request }),
  /** Clear a block's dependencies and/or reason independently (D5) — both
   *  hold coexist, so clearing one alone may leave the ticket
   *  `stillBlocked`; the returned `Task` reflects whichever happened. */
  pipelineUnblock: (ticketId: string, request: PipelineUnblockRequest) =>
    invoke<Task>("pipeline_unblock", { ticketId, request }),
  /** The human sign-off lane's own review action (D11) — accept / accept
   *  with comments (spawns a `todo`-lane child with `parent` set, in the
   *  same action the parent advances) / reject (the lane's `on_fail` edge,
   *  counted as a bounce). */
  pipelineSignoff: (ticketId: string, decision: PipelineSignoffDecision, comment?: string) =>
    invoke<PipelineSignoffDto>("pipeline_signoff", { ticketId, decision, comment }),
  /** Propose a documentation-lane idea citing `ticketId` (D7's required
   *  `spawned_by`); deduped against in-scope tickets before landing — a
   *  match above threshold appends a log note instead of creating a
   *  ticket. */
  pipelineProposeIdea: (ticketId: string, title: string, body: string) =>
    invoke<PipelineIdeaOutcome>("pipeline_propose_idea", { ticketId, title, body }),
  /** One ticket's artifact manifest (D9), or `null` if the folder doesn't
   *  exist yet. */
  pipelineArtifacts: (ticketId: string) => invoke<PipelineArtifactManifestDto | null>("pipeline_artifacts", { ticketId }),
  /** Lazily create `artifacts/<ticket-id>/` + its manifest on first use, or
   *  append `filename` to an existing manifest (idempotent). Every path
   *  this writes is rooted under `.ken-workspace/artifacts/<ticket-id>/` —
   *  structurally unable to land inside a member repo (D9). */
  pipelineRegisterArtifact: (ticketId: string, filename: string) =>
    invoke<PipelineArtifactManifestDto>("pipeline_register_artifact", { ticketId, filename }),
  /** The human-invoked prune action (OPEN-5/D9) — Ken never calls this on a
   *  timer; expired artifact folders are only ever surfaced, never
   *  auto-deleted. */
  pipelinePruneArtifacts: (ticketId: string) => invoke<void>("pipeline_prune_artifacts", { ticketId }),
  /** `pipeline-runs`: the run queue, re-emitted alongside `board-state`
   *  from the same recompute (task 2.2) so the two events never drift out
   *  of sync with each other. */
  onPipelineRuns: (fn: (queue: PipelineRunQueue) => void): Promise<UnlistenFn> =>
    listen<PipelineRunQueue>("pipeline-runs", (e) => fn(e.payload)),
};
