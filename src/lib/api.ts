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

/** The workspace's index in one health line (mirrors `IndexHealthDto` and
 *  `db::IndexHealth`): repos indexed of all, then their files indexed,
 *  failed, skipped (name only) and queued; passages and how many have a
 *  meaning vector; controls passing of all; whether the embedding model is
 *  missing; files cut at the per-file chunk cap. */
export interface IndexHealth {
  reposIndexed: number;
  repos: number;
  files: number;
  indexed: number;
  failed: number;
  skipped: number;
  queued: number;
  capped: number;
  chunks: number;
  embedded: number;
  controlsPassed: number;
  controlsTotal: number;
  modelMissing: boolean;
}

/** A repo's index state, the two of knowledge-core decision 34: switched
 *  off as a whole, or indexed (its paths follow the index rules). */
export type IndexState = "off" | "indexed";

/** Mirrors `setup::RepoRow`: one source the set-up scan found. `include`
 *  is its index state: a source or left out. */
export interface SetupRepoRow {
  member: string;
  include: boolean;
  kind: RepoKind[];
  evidence: string[];
  remote: string | null;
  existing: boolean;
  hasGit: boolean;
  /** The repo's folder (always set when repos are picked one by one). */
  path: string | null;
  /** What it is for and how it is used; first proposed from its README. */
  description: string;
}

/** Mirrors `wikinew::Covered`: a repo a new knowledge base names. */
export interface CoveredRepo {
  name: string;
  description: string;
  kind?: RepoKind[];
  path?: string | null;
}

/** Mirrors `setup::IgnoreRow`: one ignore line the scan would add. */
export interface SetupIgnoreRow {
  pattern: string;
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
  ignores: SetupIgnoreRow[];
  existingWorkspace: boolean;
}

export type SetupMoved =
  | { change: "NewFolder"; member: string; evidence: string[] }
  | { change: "Gone"; member: string }
  | { change: "NewWorktree"; pattern: string; evidence: string };

/** One source of the workspace, as set-up wrote it (the workspace settings
 *  screen's rows). Mirrors `SourceDto`. */
export interface SourceRow {
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
  /** The one health line (decision 19), null before it has an index. */
  health: string | null;
  /** Its name in `.ken/knowledge.json` (`repos`, a control's `source`). */
  settingsName: string;
  /** Its role rules (decision 17), `pattern = role` lines. */
  roles: string[];
  /** Whether "Analyse again" is offered: its kind is said, a knowledge base
   *  lists it, and its settings are not a code repo's own file. */
  canAnalyzeAgain: boolean;
  /** Its analysis, asked for again, is queued or running. */
  analyzing: boolean;
  /** A repo analysis for it waits in the Inbox. */
  analysisWaiting: boolean;
}

/** Something a source lacks for its kind, or the workspace lacks. */
export interface SettingsGap {
  repo: string | null;
  text: string;
  open: string | null;
}

export interface SettingsPage {
  path: string;
  title: string;
}

/** One control (decision 19) with its source's last run. */
export interface ControlRow {
  question: string;
  expect: string;
  /** Its source's settings name; null is the knowledge base. */
  source: string | null;
  ran: boolean;
  rank: number | null;
  passed: boolean;
}

/** One entry of the knowledge base's one findings list: drift, the lint,
 *  ingest and the supersession pass all file there. */
export interface Finding {
  /** contradiction · question · drift · broken-link · missing-page ·
   *  ambiguous-name · orphan · suggested-page · unknown-ruling ·
   *  supersession, as the backend names it. */
  kind: string;
  title: string;
  detail: string;
  /** The page to open, inside `projectId`'s repo. */
  path: string | null;
  projectId: string | null;
  /** The stored finding's id: Mark as done, or Confirm a supersession. */
  itemId?: number | null;
}

/** Mirrors `AnalysisDto` (`repo_analysis::Proposal`): the role rules and
 *  controls the repo analysis proposed for one source, waiting for a person
 *  to accept or dismiss. Nothing applies until accepted. */
