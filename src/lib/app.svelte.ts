// Global app state (Svelte 5 runes). One store: project, tree, navigation.
import {
  api,
  type FileRow,
  type FolderInfo,
  type ProjectInfo,
  type RegistryEntryStatus,
  type ScanStats,
  type SemanticIndexState,
  type SetupIgnoreRow,
  type SetupRepoRow,
  type SyncStateName,
  type WorkspaceOverview,
} from "./api";
import {
  addFavorite,
  loadFavorites,
  pruneFavorites,
  removeFavorite,
  renameFavoritesForMove,
  saveFavorites,
  type Favorite,
} from "./favorites";
import {
  loadRecents,
  recordRecent,
  saveRecents,
  type RecentEntry,
} from "./recent";
import {
  clampChatWidth,
  loadChatWidth,
  saveChatWidth,
} from "./chatWidth";
import {
  clampSidebarWidth,
  loadSidebarWidth,
  saveSidebarWidth,
} from "./sidebar";
import { loadFollowOpen, saveFollowOpen } from "./followOpen";
import {
  closeAll as reduceCloseAll,
  closeOthers as reduceCloseOthers,
  closeRight as reduceCloseRight,
  closeTab as reduceCloseTab,
  makePersistent as reduceMakePersistent,
  openTab as reduceOpenTab,
  renameTabsForMove,
  setPinned as reduceSetPinned,
  type FileTab,
  type TabState,
} from "../files/tabs";
import { coalesce, singleFlight } from "./refresh";

/** How long the tree and the unread set wait after an `index-updated`. */
const TREE_READ_MS = 500;

/** Where to scroll an opened file to: a source line or a heading slug. A new
 *  `nonce` asks again for the same place. */
export interface Reveal {
  path: string;
  line?: number;
  anchor?: string;
  nonce: number;
}

export type Screen =
  | "home"
  | "inbox"
  | "files"
  | "ingests"
  | "explore"
  | "settings";

/** Settings' sections (the Briefing design's sub-nav). Team library holds what
 *  the Team screen did: the team's folders, ignore list, checks and rules. */
export type SettingsSection = "general" | "team" | "sync" | "ai" | "agents" | "rules";

/** Explore's two views of the same knowledge. */
export type ExploreView = "timeline" | "map";

/** One entry in `app.members` (workspace task 4.3). In Single mode (no
 *  workspace open) this is just the one open project, `status: "active"`
 *  always — a `ProjectInfo` subset, so that path stays the same shape it
 *  always was. In Workspace mode it's sourced from `WorkspaceOverview.
 *  members`: `id` is `null` for `missing`/`invalid` members (no resolvable
 *  `ProjectHandle` to open). */
export interface MemberInfo {
  id: string | null;
  name: string;
  status: "active" | "dormant" | "missing" | "invalid";
}

class AppStore {
  /** The focused member. Kept as the pre-workspace field name/shape so every
   *  existing read/write site keeps working unchanged — see `members` and
   *  `focused` below for the workspace-aware view onto the same data. */
  project = $state<ProjectInfo | null>(null);
  registry = $state<RegistryEntryStatus[]>([]);
  screen = $state<Screen>("home");
  settingsSection = $state<SettingsSection>("general");
  exploreView = $state<ExploreView>("timeline");

  files = $state<FileRow[]>([]);
  folders = $state<FolderInfo[]>([]);

  /** Open editor tabs (VS Code-style preview + pinning), persisted per project. */
  fileTabs = $state<FileTab[]>([]);
  activeTab = $state<string | null>(null);

  /** Favorites shown above the Files tree, persisted per project. */
  favorites = $state<Favorite[]>([]);

  /** Files opened recently, newest first — Home's "pick up where you left off". */
  recents = $state<RecentEntry[]>([]);

  /** Files sidebar width in px — a window preference, so it spans projects. */
  sidebarWidth = $state(loadSidebarWidth());

  /** Whether the Files tree follows the open file (expands + scrolls to it as
   *  tabs change). A window preference, so it spans projects. */
  followOpen = $state(loadFollowOpen());

