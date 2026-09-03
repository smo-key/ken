<script lang="ts">
  // ken-tasks task 4.2: create/edit dialog for an overarching goal (design
  // D7). Same modal shell as `AutomationForm.svelte`. Status is only
  // offered on edit — a fresh goal always starts `active` (backend default),
  // matching `goal_create`'s own `NewGoal.status` being optional and unused
  // here.
  import { tasksStore } from "../lib/tasks.svelte";
  import type { Goal, GoalStatus } from "../lib/api";

  let { goal, close }: { goal: Goal | null; close: () => void } = $props();

  let title = $state(goal?.title ?? "");
  let body = $state(goal?.body ?? "");
  let status = $state<GoalStatus>(goal?.status ?? "active");
  let saving = $state(false);
  let error = $state<string | null>(null);

  const statusOptions: { value: GoalStatus; label: string }[] = [
    { value: "active", label: "Active" },
    { value: "done", label: "Done" },
    { value: "dropped", label: "Dropped" },
  ];

  async function save() {
    error = null;
    const t = title.trim();
    if (!t) {
      error = "A goal needs a title.";
      return;
    }
    saving = true;
    try {
      if (goal) {
        await tasksStore.updateGoal(goal.id, { title: t, status });
      } else {
        await tasksStore.createGoal(t, body);
      }
      await tasksStore.refresh();
      close();
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }
</script>

<button class="scrim" onclick={close} aria-label="Close"></button>
<div class="modal" role="dialog" aria-label={goal ? "Edit goal" : "New goal"}>
  <h2>{goal ? "Edit goal" : "New goal"}</h2>

  <label>
    Title
    <input bind:value={title} placeholder="Ship multi-project Ken" />
  </label>

  {#if !goal}
    <label>
      Description (optional)
      <textarea bind:value={body} rows="4" placeholder="What does done look like?"></textarea>
    </label>
  {/if}

  {#if goal}
    <label>
      Status
      <div class="seg" role="group" aria-label="Status">
        {#each statusOptions as opt (opt.value)}
          <button class:on={status === opt.value} onclick={() => (status = opt.value)}>
            {opt.label}
          </button>
        {/each}
      </div>
    </label>
  {/if}

  {#if error}
    <div class="error">{error}</div>
  {/if}

  <div class="actions">
    <button class="btn btn-primary" onclick={save} disabled={saving}>
      {saving ? "Saving…" : goal ? "Save changes" : "Create goal"}
    </button>
    <button class="btn btn-ghost" onclick={close}>Cancel</button>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    border: none;
    z-index: 60;
  }
  .modal {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(480px, calc(100vw - 80px));
    max-height: calc(100vh - 120px);
    overflow-y: auto;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-overlay);
    box-shadow: var(--shadow-overlay);
    padding: 24px 26px;
    z-index: 61;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  h2 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 20px;
    font-weight: 500;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-secondary);
  }
  input,
  textarea {
    font-family: inherit;
    font-size: 13.5px;
    color: var(--ink);
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    background: var(--surface);
    padding: 8px 12px;
    outline: none;
    resize: vertical;
  }
  input:focus,
  textarea:focus {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .seg {
    display: flex;
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    overflow: hidden;
    align-self: flex-start;
  }
  .seg button {
    font-size: 12px;
    font-weight: 500;
    padding: 7px 12px;
    border: none;
    background: var(--surface);
    color: var(--ink-secondary);
  }
  .seg button.on {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    color: var(--accent-deep);
    font-weight: 600;
  }
  .error {
    font-size: 12.5px;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 7%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 25%, transparent);
    border-radius: 9px;
    padding: 9px 12px;
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }
</style>
