// The find-in-document controller. One instance for the whole app: it owns the
// query, the match count, the current index and whether the bar is open — and
// nothing else. What a "match" is, and how to show it, belongs to the surface,
// which registers an adapter when it mounts.
//
// The query deliberately outlives the adapter: tabs remount EditorPane on every
// switch, and ⌘F → Enter has to keep working as the user moves between files.
import { untrack } from "svelte";
import { counterLabel, stepMatch } from "./find";

export interface FindResult {
  total: number;
  /** The scan stopped at a cap — the count is a floor. */
  capped?: boolean;
  /** Honest one-liner about what this surface can't do (sandboxed page, etc.). */
  note?: string;
}

export interface FindAdapter {
  /** Find (and highlight) every hit. Returns how many there are. */
  search(query: string): FindResult | Promise<FindResult>;
  /** Accent hit `index` and, unless `opts.scroll` is false, scroll it into view.
      `scroll: false` is for edit-driven refreshes: the accent must reappear on
      the current match, but yanking the view under the user's cursor while they
      type is jarring. Adapters that never edit may ignore `opts` and always
      scroll. */
  reveal(index: number, opts?: { scroll?: boolean }): void | Promise<void>;
  /** Remove every highlight and put the surface back the way it was. */
  clear(): void;
}

class FindController {
  open = $state(false);
  query = $state("");
  total = $state(0);
  index = $state(0);
  capped = $state(false);
  note = $state<string | null>(null);
  /** Bumped to ask FindBar to focus and select its input. */
  focusToken = $state(0);

  #adapter: FindAdapter | null = null;
  /** Guards against a slow search (a big PDF) landing after a newer one. */
  #generation = 0;

  readonly counter = $derived(
    counterLabel(this.query, this.index, this.total, this.capped),
  );

  register(adapter: FindAdapter): void {
    this.#adapter = adapter;
    this.#reset();
    // Adapters register from inside a `$effect`. Reading `open`/`query` here
    // would subscribe that effect to them, so every keystroke would tear the
    // adapter down and re-register it — harmless for a synchronous search, but
    // it lets a slow async search (a PDF's) race its own re-registration and
    // strand the result at zero. Untracked: the read decides whether to kick
    // off an initial search without ever becoming a dependency.
    untrack(() => {
      if (this.open && this.query) void this.#run();
    });
  }

  unregister(adapter: FindAdapter): void {
    if (this.#adapter !== adapter) return; // a later surface already took over
    this.#adapter = null;
    this.#reset();
  }

  toggle(): void {
    if (this.open) this.close();
    else this.show();
  }

  show(): void {
    this.open = true;
    this.focusToken++;
    if (this.query) void this.#run();
  }

  close(): void {
    if (!this.open) return;
    this.open = false;
    this.#generation++; // strand any search still in flight
    this.#adapter?.clear();
    this.#reset();
  }

  setQuery(query: string): void {
    this.query = query;
    void this.#run();
  }

  /** Re-run the current query — for surfaces whose content changed underneath —
      keeping the current index and re-applying its accent. `scroll` defaults to
      true for the content-load case (a PDF's pages or an XLSX workbook arriving
      after the query was typed: find must jump to the first match). Editors pass
      `false` so re-searching on every keystroke re-accents the current match
      without fighting the user's cursor. */
  refresh(scroll = true): void {
    if (this.open && this.query) void this.#run(true, true, scroll);
  }

  next(): void {
    this.#step(1);
  }

  previous(): void {
    this.#step(-1);
  }

  #step(delta: number): void {
    if (this.total === 0) return;
    this.index = stepMatch(this.index, this.total, delta);
    void this.#adapter?.reveal(this.index);
  }

  #reset(): void {
    this.total = 0;
    this.index = 0;
    this.capped = false;
    this.note = null;
  }

  async #run(keepIndex = false, reveal = true, scroll = true): Promise<void> {
    const generation = ++this.#generation;
    const adapter = this.#adapter;
    if (!adapter) {
      this.#reset();
      return;
    }
    if (!this.query.trim()) {
      adapter.clear();
      this.#reset();
      return;
    }

    const result = await adapter.search(this.query);
    if (generation !== this.#generation || adapter !== this.#adapter) return;

    this.total = result.total;
    this.capped = result.capped ?? false;
    this.note = result.note ?? null;
    this.index =
      this.total === 0
        ? 0
        : Math.min(keepIndex ? this.index : 0, this.total - 1);
    if (this.total > 0 && reveal) await adapter.reveal(this.index, { scroll });
  }
}

export const find = new FindController();
