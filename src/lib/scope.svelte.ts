// The workspace-wide question scope (ken-home-workspace): "which projects
// am I asking about right now".
//
// One store, read by Home's dropdown, the ⌘K overlay, and chat sends, so
// the answer is the same everywhere instead of each surface inventing its
// own. Deliberately NOT the same thing as `app.project` (the focused
// member, i.e. "which project am I working in") — you can be editing one
// project while asking a question about all of them.
import { api, memberLeaf, type ProjectGroup } from "./api";
import { app } from "./app.svelte";

export type ScopeKind = "all" | "group" | "project";

class ScopeStore {
  /** Defaults to all projects: a workspace-wide question is the common
   *  case, and defaulting to one project is what made Ken feel
   *  single-project. */
  kind = $state<ScopeKind>("all");
  /** Group name when `kind === "group"`, project id when `"project"`. */
  value = $state<string | null>(null);
  /** The workspace's groups: a way to sort repos, not teams (decision 35). */
  groups = $state<ProjectGroup[]>([]);
  /** The workspace's knowledge base (the repo with the inbox): what Files
   *  opens on under Wiki. */
  wikiId = $state<string | null>(null);

  get enabled(): boolean {
    return !!app.workspace;
  }

  /** What `send_chat_message` and `route_search` want: `null` for a
   *  single project (they take the project id separately), `"all"`, or a
   *  group name. */
  get chatScope(): string | null {
    if (!this.enabled) return null;
    if (this.kind === "all") return "all";
    if (this.kind === "group") return this.value;
    return null;
  }

  /** The project id to pin a search to, or null. */
  get projectId(): string | null {
    return this.kind === "project" ? this.value : null;
  }

  /** The group name to scope a search to, or null. */
  get groupName(): string | null {
    return this.kind === "group" ? this.value : null;
  }

  /** Every repo of the workspace, as project ids. */
  get workspaceProjectIds(): string[] {
    return (app.workspace?.members ?? []).flatMap((m) => (m.projectId ? [m.projectId] : []));
  }

  get label(): string {
    if (this.kind === "all") return app.workspace?.name ?? "This workspace";
    if (this.kind === "group") return this.value ?? "Group";
    const member = app.workspace?.members.find((m) => m.projectId === this.value);
    return member ? memberLeaf(member.name) : "One project";
  }

  async init() {
    await this.refreshGroups();
  }

  async refreshGroups() {
    if (!this.enabled) {
      this.groups = [];
      return;
    }
    this.groups = await api.workspaceGroups().catch(() => []);
    void this.refreshWiki();
    // A group that was deleted (or emptied) must not stay chosen: it would
    // scope every question to nothing.
    if (this.kind === "group" && !this.groups.some((g) => g.name === this.value)) this.set("all", null);
  }

  async refreshWiki() {
    const ws = app.workspace?.id;
    const id = this.enabled ? await api.knowledgeBaseId().catch(() => null) : null;
    if (ws === app.workspace?.id) this.wikiId = id;
  }

  /** The broad scope: the whole workspace. */
  setWorkspaceScope() {
    this.set("all", null);
  }

  set(kind: ScopeKind, value: string | null) {
    this.kind = kind;
    this.value = value;
  }
}

export const scope = new ScopeStore();