export interface AnalysisProposal {
  itemId: number;
  /** The knowledge base the card waits in. */
  projectId: string;
  title: string;
  /** Seconds since the epoch. */
  createdAt: number;
  /** The source's name in `.ken/knowledge.json`. */
  source: string;
  own: boolean;
  roles: { pattern: string; role: string }[];
  controls: { question: string; expect: string; source?: string | null }[];
}

/** Mirrors `wikinew::TemplateGap`: a template file the knowledge base lacks,
 *  or one Ken owns that differs from its template (`diff`: `- ` its lines,
 *  `+ ` the template's). */
export interface TemplateGap {
  path: string;
  state: "missing" | "out-of-date";
  diff: string[];
}

/** Mirrors `wikimap::Move`: one page of an existing wiki and the section
 *  it goes to. */
export interface WikiMove {
  from: string;
  to: string;
  section: string;
  why: string;
}

/** Mirrors `wikimap::Plan`: the dry run. `left`: [page, why]. */
export interface WikiMapPlan {
  moves: WikiMove[];
  left: [string, string][];
}

export interface WikiMapApplied {
  moved: WikiMove[];
  rewritten: string[];
  unresolved: string[];
  commit: string | null;
  commitError: string | null;
}

/** The one background throttle (knowledge-core decision 34). */
export type Pace = "gentle" | "normal" | "fast";

export interface BackgroundJob {
  kind: string;
  label: string;
  done: number;
  total: number;
  /** Waiting for its turn (paused, the gap, another call), not working. */
  waiting?: boolean;
}

export interface BackgroundFailure {
  kind: string;
  label: string;
  reason: string;
  at: number;
}

/** Mirrors `buildqueue::Progress`: the library's build queue in numbers. */
export interface BuildProgress {
  total: number;
  done: number;
  queued: number;
  reading: string[];
  failed: [string, string][];
  atOnce: number;
}

/** The one status panel: running, queued, progress, failed with reasons. */
export interface BackgroundStatus {
  throttle: {
    setting: { pace: Pace; paused: boolean };
    callsLastHour: number;
    callsPerHour: number;
    calling: boolean;
  };
  running: BackgroundJob[];
  queued: { kind: string; label: string; count: number }[];
  failed: BackgroundFailure[];
  build: BuildProgress | null;
}

/** The workspace settings screen (knowledge-core decision 37): the
 *  workspace's sources and its knowledge base's settings. Mirrors
 *  `WorkspaceSettingsDto`. */
export interface WorkspaceSettings {
  workspace: string;
  root: string;
  repos: SourceRow[];
  /** The knowledge base, when the workspace has one. */
  wiki: SourceRow | null;
  gaps: SettingsGap[];
  /** The index rules in the knowledge base's `.ken/knowledge.json`. */
  ignores: string[];
  /** Older workspace `.kenignore` lines, while a source still reads them
   *  (until they move into `.ken/knowledge.json`); never written. */
  legacyIgnores: string[];
  /** The tier overrides (`search.weights`), `pattern = tier` lines. */
  weights: string[];
  controls: ControlRow[];
  templateGaps: TemplateGap[];
  sweep: DriftRun | null;
  rules: SettingsPage[];
  templates: SettingsPage[];
  findings: Finding[];
}

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
  /** Its card, once read (a source read before sources were claimed as
   *  their note was written still waits in Raw with its card). */
  cardId: number | null;
  /** How many pages were written from it. */
  pages: number;
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
  /** How many pages were written from it. */
  pages: number;
  /** What waits from it: rulings, tickets, held edits. */
  waiting: number;
  /** Its note is written and Claude is still writing the pages. */
  writing: boolean;
}

/** The Ingest screen: the knowledge base's inbox and its history. */
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
  /** meeting | recording | document | note | session, from the note's folder. */
  kind: string;
  present: string[];
  length: string;
  summary: string;
  keyTakeaways: string[];
  overturns: string;
  contradictions: string[];
  rulings: string[];
  actions: string[];
  openQuestions: string[];
}

