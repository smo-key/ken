// Workspace-scoped Home state (ken-home-workspace 3.1): the composed
// digest across every member, and the members strip.
//
// Deliberately separate from `digest.svelte.ts`, which stays the FOCUSED
// member's own digest card. This store never triggers generation — the
// backend command composes stored rows only, so refreshing it can never
// cost an AI call or write a `digests` row.
import { api, type MemberOverview, type WorkspaceDigest } from "./api";
import { app } from "./app.svelte";

/** Above this many members the strip collapses to a summary line. */
const COLLAPSE_ABOVE = 4;

class WorkspaceHomeStore {
  digest = $state<WorkspaceDigest | null>(null);
  members = $state<MemberOverview[]>([]);
  loading = $state(false);
  /** Set when the backend refused or failed; the blocks hide rather than
   *  render a broken state. */
  error = $state<string | null>(null);

  /** Only meaningful with a workspace open — every command behind this
   *  store requires the `workspace` flag and an open workspace. */
  get enabled(): boolean {
    return !!app.workspace;
  }

  /** Members that resolved and can be searched/focused. */
  get healthy(): MemberOverview[] {
    return this.members.filter((m) => m.status === "ok");
  }

  /** Members needing a look: unresolvable, still indexing, or carrying
   *  failed files. This is the count the collapsed strip reports. */
  get needsAttention(): MemberOverview[] {
    return this.members.filter(
      (m) => m.status !== "ok" || !m.indexReady || m.failedFiles > 0,
    );
  }

  get shouldCollapse(): boolean {
    return this.members.length > COLLAPSE_ABOVE;
  }

  get totalUnread(): number {
    return this.members.reduce((sum, m) => sum + m.unread, 0);
  }

  async init() {
    // A rescan in any member can change unread/failed counts and may have
    // stored a digest row since we last looked.
    await api.onIndexUpdated(() => void this.refresh());
    await api.onDigestUpdated(() => void this.refresh());
    await this.refresh();
  }

  async refresh() {
    if (!this.enabled) {
      this.digest = null;
      this.members = [];
      return;
    }
    this.loading = true;
    // Settled independently, NOT `Promise.all`: the digest command scans
    // the board and can fail on its own, and there is no reason a board
    // problem should blank the members strip too. Each block renders from
    // whichever half answered.
    const [digest, members] = await Promise.all([
      api.workspaceDigest().catch((e) => {
        this.error = String(e);
        return null;
      }),
      api.workspaceMembersOverview().catch((e) => {
        this.error = String(e);
        return [] as MemberOverview[];
      }),
    ]);
    this.digest = digest;
    this.members = members;
    this.loading = false;
  }
}

export const workspaceHome = new WorkspaceHomeStore();
