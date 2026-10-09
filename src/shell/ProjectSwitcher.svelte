<script lang="ts">
  import { api, memberGroup, type RegistryEntryStatus } from "../lib/api";
  import { app } from "../lib/app.svelte";
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import Pencil from "@lucide/svelte/icons/pencil";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import { openContextMenu } from "../lib/ui/ContextMenu.svelte";
  import { openConfirm } from "../lib/ui/ConfirmMenu.svelte";

  let { close }: { close: () => void } = $props();
  let error = $state<string | null>(null);

  // Inline rename: which row is being edited, its draft text, and a guard so
  // an Enter-then-blur pair doesn't submit twice.
  let renamingId = $state<string | null>(null);
  let renameValue = $state("");
  let renameBusy = $state(false);

  // Grouping is the DEFAULT, not a feature you switch on. The workspace
  // manifest already stores members as `Hytale/Shattered-Realms`, so the
  // folder structure is known — a flat list here was throwing it away at the
  // render, not reflecting some simpler truth. Someone who genuinely wants
  // the four Shattered-Realms folders listed apart can say so, and that
  // choice is what gets persisted; the grouped view needs no configuration.
  const GROUP_PREF = "ken.projectSwitcher.grouped";
  let grouped = $state(readGroupPref());

  function readGroupPref(): boolean {
    try {
      return localStorage.getItem(GROUP_PREF) !== "false";
    } catch {
      // Private mode, blocked site data, whatever — default to grouped
      // rather than letting a storage failure change what the user sees.
      return true;
    }
  }

  function setGrouped(on: boolean) {
    grouped = on;
    try {
      localStorage.setItem(GROUP_PREF, String(on));
    } catch {
      // Preference is lost on restart; the view is still correct now.
    }
  }

  /** Manifest name for a registry entry, or null if it isn't a member of the
   *  open workspace. `WorkspaceMember.projectId` is the registry id. */
  const memberNameById = $derived(
    new Map(
      (app.workspace?.members ?? [])
        .filter((m) => m.projectId)
        .map((m) => [m.projectId as string, m.name]),
    ),
  );

  type Section = { key: string; label: string | null; rows: RegistryEntryStatus[] };

  // The repos of this workspace, and only those: Ken works in workspaces,
  // so a folder outside it is opened as its own workspace, not here. Folder
  // groups first (alphabetical), then the members at the root.
  const sections = $derived.by((): Section[] => {
    const members = app.registry.filter((e) => memberNameById.has(e.id));
    if (!app.workspace) {
      return [{ key: "all", label: null, rows: app.registry }];
    }
    if (!grouped) {
      return [{ key: "all", label: null, rows: members }];
    }

    const byGroup = new Map<string, RegistryEntryStatus[]>();
    const rootRows: RegistryEntryStatus[] = [];

    for (const entry of members) {
      const name = memberNameById.get(entry.id) as string;
      const group = memberGroup(name);
      if (!group) {
        rootRows.push(entry);
        continue;
      }
      const bucket = byGroup.get(group);
      if (bucket) bucket.push(entry);
      else byGroup.set(group, [entry]);
    }

    const out: Section[] = [...byGroup.entries()]
      .sort((a, b) => a[0].localeCompare(b[0]))
      .map(([label, rows]) => ({ key: `g:${label}`, label, rows }));

    if (rootRows.length > 0) {
      out.push({ key: "root", label: "In this workspace", rows: rootRows });
    }
    return out;
  });

  /** "All projects" means the merged Files tree, which only Files can draw —
   *  so choosing it from another screen goes there rather than doing nothing
   *  visible. Hiding the option everywhere else was the wrong fix for that. */
  function chooseAllProjects() {
    void app.setTreeShowsAllProjects(true);
    app.screen = "files";
    close();
  }

  async function forgetId(id: string) {
    await api.forgetProject(id);
    await app.refreshRegistry();
  }

  function startRename(entry: RegistryEntryStatus) {
    renamingId = entry.id;
    renameValue = entry.name;
    error = null;
  }

  async function submitRename(entry: RegistryEntryStatus) {
    if (renameBusy || renamingId !== entry.id) return;
    const name = renameValue.trim();
    if (!name || name === entry.name) {
      renamingId = null;
      return;
    }
    renameBusy = true;
    try {
      const updated = await api.renameProject(entry.id, name);
      await app.refreshRegistry();
      // Keep the live title bar/switcher in step when the open project is
      // the one renamed; app.project is reactive so the UI follows.
      if (app.project?.id === entry.id) app.project = updated;
      renamingId = null;
    } catch (e) {
      error = String(e);
    } finally {
      renameBusy = false;
    }
  }

  function focusInput(node: HTMLInputElement) {
    node.focus();
    node.select();
  }

  function rowMenu(e: MouseEvent, entry: RegistryEntryStatus) {
    e.preventDefault();
    const x = e.clientX;
    const y = e.clientY;
    openContextMenu(x, y, [
      {
        label: "Open",
        icon: FolderOpen,
        disabled: !entry.available,
        onSelect: () => pick(entry),
      },
      {
        label: "Rename…",
        icon: Pencil,
        onSelect: () => startRename(entry),
      },
      "separator",
      {
        label: "Remove from Ken…",
        icon: Trash2,
        danger: true,
        onSelect: () =>
          openConfirm(x, y, {
            title: `Remove “${entry.name}”?`,
            body: "Removes it from Ken's list — the folder and its files stay on disk.",
            confirmLabel: "Remove from Ken",
            onConfirm: () => void forgetId(entry.id),
          }),
      },
    ]);
  }

  async function pick(entry: RegistryEntryStatus) {
    if (!entry.available) return;
    try {
      // A member of the open workspace is focused, not reopened alone:
      // opening it as a project would close the workspace around it.
      if (app.workspace && memberNameById.has(entry.id)) await app.focusMember(entry.id);
      else await app.openProject(entry.path);
      close();
    } catch (e) {
      error = String(e);
    }
  }

  async function forget(id: string, e: MouseEvent) {
    e.stopPropagation();
    await api.forgetProject(id);
    await app.refreshRegistry();
  }