/** What waits from a note: a ready ruling for the decisions log, a risk or
 *  issue for the RAID register, a drafted ticket, or an edit held for a
 *  person ("page" a change to one, "new page"). */
export interface IngestProposal {
  id: number;
  kind: "page" | "new page" | "ruling" | "risk" | "issue" | "ticket";
  title: string;
  page: string;
  body: string;
  /** The proposal (page as it is and with the change), for the diff. */
  payload: string | null;
  /** Who made a ruling, or who watches a risk or issue, as the note says;
   *  anyone may add it. */
  who: string | null;
}

/** Mirrors `LooseProposalDto`: a proposal no ingest card holds, a ruling
 *  proposed in chat or an edit a drift run held. */
export interface LooseProposal extends IngestProposal {
  from: "chat" | "drift";
  projectId: string;
}

/** One page an ingest wrote. */
export interface IngestPage {
  path: string;
  /** The change in a line, or a new page's purpose. */
  label: string;
  /** A new page, not an edit. */
  new: boolean;
}

/** One ingested source for its card on the Ingest screen. Git is the undo:
 *  the ingest is one commit in the knowledge base. */
export interface IngestCard {
  id: number;
  open: boolean;
  note: string;
  source: string;
  takeaways: IngestTakeaways;
  pages: IngestPage[];
  proposals: IngestProposal[];
  /** The commit that holds the ingest (short sha), when there is one. */
  commit: string | null;
  /** Why the commit failed, when it did. */
  commitError: string | null;
  /** Its note is written and Claude is still writing the pages. */
  writing: boolean;
}

/** Mirrors `drift::DriftRun`: one drift run, what it found and did. */
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
    pin?: string | null;
    branch?: string | null;
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
  /** What the run did: pages whose `checked` stamp advanced, pages fixed,
   *  edits held for a person, citations rewritten after a rename, findings
   *  filed, pages waiting for a re-read, and its commit. */
  stamped: string[];
  fixed: string[];
  held: string[];
  renamed: string[];
  findings: number;
  waiting: string[];
  commit: string | null;
  commitError: string | null;
}

/** What a file is for (`contenttype::of`); every search hit carries one. */
export type ContentType =
  | "code"
  | "test"
  | "spec"
  | "doc"
  | "config"
  | "data"
  | "design"
  | "meeting"
  | "ticket"
  | "decision"
  | "note";

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
export type RepoKind = "wiki" | "code" | "reference";

