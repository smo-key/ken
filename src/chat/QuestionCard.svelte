<script lang="ts">
  import type { ChatMessage } from "../lib/api";
  import {
    answerSummary,
    buildAnswers,
    isAnswered,
    parseQuestionPayload,
    type QuestionSelection,
  } from "../lib/chatQuestion";
  import { chats } from "../lib/chats.svelte";

  let { msg }: { msg: ChatMessage } = $props();

  const payload = $derived(parseQuestionPayload(msg.content));
  const answered = $derived(payload ? isAnswered(payload) : false);
  // One question, single-select: a click is the whole answer, so skip the
  // submit step entirely.
  const immediate = $derived(
    !!payload && payload.questions.length === 1 && !payload.questions[0].multiSelect,
  );

  let selections = $state<Record<string, QuestionSelection>>({});
  let otherOpen = $state<Record<string, boolean>>({});
  let submitting = $state(false);

  const pending = $derived(
    payload && !answered ? buildAnswers(payload, new Map(Object.entries(selections))) : null,
  );

  function chosen(q: string, label: string): boolean {
    if (payload && answered) {
      const a = payload.answers?.[q] ?? "";
      return a.split(", ").includes(label);
    }
    return (selections[q]?.labels ?? []).includes(label);
  }

  async function submit(answers: Record<string, string>) {
    if (submitting) return;
    submitting = true;
    await chats.answerQuestion(msg.id, answers);
    submitting = false;
  }

  function pick(q: string, label: string, multi: boolean) {
    if (immediate) {
      void submit({ [q]: label });
      return;
    }
    const cur = selections[q]?.labels ?? [];
    const labels = multi
      ? cur.includes(label)
        ? cur.filter((l) => l !== label)
        : [...cur, label]
      : [label];
    selections = { ...selections, [q]: { ...selections[q], labels } };
  }

  function setOther(q: string, value: string) {
    selections = { ...selections, [q]: { labels: selections[q]?.labels ?? [], other: value } };
  }

  function otherKey(e: KeyboardEvent, q: string) {
    if (e.key !== "Enter") return;
    e.preventDefault();
    const value = (e.currentTarget as HTMLInputElement).value.trim();
    if (!value) return;
    if (immediate) void submit({ [q]: value });
    else if (pending) void submit(pending);
  }
</script>

{#if payload}
  <div class="card" class:answered>
    {#each payload.questions as q (q.question)}
      <div class="q">
        <div class="head">
          {#if q.header}<span class="chip">{q.header}</span>{/if}
          <span class="text">{q.question}</span>
        </div>
        {#if answered}
          <div class="opts">
            {#each q.options.filter((o) => chosen(q.question, o.label)) as o (o.label)}
              <div class="opt picked">
                <span class="label">{o.label}</span>
              </div>
            {/each}
          </div>
        {:else}
          <div class="opts">
            {#each q.options as o (o.label)}
              <button
                class="opt"
                class:picked={chosen(q.question, o.label)}
                disabled={submitting}
                onclick={() => pick(q.question, o.label, q.multiSelect)}
              >
                <span class="label">{o.label}</span>
                {#if o.description}<span class="desc">{o.description}</span>{/if}
              </button>
            {/each}
            {#if otherOpen[q.question]}
              <!-- svelte-ignore a11y_autofocus -->
              <input
                class="other-input"
                autofocus
                placeholder="Something else…"
                disabled={submitting}
                value={selections[q.question]?.other ?? ""}
                oninput={(e) => setOther(q.question, e.currentTarget.value)}
                onkeydown={(e) => otherKey(e, q.question)}
              />
            {:else}
              <button
                class="other"
                disabled={submitting}
                onclick={() => (otherOpen = { ...otherOpen, [q.question]: true })}>Other…</button
              >
            {/if}
          </div>
        {/if}
      </div>
    {/each}

    {#if answered}
      <div class="summary">
        {#each answerSummary(payload) as line}
          <div>{line}</div>
        {/each}
      </div>
    {:else if !immediate}
      <button class="submit" disabled={!pending || submitting} onclick={() => pending && submit(pending)}
        >Answer</button
      >
    {/if}
  </div>
{/if}

<style>
  .card {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin-left: var(--gutter);
    padding: 12px;
    border: 1px solid var(--border-strong);
    border-radius: 10px;
    background: var(--surface);
  }
  .card.answered {
    opacity: 0.85;
  }
  .q {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex-wrap: wrap;
  }
  .chip {
    flex: none;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    padding: 2px 6px;
    border-radius: 5px;
    background: var(--sunken);
    color: var(--ink-secondary);
  }
  .text {
    font-size: 13px;
    line-height: 1.5;
    color: var(--ink);
  }
  .opts {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .opt {
    display: flex;
    flex-direction: column;
    gap: 2px;
    text-align: left;
    padding: 8px 11px;
    border: 1px solid var(--border-strong);
    border-radius: 9px;
    background: var(--surface);
    color: var(--ink);
  }
  button.opt:hover:not(:disabled) {
    background: var(--sunken);
  }
  .opt.picked {
    border-color: var(--accent);
    background: var(--sunken);
  }
  .label {
    font-size: 12.5px;
    line-height: 1.4;
  }
  .desc {
    font-size: 11px;
    line-height: 1.45;
    color: var(--ink-tertiary);
  }
  .other {
    align-self: flex-start;
    font-size: 11.5px;
    padding: 4px 8px;
    border-radius: 7px;
    background: none;
    color: var(--ink-tertiary);
  }
  .other:hover:not(:disabled) {
    color: var(--ink);
  }
  .other-input {
    font: inherit;
    font-size: 12.5px;
    padding: 8px 11px;
    border: 1px solid var(--border-strong);
    border-radius: 9px;
    background: var(--surface);
    color: var(--ink);
  }
  .submit {
    align-self: flex-start;
    font-size: 12px;
    padding: 6px 14px;
    border-radius: 8px;
    background: var(--ink);
    color: var(--paper);
  }
  .submit:disabled {
    opacity: 0.4;
  }
  .summary {
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 11.5px;
    color: var(--ink-secondary);
  }
</style>
