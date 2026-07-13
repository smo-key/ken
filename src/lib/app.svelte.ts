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
  renameFavorite,
  saveFavorites,
  type Favorite,
} from "./favorites";
import {
  closeOthers as reduceCloseOthers,
  closeTab as reduceCloseTab,
  makePersistent as reduceMakePersistent,
  openTab as reduceOpenTab,
  renameTab as reduceRenameTab,
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

  get failedFiles(): FileRow[] {
    return this.files.filter((f) => f.status === "failed");
  }

  async init() {
    this.registry = await api.listProjects();
    this.project = await api.currentProject();
    await api.onIndexUpdated((stats) => {
      this.scanning = false;
      this.lastScan = stats;
      this.lastScanAt = Date.now();
      void this.refreshTree();
    });
    await api.onScanError((message) => {
      this.scanning = false;
      this.scanError = message;
    });
    await api.onSyncState((ev) => {
      this.syncState = ev.state;
      this.syncDetail = ev.detail;
    });
    if (this.project) {
      this.loadProjectLocalState();
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
    this.screen = "home";
    await this.refreshRegistry();
    await this.refreshTree();
  }

  /** Restore tabs + favorites for the current project from localStorage. */
  private loadProjectLocalState() {
    this.fileTabs = [];
    this.activeTab = null;
    this.favorites = [];
    if (!this.project) return;
    this.favorites = loadFavorites(this.project.id);
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

  /** Move a file: update its open tab, favorites, and the active selection. */
  async moveFile(fromRel: string, toRel: string) {
    await api.moveFile(fromRel, toRel);
    this.applyTabState(
      reduceRenameTab({ tabs: this.fileTabs, active: this.activeTab }, fromRel, toRel),
    );
    if (this.isFavorite(fromRel)) {
      this.favorites = renameFavorite(this.favorites, fromRel, toRel);
      if (this.project) saveFavorites(this.project.id, this.favorites);
    }
    await this.refreshTree();
  }
}

export const app = new AppStore();
