// Typed wrappers over Tauri commands + events. The only file that talks IPC.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface ProjectInfo {
  id: string;
  name: string;
  root: string;
  excluded: string[];
  /** Left over from the recipe runner; nothing reads it. */
  ingestRunner?: "hidden-tui" | "headless";
}

/** The team's index as three figures: repos indexed of all, files queued,
 *  files failed. */
export interface IndexHealth {
  indexed: number;
  total: number;
  queued: number;
  failed: number;
}

/** A repo's index state: not indexed, searchable only, read for entities. */
export type IndexState = "off" | "search" | "entities";

/** Mirrors `setup::RepoRow`: one repo the set-up scan found. */
export interface SetupRepoRow {
  member: string;
  include: boolean;
  kind: RepoKind[];
  team: string | null;
  index: IndexState;
  evidence: string[];
  remote: string | null;
  existing: boolean;
  hasGit: boolean;
  /** The repo's folder (always set when repos are picked one by one). */
  path: string | null;
  /** What it is for and how it is used; first proposed from its README. */
  description: string;
}

/** Mirrors `wikinew::Covered`: a repo a new wiki or team repo names. */
export interface CoveredRepo {
  name: string;
  description: string;
  kind?: RepoKind[];
  path?: string | null;
}

/** Mirrors `setup::IgnoreRow`: one ignore line the scan would add. */
export interface SetupIgnoreRow {
  pattern: string;
  state: IndexState;
  reason: string;
  evidence: string;
  ticked: boolean;
  /** Shown, never a choice (built in). */
  fixed: boolean;
}

export interface SetupProposal {
  folder: string;
  repos: number;
  withoutGit: number;
  rows: SetupRepoRow[];
  teams: string[];
  ignores: SetupIgnoreRow[];
  existingWorkspace: boolean;
}

export type SetupMoved =
  | { change: "NewFolder"; member: string; evidence: string[] }
  | { change: "Gone"; member: string }
  | { change: "NewWorktree"; pattern: string; evidence: string };

/** One repo of a team, as set-up wrote it (the Team screen's rows). */
export interface TeamRepo {
  id: string;
  name: string;
  path: string;
  kind: RepoKind[];
  index: IndexState | null;
  effectiveIndex: IndexState;
  description: string;
  available: boolean;
  branch: string | null;
  head: string | null;
  behind: number | null;
  files: number;
}

/** Something a repo lacks for its kind, or a team lacks. */
export interface TeamGap {
  repo: string | null;
  text: string;
  open: string | null;
}

export interface TeamPage {
  path: string;
  title: string;
}

/** Something the wiki's own checks found: a page drifted from its source
 *  or unverified for thirty days, the link report, the first draft. */
export interface TeamFinding {
  /** drift · aged · links · draft, as the backend names it. */
  kind: string;
  title: string;
  detail: string;
  /** The page to open, inside `projectId`'s repo. */
  path: string | null;
  projectId: string | null;
}

/** The Team screen: the chosen team's repos and configuration. */
export interface TeamOverview {
  team: string | null;
  workspace: string;
  root: string;
  repos: TeamRepo[];
  wiki: TeamRepo | null;
  gaps: TeamGap[];
  ignores: string[];
  sweep: DriftRun | null;
  rules: TeamPage[];
  templates: TeamPage[];
  findings: TeamFinding[];
}

/** What one write from an ingested note was. */
export type IngestWriteKind = "edit" | "page" | "idea" | "escalation" | "task";

/** Where a source in Raw/ is. `transcribing`: a recording being turned into
 *  text; `waiting`: it needs a transcript Ken cannot make yet (`detail` says
 *  why); `failed`: Ken could not read it (`detail` says why). */
export type RawState = "queued" | "transcribing" | "waiting" | "reading" | "read" | "failed";

/** One source in the library's Raw/ folder and where it is in the read. */
export interface RawSource {
  path: string;
  name: string;
  state: RawState;
  detail: string | null;
  /** meeting | recording | document, or "" when not known yet. */
  kind: string;
  /** "48 min", "14 pages" once read; the file's size before. */
  length: string;
  present: string[];
  /** Its card, once read. */
  cardId: number | null;
  /** One kind per write from it still in place. */
  written: IngestWriteKind[];
  /** What waits from it. */
  waiting: number;
}

/** One ingested source: its note, and whether its card is still open. */
export interface IngestedSource {
  id: number;
  note: string;
  title: string;
  kind: string;
  length: string;
  present: string[];
  at: number;
  open: boolean;
  /** One kind per write from it still in place. */
  written: IngestWriteKind[];
  /** What waits from it: rulings, tickets, held edits. */
  waiting: number;
}

/** The Ingest screen: the team library's inbox and its history. */
export interface IngestOverview {
  projectId: string;
  library: string;
  raw: RawSource[];
  ingested: IngestedSource[];
  running: boolean;
  claudeFound: boolean;
  /** Sources with something waiting: the Ingest tab's count. */
  waiting: number;
  /** Why the last read stopped before the end, until the next starts. */
  stopped: string | null;
}

