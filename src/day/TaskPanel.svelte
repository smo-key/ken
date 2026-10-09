<script lang="ts">
  // The task panel (Y1b): one task's fields, each saved as it changes. In
  // "new" mode it is an empty title with the cursor in it; Enter adds the
  // task and the panel stays open on it.
  import { onMount, tick } from "svelte";
  import { api, type DayTask, type DayTaskPatch } from "../lib/api";
  import { day, memberNames } from "../lib/day.svelte";
  import { families } from "../lib/families.svelte";
  import { linkLabel, localToday, repeatChoices, sendBody, splitTicketLink, stampLabel } from "../lib/day";
  import { openConfirm } from "../lib/ui/ConfirmMenu.svelte";
  import X from "@lucide/svelte/icons/x";
  import Check from "@lucide/svelte/icons/check";
  import DatePicker from "../lib/ui/DatePicker.svelte";

  let { task, newLinks = [] }: { task: DayTask | null; newLinks?: string[] } = $props();

  const isNew = $derived(task === null);

  let title = $state("");
  let description = $state("");
  /** The field being typed in, so an outside change does not overwrite it. */
  let editing = $state<"title" | "description" | null>(null);
  let pendingLinks = $state<string[]>([]);
  let pendingTarget = $state("");
  let pendingRepeat = $state("");
  let titleEl = $state<HTMLInputElement | null>(null);
  let error = $state<string | null>(null);

  let linkOpen = $state(false);
  let linkDraft = $state("");

  // Follow the task as it changes elsewhere (the chat, an agent, a hand
  // edit), except for the field being typed in.
  $effect(() => {
    const t = task;
    if (!t) return;
    if (editing !== "title") title = t.title;
    if (editing !== "description") description = t.description;
  });

  $effect(() => {
    if (isNew) pendingLinks = [...newLinks];
  });

  onMount(() => {
    if (isNew) void tick().then(() => titleEl?.focus());
  });

  const links = $derived(task ? task.links : pendingLinks);
  const repos = $derived(memberNames());
  const target = $derived(task ? (task.target ?? "") : pendingTarget);
  const repeat = $derived(task ? (task.repeat ?? "") : pendingRepeat);
  const anchor = $derived(target || localToday());
  const choices = $derived.by(() => {
    const list = repeatChoices(anchor);
    // A repeat stored on another day than the target still shows as itself.
    const current = task?.repeat ?? "";
    if (current && !list.some((c) => c.value === current)) list.push({ value: current, label: current });
    return list;
  });

  async function save(patch: DayTaskPatch) {
    if (!task) return;
    error = null;
    try {
      await day.update(task.id, patch);
    } catch (e) {
      error = String(e);
    }
  }

  // Text saves ~400 ms after the last keystroke, and at once on blur.
  let timers: Record<string, ReturnType<typeof setTimeout>> = {};
  function debounce(key: "title" | "description", run: () => void) {
    clearTimeout(timers[key]);
    timers[key] = setTimeout(run, 400);
  }
  function flush(key: "title" | "description") {
    clearTimeout(timers[key]);
    delete timers[key];
    editing = null;
    if (!task) return;
    if (key === "title" && title.trim() && title.trim() !== task.title) void save({ title: title.trim() });
    if (key === "description" && description !== task.description) void save({ description });
  }
  function clearTimers() {
    Object.values(timers).forEach(clearTimeout);
    timers = {};
  }
  // Closing the panel or switching task saves what is still waiting. The
  // prop may already read null by then, so keep the last task seen.
  let lastTask: DayTask | null = null;
  $effect(() => {
    if (task) lastTask = task;
  });
  $effect(() => () => {
    const t = lastTask;
    const pending = { ...timers };
    clearTimers();
    if (!t) return;
    const patch: DayTaskPatch = {};
    if (pending.title && title.trim() && title.trim() !== t.title) patch.title = title.trim();
    if (pending.description && description !== t.description) patch.description = description;
    if (Object.keys(patch).length > 0) void day.update(t.id, patch).catch(() => {});
  });

  function onTitleInput() {
    editing = "title";
    if (task) debounce("title", () => title.trim() && title.trim() !== task!.title && void save({ title: title.trim() }));
  }

  function onDescriptionInput() {
    editing = "description";
    if (task) debounce("description", () => description !== task!.description && void save({ description }));
  }

  let creating = $state(false);

  async function createFromTitle() {
    const t = title.trim();
    if (!t || creating) return;
    creating = true;
    error = null;
    try {
      const created = await day.create({
        title: t,
        links: pendingLinks,
        description,
        target: pendingTarget || null,
        repeat: pendingRepeat || null,
      });
      editing = null;
      day.openTask(created.id, { task: created });
    } catch (e) {
      error = String(e);
    } finally {
      creating = false;
    }
  }

  function onTitleKey(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      if (isNew) void createFromTitle();
      else (e.currentTarget as HTMLInputElement).blur();
    }
  }

  function setTarget(value: string) {
    if (task) void save({ target: value || null });
    else pendingTarget = value;
  }

  function setRepeat(value: string) {
    if (task) void save({ repeat: value || null });
    else pendingRepeat = value;
  }

  function addLink() {
    const l = linkDraft.trim();
    linkDraft = "";
    linkOpen = false;
    if (!l || links.some((x) => x.toLowerCase() === l.toLowerCase())) return;
    if (task) void save({ links: [...task.links, l] });
    else pendingLinks = [...pendingLinks, l];
  }

  function removeLink(l: string) {
    if (task) void save({ links: task.links.filter((x) => x !== l) });
    else pendingLinks = pendingLinks.filter((x) => x !== l);
  }

  // ── For: me, or send to a teammate through the team inbox ──────────

  type Mate = { key: string; familyId: string; memberId: string; label: string };
  let mates = $state<Mate[]>([]);
  let sendTo = $state("");
  let sending = $state(false);

  $effect(() => {
    const conns = families.connections;
    if (conns.length === 0) {
      mates = [];
      return;
    }
    void Promise.all(
      conns.map(async (c) => {
        const manifest = await api.familyManifestGet(c.connection.familyId).catch(() => null);
        return (manifest?.members ?? [])
          .filter((m) => m.id !== c.connection.memberId)
          .map((m) => ({
            key: `${c.connection.familyId}:${m.id}`,
            familyId: c.connection.familyId,
            memberId: m.id,
            label: conns.length > 1 ? `${m.name || m.id} (${c.connection.name})` : m.name || m.id,
          }));
      }),
    ).then((lists) => (mates = lists.flat()));
  });

  const mate = $derived(mates.find((m) => m.key === sendTo) ?? null);

  function confirmSend(e: MouseEvent) {
    const to = mate;
    if (!task || !to) return;
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openConfirm(r.left, r.bottom + 4, {
      title: `Send to ${to.label}?`,
      body: "It goes to their inbox and leaves your list.",
      confirmLabel: "Send",
      onConfirm: () => void send(to),
    });
  }

  async function send(to: Mate) {
    const t = task;
    if (!t || sending) return;
    sending = true;
    error = null;
    try {
      // The inbox carries a title and a body; the rest goes in the body.
      await api.familySend(to.familyId, to.memberId, "task", title.trim() || t.title, sendBody({ ...t, description }));
      clearTimers();
      await day.remove(t.id);
      sendTo = "";
    } catch (e) {
      error = String(e);
    } finally {
      sending = false;
    }
  }

  // ── Footer ──────────────────────────────────────────────────────────

  function who(by: string | null): string {
    if (!by) return "you";
    if (by === "chat") return "Ken chat";
    if (by === "mcp") return "an agent";
    return by;
  }

  const footer = $derived.by(() => {
    if (!task) return "";
    const parts: string[] = [];
    const added = stampLabel(task.created, day.today);
    if (added) parts.push(`Added ${added}`);
    const updated = stampLabel(task.updated, day.today);
    if (updated) parts.push(`updated by ${who(task.updatedBy)}, ${updated}`);
    return parts.join(" · ");
  });

  async function remove() {
    if (!task) return;
    try {
      clearTimers();
      await day.remove(task.id);
    } catch (e) {
      error = String(e);
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      if (linkOpen) {
        linkOpen = false;
        linkDraft = "";
        return;
      }
      if (editing) flush(editing);
      day.closePanel();
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<aside class="panel" aria-label={isNew ? "New task" : "Task"} onkeydown={onKey}>
  <div class="head">
    {#if task}
      <button
        class="cb"
        class:on={task.state === "done"}
        aria-label={task.state === "done" ? "Mark open" : "Mark done"}
        onclick={() => void day.toggle(task)}
      >
        {#if task.state === "done"}<Check size={11} strokeWidth={2.5} />{/if}
      </button>
    {:else}
      <span class="cb ghost"></span>
    {/if}
    <input
      class="title"
      class:done={task?.state === "done"}
      bind:this={titleEl}
      bind:value={title}
      placeholder="Title"
      aria-label="Title"
      oninput={onTitleInput}
      onblur={() => !isNew && flush("title")}
      onkeydown={onTitleKey}
    />
    <button class="close" aria-label="Close" title="Close (Esc)" onclick={() => day.closePanel()}>
      <X size={14} strokeWidth={1.75} />
    </button>
  </div>

  {#if isNew}
    <p class="hint">Enter in the title adds it.</p>
  {/if}

  <div class="field">
    <span class="label">Target</span>
    <DatePicker value={target ?? ""} label="Target date" onchange={(v) => setTarget(v)} />
  </div>

  <div class="field">
    <span class="label">Repeat</span>
    <select value={repeat} aria-label="Repeat" onchange={(e) => setRepeat(e.currentTarget.value)}>
      {#each choices as c (c.value)}<option value={c.value}>{c.label}</option>{/each}
    </select>
  </div>

  <div class="field wrap">
    <span class="label">Linked</span>
    {#each links as l (l)}
      {@const ticket = splitTicketLink(l, repos)}
      <span class="chip" title={ticket ? `${ticket.id} in ${ticket.repo}` : l}>
        {linkLabel(l, repos)}
        <button class="x" aria-label="Remove {l}" onclick={() => removeLink(l)}><X size={11} strokeWidth={2} /></button>
      </span>
    {/each}
    {#if linkOpen}
      <!-- svelte-ignore a11y_autofocus -->
      <input
        class="link-input"
        bind:value={linkDraft}
        placeholder="ATT-014, a file path or a repo"
        aria-label="Link"
        autofocus
        onkeydown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            addLink();
          }
        }}
        onblur={addLink}
      />
    {:else}
      <button class="chip ghost" onclick={() => (linkOpen = true)}>+ link</button>
    {/if}
  </div>

  {#if task && !task.inbox}
    <div class="field">
      <span class="label">For</span>
      <span class="chip on">me</span>
      {#if mates.length > 0}
        <select class="send" bind:value={sendTo} aria-label="Send to a teammate" disabled={sending}>
          <option value="">send to a teammate</option>
          {#each mates as m (m.key)}<option value={m.key}>{m.label}</option>{/each}
        </select>
        {#if mate}
          <button class="btn btn-small" disabled={sending} onclick={confirmSend}>Send</button>
        {/if}
      {/if}
    </div>
  {/if}

  <textarea
    class="desc"
    bind:value={description}
    placeholder="Description. Markdown."
    aria-label="Description"
    oninput={onDescriptionInput}
    onblur={() => !isNew && flush("description")}
  ></textarea>

  {#if error}
    <p class="error">{error}</p>
  {/if}

  {#if task}
    {#if footer}<p class="footer">{footer}</p>{/if}
    <div class="actions">
      <button class="btn btn-small btn-ghost" onclick={() => void remove()}>Delete</button>
      <span class="sp"></span>
      <button class="btn btn-small" class:btn-primary={task.state === "open"} onclick={() => void day.toggle(task)}>
        {task.state === "done" ? "Mark open" : "Done"}
      </button>
    </div>
  {:else}
    <div class="actions">
      <span class="sp"></span>
      <button class="btn btn-small btn-primary" disabled={!title.trim() || creating} onclick={() => void createFromTitle()}>Add</button>
    </div>
  {/if}
</aside>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    box-shadow: var(--shadow-card);
    min-height: 420px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .title {
    flex: 1;
    min-width: 0;
    border: none;
    background: transparent;
    font: inherit;
    font-size: 15px;
    font-weight: 600;
    color: var(--ink);
    padding: 4px 0;
    outline: none;
    border-bottom: 1px solid transparent;
  }
  .title:focus {
    border-bottom-color: var(--border-strong);
  }
  .title.done {
    text-decoration: line-through;
    color: var(--ink-tertiary);
    font-weight: 400;
  }
  .close {
    display: inline-flex;
    border: none;
    background: transparent;
    color: var(--ink-tertiary);
    padding: 4px;
    border-radius: 6px;
  }
  .close:hover {
    background: var(--sunken);
    color: var(--ink);
  }
  .hint {
    margin: -6px 0 0 28px;
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .cb {
    flex: none;
    width: 16px;
    height: 16px;
    border-radius: 4px;
    border: 1.5px solid var(--border-strong);
    background: var(--surface);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    color: var(--surface);
  }
  .cb.on {
    background: var(--accent);
    border-color: var(--accent);
  }
  .cb.ghost {
    border-style: dashed;
  }
  .field {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12.5px;
  }
  .field.wrap {
    flex-wrap: wrap;
  }
  .label {
    width: 56px;
    flex: none;
    color: var(--ink-tertiary);
    font-size: 11.5px;
  }
  .field select,
  .link-input {
    font: inherit;
    font-size: 12.5px;
    color: var(--ink);
    background-color: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 3px 8px;
  }
  .field select {
    padding-right: 24px;
    background-position: right 6px center;
    background-size: 11px;
  }
  .link-input {
    min-width: 200px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 11.5px;
    color: var(--ink-secondary);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 2px 8px;
    background: var(--sunken);
    max-width: 240px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip.on {
    color: var(--accent-deep);
    border-color: color-mix(in srgb, var(--accent) 40%, var(--border));
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .chip.ghost {
    background: transparent;
    color: var(--ink-tertiary);
    cursor: pointer;
  }
  .chip.ghost:hover {
    color: var(--ink);
    border-color: var(--border-strong);
  }
  .x {
    display: inline-flex;
    border: none;
    background: transparent;
    padding: 0;
    color: var(--ink-tertiary);
  }
  .x:hover {
    color: var(--ink);
  }
  .send {
    color: var(--ink-tertiary) !important;
  }
  .desc {
    flex: 1;
    min-height: 160px;
    resize: vertical;
    font: inherit;
    font-size: 13px;
    line-height: 1.6;
    color: var(--ink);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 10px 12px;
  }
  .footer {
    margin: 0;
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .error {
    margin: 0;
    font-size: 12px;
    color: var(--danger);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .sp {
    flex: 1;
  }
</style>