  /** Chat drawer width in px — a window preference, so it spans projects. */
  chatWidth = $state(loadChatWidth());

  /** Reveal request: tree folders on the path to this rel-path auto-expand. */
  revealTarget = $state<string | null>(null);
  revealNonce = $state(0);

  /** The active tab's path — drives tree selection and the mounted editor. */
  get openFile(): string | null {
    return this.activeTab;
  }

  /** The open workspace's overview (workspace task 4.3), or `null` when no
   *  workspace is open — including Single mode, and whenever the
   *  `workspace` flag is off. Populated by `openWorkspace`/`createWorkspace`
   *  and kept live by the `workspace-state`/`member-status` events; `null`s
   *  out on `close_workspace`. */
  workspace = $state<WorkspaceOverview | null>(null);

  /** Workspaces are how Ken works, no longer a flag: always true. Kept so
   *  the screens that read it stay as they are. */
  readonly workspaceFlagEnabled = true;

  /** Every known member: in Workspace mode, the full roster from
   *  `workspace.members` (workspace task 4.3 — click/`Ctrl+P` cycle target,
   *  status dots); in Single mode, just the one open project as a
   *  single-entry list — derived, never stored separately, so it can never
   *  drift from `project`. */
  get members(): MemberInfo[] {
    if (this.workspace) {
      return this.workspace.members.map((m) => ({
        id: m.projectId,
        name: m.name,
        status: m.status,
      }));
    }
    return this.project
      ? [{ id: this.project.id, name: this.project.name, status: "active" }]
      : [];
  }

  /** The focused member's id, or null with nothing open. Event payloads that
   *  carry an optional `project_id` (S9 step 5) are compared against this —
   *  see `forFocused` below — to ignore events for a member other than the
   *  one currently focused. Workspace mode reads the manifest's `focused`
   *  field directly (kept in step with `project.id` by `loadFocusedMemberState`
   *  below); Single mode falls back to `project.id` unchanged. */
  get focused(): string | null {
    if (this.workspace) return this.workspace.focused;
    return this.project?.id ?? null;
  }

  scanning = $state(false);
  lastScan = $state<ScanStats | null>(null);
  lastScanAt = $state<number | null>(null);
  scanError = $state<string | null>(null);

  searchOpen = $state(false);

  /** The place an open file should scroll to, set by a clicked citation. */
  revealAt = $state<Reveal | null>(null);

  /** Team-sync state for the title-bar dot ("off" = not a synced project). */
  syncState = $state<SyncStateName>("off");
  syncDetail = $state<string | null>(null);

  /** Whether cloud-offline documents are indexed in the background (on by
   *  default). Shared so Settings and the Home footer agree instantly. */
  backgroundIndex = $state(true);

  /** Whether videos are auto-transcribed on-device during indexing (off by
   *  default — Whisper is slow). Shared so Settings reflects it instantly. */
  transcribeVideosOnIndex = $state(false);

  /** Whether the semantic (meaning-based) index is enabled for this project.
   *  Persisted on the backend and read back via `api.getSemanticIndex` on
   *  project activation (see `loadSemanticIndex`). */
  semanticIndex = $state(false);

  /** Live build/availability status of the semantic index, driven by the
   *  `semantic-index-state` event. `null` until the first event arrives. */
  semanticIndexState = $state<SemanticIndexState | null>(null);

  /** Files the user has ignored (per-user, app-data, never synced). Their
   *  issues are hidden but they stay indexed and searchable. */
  ignored = $state<string[]>([]);

  /** Files changed by someone/something else since the user last looked —
   *  per-user, app-data, never synced. Drives the Files nav dot, the tree's
   *  unread markers, and the "unread" filter. */
  unread = $state<string[]>([]);

  /** Membership set for O(1) per-row unread checks in the tree. */
  unreadSet = $derived(new Set(this.unread));

  isUnread(path: string): boolean {
    return this.unreadSet.has(path);
  }