/** A note's key takeaways. */
export interface IngestTakeaways {
  title: string;
  kind: string;
  present: string[];
  length: string;
  summary: string;
  keyTakeaways: string[];
  overturns: string;
  contradictions: string[];
  rulings: string[];
  actions: string[];
  escalations: string[];
  nextSteps: string[];
}

/** What waits from a note: a ruling, a ticket, or an edit staging held
 *  ("page" a change to one, "new page"). */
export interface IngestProposal {
  id: number;
  kind: "page" | "new page" | "ruling" | "ticket";
  title: string;
  page: string;
  body: string;
  /** The proposal (page as it is and with the change), for the diff. */
  payload: string | null;
  /** A ruling's decider, when the note names one. */
  decider: string | null;
  /** A ruling only its decider may accept. */
  canAccept: boolean;
}

/** One write from a note, with Open and Undo on its card. */
export interface IngestWrite {
  index: number;
  kind: IngestWriteKind;
  path: string;
  label: string;
  to: string | null;
  undone: boolean;
  /** The member the file is in; null for a task on Your day. */
  projectId: string | null;
}

/** Something a note calls for that stays on the card only. */
export interface IngestListed {
  kind: "idea" | "escalation" | "next step" | "ticket";
  text: string;
  to?: string | null;
}

/** One ingested source for its card on the Ingest screen. */
export interface IngestCard {
  id: number;
  open: boolean;
  /** Undo all ran; the source waits in Raw until it is read again. */
  undone: boolean;
  note: string;
  source: string;
  takeaways: IngestTakeaways;
  writes: IngestWrite[];
  proposals: IngestProposal[];
  listed: IngestListed[];
  me: string | null;
}

/** What Undo all did. */
export interface IngestUndoReport {
  noteRemoved: boolean;
  /** Writes left in place because their file changed since. */
  kept: string[];
}

/** Mirrors `drift::DriftRun`: one standing sweep. */
export interface DriftRun {
  at: number;
  /** 0 clean · 1 a Finding or a broken instrument · 2 drift only. */
  exitCode: number;
  pagesExamined: number;
  rulingsExamined: number;
  codeCitations: number;
  crossReferences: number;
  mismatches: {
    subject: string;
    citation: string;
    severity: "autoRecleared" | "judgment" | "finding";
    detail: string;
    research: boolean;
  }[];
  aged: [string, string | null][];
  voidReason: string | null;
  uncontrolled: boolean;
  unmeasured: string[];
  branches: string[];
  /** Citations measured with git this sweep, and those carried from the
   *  last sweep because no commit since touched their file. */
  measured: number;
  reused: number;
}

/** Who a search is for (item 2b); null is any reader. */
export type Audience = "business" | "dev" | null;

/** What a file is for (`contenttype::of`); every search hit carries one. */
export type ContentType = "code" | "test" | "spec" | "doc" | "config" | "data" | "design" | "meeting" | "ticket";

/** One definition in a code file's outline. Lines are 1-based. */
export interface CodeSymbol {
  path: string;
  name: string;
  kind: string;
  line: number;
  endLine: number;
  isDef: boolean;
  docs: string | null;
  depth: number;
}

export interface CodeFile {
  outline: CodeSymbol[];
  related: { imports: string[]; external: string[]; importedBy: string[] };
}

/** A definition or use of a symbol, in some workspace project. */
export interface CodeUse {
  projectId: string;
  memberName: string;
  path: string;
  line: number;
  kind: string;
  within: string | null;
}

export interface CodeUsages {
  definitions: CodeUse[];
  uses: CodeUse[];
}

/** A search's kind filter: one kind, several comma-separated, or null for all. */
export type KindFilter = string | null;

/** What a repo is for; decides sync and how deep Ken reads it. */
export type RepoKind = "team" | "wiki" | "code" | "reference";

export interface RegistryEntryStatus {
  id: string;
  name: string;
  path: string;
  available: boolean;
  /** Absent until someone says what the repo is. */
  kind?: RepoKind[];
  team?: string;
  /** Set only when a person chose other than what the kind implies. */
  index?: IndexState;
  /** What the repo is for, in a person's words. */
  description?: string;
}

/** A workspace opened recently (the start screen's list). */
export interface RecentWorkspace {
  id: string;
  name: string;
  /** The folder holding its repos. */
  path: string;
  /** Unix seconds. */
  openedAt: number;
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
  contentType: ContentType;
  path: string;
  chunkId: number;
  snippet: string;
  source: string;
  tier: number | null;
  /** The line the chunk starts on, when known. */
  line: number | null;
  /** Present for a Markdown page. */
  page: HitPage | null;
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

/** A proposed change to one wiki page a person keeps (a repo was added). */
export interface PageProposalPayload {
  page: string;
  base: string;
  proposed: string;
}

/** A sync conflict (two edits to one file) or a conflicted copy (a shared
 *  drive saved a second file beside the original), for the banner at the
 *  top of Files. `payload` is the kind's JSON: `ConflictPayload` or
 *  `ConflictCopyPayload`. */
export interface ConflictItem {
  /** Kind-prefixed and stable across reads: "item-12". */
  id: string;
  kind: "conflict" | "conflict-copy";
  title: string;
  body: string;
  when: number;
  /** The file, inside its repo. */
  sourceRef: string;
  payload: string | null;
  /** The repo it is in; absent or null for the focused one. */
  projectId?: string | null;
}

/** What the banner at the top of Files says. */
export interface FilesBanner {
  conflicts: ConflictItem[];
  conflictedCopies: number;
}

/** Parsed payload of a `conflict` item. */
export interface ConflictPayload {
  path: string;
  ours: string;
  theirs: string;
  draft: string | null;
  draftStatus: "pending" | "ready" | "failed";
}

/** Parsed payload of a `conflict-copy` item. */
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
  role: "user" | "assistant" | "activity" | "question" | "edit" | "tool" | "divider";
  content: string;
  createdAt: number;
  /** Set on member-scoped emits of `chat-message` (S9 step 5). */
  project_id?: string;
}

