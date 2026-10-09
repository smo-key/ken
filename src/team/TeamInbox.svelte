<script lang="ts">
  // The team inbox, on Team: the messages, tasks and notices teammates sent
  // (accept a task onto Your day, reply, dismiss, or send one), and the inbox
  // repos themselves (connect, create, join, sync, members). A team inbox is
  // a shared git repo the team's copies of Ken sync through.
  //
  // Accepting is the only way a task in the inbox reaches Your day, and it
  // only happens from the Accept click below.
  import { app } from "../lib/app.svelte";
  import { families } from "../lib/families.svelte";
  import { day } from "../lib/day.svelte";
  import { toast } from "../lib/toast.svelte";
  import {
    api,
    type FamilyConnectionState,
    type FamilyInboxItem,
    type FamilyInboxKind,
    type FamilyMember,
  } from "../lib/api";
  import Send from "@lucide/svelte/icons/send";
  import SquareCheck from "@lucide/svelte/icons/square-check";
  import MessageSquare from "@lucide/svelte/icons/message-square";
  import Bell from "@lucide/svelte/icons/bell";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import Reply from "@lucide/svelte/icons/reply";
  import Archive from "@lucide/svelte/icons/archive";
  import Check from "@lucide/svelte/icons/check";

  void families.init();

  // ── Members of each inbox, for its card and for Send ─────────────────
  let members = $state<Record<string, FamilyMember[]>>({});
  const asked = new Set<string>();
  $effect(() => {
    for (const c of families.connections) {
      const id = c.connection.familyId;
      if (asked.has(id)) continue;
      asked.add(id);
      void api
        .familyManifestGet(id)
        .then((m) => (members = { ...members, [id]: m.members }))
        .catch(() => {});
    }
  });

  function stateLabel(state: FamilyConnectionState): string {
    switch (state.state) {
      case "idle":
        return "Synced";
      case "syncing":
        return "Syncing…";
      case "conflict":
        return "Conflict";
      case "error":
        return "Sync failed";
      case "unavailable":
        return "Unavailable";
    }
  }

  async function run(what: string, p: Promise<unknown>) {
    try {
      await p;
    } catch (e) {
      toast.error(what, e);
    }
  }

  // ── Messages ─────────────────────────────────────────────────────────
  let replyFor = $state<string | null>(null);
  let replyNote = $state("");

  function preview(body: string, max = 220): string {
    const t = body.trim();
    return t.length > max ? `${t.slice(0, max)}…` : t;
  }

  async function accept(familyId: string, item: FamilyInboxItem) {
    await run("Could not accept the task", families.acceptTask(familyId, item.id).then(() => day.refreshState()));
  }

  async function dismiss(familyId: string, item: FamilyInboxItem) {
    await run("Could not dismiss it", families.setItemStatus(familyId, item.id, "archived"));
  }

  async function sendReply(familyId: string, item: FamilyInboxItem) {
    const note = replyNote.trim();
    if (!note) return;
    await run("Could not send the reply", families.pushBack(familyId, item.id, note));
    replyFor = null;
    replyNote = "";
  }

  // ── Send: delivery, not assignment; it waits in their inbox ──────────
  let composeOpen = $state(false);
  let composeFamily = $state("");
  let composeTo = $state("");
  let composeKind = $state<FamilyInboxKind>("message");
  let composeTitle = $state("");
  let composeBody = $state("");
  let composeBusy = $state(false);

  const composeMembers = $derived.by(() => {
    const own = families.connections.find((c) => c.connection.familyId === composeFamily)?.connection.memberId;
    return (members[composeFamily] ?? []).filter((m) => m.id !== own);
  });

  function openCompose() {
    composeOpen = true;
    if (!composeFamily) composeFamily = families.connections[0]?.connection.familyId ?? "";
  }

  $effect(() => {
    if (composeOpen && !composeMembers.some((m) => m.id === composeTo)) composeTo = composeMembers[0]?.id ?? "";
  });

  async function sendCompose() {
    if (!composeFamily || !composeTo || !composeTitle.trim() || composeBusy) return;
    composeBusy = true;
    try {
      await api.familySend(composeFamily, composeTo, composeKind, composeTitle.trim(), composeBody);
      const who = composeMembers.find((m) => m.id === composeTo)?.name || composeTo;
      toast.show(`Sent to ${who}'s inbox. It waits there until they act on it.`);
      composeTitle = "";
      composeBody = "";
      composeOpen = false;
    } catch (e) {
      toast.error("Could not send it", e);
    } finally {
      composeBusy = false;
    }
  }

  // ── Connect: create a new inbox repo, or join the team's ─────────────
  let connectOpen = $state(false);
  let createName = $state("");
  let createMember = $state("");
  let createRemote = $state("");
  let joinRemote = $state("");
  let joinMode = $state<"existing" | "new">("existing");
  let joinMemberId = $state("");
  let joinNewName = $state("");

  async function submitCreate() {
    const name = createName.trim();
    const member = createMember.trim();
    const remote = createRemote.trim();
    if (!name || !member || !remote) return;
    try {
      await families.create(name, member, remote);
      createName = createMember = createRemote = "";
      connectOpen = false;
    } catch {
      // families.createError shows the reason under the form.
    }
  }

  async function submitJoin() {
    const remote = joinRemote.trim();
    if (!remote) return;
    try {
      if (joinMode === "existing") {
        const id = joinMemberId.trim();
        if (!id) return;
        await families.join(remote, id, undefined);
        joinMemberId = "";
      } else {
        const name = joinNewName.trim();
        if (!name) return;
        await families.join(remote, undefined, name);
        joinNewName = "";
      }
      joinRemote = "";
      connectOpen = false;
    } catch {
      // families.joinError shows the reason under the form.
    }
  }

  function setPoll(familyId: string, secs: number) {
    if (!Number.isFinite(secs) || secs <= 0) return;
    void run("Could not change how often it syncs", families.setPollInterval(familyId, Math.round(secs)));
  }

  const showConnect = $derived(connectOpen || (!families.loading && families.connections.length === 0));
