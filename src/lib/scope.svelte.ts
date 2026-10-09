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
  groups = $state<ProjectGroup[]>([]);
  /** The team chosen in the title bar: questions, search and Home are about
   *  it. Teams are the workspace's groups, each chosen on its own; there is
   *  no "all teams". Null only in a workspace with no team. */
  team = $state<string | null>(null);
  /** The chosen team's wiki (the repo with the inbox): what Files opens on. */
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

  /** The team's repos, as project ids (every member when there is no team). */
  get teamProjectIds(): string[] {
    const group = this.groups.find((g) => g.name === this.team);
    if (group) return group.projectIds;
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
    // The team chosen last in this workspace, else its first. A team that
    // was deleted (or emptied) must not stay chosen: it would scope every
    // question to nothing.
    const remembered = readTeam(app.workspace?.id);
    const team = [remembered, this.team].find((t) => t && this.groups.some((g) => g.name === t)) ?? this.groups[0]?.name ?? null;
    this.team = team;
    void this.refreshWiki();
    if (this.kind === "all" || (this.kind === "group" && !this.groups.some((g) => g.name === this.value))) {
      this.set(team ? "group" : "all", team);
    }
  }

  /** Choose the team in the title bar: questions go to it, and a repo of it
   *  is focused (its wiki first) when the one focused is not in it. */
  async selectTeam(name: string) {
    this.team = name;
    writeTeam(app.workspace?.id, name);
    this.set("group", name);
    await this.refreshWiki();
    const ids = this.teamProjectIds;
    if (app.focused && ids.includes(app.focused)) return;
    const next = this.wikiId ?? ids[0];
    if (next) await app.focusMember(next);
  }

  async refreshWiki() {
    const team = this.team;
    const id = this.enabled ? await api.teamWiki(team).catch(() => null) : null;
    if (team === this.team) this.wikiId = id;
  }

  /** The broad scope: the chosen team, or the workspace when it has none. */
  setTeamScope() {
    this.set(this.team ? "group" : "all", this.team);
  }

  set(kind: ScopeKind, value: string | null) {
    this.kind = kind;
    this.value = value;
  }

  async saveGroup(name: string, members: string[]) {
    this.groups = await api.workspaceSetGroup(name, members);
  }

  async deleteGroup(name: string) {
    this.groups = await api.workspaceRemoveGroup(name);
    if (this.kind === "group" && this.value === name) this.set("all", null);
    await this.refreshGroups();
  }
}

const TEAM_KEY = "ken.team.";

function readTeam(workspaceId: string | undefined): string | null {
  if (!workspaceId) return null;
  try {
    return localStorage.getItem(TEAM_KEY + workspaceId);
  } catch {
    return null;
  }
}

function writeTeam(workspaceId: string | undefined, team: string) {
  if (!workspaceId) return;
  try {
    localStorage.setItem(TEAM_KEY + workspaceId, team);
  } catch {
    // Forgotten on restart; the choice still holds now.
  }
}

export const scope = new ScopeStore();