/** A piece of a chat reply as it streams (`chat-delta`). Not kept: the whole
 *  reply follows as an `assistant` message. */
export interface ChatDelta {
  chatId: string;
  text: string;
  project_id?: string;
}

/** A tool Claude called in chat: the content of a `tool` message. */
export interface ToolCard {
  toolUseId: string;
  name: string;
  summary: string;
  status: "running" | "done" | "error";
  /** The start of the result, once it came back. */
  result?: string;
}

/** An edit Claude proposed in chat (`chat::EditProposal`): the file before
 *  and after, for the person to accept or decline change by change. */
export interface EditProposal {
  requestId: string;
  toolUseId: string;
  tool: string;
  path: string;
  relPath: string | null;
  base: string;
  proposed: string;
  /** accepted · declined · partial, once decided. */
  decision: "accepted" | "declined" | "partial" | null;
  note: string | null;
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
  kind: "person" | "organization" | "repo" | "topic" | "decision" | "other";
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
type EntityKind = "person" | "organization" | "repo" | "topic" | "decision" | "other";

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

/** What an on-device model is for: speech to text, search by meaning, or
 *  building the map on this computer instead of with Claude. */
export type ModelCategory = "transcription" | "embedding";

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
  category: ModelCategory;
  tier: "light" | "recommended" | "best" | "advanced";
  /** Plain: "Search by meaning, English, 146 MB". */
  blurb: string;
  /** The languages it handles, as a person says them. */
  languages?: string;
  /** Whether this is the selected model for its category. */
  selected: boolean;
}

/** The search-by-meaning index: which model filled it, how far it is, and
 *  whether a re-read after a model switch is running. Also the payload of
 *  `semantic-progress`. */
export interface EmbeddingState {
  model: string | null;
  dims: number | null;
  embedded: number;
  total: number;
  rebuilding: boolean;
  device: string;
}

