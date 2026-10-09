// The banner at the top of Files: sync conflicts and conflicted copies in
// the team's repos, and the conflict view it opens. Read on start, on a
// change of team, and when a sync, a scan or a filed conflict says so.
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  api,
  type ConflictCopyResolution,
  type ConflictItem,
  type ConflictResolution,
  type FilesBanner,
} from "./api";
import { app } from "./app.svelte";
import { scope } from "./scope.svelte";
import { numericId } from "./conflicts";
import { coalesce, singleFlight } from "./refresh";

/** Bursts of sync and scan events are read once, after this quiet spell. */
const REFRESH_DEBOUNCE_MS = 400;

class ConflictsStore {
  banner = $state<FilesBanner | null>(null);
  /** The conflict view is showing in Files. */
  open = $state(false);
  selected = $state<string | null>(null);

  private subscribed = false;
  private unlisteners: UnlistenFn[] = [];

  get items(): ConflictItem[] {
    return this.banner?.conflicts ?? [];
  }

  get selectedItem(): ConflictItem | null {
    return this.items.find((c) => c.id === this.selected) ?? this.items[0] ?? null;
  }

  async subscribe() {
    if (this.subscribed) return;
    this.subscribed = true;
    this.unlisteners = await Promise.all([
      api.onReviewChanged(() => this.queue()),
      api.onSyncState(() => this.queue()),
      api.onIndexUpdated(() => this.queue()),
    ]);
    $effect.root(() => {
      $effect(() => {
        void scope.team;
        void app.workspace?.id;
        void this.refresh();
      });
    });
  }

  private readonly queue = coalesce(() => void this.refresh(), REFRESH_DEBOUNCE_MS);

  /** One read at a time (see `singleFlight`). */
  readonly refresh = singleFlight(async () => {
    if (!app.project) return;
    const team = scope.team;
    // An optional read: a failure leaves the last banner in place.
    const next = await api.filesBanner(team).catch(() => null);
    if (next === null || team !== scope.team) return;
    this.banner = next;
    if (this.items.length === 0) this.open = false;
    if (this.selected && !this.items.some((c) => c.id === this.selected)) this.selected = null;
  });

  /** Open the conflict view in Files, on one conflict or the first. */
  async show(id: string | null = null) {
    app.screen = "files";
    this.open = true;
    const first = id ?? this.items[0]?.id ?? null;
    if (first) await this.select(first);
  }

  /** Show one conflict. Its repo is focused first: the view reads both
   *  versions from it. */
  async select(id: string) {
    const item = this.items.find((c) => c.id === id);
    if (item) await this.focusFor(item);
    this.selected = id;
  }

  close() {
    this.open = false;
  }

  /** The conflict's own repo has to be the focused one for the resolve
   *  commands; focus it without leaving Files. */
  private async focusFor(item: ConflictItem) {
    if (item.projectId && item.projectId !== app.focused) {
      await app.focusMember(item.projectId, { stay: true });
    }
  }

  /** Resolve a merge conflict; returns the path written. */
  async resolveConflict(item: ConflictItem, resolution: ConflictResolution, content?: string): Promise<string> {
    await this.focusFor(item);
    const path = await api.resolveConflict(numericId(item.id), resolution, content);
    await this.refresh();
    return path;
  }

  /** Keep one of a file and its conflicted copy; returns the one kept. */
  async resolveCopy(item: ConflictItem, resolution: ConflictCopyResolution): Promise<string> {
    await this.focusFor(item);
    const path = await api.resolveConflictCopy(numericId(item.id), resolution);
    await this.refresh();
    return path;
  }

  /** Open a file of a conflict in Files, in its own repo. */
  async openFile(item: ConflictItem, path: string) {
    await this.focusFor(item);
    this.open = false;
    app.openInFiles(path);
  }
}

export const conflicts = new ConflictsStore();
