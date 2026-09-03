// ken-families task 4.1/4.2/4.3 frontend store: the `kenFamilies` flag,
// the connection list + live sync state (via `family_list` +
// `family-sync`), and each connection's own inbox (unread badge, tray
// grouping, accept/dismiss). Mirrors `memory.svelte.ts`/`tasks.svelte.ts`'s
// shape: flag-gated `init()`, an event subscription that updates `$state`,
// thin wrappers over `api` calls.
//
// LOCKED (design.md D4, spec "Typed inbox items"): nothing arriving from a
// family repo enters a board, daily board, or agent queue until the user
// explicitly accepts it. Every mutation this store exposes for inbox items
// is a deliberate one-item action wired to a click — there is no bulk
// accept, no auto-accept, and no path that calls `familyAcceptTask` other
// than a direct user action.
import {
  api,
  type FamilyConnectionDto,
  type FamilyInboxItem,
  type FamilyInboxStatus,
  type FamilySyncEvent,
} from "./api";

class FamiliesStore {
  /** Whether the `kenFamilies` flag resolves on — gates the Settings
   *  "Families" section and the nav-rail tray entirely (proposal: "off
   *  means... no Families settings page, no new tools"). */
  enabled = $state(false);

  connections = $state<FamilyConnectionDto[]>([]);
  loading = $state(false);
  loadError = $state<string | null>(null);

  /** This device's own inbox per family id, refreshed on init, after every
   *  `family-sync` event for that family, and after any mutation that
   *  could change it. Not paginated — inbox folders are small by design
   *  (D4's lifecycle archives items in place). */
  inbox = $state<Record<string, FamilyInboxItem[]>>({});

  /** Busy state for per-connection "Sync now" buttons. */
  syncingIds = $state<Set<string>>(new Set());
  /** Busy state for per-item accept/dismiss/push-back actions, keyed
   *  `${familyId}:${itemId}`. */
  resolvingItem = $state<string | null>(null);

  createBusy = $state(false);
  createError = $state<string | null>(null);
  joinBusy = $state(false);
  joinError = $state<string | null>(null);

  private initDone = false;
  private unlistenSync: (() => void) | null = null;

  /** Call once (Settings screen mount, or nav-rail tray's first render).
   *  Cheap even if the user never opens either surface this session —
   *  nothing here starts a sync loop on its own (that's the backend
   *  poller, gated by each connection's own `liveSync`). */
  async init() {
    if (this.initDone) return;
    this.initDone = true;
    this.enabled = await this.checkEnabled();
    if (!this.enabled) return;
    this.unlistenSync = await api.onFamilySync((ev) => this.onSyncEvent(ev));
    await this.refresh();
  }

  private async checkEnabled(): Promise<boolean> {
    const features = await api.listFeatures().catch(() => []);
    return features.find((f) => f.name === "kenFamilies")?.effective ?? false;
  }

  async refresh() {
    if (!this.enabled) return;
    this.loading = true;
    this.loadError = null;
    try {
      this.connections = await api.familyList();
      await Promise.all(this.connections.map((c) => this.refreshInbox(c.connection.familyId)));
    } catch (e) {
      this.loadError = String(e);
    } finally {
      this.loading = false;
    }
  }

  async refreshInbox(familyId: string) {
    try {
      this.inbox = { ...this.inbox, [familyId]: await api.familyInboxList(familyId) };
    } catch {
      // A connection that just errored/went unavailable can't list its
      // inbox either — leave whatever was last known rather than clearing
      // it out from under the tray.
    }
  }

  private onSyncEvent(ev: FamilySyncEvent) {
    this.connections = this.connections.map((dto) =>
      dto.connection.familyId === ev.familyId ? { ...dto, state: ev.report.state } : dto,
    );
    void this.refreshInbox(ev.familyId);
  }

  // ── Task 4.3: matching a board task back to its family connection ──────