/** The graphics card the on-device models can use. */
export interface GpuInfo {
  device: string | null;
  backend: "vulkan" | "metal" | "cpu";
  useGpu: boolean;
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

/** What this machine can record: the microphone, the system audio (the
 *  other side of a call), and why not when one is missing. */
export interface RecordSupport {
  mic: boolean;
  system: boolean;
  reason: string | null;
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
  /** Plain name for Settings ("Search by meaning"). */
  label: string;
  /** Where it applies: this repo, this machine, the team, mine. */
  applies: string;
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
  contentType: ContentType;
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
  contentType: ContentType;
  path: string;
  chunkId: number;
  snippet: string;
  source: string;
  projectId: string;
  memberName: string;
  /** `ken://<project-id>/<rel-path>`. */
  address: string;
  /** The line the hit's chunk starts on, when known. */
  line: number | null;
  /** How to cite it: `repo:path:line`, `repo@sha:…`, led by `[[Note]]` in a wiki. */
  locator: string;
  /** Present for a Markdown page. */
  page: HitPage | null;
  kgBreadcrumbs: string[];
}

/** What a hit on a wiki page carries (mirrors `pagemeta::HitPage`). */
export interface HitPage {
  section: string | null;
  title: string | null;
  /** When a person last read it against its sources, `YYYY-MM-DD`. */
  verified: string | null;
  /** For Research: the date the evidence is from. */
  dated: string | null;
  retired: boolean;
  generated: boolean;
  replacedBy: string[];
  /** 0 binding, 1 the rest, 2 evidence or no longer current. */
  band: number;
  /** How much the hit counts, one short line: `ruling D-410 · 2026-10-06`,
   *  `ticket · cancelled · 2026-07-21`, `current · 2026-10-07`. */
  label: string | null;
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

/** Mirrors `ProjectGroupDto` — a named set of members that belong
 *  together despite being separate repos. `members` are parent-relative
 *  folder names; `projectIds` are the resolvable ones a scoped search
 *  actually targets (shorter than `members` when one can't resolve). */
export interface ProjectGroup {
  name: string;
  members: string[];
  projectIds: string[];
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

// ---- Your day: tasks and tickets ----

/** A task is open or done. Older files with other statuses read as open. */
export type DayTaskState = "open" | "done";

/** Mirrors the Rust `DayTaskDto`: one task file, as Your day lists it. */
export interface DayTask {
  id: string;
  title: string;
  state: DayTaskState;
  /** YYYY-MM-DD; for a recurring task, its next occurrence. */
  target: string | null;
  /** `daily` | `weekdays` | `weekly:mon`..`weekly:sun` | `monthly:<1-31>`; null = one-off. */
  repeat: string | null;
  /** Ticket ids, ken:// or member-relative file paths, or repo names. */
  links: string[];
  /** The sender, for a task accepted from the team inbox. */
  from: string | null;
  /** The body, markdown. */
  description: string;
  updatedBy: string | null;
  updated: string | null;
  created: string | null;
  /** Relative to the workspace root. */
  relPath: string;
  /** Set only for a pending inbox task (not yet accepted). */
  inbox: { familyId: string; itemId: string } | null;
}

/** Mirrors the Rust `DayTicketDto`: one open ticket file assigned to me. */
export interface DayTicket {
  id: string;
  title: string;
  /** As the ticket file states it. */
  state: string;
  target: string | null;
  projectId: string;
  repo: string;
  /** Inside that repo. */
  relPath: string;
  linkedTasks: number;
  linkedDone: number;
}

/** Mirrors the Rust `DayEscalation`: an open `escalations/*.md` in a team
 *  repo, addressed to me. Read only; the row opens the file. */
export interface DayEscalation {
  id: string;
  title: string;
  raisedBy: string;
  status: string;
  ticket: string | null;
  /** When it was raised, as the file states it. */
  raised: string | null;
  /** What it blocks, as the file states it. */
  blocks: string | null;
  projectId: string;
  repo: string;
  /** Inside that repo. */
  relPath: string;
}

export interface DayState {
  tickets: DayTicket[];
  /** Escalations addressed to me, from every team repo. */
  escalations?: DayEscalation[];
  tasks: DayTask[];
  me: { name: string | null; email: string | null };
  /** The team has any ticket file at all. */
  hasTickets: boolean;
}

export interface DayTaskInput {
  title: string;
  target?: string | null;
  description?: string;
  repeat?: string | null;
  links?: string[];
}

/** Absent = leave alone; `target: null` clears it. */
export interface DayTaskPatch {
  title?: string;
  target?: string | null;
  description?: string;
  repeat?: string | null;
  links?: string[];
  state?: DayTaskState;
}

/** The digest written for the team, across all its repos. `sources` are
 *  member-relative paths, `repo/path`. */
export interface TeamDigest {
  generatedAt: number;
  body: string;
  sources: string[];
}

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
  /** Draft the first wiki pages into workspace member `wiki` from every
   *  member and an optional folder of documents. Background; Team lists the
   *  result among its findings. Never touches a page a person wrote. */
  draftWiki: (wiki: string, extra: string | null) => invoke<void>("draft_wiki", { wiki, extra }),
  /** Whether Claude is drafting this wiki's pages now. */
  wikiDrafting: (wiki: string) => invoke<boolean>("wiki_drafting", { wiki }),
  /** A wiki's draft ended (done or failed): its member name. */
  onWikiDrafted: (fn: (wiki: string) => void): Promise<UnlistenFn> => listen<string>("wiki-drafted", (e) => fn(e.payload)),
  /** The page a [[link]] names, by file name or frontmatter alias. */
  resolvePageLink: (from: string, target: string) => invoke<string | null>("resolve_page_link", { from, target }),
  /** Repos joined: each team wiki covering them drafts their Repo Map pages
   *  and proposes changes to kept pages. Returns the wikis being updated. */
  wikiAddRepos: (members: string[]) => invoke<string[]>("wiki_add_repos", { members }),
  /** A team wiki from the bundled template, in a new or empty folder. */
  /** `holdsTeam`: the team has no team repo, so the new docs repo also holds
   *  its tickets, decisions, ideas and people (one repo of both kinds). */
  setupCreateWiki: (dir: string, team: string, repos: CoveredRepo[], taken: string[], holdsTeam = false) =>
    invoke<SetupRepoRow>("setup_create_wiki", { dir, team, repos, taken, holdsTeam }),
  /** Set-up's "Create a team repo": the method's team template at `dir`,
   *  its manifest naming the team's `repos` with their kinds. */
  setupCreateTeamRepo: (dir: string, team: string, wiki: string | null, taken: string[], repos: CoveredRepo[] = []) =>
    invoke<SetupRepoRow>("setup_create_team_repo", { dir, team, wiki, taken, repos }),
  /** Apply a proposed page change; returns the page written. */
  applyPageProposal: (itemId: number, projectId: string | null = null) =>
    invoke<string>("apply_page_proposal", { itemId, projectId }),
  /** The Ingest screen for the team's library (its wiki's inbox). */
  ingestOverview: (team: string | null) => invoke<IngestOverview>("ingest_overview", { team }),
  ingestCard: (team: string | null, itemId: number) => invoke<IngestCard>("ingest_card", { team, itemId }),
  /** Copy files into the library's Raw/ and start reading them. */
  ingestAdd: (team: string | null, paths: string[]) => invoke<string[]>("ingest_add", { team, paths }),
  /** A file dropped in the window (bytes, no path) into the library's Raw/. */
  ingestAddBytes: async (team: string | null, file: File) =>
    invoke<string>("ingest_add_bytes", new Uint8Array(await file.arrayBuffer()), {
      headers: { "x-name": encodeURIComponent(file.name), "x-team": encodeURIComponent(team ?? "") },
    }),
  /** Read what waits in Raw/ now; false when a pass is already running. */
  ingestNow: (team: string | null = null) => invoke<boolean>("ingest_now", { team }),
  /** A note written in Ken, into Raw/ as `<date> Note - <title>.md`, then
   *  read at once. Returns its path. */
  ingestAddText: (team: string | null, title: string | null, text: string) =>
    invoke<string>("ingest_add_text", { team, title, text }),
  /** A chat, whole or a few of its messages (only the person's and Ken's
   *  turns), into Raw/ as `<date> Chat - <title>.md`, then read at once.
   *  `messageIds` null is the whole chat. Returns its path. */
  ingestAddChat: (team: string | null, projectId: string, chatId: string, messageIds: number[] | null) =>
    invoke<string>("ingest_add_chat", { team, projectId, chatId, messageIds }),
  /** Try a source that failed again: its failure is cleared and a pass starts. */
  ingestRetry: (team: string | null, path: string) => invoke<boolean>("ingest_retry", { team, path }),
  /** Move a source out of Raw/ to the system trash. */
  ingestRemove: (team: string | null, path: string) => invoke<void>("ingest_remove", { team, path }),
  /** Undo all on an ingest card: every write, last first, then the note
   *  (unless edited) and the source back to Raw/; what waits is closed. */
  ingestUndo: (itemId: number, team: string | null = null) =>
    invoke<IngestUndoReport>("ingest_undo", { itemId, team }),
  /** Read an undone source again: its card closes and a pass starts. */
  ingestReadAgain: (itemId: number, team: string | null = null) =>
    invoke<boolean>("ingest_read_again", { itemId, team }),
  /** Undo one write on an ingest card; refused when its file changed since. */
  ingestUndoWrite: (itemId: number, index: number, team: string | null = null) =>
    invoke<void>("ingest_undo_write", { itemId, index, team }),
  /** Seen: the source moves beside its note and the card closes. Returns where. */
  ingestFile: (itemId: number, team: string | null = null) => invoke<string>("ingest_file", { itemId, team }),
  /** The last drift sweep (null before the first). */
  /** Run the drift sweep now. */
  runDriftNow: (projectId: string | null = null) => invoke<DriftRun | null>("run_drift_now", { projectId }),
  /** The Team screen for the chosen team. */
  teamOverview: (team: string | null) => invoke<TeamOverview>("team_overview", { team }),
  /** The team's wiki (the repo with the inbox), or null. */
  teamWiki: (team: string | null) => invoke<string | null>("team_wiki", { team }),
  teamSaveIgnores: (lines: string[]) => invoke<void>("team_save_ignores", { lines }),
  /** Sync conflicts and conflicted copies across the team's repos: the
   *  banner at the top of Files. */
  filesBanner: (team: string | null) => invoke<FilesBanner>("files_banner", { team }),
  /** A new rule page in the wiki, from its rule template; returns its path. */
  teamAddRule: (wikiId: string, rule: string) => invoke<string>("team_add_rule", { wikiId, rule }),
  /** A page's links both ways: pages it reaches, pages that reach it. */
  pageLinks: (path: string) =>
    invoke<{ outgoing: string[]; incoming: string[] }>("page_links", { path }),
  /** A code file's outline (definitions, nested) and the files it imports
   *  and is imported by, from Ken's code map. */
  codeFile: (path: string) => invoke<CodeFile>("code_file", { path }),
  /** Go to definition and find usages for a symbol, across the workspace. */
  codeUsages: (name: string) => invoke<CodeUsages>("code_usages", { name }),
  setProjectIndex: (id: string, index: IndexState | null) =>
    invoke<RegistryEntryStatus[]>("set_project_index", { id, index }),
  setProjectKind: (id: string, kind: RepoKind[], team: string | null) =>
    invoke<RegistryEntryStatus[]>("set_project_kind", { id, kind, team }),
  createProject: (path: string, name: string) =>
    invoke<ProjectInfo>("create_project", { path, name }),
  openProject: (path: string) => invoke<ProjectInfo>("open_project", { path }),

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
  /** Set-up Confirm: writes the manifest, kinds, teams, index states and
   *  ignore lines, then opens the workspace. */
  setupConfirm: (parent: string, name: string, rows: SetupRepoRow[], ignores: SetupIgnoreRow[]) =>
    invoke<WorkspaceOverview>("setup_confirm", { parent, name, rows, ignores }),
  /** Set-up from repos picked one by one (a folder of repos stands for
   *  each inside it). Reads only. */
  setupProposeRepos: (paths: string[]) => invoke<SetupProposal>("setup_propose_repos", { paths }),
  /** Confirm picked repos: a new workspace in Ken's app data, or (add) into
   *  the open one. Opens it. */
  setupConfirmRepos: (name: string, rows: SetupRepoRow[], add = false) =>
    invoke<WorkspaceOverview>("setup_confirm_repos", { name, rows, add }),
  /** What a repo is for, in a person's words. */
  setProjectDescription: (id: string, description: string) =>
    invoke<RegistryEntryStatus[]>("set_project_description", { id, description }),
  /** Scan again: what moved since set-up. Never changes anything. */
  setupRescan: (parent: string) => invoke<SetupMoved[]>("setup_rescan", { parent }),
  /** Members + per-member status + counts for the currently open workspace. */
  workspaceOverview: () => invoke<WorkspaceOverview>("workspace_overview"),
  /** Switch focus to `id`, activating a dormant member (LRU-evicting past
   *  the resident cap) without closing any other member. Emits
   *  `workspace-state`'s `focus` variant. */
  focusProject: (id: string) => invoke<void>("focus_project", { id }),
  /** Close the open workspace, tearing down every member's runtime. */
  closeWorkspace: () => invoke<void>("close_workspace"),
  /** Keyword FTS fan-out over every ACTIVE workspace member, merged by
   *  round-robin rank-position interleave and labeled per hit (design D6 —
   *  BM25 scores aren't comparable across corpora). Requires `workspace`;
   *  upgrades to `routeSearch` when `kgRouting` is also on (kg-routing
   *  proposal: "same UI slot, richer results"). */
  searchAllProjects: (query: string, limit = 30, audience: Audience = null, types: KindFilter = null) =>
    invoke<SearchAllProjectsResult>("search_all_projects", { query, limit, audience, types }),
  forgetProject: (id: string) => invoke<void>("forget_project", { id }),
  /** Workspaces opened recently, newest first. */
  listRecentWorkspaces: () => invoke<RecentWorkspace[]>("list_recent_workspaces"),
  forgetWorkspace: (id: string) => invoke<void>("forget_workspace", { id }),
  renameProject: (id: string, name: string) =>
    invoke<ProjectInfo>("rename_project", { id, name }),
  currentProject: () => invoke<ProjectInfo | null>("current_project"),
  setFolderSelection: (excluded: string[]) =>
    invoke<ProjectInfo>("set_folder_selection", { excluded }),
  getTree: () => invoke<TreeData>("get_tree"),
  /** The whole workspace as one tree: every member's files and folders,
   *  each path prefixed with the member's folder name. Same shape as
   *  getTree, so FileTree renders it unchanged. */
  getTreeAll: () => invoke<TreeData>("get_tree_all"),
  /** Keyword FTS merged with semantic (when the `semanticIndex` feature is
   *  on for the project); transparently degrades to FTS-only results when
   *  it's off. */
  /** `audience` "business" keeps only pages written for readers who never
   *  see code (Current, Design, Work, or `audience: business`); "dev" keeps
   *  everything else but the method pages, code included; null is any. */
  hybridSearch: (query: string, limit = 30, audience: Audience = null, types: KindFilter = null) =>
    invoke<HybridHit[]>("hybrid_search", { query, limit, audience, types }),
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
    audience: Audience = null,
    types: KindFilter = null,
  ) =>
    invoke<RouteSearchResult>("route_search", {
      query,
      limit,
      scope: scope ?? null,
      group: group ?? null,
      audience,
      types,
    }),
  /** Named groups of members, stored in the workspace manifest. */
  workspaceGroups: () => invoke<ProjectGroup[]>("workspace_groups"),
  workspaceSetGroup: (name: string, members: string[]) =>
    invoke<ProjectGroup[]>("workspace_set_group", { name, members }),
  workspaceRemoveGroup: (name: string) =>
    invoke<ProjectGroup[]>("workspace_remove_group", { name }),
  /** Take a repo out of the workspace (its folder and files stay). Its
   *  team wiki's link report names the pages that still cite it. */
  workspaceRemoveMember: (name: string) =>
    invoke<{ members: MemberOverview[]; wiki: string | null; citingPages: number }>("workspace_remove_member", { name }),
  /** Join an existing sibling folder to the open workspace. It lands
   *  dormant and opens on first focus. */
  workspaceAddMember: (folder: string) =>
    invoke<MemberOverview[]>("workspace_add_member", { folder }),
  readFile: (relPath: string) => invoke<string>("read_file", { relPath }),
  readFileBytes: (relPath: string) =>
    invoke<ArrayBuffer>("read_file_bytes", { relPath }),
  isCloudOnly: (relPath: string) =>
    invoke<boolean>("is_cloud_only", { relPath }),
  /// Downloads an online-only file from the cloud provider. Slow by nature.
  hydrateFile: (relPath: string) => invoke<void>("hydrate_file", { relPath }),
  saveFile: (relPath: string, content: string) =>
    invoke<number>("save_file", { relPath, content }),
  /** Overwrite a file with raw bytes (PDF form fills). Returns the new mtime like saveFile. */
  saveFileBytes: (relPath: string, bytes: Uint8Array) =>
    invoke<number>("save_file_bytes", { relPath, bytes: Array.from(bytes) }),
  /** Create a NEW folder `name` in `dirRel` ("" = root), deduped (`Photos-1`)
   *  so a dropped folder never merges into an existing one; resolves to its
   *  project-relative path. */
  createUniqueFolder: (dirRel: string, name: string) =>
    invoke<string>("create_unique_folder", { dirRel, name }),
  /**
   * Save a pasted/dropped file as a new file in folder `dirRel` ("" = project
   * root). The backend sanitizes and dedupes the name, never overwriting;
   * resolves to the project-relative path it landed at. The bytes go as the
   * raw IPC body (no JSON number array), the strings as encoded headers.
   */
  writeNewFile: (dirRel: string, fileName: string, bytes: Uint8Array) =>
    invoke<string>("write_new_file", bytes, {
      headers: {
        "x-dir": encodeURIComponent(dirRel),
        "x-name": encodeURIComponent(fileName),
      },
    }),
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
  /// Show a file in Finder/Explorer rather than opening it.
  revealInFolder: (relPath: string, projectId: string | null = null) =>
    invoke<void>("reveal_in_folder", { relPath, projectId }),
  /// Open an http(s) link in the system browser (anything else is refused by
  /// the backend).
  openWebUrl: (url: string) => invoke<void>("open_web_url", { url }),

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
  setModelSelection: (category: ModelCategory, id: string) =>
    invoke<void>("set_model_selection", { category, id }),
  /** The search-by-meaning index now; `semantic-progress` follows it. */
  embeddingState: () => invoke<EmbeddingState>("embedding_state"),
  onSemanticProgress: (fn: (ev: EmbeddingState) => void): Promise<UnlistenFn> =>
    listen<EmbeddingState>("semantic-progress", (e) => fn(e.payload)),
  /** The graphics card the on-device models can use, and whether they do. */
  gpuInfo: () => invoke<GpuInfo>("gpu_info"),
  setUseGpu: (on: boolean) => invoke<void>("set_use_gpu", { on }),