</script>

<div class="divider">
  team inbox{#if families.totalUnread > 0} · {families.totalUnread} unread{/if}
</div>

{#if families.loadError}
  <p class="note warn">Could not read the team inbox: {families.loadError}</p>
{/if}

<div class="cols">
  <div class="messages">
    {#if families.connections.length > 0}
      <div class="bar">
        <span class="overline">Messages</span>
        <span class="sp"></span>
        {#if !composeOpen}
          <button class="btn btn-small btn-ghost" onclick={openCompose}><Send size={12} strokeWidth={1.75} /> Send</button>
        {/if}
      </div>
    {/if}

    {#if composeOpen}
      <div class="compose">
        {#if families.connections.length > 1}
          <select bind:value={composeFamily} aria-label="Team inbox">
            {#each families.connections as c (c.connection.familyId)}
              <option value={c.connection.familyId}>{c.connection.name}</option>
            {/each}
          </select>
        {/if}
        <div class="compose-row">
          <select bind:value={composeTo} aria-label="To">
            {#each composeMembers as m (m.id)}<option value={m.id}>{m.name || m.id}</option>{/each}
          </select>
          <select bind:value={composeKind} aria-label="Kind">
            <option value="message">message</option>
            <option value="task">task</option>
            <option value="notification">notification</option>
          </select>
        </div>
        <input bind:value={composeTitle} placeholder="Title" aria-label="Title" />
        <textarea bind:value={composeBody} rows="3" placeholder="Details (optional)" aria-label="Details"></textarea>
        <div class="actions">
          <button class="btn btn-small btn-ghost" onclick={() => (composeOpen = false)}>Cancel</button>
          <button
            class="btn btn-small btn-primary"
            disabled={composeBusy || !composeTo || !composeTitle.trim()}
            onclick={() => void sendCompose()}>Send</button
          >
        </div>
      </div>
    {/if}

    {#each families.trayGroups as group (group.connection.connection.familyId)}
      {@const familyId = group.connection.connection.familyId}
      {#if families.connections.length > 1}
        <div class="group-name">{group.connection.connection.name}</div>
      {/if}
      {#each group.items as item (item.id)}
        {@const busy = families.isResolving(familyId, item.id)}
        <div class="item" class:malformed={item.malformed}>
          <div class="item-head">
            <span class="kind-icon" class:task={item.kind === "task"}>
              {#if item.malformed}
                <TriangleAlert size={12} strokeWidth={1.75} />
              {:else if item.kind === "task"}
                <SquareCheck size={12} strokeWidth={1.75} />
              {:else if item.kind === "notification"}
                <Bell size={12} strokeWidth={1.75} />
              {:else}
                <MessageSquare size={12} strokeWidth={1.75} />
              {/if}
            </span>
            <span class="item-title">{item.title || item.fileName}</span>
            {#if item.status === "unread"}<span class="unread-dot" title="Unread"></span>{/if}
            <span class="item-meta">from {item.from || "unknown"}{#if item.created} · {item.created}{/if}</span>
          </div>
          {#if item.malformed}
            <p class="item-body warn">Its header could not be read. It is shown as it came and never rewritten.</p>
          {/if}
          {#if item.body}<p class="item-body">{preview(item.body)}</p>{/if}
          {#if replyFor === item.id}
            <div class="reply">
              <textarea bind:value={replyNote} rows="2" placeholder="Reply to {item.from || 'the sender'}" aria-label="Reply"></textarea>
              <div class="actions">
                <button class="btn btn-small btn-ghost" onclick={() => (replyFor = null)}>Cancel</button>
                <button class="btn btn-small btn-primary" disabled={busy || !replyNote.trim()} onclick={() => void sendReply(familyId, item)}>Send</button>
              </div>
            </div>
          {:else}
            <div class="actions left">
              {#if item.kind === "task" && !item.malformed}
                <button class="btn btn-small btn-primary" disabled={busy} title="Put it on Your day" onclick={() => void accept(familyId, item)}>
                  <Check size={12} strokeWidth={2} /> Accept
                </button>
              {/if}
              {#if !item.malformed && item.from}
                <button
                  class="btn btn-small btn-ghost"
                  disabled={busy}
                  onclick={() => {
                    replyFor = item.id;
                    replyNote = "";
                  }}><Reply size={12} strokeWidth={1.75} /> Reply</button
                >
              {/if}
              <button class="btn btn-small btn-ghost" disabled={busy} onclick={() => void dismiss(familyId, item)}>
                <Archive size={12} strokeWidth={1.75} /> Dismiss
              </button>
            </div>
          {/if}
        </div>
      {/each}
    {/each}
    {#if families.connections.length > 0 && families.trayGroups.length === 0}
      <p class="note">Nothing in the inbox.</p>
    {/if}
    {#if families.connections.length === 0 && !families.loading}
      <p class="note">
        A team inbox is a shared git repo the team's copies of Ken sync through. A task or message sent to you
        waits here until you act on it; an accepted task goes on Your day. Connect one to the right.
      </p>
    {/if}
  </div>

  <div class="inboxes">
    {#each families.connections as dto (dto.connection.familyId)}
      {@const id = dto.connection.familyId}
      <div class="card">
        <div class="card-head">
          <span class="card-title">{dto.connection.name}</span>
          <span class="state state-{dto.state.state}"><span class="state-dot"></span>{stateLabel(dto.state)}</span>
        </div>
        {#if dto.state.state === "conflict"}
          <p class="note warn">Sync stopped: a change here and one in the remote touch the same lines. Settle it by hand in the clone, then say so.</p>
          <p class="note mono">{dto.state.detail}</p>
          <div class="actions left">
            <button class="btn btn-small" onclick={() => void run("Could not clear the conflict", families.resolveConflict(id))}>I settled it by hand</button>
          </div>
        {:else if dto.state.state === "unavailable"}
          <p class="note">
            {dto.state.reason.toLowerCase().includes("git") ? `Git is not on the PATH: ${dto.state.reason}. Install git and restart Ken.` : dto.state.reason}
          </p>
        {:else if dto.state.state === "error"}
          <p class="note warn">The last sync failed: {dto.state.detail}</p>
        {/if}
        <div class="kv"><span class="k">Remote</span><span class="mono small v" title={dto.connection.remoteUrl}>{dto.connection.remoteUrl}</span></div>
        <div class="kv"><span class="k">You are</span><span class="v">{dto.connection.memberId}</span></div>
        {#if (members[id] ?? []).length > 0}
          <div class="kv">
            <span class="k">Members</span>
            <span class="v">{(members[id] ?? []).map((m) => m.name || m.id).join(", ")}</span>
          </div>
        {/if}
        <label class="check">
          <input
            type="checkbox"
            checked={dto.connection.liveSync}
            disabled={dto.state.state === "unavailable"}
            onchange={(e) => void run("Could not change sync", families.setLiveSync(id, e.currentTarget.checked))}
          />
          Sync every
          <input
            type="number"
            class="poll"
            min="30"
            max="1800"
            step="10"
            value={dto.connection.pollIntervalSecs}
            disabled={dto.state.state === "unavailable"}
            aria-label="Seconds between syncs"
            onchange={(e) => setPoll(id, e.currentTarget.valueAsNumber)}
          />
          seconds
        </label>
        <div class="actions left">
          <button
            class="btn btn-small"
            disabled={families.syncingIds.has(id) || dto.state.state === "unavailable" || dto.state.state === "conflict"}
            onclick={() => void run("Could not sync the team inbox", families.syncNow(id))}
          >
            {families.syncingIds.has(id) ? "Syncing…" : "Sync now"}
          </button>
          {#if dto.connection.attachedWorkspaceId && app.workspace?.id === dto.connection.attachedWorkspaceId}
            <button
              class="btn btn-small btn-ghost"
              title="Its shared/ folder leaves this workspace's search"
              onclick={() => void run("Could not detach it", families.detachWorkspace(id))}>Detach from search</button
            >
          {:else if app.workspace}
            <button
              class="btn btn-small btn-ghost"
              title="Its shared/ folder joins this workspace's search and map"
              onclick={() => app.workspace && void run("Could not attach it", families.attachWorkspace(id, app.workspace.id))}
            >
              {dto.connection.attachedWorkspaceId ? "Attach here instead" : "Search its shared folder"}
            </button>
          {/if}
          <button
            class="btn btn-small btn-ghost forget"
            title="Forget this connection. The clone on disk stays."
            onclick={() => void run("Could not forget it", families.remove(id))}>Forget</button
          >
        </div>
      </div>
    {/each}

    {#if families.connections.length > 0 && !connectOpen}
      <button class="link" onclick={() => (connectOpen = true)}>Connect another team inbox</button>
    {/if}

    {#if showConnect}
      <div class="card">
        <div class="card-title">Create a team inbox</div>
        <p class="note">Make an empty repo on your git host first, then give its address here. Ken sets it up and makes you its first member.</p>
        <input class="text" bind:value={createName} placeholder="Name, e.g. ATT team" aria-label="Name" />
        <input class="text" bind:value={createMember} placeholder="Your name" aria-label="Your name" />
        <input class="text mono" bind:value={createRemote} placeholder="git@host:team/inbox.git" aria-label="Remote" />
        {#if families.createError}<p class="note warn">{families.createError}</p>{/if}
        <div class="actions left">
          <button class="btn btn-small" disabled={families.createBusy} onclick={() => void submitCreate()}>
            {families.createBusy ? "Creating…" : "Create"}
          </button>
        </div>
      </div>
      <div class="card">
        <div class="card-title">Join the team's inbox</div>
        <input class="text mono" bind:value={joinRemote} placeholder="git@host:team/inbox.git" aria-label="Remote" />
        <div class="row">
          <label class="check"><input type="radio" name="join-mode" checked={joinMode === "existing"} onchange={() => (joinMode = "existing")} /> I am a member</label>
          <label class="check"><input type="radio" name="join-mode" checked={joinMode === "new"} onchange={() => (joinMode = "new")} /> I am new</label>
        </div>
        {#if joinMode === "existing"}
          <input class="text" bind:value={joinMemberId} placeholder="Your member id, e.g. sarah" aria-label="Member id" />
          <p class="note">A teammate can tell you the member id the inbox knows you by.</p>
        {:else}
          <input class="text" bind:value={joinNewName} placeholder="Your name" aria-label="Your name" />
          <p class="note">You are added to its members.</p>
        {/if}
        {#if families.joinError}<p class="note warn">{families.joinError}</p>{/if}
        <div class="actions left">
          <button class="btn btn-small" disabled={families.joinBusy} onclick={() => void submitJoin()}>
            {families.joinBusy ? "Joining…" : "Join"}
          </button>
          {#if connectOpen}
            <button class="btn btn-small btn-ghost" onclick={() => (connectOpen = false)}>Cancel</button>
          {/if}
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .divider {
    margin-top: 18px;
    padding-bottom: 4px;
    border-bottom: 1px solid var(--border);
    font-size: 10.5px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink-tertiary);
  }
  .cols {
    display: grid;
    grid-template-columns: minmax(0, 1.7fr) minmax(280px, 1fr);
    gap: 24px;
    margin-top: 6px;
  }
  .messages,
  .inboxes {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .sp {
    flex: 1;
  }
  .bar .btn,
  .actions .btn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .compose,
  .reply {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .compose {
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: 9px;
    background: var(--surface);
  }
  .compose-row {
    display: flex;
    gap: 6px;
  }
  .compose-row select {
    flex: 1;
  }
  select,
  input.text,
  .compose input,
  textarea {
    font: inherit;
    font-size: 12.5px;
    padding: 5px 8px;
    border-radius: 7px;
    border: 1px solid var(--border-strong);
    background-color: var(--surface);
    color: var(--ink);
    min-width: 0;
  }
  select {
    padding-right: 26px;
  }
  textarea {
    resize: vertical;
  }
  .group-name {
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-secondary);
    margin-top: 4px;
  }
  .item {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 9px 12px;
    border: 1px solid var(--border);
    border-radius: 9px;
    background: var(--surface);
  }
  .item.malformed {
    border-color: color-mix(in srgb, var(--danger) 35%, var(--border));
  }
  .item-head {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
  }
  .kind-icon {
    flex: none;
    width: 18px;
    height: 18px;
    border-radius: 5px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: var(--sunken);
    color: var(--ink-tertiary);
  }
  .kind-icon.task {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    color: var(--accent-deep);
  }
  .item-title {
    min-width: 0;
    font-size: 13px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .unread-dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 4px;
    background: var(--accent);
  }
  .item-meta {
    margin-left: auto;
    flex: none;
    font-size: 11px;
    color: var(--ink-tertiary);
  }
  .item-body {
    margin: 0;
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--ink-secondary);
    white-space: pre-wrap;
  }
  .item-body.warn {
    color: var(--danger);
  }
  .actions {
    display: flex;
    gap: 6px;
    justify-content: flex-end;
    flex-wrap: wrap;
  }
  .actions.left {
    justify-content: flex-start;
  }
  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .card-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .card-title {
    flex: 1;
    font-size: 13.5px;
    font-weight: 600;
  }
  .state {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  .state-dot {
    width: 7px;
    height: 7px;
    border-radius: 4px;
    background: var(--ink-tertiary);
  }
  .state-idle .state-dot {
    background: var(--healthy);
  }
  .state-syncing .state-dot {
    background: var(--accent);
  }
  .state-conflict {
    color: var(--danger);
  }
  .state-conflict .state-dot {
    background: var(--danger);
  }
  .state-error {
    color: var(--needs-input-text);
  }
  .state-error .state-dot {
    background: var(--needs-input);
  }
  .state-unavailable {
    color: var(--ink-tertiary);
  }
  .kv {
    display: flex;
    gap: 10px;
    font-size: 12.5px;
    min-width: 0;
  }
  .k {
    width: 64px;
    flex: none;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  .v {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .small {
    font-size: 11.5px;
  }
  .row {
    display: flex;
    gap: 14px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 12.5px;
    cursor: pointer;
  }
  .check input {
    accent-color: var(--accent);
  }
  .poll {
    width: 64px;
    font: inherit;
    font-size: 12px;
    padding: 3px 6px;
    border-radius: 6px;
    border: 1px solid var(--border-strong);
    background: var(--surface);
    color: var(--ink);
  }
  .forget {
    margin-left: auto;
  }
  .link {
    align-self: flex-start;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: 12.5px;
    color: var(--accent);
    cursor: pointer;
  }
  .note {
    margin: 0;
    font-size: 12.5px;
    line-height: 1.55;
    color: var(--ink-tertiary);
  }
  .note.warn {
    color: var(--needs-input-text);
  }
</style>
