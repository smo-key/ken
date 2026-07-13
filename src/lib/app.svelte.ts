// Global app state (Svelte 5 runes). One store: project, tree, navigation.
import {
  api,
  type FileRow,
  type FolderInfo,
  type ProjectInfo,
  type RegistryEntryStatus,
  type ScanStats,
} from "./api";

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
  openFile = $state<string | null>(null);

  scanning = $state(false);
  lastScan = $state<ScanStats | null>(null);
  lastScanAt = $state<number | null>(null);
  scanError = $state<string | null>(null);

  searchOpen = $state(false);

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
    if (this.project) await this.refreshTree();
  }

  async refreshRegistry() {
    this.registry = await api.listProjects();
  }

  async refreshTree() {
    if (!this.project) return;
    const tree = await api.getTree();
    this.files = tree.files;
    this.folders = tree.folders;
  }

  private async activated(info: ProjectInfo) {
    this.project = info;
    this.scanning = true;
    this.scanError = null;
    this.openFile = null;
    this.screen = "home";
    await this.refreshRegistry();
    await this.refreshTree();
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
    this.openFile = relPath;
    this.screen = "files";
    this.searchOpen = false;
  }
}

export const app = new AppStore();
