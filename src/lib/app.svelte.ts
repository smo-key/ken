// Global app state (Svelte 5 runes). One store: project, tree, navigation.
import {
  api,
  type FileRow,
  type FolderInfo,
  type ProjectInfo,
  type RegistryEntryStatus,
  type ScanStats,
  type SyncStateName,
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
import { review } from "./review.svelte";
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
import {
  closeOthers as reduceCloseOthers,
  closeTab as reduceCloseTab,
  makePersistent as reduceMakePersistent,
  openTab as reduceOpenTab,
  renameTabsForMove,
  setPinned as reduceSetPinned,
  type FileTab,
  type TabState,
} from "../files/tabs";

export type Screen =
  | "home"
  | "files"
  | "review"
  | "ingests"
  | "map"
  | "timeline"
  | "settings";

class AppStore {
  project = $state<ProjectInfo | null>(null);
  registry = $state<RegistryEntryStatus[]>([]);
  screen = $state<Screen>("home");

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

  /** Chat drawer width in px — a window preference, so it spans projects. */
  chatWidth = $state(loadChatWidth());

  /** Reveal request: tree folders on the path to this rel-path auto-expand. */
  revealTarget = $state<string | null>(null);
  revealNonce = $state(0);

  /** The active tab's path — drives tree selection and the mounted editor. */
  get openFile(): string | null {
    return this.activeTab;
  }

  scanning = $state(false);
  lastScan = $state<ScanStats | null>(null);
  lastScanAt = $state<number | null>(null);
  scanError = $state<string | null>(null);

  searchOpen = $state(false);

  /** Team-sync state for the title-bar dot ("off" = not a synced project). */
  syncState = $state<SyncStateName>("off");
  syncDetail = $state<string | null>(null);

  /** Whether cloud-offline documents are indexed in the background (on by
   *  default). Shared so Settings and the Home footer agree instantly. */
  backgroundIndex = $state(true);

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

  /** Hide a file's issues everywhere (Review inbox, badge, Home) for this user. */
  async ignoreFile(relPath: string) {
    await api.ignoreFile(relPath);
    if (!this.ignored.includes(relPath)) this.ignored = [...this.ignored, relPath];
    // The badge/inbox recompute on the backend; refresh so they reflect it now.
    await review.refresh();
  }

  /** Stop ignoring a file so its issues can surface again. */
  async unignoreFile(relPath: string) {
    await api.unignoreFile(relPath);
    this.ignored = this.ignored.filter((p) => p !== relPath);
    await review.refresh();
  }

  private async loadIgnored() {
    this.ignored = await api.listIgnored().catch(() => []);
  }

  private async loadUnread() {
    this.unread = await api.unreadFiles().catch(() => []);
  }

  /** Record a file as seen (viewing it clears its unread state). The backend
   *  no-ops when the seen version already matches, so calling on every open is
   *  cheap and stays correct even if the local unread list is momentarily stale. */
  async markSeen(relPath: string) {
    if (this.isUnread(relPath)) {
      this.unread = this.unread.filter((p) => p !== relPath);
    }
    await api.markSeen(relPath).catch(() => {});
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
      this.scanning = false;
      this.lastScan = stats;
      this.lastScanAt = Date.now();
      void this.refreshTree();
      // The index changing is exactly when files become (un)read — a synced or
      // externally-edited file lands here — so recompute the unread set live.
      void this.loadUnread();
    });
    await api.onScanError((message) => {
      this.scanning = false;
      this.scanError = message;
    });
    await api.onSyncState((ev) => {
      this.syncState = ev.state;
      this.syncDetail = ev.detail;
    });
    // Wire the review inbox to its events here (app start), not on Review-tab
    // mount, so the nav badge stays live wherever the user is.
    void review.subscribe();
    if (this.project) {
      this.loadProjectLocalState();
      void this.loadBackgroundIndex();
      void this.loadIgnored();
      void this.loadUnread();
      await this.refreshTree();
    } else {
      // Launch straight into the last-used project when it's still around;
      // any failure just leaves the picker showing.
      const lastId = await api.lastProjectId().catch(() => null);
      const entry = this.registry.find((e) => e.id === lastId && e.available);
      if (entry) await this.openProject(entry.path).catch(() => {});
    }
  }

  async refreshRegistry() {
    this.registry = await api.listProjects();
  }

  async refreshTree() {
    if (!this.project) return;
    const tree = await api.getTree();
    this.files = tree.files;
    this.folders = tree.folders;
    this.pruneFavorites();
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
    this.loadProjectLocalState();
    void this.loadBackgroundIndex();
    void this.loadIgnored();
    void this.loadUnread();
    this.screen = "home";
    await this.refreshRegistry();
    await this.refreshTree();
    // Repopulate the badge for the newly-active project immediately, ahead of
    // the first scan/sync event that would otherwise refresh it.
    void review.refresh();
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
    this.applyTabState(reduceOpenTab({ tabs: this.fileTabs, active: this.activeTab }, path, persistent));
    this.recents = recordRecent(this.recents, path);
    if (this.project) saveRecents(this.project.id, this.recents);
  }

  activateTab(path: string) {
    this.activeTab = path;
    this.persistTabs();
  }

  closeTab(path: string) {
    this.applyTabState(reduceCloseTab({ tabs: this.fileTabs, active: this.activeTab }, path));
  }

  closeOtherTabs(path: string) {
    this.applyTabState(reduceCloseOthers({ tabs: this.fileTabs, active: this.activeTab }, path));
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
}

export const app = new AppStore();