  /** Files-tree filter: everything, or only files changed since last looked.
   *  Ephemeral view state (not persisted). */
  filesFilter = $state<"all" | "unread">("all");

  get failedFiles(): FileRow[] {
    const hidden = new Set(this.ignored);
    return this.files.filter(
      (f) => f.status === "failed" && !hidden.has(f.relPath),
    );
  }

  /** Stop flagging a file that could not be read (the Files dot, its mark
   *  in the tree, the reason in its preview), for this user only. */
  async ignoreFile(relPath: string) {
    await api.ignoreFile(relPath);
    if (!this.ignored.includes(relPath)) this.ignored = [...this.ignored, relPath];
  }

  /** Stop ignoring a file so its issues can surface again. */
  async unignoreFile(relPath: string) {
    await api.unignoreFile(relPath);
    this.ignored = this.ignored.filter((p) => p !== relPath);
  }

  private async loadIgnored() {
    this.ignored = await api.listIgnored().catch(() => []);
  }

  private readonly loadUnread = singleFlight(async () => {
    this.unread = await api.unreadFiles().catch(() => []);
  });

  private readonly queueTreeRead = coalesce(() => {
    void this.refreshTree();
    void this.loadUnread();
  }, TREE_READ_MS);

  /** Record a file as seen (viewing it clears its unread state). The backend
   *  no-ops when the seen version already matches, so calling on every open is
   *  cheap and stays correct even if the local unread list is momentarily stale. */
  async markSeen(relPath: string) {
    if (this.isUnread(relPath)) {
      this.unread = this.unread.filter((p) => p !== relPath);
    }
    await api.markSeen(relPath).catch(() => {});
  }

  /** Mark a whole folder's files seen ("Mark folder as viewed"). Clears the
   *  subtree optimistically so the tree settles before the round-trip. */
  async markFolderSeen(prefix: string) {
    const under = prefix + "/";
    this.unread = this.unread.filter(
      (p) => p !== prefix && !p.startsWith(under),
    );
    await api.markSeenUnder(prefix).catch(() => {});
  }

  /** Clear every unread file at once ("Mark all as viewed"). */
  async markAllSeen() {
    this.unread = [];
    await api.markAllSeen().catch(() => {});
  }

  async init() {
    this.registry = await api.listProjects();
    this.project = await api.currentProject();
    await api.onIndexUpdated((stats) => {
      if (!forFocused(stats.project_id)) return;
      this.scanning = false;
      this.lastScan = stats;
      this.lastScanAt = Date.now();
      // The index changing is exactly when files become (un)read — a synced or
      // externally-edited file lands here — so recompute the unread set live.
      // Indexing sends this many times a second: read the tree and the
      // unread set once per burst (see `coalesce`).
      this.queueTreeRead();
    });
    await api.onScanError((message) => {
      this.scanning = false;
      this.scanError = message;
    });
    await api.onSemanticIndexState((ev) => {
      if (!forFocused(ev.project_id)) return;
      this.semanticIndexState = ev;
    });
    // Non-intrusive: log for now rather than a toast/banner component, since
    // no such pattern exists elsewhere in the app yet. Still surfaces the
    // 1-based malformed line numbers for anyone checking devtools.
    await api.onKenignoreWarning((ev) => {
      if (!forFocused(ev.project_id)) return;
      console.warn(
        `.kenignore: skipped malformed line(s) ${ev.malformedLines.join(", ")}`,
      );
    });
    await api.onSyncState((ev) => {
      if (!forFocused(ev.project_id)) return;
      this.syncState = ev.state;
      this.syncDetail = ev.detail;
    });
    // Workspace lifecycle events (workspace task 4.1/4.3).
    await api.onWorkspaceState((ev) => {
      if (ev.state === "closed") {
        this.workspace = null;
      } else if (ev.state === "open" || ev.state === "focus") {
        void this.refreshWorkspaceOverview();
      }
      // "opening" carries no actionable state yet (members not activated) —
      // the launcher shows its own inline progress via `member-status`.
    });
    await api.onMemberStatus(() => {
      // A member's runtime transitioned (active/dormant) — refresh the
      // roster so status dots stay live even when the transition didn't
      // move focus (e.g. an LRU eviction triggered by someone else's
      // focus change). No-ops when no workspace is open.
      if (!this.workspace) return;
      void this.refreshWorkspaceOverview();
    });
    if (this.project) {
      this.loadProjectLocalState();
      void this.loadBackgroundIndex();
      void this.loadIgnored();
      void this.loadUnread();
      await this.refreshTree();
    } else {
      // Launch straight into the workspace used last when it's still around;
      // any failure just leaves the start screen showing.
      const recent = await api.listRecentWorkspaces().catch(() => []);
      const last = recent.find((w) => w.available);
      if (last) await this.openWorkspace(last.path).catch(() => {});
    }
  }