  /** Close a page proposal or another stored item without applying it. */
  resolveReviewItem: (id: number, projectId: string | null = null) =>
    invoke<void>("resolve_review_item", { id, projectId }),
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
  /// Mark every indexed file under one folder seen.
  markSeenUnder: (relPath: string) =>
    invoke<void>("mark_seen_under", { relPath }),
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
  /// Whether videos are auto-transcribed on-device (Whisper) during indexing.
  getTranscribeOnIndex: () => invoke<boolean>("get_transcribe_on_index"),
  setTranscribeOnIndex: (enabled: boolean) =>
    invoke<void>("set_transcribe_on_index", { enabled }),
  claudeDoctor: () => invoke<ClaudeDoctor>("claude_doctor"),
  mcpInfo: () => invoke<McpInfo>("mcp_info"),

  quickAnswer: (query: string) => invoke<boolean>("quick_answer", { query }),
  /** Search found nothing that answers: a read-only Claude session looks
   *  through the workspace's folders itself and answers with ken:// sources. */
  /** `leads`: what search ranked, as `member/path`, best first; the look reads them first. */
  lookFor: (query: string, leads: string[] = []) => invoke<{ body: string; sources: string[] }>("look_for", { query, leads }),
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
  workspaceKgOverview: (team: string | null = null) =>
    invoke<WorkspaceKgOverview>("workspace_kg_overview", { team }),
  /** The full wiki-page payload for one global entity — summary, out-links,
   *  back-links, and per-project doc pointers in one call. */
  workspaceKgEntity: (id: number) =>
    invoke<WorkspaceKgEntity>("workspace_kg_entity", { id }),
  /** Case-insensitive substring search over global entity names + summaries
   *  (name matches ranked above summary-only matches), capped at 50 hits.
   *  An empty/whitespace-only query always returns `[]` (no "list all"). */
  workspaceKgSearch: (query: string, team: string | null = null) =>
    invoke<WorkspaceKgSearchHit[]>("workspace_kg_search", { query, team }),

