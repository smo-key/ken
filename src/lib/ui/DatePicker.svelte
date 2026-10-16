<script lang="ts">
  // A date in the design system: the date (or "No date") on a button, and
  // a calendar under it with the usual choices first. The value is a local
  // YYYY-MM-DD, or "" for none.
  import CalendarDays from "@lucide/svelte/icons/calendar-days";
  import ChevronLeft from "@lucide/svelte/icons/chevron-left";
  import ChevronRight from "@lucide/svelte/icons/chevron-right";

  let {
    value,
    onchange,
    label = "Date",
  }: { value: string; onchange: (value: string) => void; label?: string } = $props();

  let open = $state(false);
  const pad = (n: number) => String(n).padStart(2, "0");
  const iso = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  const parse = (v: string) => {
    const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(v);
    return m ? new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])) : null;
  };
  const today = () => {
    const d = new Date();
    return new Date(d.getFullYear(), d.getMonth(), d.getDate());
  };
  const addDays = (d: Date, n: number) => new Date(d.getFullYear(), d.getMonth(), d.getDate() + n);

  const selected = $derived(parse(value));
  let shown = $state(new Date(new Date().getFullYear(), new Date().getMonth(), 1));

  function toggle() {
    if (!open) {
      const base = selected ?? today();
      shown = new Date(base.getFullYear(), base.getMonth(), 1);
    }
    open = !open;
  }

  function pick(d: Date | null) {
    onchange(d ? iso(d) : "");
    open = false;
  }

  function nextMonday(): Date {
    const t = today();
    const add = ((8 - t.getDay()) % 7) || 7;
    return addDays(t, add);
  }

  /** The days to draw: the weeks that cover the shown month, Sunday first. */
  const days = $derived.by(() => {
    const first = new Date(shown.getFullYear(), shown.getMonth(), 1);
    const start = addDays(first, -first.getDay());
    return Array.from({ length: 42 }, (_, i) => addDays(start, i));
  });
  const monthName = $derived(shown.toLocaleDateString(undefined, { month: "long", year: "numeric" }));
  const shownText = $derived(
    selected ? selected.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" }) : "No date",
  );
  const sameDay = (a: Date | null, b: Date) => !!a && iso(a) === iso(b);

  // Esc closes the calendar only, not the panel it sits in.
  function onkey(e: KeyboardEvent) {
    if (e.key === "Escape" && open) {
      e.stopImmediatePropagation();
      e.preventDefault();
      open = false;
    }
  }
</script>

<svelte:window onkeydowncapture={onkey} />

<span class="dp">
  <button class="trigger" class:empty={!selected} aria-label={label} aria-expanded={open} onclick={toggle}>
    <CalendarDays size={14} strokeWidth={1.75} aria-hidden="true" />
    {shownText}
  </button>
  {#if open}
    <button class="scrim" aria-label="Close the calendar" onclick={() => (open = false)}></button>
    <div class="pop" role="dialog" aria-label={label}>
      <div class="quick">
        <button class="chip" onclick={() => pick(today())}>Today</button>
        <button class="chip" onclick={() => pick(addDays(today(), 1))}>Tomorrow</button>
        <button class="chip" onclick={() => pick(nextMonday())}>Next Monday</button>
        {#if selected}<button class="chip ghost" onclick={() => pick(null)}>Clear</button>{/if}
      </div>
      <div class="head">
        <button class="nav" aria-label="Previous month" onclick={() => (shown = new Date(shown.getFullYear(), shown.getMonth() - 1, 1))}>
          <ChevronLeft size={15} strokeWidth={1.75} />
        </button>
        <span class="month">{monthName}</span>
        <button class="nav" aria-label="Next month" onclick={() => (shown = new Date(shown.getFullYear(), shown.getMonth() + 1, 1))}>
          <ChevronRight size={15} strokeWidth={1.75} />
        </button>
      </div>
      <div class="grid">
        {#each ["S", "M", "T", "W", "T", "F", "S"] as w, i (i)}<span class="wd">{w}</span>{/each}
        {#each days as d (iso(d))}
          <button
            class="day"
            class:out={d.getMonth() !== shown.getMonth()}
            class:today={sameDay(today(), d)}
            class:on={sameDay(selected, d)}
            aria-label={d.toDateString()}
            onclick={() => pick(d)}>{d.getDate()}</button
          >
        {/each}
      </div>
    </div>
  {/if}
</span>

<style>
  .dp {
    position: relative;
    display: inline-flex;
  }
  .trigger {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 30px;
    padding: 0 10px;
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-control);
    background: var(--surface);
    color: var(--ink);
    font-size: 12.5px;
  }
  .trigger:hover {
    border-color: var(--ink-3);
  }
  .trigger.empty {
    color: var(--ink-3);
  }
  .trigger :global(svg) {
    color: var(--ink-3);
  }
  .scrim {
    position: fixed;
    inset: 0;
    background: transparent;
    border: none;
    z-index: 49;
  }
  .pop {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: 50;
    width: 252px;
    padding: 10px;
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--radius-overlay);
    box-shadow: var(--shadow-overlay);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .quick {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
  }
  .chip {
    padding: 3px 9px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--surface);
    color: var(--ink-2);
    font-size: 12px;
  }
  .chip:hover {
    border-color: var(--accent);
    color: var(--accent-ink);
  }
  .chip.ghost {
    border-color: transparent;
    color: var(--ink-3);
  }
  .head {
    display: flex;
    align-items: center;
  }
  .month {
    flex: 1;
    text-align: center;
    font-size: 13px;
    font-weight: 600;
  }
  .nav {
    width: 26px;
    height: 26px;
    display: grid;
    place-items: center;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--ink-2);
  }
  .nav:hover {
    background: var(--line-soft);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    gap: 2px;
  }
  .wd {
    text-align: center;
    font-size: 10.5px;
    font-weight: 600;
    color: var(--ink-3);
    padding: 2px 0 4px;
  }
  .day {
    height: 30px;
    border: none;
    border-radius: 7px;
    background: transparent;
    color: var(--ink);
    font-size: 12.5px;
    font-variant-numeric: tabular-nums;
  }
  .day:hover {
    background: var(--line-soft);
  }
  .day.out {
    color: var(--ink-3);
  }
  .day.today {
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .day.on {
    background: var(--accent);
    color: var(--on-accent);
    font-weight: 600;
  }
</style>