  async refreshRegistry() {
    this.registry = await api.listProjects();
  }

  /** Files shows the whole workspace as one tree, projects at the top
   *  level, rather than only the focused member. Off in single-project
   *  mode, where there is nothing to merge. */
  treeShowsAllProjects = $state(false);

  /** One tree read at a time (see `singleFlight`). */
  readonly refreshTree = singleFlight(async () => {
    if (!this.project) return;
    const merged = this.treeShowsAllProjects && !!this.workspace;
    // A failed merged read must not blank the tree — fall back to the
    // focused member rather than showing nothing.
    const tree = merged
      ? await api.getTreeAll().catch(() => null)
      : await api.getTree();
    const resolved = tree ?? (await api.getTree());
    this.files = resolved.files;
    this.folders = resolved.folders;
    this.pruneFavorites();
  });

  /** Switch Files between the merged workspace tree and the focused
   *  member's own. */
  async setTreeShowsAllProjects(all: boolean) {
    if (this.treeShowsAllProjects === all) return;
    this.treeShowsAllProjects = all;
    await this.refreshTree();
  }

  private pruneFavorites() {
    if (!this.project) return;
    const existing = new Set<string>();
    for (const f of this.files) existing.add(f.relPath);
    for (const f of this.folders) existing.add(f.relPath);
    const next = pruneFavorites(this.favorites, existing);
    if (next.length !== this.favorites.length) {
      this.favorites = next;
      saveFavorites(this.project.id, this.favorites);
    }
  }

  private async activated(info: ProjectInfo) {
    this.project = info;
    this.scanning = true;
    this.scanError = null;
    this.syncState = "off";
    this.syncDetail = null;
    // Live build/availability status is per-session — reset on every switch,
    // then repopulated by the semantic-index-state event once it fires.
    this.semanticIndexState = null;
    this.loadProjectLocalState();
    void this.loadBackgroundIndex();
    void this.loadTranscribeOnIndex();
    void this.loadSemanticIndex();
    void this.loadIgnored();
    void this.loadUnread();
    this.screen = "home";
    await this.refreshRegistry();
    await this.refreshTree();
  }

  /** Read the persisted background-index preference for the open project. */
  private async loadBackgroundIndex() {
    this.backgroundIndex = await api.getBackgroundIndex().catch(() => true);
  }

  /** Toggle background indexing of cloud-offline documents (persisted). */
  async setBackgroundIndex(enabled: boolean) {
    this.backgroundIndex = enabled;
    await api.setBackgroundIndex(enabled);
  }

  /** Read the persisted auto-transcription preference for the open project. */
  private async loadTranscribeOnIndex() {
    this.transcribeVideosOnIndex = await api.getTranscribeOnIndex().catch(() => false);
  }

  /** Toggle automatic video transcription during indexing (persisted). */
  async setTranscribeVideosOnIndex(enabled: boolean) {
    this.transcribeVideosOnIndex = enabled;
    await api.setTranscribeOnIndex(enabled);
  }

  /** Read the persisted semantic-index preference for the open project. */
  private async loadSemanticIndex() {
    this.semanticIndex = await api.getSemanticIndex().catch(() => false);
  }