  // ---- Memory (ken-memory task 4.1) ----
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
  /** Answer an edit proposal: accepted, declined, or partial with the merged
   *  text Ken writes and the changes that were left out. */
  answerEditProposal: (
    chatId: string,
    messageId: number,
    decision: "accepted" | "declined" | "partial",
    merged: string | null,
    declinedChanges: string[],
  ) => invoke<void>("answer_edit_proposal", { chatId, messageId, decision, merged, declinedChanges }),
  answerChatQuestion: (
    chatId: string,
    messageId: number,
    answers: Record<string, string>,
  ) => invoke<void>("answer_chat_question", { chatId, messageId, answers }),
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
  onChatDelta: (fn: (d: ChatDelta) => void): Promise<UnlistenFn> =>
    listen<ChatDelta>("chat-delta", (e) => fn(e.payload)),
  onChatPtyData: (fn: (chunk: PtyChunk) => void): Promise<UnlistenFn> =>
    listen<PtyChunk>("chat-pty-data", (e) => fn(e.payload)),

  onIndexUpdated: (fn: (stats: ScanStats) => void): Promise<UnlistenFn> =>
    listen<ScanStats>("index-updated", (e) => fn(e.payload)),
  /** Claude, asked by the person, opens a file in Ken (ken-mcp open_in_ken). */
  onKenOpen: (
    fn: (req: { projectId: string | null; path: string; line: number | null; anchor: string | null }) => void,
  ): Promise<UnlistenFn> =>
    listen<{ projectId: string | null; path: string; line: number | null; anchor: string | null }>("ken-open", (e) =>
      fn(e.payload),
    ),
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
  /** What this machine can capture: the mic, the system audio, and why not. */
  recordSupport: () => invoke<RecordSupport>("record_support"),
  /** Start a take for `team`: the transcript goes to that team wiki's Raw/
   *  and is read at once. Errors when nothing could start capturing. */
  recordStart: (mic: boolean, system: boolean, deviceId: string | null, team: string | null = null) =>
    invoke<void>("record_start", { mic, system, deviceId, team }),
  recordPause: () => invoke<void>("record_pause"),
  recordResume: () => invoke<void>("record_resume"),
  recordStop: (storage: RecordStorage) =>
    invoke<void>("record_stop", { storage }),
  recordCancel: () => invoke<void>("record_cancel"),
  /**
   * Open the OS's privacy settings for a permission (an `x-apple.systempreferences:`
   * link on macOS, `ms-settings:privacy-microphone` on Windows). Routed through
   * Rust because the frontend opener capability scope forbids those schemes.
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

  // ---- Your day ----
  /** Your day for `team` (a group name; null = every workspace member). */
  dayState: (team: string | null) => invoke<DayState>("day_state", { team }),
  /** A new task in the workspace home; updated_by "you". */
  dayTaskCreate: (input: DayTaskInput) => invoke<DayTask>("day_task_create", { input }),
  /** Patch a task: only the keys named change; `target: null` clears it. */
  dayTaskUpdate: (id: string, patch: DayTaskPatch) => invoke<DayTask>("day_task_update", { id, patch }),
  /** Moves the file to its home's archive folder. */
  dayTaskDelete: (id: string) => invoke<void>("day_task_delete", { id }),
  /** Tasks linked to a ticket, done included. */
  ticketTasks: (projectId: string, ticketId: string) =>
    invoke<DayTask[]>("ticket_tasks", { projectId, ticketId }),
  /** The newest stored digest for the team, or null. */
  teamDigest: (team: string | null) => invoke<TeamDigest | null>("team_digest", { team }),
  /** Write the team digest now. Outcome arrives on the team-digest events. */
  refreshTeamDigest: (team: string | null) => invoke<void>("refresh_team_digest", { team }),
  /** Move a ticket to a status (todo, in-progress, blocked, in-review, testing, done, cancelled). */
  ticketSetStatus: (projectId: string, relPath: string, status: string) =>
    invoke<void>("ticket_set_status", { projectId, relPath, status }),
  /** Reply in an escalation's thread; `resolve` also closes it. */
  escalationReply: (projectId: string, relPath: string, text: string, resolve: boolean) =>
    invoke<void>("escalation_reply", { projectId, relPath, text, resolve }),
  /** Whether the team's digest is being written or waits its turn. */
  teamDigestWriting: (team: string | null) => invoke<boolean>("team_digest_writing", { team }),
  /** Repos indexed, files queued and failed, across the team. */
  indexHealth: (team: string | null) => invoke<IndexHealth>("index_health", { team }),
  /** A task file, ticket file or team inbox changed. */
  onDayChanged: (fn: () => void): Promise<UnlistenFn> => listen<null>("day-changed", () => fn()),
  onTeamDigestUpdated: (fn: (digest: TeamDigest) => void): Promise<UnlistenFn> =>
    listen<TeamDigest>("team-digest-updated", (e) => fn(e.payload)),
  /** A team's digest was asked for: it is being written or waits its turn. */
  onTeamDigestGenerating: (fn: (team: string | null) => void): Promise<UnlistenFn> =>
    listen<{ team: string | null }>("team-digest-generating", (e) => fn(e.payload?.team ?? null)),
  onTeamDigestError: (fn: (team: string | null, message: string) => void): Promise<UnlistenFn> =>
    listen<{ team: string | null; message: string | null }>("team-digest-error", (e) =>
      fn(e.payload?.team ?? null, e.payload?.message ?? ""),
    ),