  /** Every attached connection for the currently open workspace — the
   *  universe of families the Tasks tab's per-family filter chip can
   *  possibly show (task_homes_scan only ever merges boards for
   *  connections attached to the OPEN workspace, so anything else here
   *  would be a filter option with no matching tasks). */
  attachedTo(workspaceId: string | null | undefined): FamilyConnectionDto[] {
    if (!workspaceId) return [];
    return this.connections.filter((c) => c.connection.attachedWorkspaceId === workspaceId);
  }

  /** Which family connection produced a given board task, or `null` for an
   *  ordinary workspace/per-repo task.
   *
   *  There is no dedicated field for this on `Task` — `home` is
   *  `HomeKind::Project` for a family board task exactly like it is for a
   *  per-repo one (`tasks.rs` has no `Family` variant reachable without
   *  editing a file outside this build's touch-boundary; see
   *  `task_homes_scan`'s own comment in `src-tauri/src/lib.rs`). The one
   *  field that actually differs is `homeDir`, the real directory the task
   *  file was scanned from: `family::board_dir` builds it as
   *  `<app data>/ken/families/<family-id>/members/<member-id>/board`,
   *  never a per-repo `.ken/tasks` or the workspace's own task folder. This
   *  matches on that real path shape rather than `task.project` (which is
   *  just the family's display name, cosmetic and not guaranteed unique)
   *  — the smallest honest marker actually available client-side. */
  familyForTask(task: { homeDir: string }): FamilyConnectionDto | null {
    const dir = task.homeDir.replace(/\\/g, "/");
    return (
      this.connections.find((dto) =>
        dir.includes(`families/${dto.connection.familyId}/members/${dto.connection.memberId}/board`),
      ) ?? null
    );
  }

  // ── Derived: unread badge + tray grouping ───────────────────────────────

  unreadFor(familyId: string): number {
    return (this.inbox[familyId] ?? []).filter((it) => it.status === "unread").length;
  }

  /** Strictly `unread` (not yet even seen) — the badge count. `seen` items
   *  still show in the tray until dismissed/accepted but don't inflate the
   *  badge, mirroring an ordinary mail client. */
  get totalUnread(): number {
    return this.connections.reduce(
      (sum, dto) =>
        sum + (this.inbox[dto.connection.familyId] ?? []).filter((it) => it.status === "unread").length,
      0,
    );
  }

  /** Tray groups: one per family with at least one open (unread/seen)
   *  item, each family's items newest-first. Accepted/archived items never
   *  show here — the tray is "what needs a decision", not a full log. */
  get trayGroups(): { connection: FamilyConnectionDto; items: FamilyInboxItem[] }[] {
    return this.connections
      .map((dto) => ({
        connection: dto,
        items: (this.inbox[dto.connection.familyId] ?? [])
          .filter((it) => it.status === "unread" || it.status === "seen" || it.malformed)
          .slice()
          .reverse(),
      }))
      .filter((g) => g.items.length > 0);
  }

  // ── Mutations: connections ──────────────────────────────────────────────

  async create(name: string, memberName: string, remoteUrl: string) {
    this.createBusy = true;
    this.createError = null;
    try {
      const dto = await api.familyCreate(name, memberName, remoteUrl);
      this.connections = [...this.connections, dto];
      await this.refreshInbox(dto.connection.familyId);
      return dto;
    } catch (e) {
      this.createError = String(e);
      throw e;
    } finally {
      this.createBusy = false;
    }
  }

  async join(remoteUrl: string, existingMemberId?: string, newMemberName?: string) {
    this.joinBusy = true;
    this.joinError = null;
    try {
      const dto = await api.familyJoin(remoteUrl, existingMemberId, newMemberName);
      this.connections = [...this.connections, dto];
      await this.refreshInbox(dto.connection.familyId);
      return dto;
    } catch (e) {
      this.joinError = String(e);
      throw e;
    } finally {
      this.joinBusy = false;
    }
  }