  /** Toggle the semantic (meaning-based) search index (persisted, so
   *  ingestion honors it after a restart and the toggle reflects it on the
   *  next project open). */
  async setSemanticIndex(enabled: boolean) {
    this.semanticIndex = enabled;
    if (!enabled) this.semanticIndexState = null;
    await api.setProjectFeature("semanticIndex", enabled);
  }

  /** Restore tabs + favorites + recents for the current project from localStorage. */
  private loadProjectLocalState() {
    this.fileTabs = [];
    this.activeTab = null;
    this.favorites = [];
    this.recents = [];
    if (!this.project) return;
    this.favorites = loadFavorites(this.project.id);
    this.recents = loadRecents(this.project.id);
    try {
      const raw = localStorage.getItem(this.tabsKey(this.project.id));
      if (raw) {
        const data = JSON.parse(raw) as {
          tabs?: FileTab[];
          active?: string | null;
        };
        if (Array.isArray(data.tabs)) {
          this.fileTabs = data.tabs
            .filter((t) => t && typeof t.path === "string")
            .map((t) => ({
              path: t.path,
              pinned: !!t.pinned,
              preview: !!t.preview,
            }));
          this.activeTab =
            typeof data.active === "string" &&
            this.fileTabs.some((t) => t.path === data.active)
              ? data.active
              : (this.fileTabs[0]?.path ?? null);
        }
      }
    } catch {
      /* corrupt tab state — start clean */
    }
  }

  private tabsKey(projectId: string) {
    return `ken.files.tabs.${projectId}`;
  }

  private persistTabs() {
    if (!this.project) return;
    try {
      localStorage.setItem(
        this.tabsKey(this.project.id),
        JSON.stringify({ tabs: this.fileTabs, active: this.activeTab }),
      );
    } catch {
      /* best-effort */
    }
  }

  private applyTabState(next: TabState) {
    this.fileTabs = next.tabs;
    this.activeTab = next.active;
    this.persistTabs();
  }

  // ── Tabs ──────────────────────────────────────────────────────────────
  /** Open a file in a preview tab (single-click) or persistent tab. */
  openTab(path: string, persistent = false) {
    // In the merged workspace tree every path is `<member folder>/<rest>`,
    // but reads, tabs and recents are all project-relative. So a merged
    // path is resolved HERE, at the one funnel every open goes through:
    // focus the member it names, then open the remainder against it.
    //
    // Opening a file this way leaves you in that project with its own
    // tree — picking a file out of the merged view is how you travel to
    // a project, which is more predictable than staying merged and having
    // tabs from several projects that look alike.
    if (this.treeShowsAllProjects && this.workspace) {
      // Longest-prefix match, not first-segment split: a nested member's
      // key is itself two segments ("SR/ShatteredRealms"), so the member
      // is whichever key the path starts with at a segment boundary.
      const member = this.workspace.members
        .filter((m) => path === m.name || path.startsWith(m.name + "/"))
        .sort((a, b) => b.name.length - a.name.length)[0];
      if (member?.projectId) {
        const rest = path === member.name ? "" : path.slice(member.name.length + 1);
        void this.focusMember(member.projectId).then(() => {
          this.treeShowsAllProjects = false;
          if (rest) this.openTab(rest, persistent);
        });
        return;
      }
    }
    this.applyTabState(reduceOpenTab({ tabs: this.fileTabs, active: this.activeTab }, path, persistent));
    this.recents = recordRecent(this.recents, path);
    if (this.project) saveRecents(this.project.id, this.recents);
    // Reveal the path we were asked to open, not `activeTab` — the reducer owns
    // which tab ends up active and could in principle land elsewhere.
    if (this.followOpen) this.reveal(path);
  }

  activateTab(path: string) {
    this.activeTab = path;
    this.persistTabs();
    if (this.followOpen) this.reveal(path);
  }

  closeTab(path: string) {
    this.applyTabState(reduceCloseTab({ tabs: this.fileTabs, active: this.activeTab }, path));
  }

  closeOtherTabs(path: string) {
    this.applyTabState(reduceCloseOthers({ tabs: this.fileTabs, active: this.activeTab }, path));
  }