  // ---- ken-families (ken-families change, task 4.1/4.2/4.3) ----
  /** Scaffold + commit a brand-new family repo whose remote is `remoteUrl`
   *  (an empty repo the user already created on their git host); the
   *  creator becomes the family's first — and owner — member. Rejects with
   *  a friendly message when `git` is unavailable. */
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
    invoke<DayTask>("family_accept_task", { familyId, itemId }),
  /** Push back on an inbox item: creates a new message item in the
   *  SENDER's inbox (lane rule 2) and leaves the original item's status
   *  untouched — call `familySetItemStatus` separately if you also want to
   *  mark the original seen/archived. */
  familyPushBack: (familyId: string, itemId: string, note: string) =>
    invoke<void>("family_push_back", { familyId, itemId, note }),
  /** Send a task, message or notification to a teammate's inbox from the
   *  app (the MCP's family_send, without an agent). Delivery, not assignment. */
  familySend: (
    familyId: string,
    to: string,
    kind: "task" | "message" | "notification",
    title: string,
    body: string,
  ) => invoke<void>("family_send", { familyId, to, kind, title, body }),
  /** `family-sync`: emitted after every poll tick and every on-demand
   *  command that touches a connection's transport. App-global — a family connection has no single owning project. */
  onFamilySync: (fn: (ev: FamilySyncEvent) => void): Promise<UnlistenFn> =>
    listen<FamilySyncEvent>("family-sync", (e) => fn(e.payload)),
};