  async remove(familyId: string) {
    await api.familyRemove(familyId);
    this.connections = this.connections.filter((c) => c.connection.familyId !== familyId);
    const { [familyId]: _removed, ...rest } = this.inbox;
    this.inbox = rest;
  }

  async setLiveSync(familyId: string, liveSync: boolean) {
    await api.familySetLiveSync(familyId, liveSync);
    this.patchConnection(familyId, (c) => ({ ...c, liveSync }));
  }

  async setPollInterval(familyId: string, secs: number) {
    await api.familySetPollInterval(familyId, secs);
    this.patchConnection(familyId, (c) => ({ ...c, pollIntervalSecs: secs }));
  }

  async syncNow(familyId: string) {
    const next = new Set(this.syncingIds);
    next.add(familyId);
    this.syncingIds = next;
    try {
      const report = await api.familySyncNow(familyId);
      this.connections = this.connections.map((dto) =>
        dto.connection.familyId === familyId ? { ...dto, state: report.state } : dto,
      );
      await this.refreshInbox(familyId);
      return report;
    } finally {
      const after = new Set(this.syncingIds);
      after.delete(familyId);
      this.syncingIds = after;
    }
  }

  /** Clear a `Conflict` after the user has resolved the clone by hand.
   *  Doesn't emit `family-sync` itself, so re-fetch the connection list to
   *  pick up the cleared state. */
  async resolveConflict(familyId: string) {
    await api.familyResolveConflict(familyId);
    await this.refresh();
  }

  async attachWorkspace(familyId: string, workspaceId: string) {
    await api.familyAttachWorkspace(familyId, workspaceId);
    this.patchConnection(familyId, (c) => ({ ...c, attachedWorkspaceId: workspaceId }));
  }

  async detachWorkspace(familyId: string) {
    await api.familyDetachWorkspace(familyId);
    this.patchConnection(familyId, (c) => ({ ...c, attachedWorkspaceId: null }));
  }

  private patchConnection(familyId: string, patch: (c: FamilyConnectionDto["connection"]) => FamilyConnectionDto["connection"]) {
    this.connections = this.connections.map((dto) =>
      dto.connection.familyId === familyId ? { ...dto, connection: patch(dto.connection) } : dto,
    );
  }

  // ── Mutations: inbox items (tray + settings) ────────────────────────────

  private itemKey(familyId: string, itemId: string) {
    return `${familyId}:${itemId}`;
  }

  isResolving(familyId: string, itemId: string): boolean {
    return this.resolvingItem === this.itemKey(familyId, itemId);
  }

  /** `seen`/`archived` only — `accepted` always goes through `acceptTask`
   *  below, which is the sole path onto the board (D4's acceptance gate). */
  async setItemStatus(familyId: string, itemId: string, status: Exclude<FamilyInboxStatus, "accepted">) {
    this.resolvingItem = this.itemKey(familyId, itemId);
    try {
      await api.familySetItemStatus(familyId, itemId, status);
      await this.refreshInbox(familyId);
    } finally {
      this.resolvingItem = null;
    }
  }

  /** Explicit, one-item accept — the only way a family task can ever reach
   *  a board. Never called automatically. */
  async acceptTask(familyId: string, itemId: string) {
    this.resolvingItem = this.itemKey(familyId, itemId);
    try {
      const task = await api.familyAcceptTask(familyId, itemId);
      await this.refreshInbox(familyId);
      return task;
    } finally {
      this.resolvingItem = null;
    }
  }

  async pushBack(familyId: string, itemId: string, note: string) {
    this.resolvingItem = this.itemKey(familyId, itemId);
    try {
      await api.familyPushBack(familyId, itemId, note);
      await this.refreshInbox(familyId);
    } finally {
      this.resolvingItem = null;
    }
  }

  /** Settings-independent teardown for tests; production code never calls
   *  this (the store lives for the app's lifetime). */
  dispose() {
    this.unlistenSync?.();
    this.unlistenSync = null;
  }
}

export const families = new FamiliesStore();