  closeTabsToRight(path: string) {
    this.applyTabState(reduceCloseRight({ tabs: this.fileTabs, active: this.activeTab }, path));
  }

  closeAllTabs() {
    this.applyTabState(reduceCloseAll({ tabs: this.fileTabs, active: this.activeTab }));
  }

  makeTabPersistent(path: string) {
    this.applyTabState(reduceMakePersistent({ tabs: this.fileTabs, active: this.activeTab }, path));
  }

  setTabPinned(path: string, pinned: boolean) {
    this.applyTabState(reduceSetPinned({ tabs: this.fileTabs, active: this.activeTab }, path, pinned));
  }

  // ── Favorites ─────────────────────────────────────────────────────────
  isFavorite(path: string): boolean {
    return this.favorites.some((f) => f.path === path);
  }

  toggleFavorite(path: string, kind: "file" | "folder") {
    this.favorites = this.isFavorite(path)
      ? removeFavorite(this.favorites, path)
      : addFavorite(this.favorites, { path, kind });
    if (this.project) saveFavorites(this.project.id, this.favorites);
  }

  removeFavorite(path: string) {
    this.favorites = removeFavorite(this.favorites, path);
    if (this.project) saveFavorites(this.project.id, this.favorites);
  }

  // ── Sidebar ───────────────────────────────────────────────────────────
  /** Live width while the divider is being dragged. */
  setSidebarWidth(width: number, windowWidth: number = window.innerWidth) {
    this.sidebarWidth = clampSidebarWidth(width, windowWidth);
  }

  /** Write the settled width — separate from the setter so a drag doesn't hit
   *  localStorage on every frame. */
  commitSidebarWidth() {
    saveSidebarWidth(this.sidebarWidth);
  }

  /** Toggle "files list follows the open file". */
  setFollowOpen(value: boolean) {
    this.followOpen = value;
    saveFollowOpen(value);
  }

  // ── Chat drawer ───────────────────────────────────────────────────────
  /** Live width while the divider is being dragged. */
  setChatWidth(width: number, windowWidth: number = window.innerWidth) {
    this.chatWidth = clampChatWidth(width, windowWidth);
  }

  /** Write the settled width — separate from the setter so a drag doesn't hit
   *  localStorage on every frame. */
  commitChatWidth() {
    saveChatWidth(this.chatWidth);
  }

  /** Ask the tree to expand every folder on the way to `path`. */
  reveal(path: string) {
    this.revealTarget = path;
    this.revealNonce += 1;
  }

  async openProject(path: string) {
    await this.activated(await api.openProject(path));
  }

  async createProject(path: string, name: string) {
    await this.activated(await api.createProject(path, name));
  }

  // ── Workspace (workspace change, task 4.3) ──────────────────────────────

  /** Open an existing workspace manifest at `parent` (launcher flow). */
  async openWorkspace(parent: string) {
    const overview = await api.openWorkspace(parent);
    this.workspace = overview;
    await this.loadFocusedMemberState(overview);
  }

  /** Create a new workspace over `parent` from the selected member folder
   *  names, then open it (launcher flow). */
  async createWorkspace(parent: string, name: string, members: string[]) {
    const overview = await api.createWorkspace(parent, name, members);
    this.workspace = overview;
    await this.loadFocusedMemberState(overview);
  }

  /** Set-up from picked repos: a new workspace in Ken's app data, or (add)
   *  more repos into the open one. Opens it. */
  async confirmSetupRepos(name: string, rows: SetupRepoRow[], add = false) {
    const overview = await api.setupConfirmRepos(name, rows, add);
    this.workspace = overview;
    await this.loadFocusedMemberState(overview);
  }

  /** Set-up's Confirm: write what the person chose and open the workspace. */
  async confirmSetup(parent: string, name: string, rows: SetupRepoRow[], ignores: SetupIgnoreRow[]) {
    const overview = await api.setupConfirm(parent, name, rows, ignores);
    this.workspace = overview;
    await this.loadFocusedMemberState(overview);
  }

