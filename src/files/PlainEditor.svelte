<script lang="ts">
  import { tick, untrack } from "svelte";
  import { MATCH_CAP, findTextMatches } from "../lib/find";
  import { find, type FindAdapter } from "../lib/find.svelte";
  import type { Reveal } from "../lib/app.svelte";

  let {
    initial,
    onchange,
    reveal = null,
  }: { initial: string; onchange: (text: string) => void; reveal?: Reveal | null } = $props();

  // A clicked citation: put the caret on its line and scroll it into view.
  $effect(() => {
    const r = reveal;
    const el = ta;
    if (!r?.line || !el) return;
    void r.nonce;
    untrack(() => {
      const lines = value.split("\n");
      const at = Math.min(r.line!, lines.length) - 1;
      const start = lines.slice(0, at).reduce((n, l) => n + l.length + 1, 0);
      el.focus();
      el.setSelectionRange(start, start + (lines[at]?.length ?? 0));
      const lineHeight = parseFloat(getComputedStyle(el).lineHeight) || 22;
      el.scrollTop = Math.max(0, at * lineHeight - el.clientHeight / 3);
      syncScroll();
    });
  });

  let value = $state(initial);

  let ta = $state<HTMLTextAreaElement | null>(null);
  let layer = $state<HTMLDivElement | null>(null);
  let starts = $state<number[]>([]);
  let hitLength = $state(0);
  let current = $state(0);

  // A textarea can't hold markup, so highlights are painted on a mirror layer
  // behind it — which means re-rendering the whole file as spans. Past this size
  // that costs more than it's worth, so a big file falls back to selecting one
  // match at a time (still counted, still scrolled to).
  const PAINT_MAX = 400_000;
  const painted = $derived(value.length <= PAINT_MAX);

  interface Segment {
    text: string;
    /** -1 for the text between hits. */
    hit: number;
  }

  const segments = $derived.by<Segment[]>(() => {
    if (!painted || starts.length === 0 || hitLength === 0) return [];
    const out: Segment[] = [];
    let at = 0;
    starts.forEach((start, i) => {
      if (start > at) out.push({ text: value.slice(at, start), hit: -1 });
      out.push({ text: value.slice(start, start + hitLength), hit: i });
      at = start + hitLength;
    });
    out.push({ text: value.slice(at), hit: -1 });
    return out;
  });

  function syncScroll() {
    if (!ta || !layer) return;
    layer.scrollTop = ta.scrollTop;
    layer.scrollLeft = ta.scrollLeft;
  }

  // scroll=false comes from an edit-driven refresh: re-accent the current match
  // (set `current`) but leave the textarea where the user's cursor is.
  async function revealMatch(index: number, scroll = true) {
    current = index;
    const start = starts[index];
    if (start === undefined || !ta) return;

    if (painted) {
      await tick();
      if (!scroll) return; // accent applied above; don't move the view
      const mark = layer?.querySelector<HTMLElement>(".ken-find-current");
      if (mark) {
        ta.scrollTop = Math.max(0, mark.offsetTop - ta.clientHeight / 2);
        syncScroll();
        return;
      }
    }
    if (!scroll) return;
    // Unpainted (very large) file: select the hit and scroll to its line.
    // Wrapped lines make that an estimate — which is why it's the fallback.
    ta.setSelectionRange(start, start + hitLength);
    const line = value.slice(0, start).split("\n").length - 1;
    const lineHeight = parseFloat(getComputedStyle(ta).lineHeight) || 22;
    ta.scrollTop = Math.max(0, line * lineHeight - ta.clientHeight / 2);
    syncScroll();
  }

  $effect(() => {
    const adapter: FindAdapter = {
      // untrack: the controller calls search() while `register` is still inside
      // this effect, and reading `value` there would re-register on every
      // keystroke — resetting the match the user is standing on.
      search: (query) =>
        untrack(() => {
          const result = findTextMatches(value, query, MATCH_CAP);
          starts = result.starts;
          hitLength = query.length;
          return {
            total: result.starts.length,
            capped: result.capped,
            note: painted
              ? undefined
              : "Large file — matches are selected one at a time rather than all highlighted.",
          };
        }),
      reveal: (index, opts) =>
        untrack(() => revealMatch(index, opts?.scroll !== false)),
      clear() {
        starts = [];
        hitLength = 0;
      },
    };
    find.register(adapter);
    return () => find.unregister(adapter);
  });
</script>

<div class="scroll">
  <div class="wrap">
    <!-- A mirror of the text, behind the textarea, carrying only the highlights.
         Every box property it shares with the textarea must stay identical or
         the marks drift away from the words they belong to. -->
    <div class="layer" bind:this={layer} aria-hidden="true">{#each segments as seg, i (i)}{#if seg.hit >= 0}<mark class="ken-find" class:ken-find-current={seg.hit === current}>{seg.text}</mark>{:else}{seg.text}{/if}{/each}</div>
    <textarea
      bind:this={ta}
      bind:value
      oninput={() => {
        onchange(value);
        find.refresh(false); // re-accent the current match, don't yank the cursor
      }}
      onscroll={syncScroll}
      spellcheck="false"
    ></textarea>
  </div>
</div>

<style>
  .scroll {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .wrap {
    position: relative;
    flex: 1;
    min-height: 0;
    max-width: 720px;
    width: 100%;
    margin: 0 auto;
    display: flex;
  }
  .layer,
  textarea {
    box-sizing: border-box;
    padding: 24px clamp(20px, 5%, 48px) 80px;
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 1.7;
    white-space: pre-wrap;
    overflow-wrap: break-word;
  }
  .layer {
    position: absolute;
    inset: 0;
    margin: 0;
    overflow: hidden;
    pointer-events: none;
    color: transparent;
  }
  textarea {
    position: relative;
    flex: 1;
    width: 100%;
    border: none;
    outline: none;
    resize: none;
    background: transparent;
    color: var(--ink);
  }
</style>