export interface RegistryEntryStatus {
  id: string;
  name: string;
  path: string;
  available: boolean;
  /** Absent until someone says what the repo is. */
  kind?: RepoKind[];
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

/** Mirrors the Rust `SemanticIndexStateEvent` internally-tagged enum
 *  (`#[serde(tag = "state", rename_all = "camelCase")]`) exactly — each
 *  variant carries only the fields that enum case has. `project_id` is set
 *  when the event was emitted for a specific project/member (S9 step 5). */
export type SemanticIndexState =
  | { state: "building"; done: number; total: number; project_id?: string }
  | { state: "ready"; project_id?: string }
  | { state: "unavailable"; reason: string; project_id?: string }
  | { state: "warning"; reason: string; project_id?: string };

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
  /** When it last pulled and last pushed, Unix seconds; null for never. */
  lastPull: number | null;
  lastPush: number | null;
  /** The knowledge base's files changed outside Ken: sync never commits
   *  them by itself; "commit my other changes" does, once shown. */
  others: string[];
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
  /** The open workspace's folder: the connect command serves all of it. */
  workspaceRoot: string;
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

/** One neighbour in the project graph (knowledge-core decision 21), from
 *  `graph_related`: nearest first, by edge kind. */
export interface RelatedNeighbour {
  /** `kind:id`, e.g. `page:Design/Saves.md`, `decision:D-012`. */
  key: string;
  /** `page` | `note` | `ticket` | `decision` | `code` | `symbol` | `topic` |
   *  `repo` | `commit`. */
  kind: string;
  /** A file's name, a ruling's id, a topic, a commit. */
  label: string;
  /** `supersedes` | `refines` | `cites` | `changed` | `code-cites` | `link` |
   *  `commit` | `imports` | `uses` | `in` | `topic`. */
  edge: string;
  /** The edge goes from the file asked about (or the node it came through)
   *  to this one. */
  outgoing: boolean;
  detail: string;
  /** 1 for a direct neighbour, 2 for one reached through `via`. */
  hops: number;
  via: string | null;
  /** Where it opens as a file: the member that holds it, and its path. */
  projectId: string | null;
  path: string | null;
}

/** One item of the Log, the project's RAID register (knowledge-core
 *  decision 37). Mirrors `raidlog::Item`. */
export interface RaidItem {
  /** `R-001`, a ticket id, `D-012`, a tag; empty for a roadmap row. */
  id: string;
  title: string;
  /** Lowercase, as its source says it: `open`, `mitigated`,
   *  `in-progress`, `live`, `superseded by D-020`, `reached`, `released`… */
  status: string;
  owner: string | null;
  /** `YYYY-MM-DD`: raised, updated, ruled, due or reached; empty when none. */
  date: string;
  /** Folded away under its group. */
  closed: boolean;
  /** `risk` | `issue` | `ticket` | `ruling` | `roadmap` | `release`. */
  kind: string;
  /** Another repo's name, for its release. */
  repo: string | null;
  /** The file it comes from, in the repo the register was read from. */
  path: string | null;
  line: number | null;
}

/** The register, group by group. */
export interface RaidRegister {
  risks: RaidItem[];
  actions: RaidItem[];
  issues: RaidItem[];
  decisions: RaidItem[];
  milestones: RaidItem[];
}

/** The Log: the RAID register of the workspace's knowledge base, else the
 *  focused repo. */
export interface RaidLog {
  projectId: string | null;
  name: string | null;
  register: RaidRegister;
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
 *  convention as `SemanticIndexState` above. App-global
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

/** What an on-device model is for: speech to text, or search by meaning. */
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
  /** Where it applies: this repo, this machine, the workspace, mine. */
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

// ---- routed search (knowledge-core decision 20) ----

/** Mirrors `RoutePlanDto`: the sources searched (project ids), why, and the
 *  reference repos held back. */
export interface RoutePlan {
  targets: string[];
  /** `named`: the question named them, or the person chose them. `every`:
   *  every source but the reference repos held back. */
  reason: "named" | "every";
  /** Reference repos held back: searched only when nothing else answered,
   *  and then in `memberStatus` too. */
  reserve: string[];
}

/** Mirrors `RoutedHitDto`: one merged, cited hit from `route_search`.
 *  `source` is `"keyword"` | `"semantic"` | `"both"`. */
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
  /** One label per hit, `kind · tier or role · date`
   *  (`decision D-410 · binding · 2026-10-06`, `ticket · superseded · 2026-07-21`,
   *  `code · service · 2026-10-01`). */
  label: string | null;
  /** For a code hit, the definition its chunk holds (`Store.open`, or
   *  `save, load` when it packs several). */
  symbol: string | null;
}

/** What a hit on a wiki page carries (mirrors `pagemeta::HitPage`). */
export interface HitPage {
  section: string | null;
  title: string | null;
  /** When the check last re-read it against its sources, `YYYY-MM-DD`. */
  checked: string | null;
  /** For Research: the date the evidence is from. */
  dated: string | null;
  /** `status: superseded`, or a page that names its successor. */
  retired: boolean;
  /** Legacy, read only: an older page's `generated` key. */
  generated: boolean;
  /** What a retired page names as its successor: `superseded_by`, or an older page's `replaced_by`. */
  supersededBy: string[];
}

/** `building`: searched by keyword while its meaning index is built.
 *  `unavailable`: its index did not open. */
export type RouteMemberStatus = "searched" | "building" | "unavailable";

/** Mirrors `MemberStatusEntryDto`: how one source's search went. */
export interface RouteMemberStatusEntry {
  projectId: string;
  memberName: string;
  status: RouteMemberStatus;
  /** While building: the passages with a meaning vector so far, of `chunks`. */
  embedded: number | null;
  chunks: number | null;
  /** How long its search took; `slow` from two seconds. */
  millis: number;
  slow: boolean;
}

/** Mirrors `RouteSearchDto`: `route_search`'s return shape. The plan, the
 *  results merged once by relevance, and how each source's search went. */
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
  /** Who the task came from, when its file says. */
  from: string | null;
  /** The body, markdown. */
  description: string;
  updatedBy: string | null;
  updated: string | null;
  created: string | null;
  /** Relative to the workspace root. */
  relPath: string;
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

export interface DayState {
  tickets: DayTicket[];
  tasks: DayTask[];
  me: { name: string | null; email: string | null };
  /** Any repo of the workspace has a ticket file at all. */
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

/** The digest written for the workspace, across all its repos. `sources`
 *  are member-relative paths, `repo/path`. */
export interface DayDigest {
  generatedAt: number;
  body: string;
  sources: string[];
}

export const api = {
  listProjects: () => invoke<RegistryEntryStatus[]>("list_projects"),
  /** The page a [[link]] names, by file name or frontmatter alias. */
  resolvePageLink: (from: string, target: string) => invoke<string | null>("resolve_page_link", { from, target }),
  /** A knowledge base from the bundled template, in a new or empty folder,
   *  named for the workspace; it holds the tickets and the decisions log. */
  setupCreateWiki: (dir: string, name: string, repos: CoveredRepo[], taken: string[]) =>
    invoke<SetupRepoRow>("setup_create_wiki", { dir, name, repos, taken }),
  /** Ken's structure laid over an existing wiki: only the files it lacks are
   *  written. Returns its row and the files added. */
  setupOverlayWiki: (dir: string, name: string, repos: CoveredRepo[], taken: string[]) =>
    invoke<{ row: SetupRepoRow; added: string[] }>("setup_overlay_wiki", { dir, name, repos, taken }),
  /** The dry run of mapping an existing wiki's pages into the sections. */
  wikiMapPlan: (dir: string) => invoke<WikiMapPlan>("wiki_map_plan", { dir }),
  /** Apply the confirmed moves: links rewritten where certain, a log line,
   *  one commit. Nothing is deleted. */
  wikiMapApply: (dir: string, moves: WikiMove[]) => invoke<WikiMapApplied>("wiki_map_apply", { dir, moves }),
  /** Lay down template files the knowledge base lacks; never overwrites. */
  workspaceAddTemplateFiles: (paths: string[]) => invoke<string[]>("workspace_add_template_files", { paths }),
  /** Start building the library from the workspace's sources. */
  buildStart: (atOnce: number | null = null) => invoke<BuildProgress>("build_start", { atOnce }),
  buildStop: () => invoke<void>("build_stop"),
  buildSetAtOnce: (atOnce: number) => invoke<void>("build_set_at_once", { atOnce }),
  /** The one status panel. */
  backgroundStatus: () => invoke<BackgroundStatus>("background_status"),
  /** Set the one throttle: the pace and Pause. */
  backgroundSet: (pace: Pace, paused: boolean) => invoke<void>("background_set", { pace, paused }),
  /** Apply a proposed page change; returns the page written. */
  applyPageProposal: (itemId: number, projectId: string | null = null) =>
    invoke<string>("apply_page_proposal", { itemId, projectId }),
  /** The Ingest screen for the knowledge base's inbox. */
  ingestOverview: () => invoke<IngestOverview>("ingest_overview"),
  ingestCard: (itemId: number) => invoke<IngestCard>("ingest_card", { itemId }),
  /** Copy files into the knowledge base's Raw/ and start reading them. */
  ingestAdd: (paths: string[]) => invoke<string[]>("ingest_add", { paths }),
  /** A file dropped in the window (bytes, no path) into the knowledge base's Raw/. */
  ingestAddBytes: async (file: File) =>
    invoke<string>("ingest_add_bytes", new Uint8Array(await file.arrayBuffer()), {
      headers: { "x-name": encodeURIComponent(file.name) },
    }),
  /** Read what waits in Raw/ now; false when a pass is already running. */
  ingestNow: () => invoke<boolean>("ingest_now"),
  /** A note written in Ken, into Raw/ as `<date> Note - <title>.md`, then
   *  read at once. Returns its path. */
  ingestAddText: (title: string | null, text: string) => invoke<string>("ingest_add_text", { title, text }),
  /** A chat, whole or a few of its messages (only the person's and Ken's
   *  turns), into Raw/ as `<date> Chat - <title>.md`, then read at once.
   *  `messageIds` null is the whole chat. Returns its path. */
  ingestAddChat: (projectId: string, chatId: string, messageIds: number[] | null) =>
    invoke<string>("ingest_add_chat", { projectId, chatId, messageIds }),
  /** Try a source that failed again: its failure is cleared and a pass starts. */
  ingestRetry: (path: string) => invoke<boolean>("ingest_retry", { path }),
  /** Move a source out of Raw/ to the system trash. */
  ingestRemove: (path: string) => invoke<void>("ingest_remove", { path }),
  /** Mark an ingest card seen. A source an older card still holds in Raw
   *  moves beside its note first. Returns where the source is. */
  ingestSeen: (itemId: number) => invoke<string>("ingest_seen", { itemId }),
  /** The last drift sweep (null before the first). */
  /** Run the drift sweep now. */
  runDriftNow: (projectId: string | null = null) => invoke<DriftRun | null>("run_drift_now", { projectId }),
  /** The workspace settings screen (decision 37). */
  workspaceSettings: () => invoke<WorkspaceSettings>("workspace_settings"),
  /** The knowledge base's findings list alone, for the Inbox and the
   *  sidebar's count. */
  knowledgeFindings: () => invoke<Finding[]>("knowledge_findings"),
  /** The workspace's knowledge base (the repo with the inbox), or null. */
  knowledgeBaseId: () => invoke<string | null>("knowledge_base_id"),
  workspaceSaveIgnores: (lines: string[]) => invoke<void>("workspace_save_ignores", { lines }),
  /** The tier overrides, `pattern = tier` lines; refused with the bad line. */
  workspaceSaveWeights: (lines: string[]) => invoke<void>("workspace_save_weights", { lines }),
  /** One source's role rules, `pattern = role` lines, under its settings
   *  name; the source is scanned again. */
  workspaceSaveRoles: (projectId: string, name: string, lines: string[]) =>
    invoke<void>("workspace_save_roles", { projectId, name, lines }),
  /** Every control; they run at each source's next rebuild or scan. */
  workspaceSaveControls: (controls: { question: string; expect: string; source?: string | null }[]) =>
    invoke<void>("workspace_save_controls", { controls }),
  /** Sync conflicts and conflicted copies across the workspace's repos: the
   *  banner at the top of Files. */
  filesBanner: () => invoke<FilesBanner>("files_banner"),
  /** A new rule page in the knowledge base, from its rule template; returns its path. */
  workspaceAddRule: (wikiId: string, rule: string) => invoke<string>("workspace_add_rule", { wikiId, rule }),
  /** A page's links both ways: pages it reaches, pages that reach it. */
  pageLinks: (path: string) =>
    invoke<{ outgoing: string[]; incoming: string[] }>("page_links", { path }),
  /** A code file's outline (definitions, nested) and the files it imports
   *  and is imported by, from Ken's code index. */
  codeFile: (path: string) => invoke<CodeFile>("code_file", { path }),
  /** Go to definition and find usages for a symbol, across the workspace. */
  codeUsages: (name: string) => invoke<CodeUsages>("code_usages", { name }),
  setProjectIndex: (id: string, index: IndexState | null) =>
    invoke<RegistryEntryStatus[]>("set_project_index", { id, index }),
  setProjectKind: (id: string, kind: RepoKind[]) => invoke<RegistryEntryStatus[]>("set_project_kind", { id, kind }),
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
  /** Set-up Confirm: writes the manifest, kinds and the ticked
   *  ignore lines, then opens the workspace. */
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
  /** Workspaces opened recently, newest first. */
  listRecentWorkspaces: () => invoke<RecentWorkspace[]>("list_recent_workspaces"),
  forgetWorkspace: (id: string) => invoke<void>("forget_workspace", { id }),
  currentProject: () => invoke<ProjectInfo | null>("current_project"),
  setFolderSelection: (excluded: string[]) =>
    invoke<ProjectInfo>("set_folder_selection", { excluded }),
  getTree: () => invoke<TreeData>("get_tree"),
  /** Search the workspace's sources, merged once by relevance (knowledge-core
   *  decisions 15 and 20): every source, or only one the query names; a
   *  reference repo only for a platform question, when named, or when nothing
   *  else answers. `scope` pins one member (the "This repo" search) and
   *  `group` a named group. Progress rides `routed-search-state`. */
  routeSearch: (
    query: string,
    limit = 30,
    scope?: string | null,
    group?: string | null,
    types: KindFilter = null,
  ) =>
    invoke<RouteSearchResult>("route_search", {
      query,
      limit,
      scope: scope ?? null,
      group: group ?? null,
      types,
    }),
  /** Named groups of members, stored in the workspace manifest. */
  workspaceGroups: () => invoke<ProjectGroup[]>("workspace_groups"),
  /** Take a repo out of the workspace (its folder and files stay). Its
   *  knowledge base's link report names the pages that still cite it. */
  workspaceRemoveMember: (name: string) =>
    invoke<{ members: MemberOverview[] }>("workspace_remove_member", { name }),
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
  /** Confirm a supersession finding: Ken writes it into the decisions log as
   *  one commit, and returns the commit when it made one. */
  confirmFinding: (id: number, projectId: string | null = null) =>
    invoke<string | null>("confirm_finding", { id, projectId }),
  /** Raise an open question's finding as an issue in the RAID register: one
   *  commit, and the finding is marked done. Returns the issue's id. */
  raiseIssue: (id: number, projectId: string | null = null) => invoke<string>("raise_issue", { id, projectId }),
  /** Proposals no ingest card holds: rulings proposed in chat, edits drift held. */
  looseProposals: () => invoke<LooseProposal[]>("loose_proposals"),
  /** The repo analyses waiting in the knowledge base's Inbox. */
  analysisProposals: () => invoke<AnalysisProposal[]>("analysis_proposals"),
  /** Accept a repo analysis: its rules and controls go into
   *  `.ken/knowledge.json` as one commit, returned when one was made.
   *  Dismiss is `resolveReviewItem`. */
  acceptAnalysis: (itemId: number, projectId: string | null = null) =>
    invoke<string | null>("accept_analysis", { itemId, projectId }),
  /** "Analyse again": clears the source's "proposed before" mark and queues
   *  one background Claude call; its proposal arrives as a new Inbox card,
   *  replacing one still waiting for the source. */
  analyzeSourceAgain: (projectId: string) => invoke<void>("analyze_source_again", { projectId }),
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
  /** `projectId` names the member; none is the focused one. */
  syncStatus: (projectId: string | null = null) => invoke<SyncStatus>("sync_status", { projectId }),
  setSyncAuto: (auto: boolean) =>
    invoke<SyncStatus>("set_sync_auto", { auto }),
  syncNow: () => invoke<void>("sync_now"),
  /** Commit exactly these files changed outside Ken in the knowledge base,
   *  named in the message, then sync. Returns the commit, when one was made. */
  syncCommitOthers: (paths: string[], projectId: string | null = null) =>
    invoke<string | null>("sync_commit_others", { paths, projectId }),
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

  /** A file's neighbours in the project graph (knowledge-core decision
   *  21): `path` in the member `projectId` (default the focused one). */
  graphRelated: (path: string, projectId: string | null = null) =>
    invoke<RelatedNeighbour[]>("graph_related", { path, projectId }),
  /** The Log: the project's RAID register (risks, actions, issues,
   *  decisions) and milestones (knowledge-core decision 37). */
  raidRegister: () => invoke<RaidLog>("raid_register"),

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
   *  `error` (reason). App-global: a run reads the whole workspace journal. */
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
  /** Claude, asked by the person, opens a file in Ken (`open_in_ken`, in the personal tool set `ken-mcp --personal`). */
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
  /** Fires when a `.kenignore` edit has malformed lines. */
  onKenignoreWarning: (
    fn: (ev: KenignoreWarning) => void,
  ): Promise<UnlistenFn> =>
    listen<KenignoreWarning>("kenignore-warning", (e) => fn(e.payload)),
  onQuickAnswer: (fn: (answer: QuickAnswer) => void): Promise<UnlistenFn> =>
    listen<QuickAnswer>("quick-answer", (e) => fn(e.payload)),
  onQuickAnswerDelta: (fn: (ev: QuickAnswerDelta) => void): Promise<UnlistenFn> =>
    listen<QuickAnswerDelta>("quick-answer-delta", (e) => fn(e.payload)),
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
  /** Start a take: the transcript goes to the knowledge base's Raw/ and is
   *  read at once. Errors when nothing could start capturing. */
  recordStart: (mic: boolean, system: boolean, deviceId: string | null) =>
    invoke<void>("record_start", { mic, system, deviceId }),
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
  /** Your day, across every repo of the workspace. */
  dayState: () => invoke<DayState>("day_state"),
  /** A new task in the workspace home; updated_by "you". */
  dayTaskCreate: (input: DayTaskInput) => invoke<DayTask>("day_task_create", { input }),
  /** Patch a task: only the keys named change; `target: null` clears it. */
  dayTaskUpdate: (id: string, patch: DayTaskPatch) => invoke<DayTask>("day_task_update", { id, patch }),
  /** Moves the file to its home's archive folder. */
  dayTaskDelete: (id: string) => invoke<void>("day_task_delete", { id }),
  /** Tasks linked to a ticket, done included. */
  ticketTasks: (projectId: string, ticketId: string) =>
    invoke<DayTask[]>("ticket_tasks", { projectId, ticketId }),
  /** The newest stored digest for the workspace, or null. */
  dayDigest: () => invoke<DayDigest | null>("day_digest"),
  /** Write the digest now. Outcome arrives on the day-digest events. */
  refreshDayDigest: () => invoke<void>("refresh_day_digest"),
  /** Move a ticket to a status (todo, in-progress, blocked, in-review, testing, done, cancelled). */
  ticketSetStatus: (projectId: string, relPath: string, status: string) =>
    invoke<void>("ticket_set_status", { projectId, relPath, status }),
  /** Whether the digest is being written or waits its turn. */
  dayDigestWriting: () => invoke<boolean>("day_digest_writing"),
  /** Repos indexed, files queued and failed, across the workspace. */
  indexHealth: () => invoke<IndexHealth>("index_health"),
  /** A task file or ticket file changed. */
  onDayChanged: (fn: () => void): Promise<UnlistenFn> => listen<null>("day-changed", () => fn()),
  onDayDigestUpdated: (fn: (digest: DayDigest) => void): Promise<UnlistenFn> =>
    listen<DayDigest>("day-digest-updated", (e) => fn(e.payload)),
  /** The digest was asked for: it is being written or waits its turn. */
  onDayDigestGenerating: (fn: () => void): Promise<UnlistenFn> => listen<unknown>("day-digest-generating", () => fn()),
  onDayDigestError: (fn: (message: string) => void): Promise<UnlistenFn> =>
    listen<{ message: string | null }>("day-digest-error", (e) =>
      fn(e.payload?.message ?? ""),
    ),
};