  /** Switch focus to member `id` — the nav-rail switcher's click/`Ctrl+P`
   *  cycle target. `focus_project` also fires `workspace-state`'s `focus`
   *  event (handled in `init()`, routes back through
   *  `refreshWorkspaceOverview`); awaiting the direct refresh here just
   *  means the caller doesn't wait an extra event round-trip to see the
   *  switch land. */
  async focusMember(id: string, opts: { stay?: boolean } = {}) {
    if (!this.workspace || this.workspace.focused === id) return;
    // A change of focus lands on Home; `stay` keeps the screen the person
    // is on (a Team repo's settings, a conflict in Files).
    this.keepScreen = !!opts.stay;
    try {
      await api.focusProject(id);
      await this.refreshWorkspaceOverview();
    } finally {
      this.keepScreen = false;
    }
  }

  /** Set while a focus change should leave the screen as it is. */
  private keepScreen = false;

  /** Cycle focus to the next resolvable member, roster order (`Ctrl+P` —
   *  workspace task 4.3). No-ops outside Workspace mode or with fewer than
   *  two resolvable (non-`missing`/`invalid`) members. */
  async cycleFocusedMember() {
    if (!this.workspace) return;
    const resolvable = this.members.filter(
      (m): m is MemberInfo & { id: string } => m.id !== null,
    );
    if (resolvable.length < 2) return;
    const idx = resolvable.findIndex((m) => m.id === this.focused);
    const next = resolvable[(idx + 1) % resolvable.length];
    await this.focusMember(next.id);
  }

  /** Close the open workspace, tearing down every member's runtime and
   *  returning to the picker — the workspace equivalent of closing the
   *  sole open project in Single mode (no such action exists there either;
   *  Single mode only ever switches projects). */
  async closeWorkspaceSession() {
    await api.closeWorkspace();
    this.workspace = null;
    this.project = null;
  }

  /** Re-fetch the workspace roster (status dots, membership) and, if focus
   *  moved, reload the newly-focused member's per-project caches. Called
   *  from `workspace-state`'s `open`/`focus` events and `member-status`
   *  events (workspace task 4.3: "screens reload their stores on the focus
   *  workspace-state event"). */
  /** Public re-read of the workspace roster, for callers that changed
   *  membership themselves (adding a project) and can't wait for the
   *  `workspace-state` event that normally drives this. */
  async refreshWorkspace() {
    await this.refreshWorkspaceOverview();
  }

  async refreshWorkspaceOverview() {
    const overview = await api.workspaceOverview().catch(() => null);
    if (!overview) return;
    const focusChanged = this.workspace?.focused !== overview.focused;
    this.workspace = overview;
    if (focusChanged) await this.loadFocusedMemberState(overview);
  }

  /** Reload every per-project cache for the workspace's currently focused
   *  member — the workspace-mode counterpart of `activated()` (same
   *  refresh list: tabs/favorites/recents, background/transcribe/semantic
   *  index settings, ignored/unread, the file tree).
   *  Kept as a separate method rather than folded into `activated()` so the
   *  Single-mode path (`openProject`/`createProject`) stays byte-identical.
   *
   *  DEVIATION: no command returns a full `ProjectInfo` for a workspace
   *  member other than the one just opened/created — `focus_project` and
   *  `workspace_overview` only carry `name`/`projectId`/`status`. `root` is
   *  still reconstructed exactly (`workspace.root + "/" + member.name` —
   *  members are stored as parent-relative folder names, workspace design
   *  D1), but `excluded` has no per-member read path, so it defaults to
   *  `[]` (`ProjectInfo::of`'s own fallback) rather than carrying over the
   *  PREVIOUS member's value, which would be actively wrong. A repo's
   *  watched-folders card may show that default instead of the member's real
   *  value until a per-member info command exists — the backend truth
   *  itself is unaffected (every mutating command resolves "the project"
   *  via `state.focused` regardless of what the frontend has cached). See
   *  final report. */
  private async loadFocusedMemberState(overview: WorkspaceOverview) {
    const member = overview.members.find((m) => m.projectId === overview.focused);
    if (!overview.focused || !member) {
      this.project = null;
      return;
    }
    this.project = {
      id: overview.focused,
      name: member.name,
      root: `${overview.root}/${member.name}`,
      excluded: [],
    };
    this.scanning = false;
    this.scanError = null;
    this.syncState = "off";
    this.syncDetail = null;
    this.semanticIndexState = null;
    this.loadProjectLocalState();
    void this.loadBackgroundIndex();
    void this.loadTranscribeOnIndex();
    void this.loadSemanticIndex();
    void this.loadIgnored();
    void this.loadUnread();
    if (!this.keepScreen) this.screen = "home";
    await this.refreshTree();
  }