</script>

<button class="scrim" onclick={close} aria-label="Close project switcher"></button>
<div class="menu">
  <!-- Files can show every member at once (`get_tree_all` returns the
       whole workspace with each path prefixed by its member folder), so
       the switcher offers it as a peer of the individual projects rather
       than hiding it in a second control. Only on Files: the other
       screens each draw one project's data. -->
  {#if app.workspace}
    <button
      class="row"
      class:current={app.treeShowsAllProjects && app.screen === "files"}
      onclick={chooseAllProjects}
    >
      <span class="badge">*</span>
      <span class="info">
        <span class="name">All projects</span>
        <span class="path">Every project in this workspace, in one Files tree</span>
      </span>
    </button>
  {/if}
  {#each sections as section (section.key)}
    {#if section.label}
      <div class="group-head">{section.label}</div>
    {/if}
    {#each section.rows as entry (entry.id)}
    <button
      class="row"
      class:current={entry.id === app.project?.id}
      class:unavailable={!entry.available}
      onclick={() => {
        // While this row's rename input is showing, the input lives INSIDE
        // this <button>. Pressing Space in the input triggers the button's
        // native activation, which synthesizes a click targeting the button
        // (not the input) — so stopPropagation on the input can't catch it.
        // Neutralize the row action during rename so the popover stays open
        // and the space is typed into the input.
        if (renamingId === entry.id) return;
        // Choosing one project leaves the merged tree behind.
        if (app.treeShowsAllProjects) void app.setTreeShowsAllProjects(false);
        pick(entry);
      }}
      oncontextmenu={(e) => rowMenu(e, entry)}
    >
      <span class="badge">{entry.name.charAt(0).toUpperCase()}</span>
      <span class="info">
        {#if renamingId === entry.id}
          <!-- Nested in the row <button>, so swallow clicks/keys that would
               otherwise trigger the row's open action. -->
          <input
            class="name rename"
            bind:value={renameValue}
            onclick={(e) => e.stopPropagation()}
            onkeydown={(e) => {
              // Stop every keydown from bubbling to the row <button>, whose
              // native Space/Enter activation would otherwise open (and close)
              // the switcher while typing the new name.
              e.stopPropagation();
              if (e.key === "Enter") {
                e.preventDefault();
                void submitRename(entry);
              } else if (e.key === "Escape") {
                renamingId = null;
              }
            }}
            onblur={() => void submitRename(entry)}
            use:focusInput
          />
        {:else}
          <span class="name">{entry.name}</span>
        {/if}
        <span class="path mono">{entry.path}</span>
        {#if !entry.available}
          <span class="missing">Folder not found — was it moved or deleted?</span>
        {/if}
      </span>
      {#if !entry.available}
        <span class="forget" role="button" tabindex="0" onclick={(e) => forget(entry.id, e)} onkeydown={() => {}}>Remove</span>
      {/if}
    </button>
    {/each}
  {/each}
  {#if app.workspace}
    <!-- The opt-out, at the bottom because it is the exception. Only shown
         when there is actually a folder group to flatten. -->
    {#if app.workspace.members.some((m) => memberGroup(m.name))}
      <label class="group-toggle">
        <input
          type="checkbox"
          checked={grouped}
          onchange={(e) => setGrouped(e.currentTarget.checked)}
        />
        Group by folder
      </label>
    {/if}
  {/if}
  {#if error}
    <div class="error">{error}</div>
  {/if}
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: transparent;
    border: none;
    z-index: 39;
  }
  .menu {
    position: absolute;
    top: 46px;
    left: 86px;
    width: 340px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-overlay);
    box-shadow: var(--shadow-overlay);
    padding: 6px;
    z-index: 40;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border-radius: 9px;
    border: none;
    background: transparent;
    text-align: left;
    font-size: 13px;
    color: var(--ink);
  }
  .row:hover {
    background: var(--sunken);
  }
  /* Quiet enough to scan past when you already know where you are going,
     present enough to give the list its shape. */
  .group-head {
    padding: 8px 10px 3px;
    font-size: 10.5px;
    font-weight: 700;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--ink-tertiary);
  }
  .group-head:first-child {
    padding-top: 4px;
  }
  .group-toggle {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 7px 10px 4px;
    margin-top: 2px;
    border-top: 1px solid var(--border);
    font-size: 11.5px;
    color: var(--ink-tertiary);
    cursor: pointer;
  }
  .group-toggle input {
    margin: 0;
    cursor: pointer;
  }
  .row.current {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .row.unavailable {
    opacity: 0.7;
    cursor: default;
  }
  .badge {
    width: 26px;
    height: 26px;
    border-radius: 7px;
    background: var(--ink);
    color: var(--paper);
    display: flex;
    align-items: center;
    justify-content: center;
    font-family: var(--font-serif);
    font-size: 13px;
    flex: none;
  }
  .info {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }
  .name {
    font-weight: 600;
  }
  input.rename {
    font: inherit;
    font-weight: 600;
    color: var(--ink);
    background: var(--paper);
    border: 1px solid var(--accent);
    border-radius: 5px;
    padding: 1px 4px;
    margin: -2px 0;
    width: 100%;
    outline: none;
  }
  .path {
    font-size: 11px;
    color: var(--ink-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .missing {
    font-size: 11.5px;
    color: var(--danger);
  }
  .forget {
    font-size: 12px;
    font-weight: 600;
    color: var(--danger);
    flex: none;
  }
  .error {
    padding: 8px 10px;
    font-size: 12px;
    color: var(--danger);
  }
</style>