  async setExcluded(excluded: string[]) {
    if (!this.project) return;
    this.project = await api.setFolderSelection(excluded);
    await this.refreshTree();
  }

  async reindex() {
    this.scanning = true;
    try {
      this.lastScan = await api.reindex();
      this.lastScanAt = Date.now();
      await this.refreshTree();
    } finally {
      this.scanning = false;
    }
  }

  openInFiles(relPath: string) {
    this.openTab(relPath, false);
    this.screen = "files";
    this.searchOpen = false;
  }

  /** Open a file in Files at a line or a heading (a clicked citation). */
  openAt(relPath: string, where: { line?: number; anchor?: string }) {
    this.openInFiles(relPath);
    this.revealAt = where.line || where.anchor ? { path: relPath, ...where, nonce: Date.now() } : null;
  }

  openSettings(section?: SettingsSection) {
    if (section) this.settingsSection = section;
    this.screen = "settings";
    this.searchOpen = false;
  }

  /** The team's folders, rules and checks (was the Team screen). */
  openTeam() {
    this.openSettings("team");
  }

  openExplore(view?: ExploreView) {
    if (view) this.exploreView = view;
    this.screen = "explore";
    this.searchOpen = false;
  }

  /** Move a file or folder: update open tabs, favorites, and the selection.
   *  Folder moves rewrite every tab/favorite under the old prefix. */
  async moveFile(fromRel: string, toRel: string) {
    await api.moveFile(fromRel, toRel);
    this.applyTabState(
      renameTabsForMove({ tabs: this.fileTabs, active: this.activeTab }, fromRel, toRel),
    );
    this.favorites = renameFavoritesForMove(this.favorites, fromRel, toRel);
    if (this.project) saveFavorites(this.project.id, this.favorites);
    await this.refreshTree();
  }

  /** Move a file or folder to the OS trash (recoverable), then forget it:
   *  close any open tab for that path — or, for a folder, every tab under it —
   *  and drop matching favorites and recents. Folder deletes are prefix-aware. */
  async deleteFile(relPath: string) {
    await api.deleteFile(relPath);
    // A folder delete takes everything under its prefix with it.
    const under = (p: string) => p === relPath || p.startsWith(relPath + "/");
    const tabs = this.fileTabs.filter((t) => !under(t.path));
    const active =
      this.activeTab && under(this.activeTab)
        ? (tabs[tabs.length - 1]?.path ?? null)
        : this.activeTab;
    this.applyTabState({ tabs, active });
    this.favorites = this.favorites.filter((f) => !under(f.path));
    this.recents = this.recents.filter((r) => !under(r.path));
    if (this.project) {
      saveFavorites(this.project.id, this.favorites);
      saveRecents(this.project.id, this.recents);
    }
    await this.refreshTree();
  }
}

export const app = new AppStore();

/** True when `projectId` is absent — an unscoped/global event, always passed
 *  through for backward compatibility — or equals the focused member's id.
 *  Stores compare a member-scoped event's optional `project_id` (S9 step 5)
 *  against this to ignore events for a workspace member other than the one
 *  currently focused (S9 step 7 / `workspace` change). */
export function forFocused(projectId: string | null | undefined): boolean {
  return projectId == null || projectId === app.focused;
}
