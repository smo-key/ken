// The adapter most surfaces use: find over the text a preview already rendered,
// wrap the hits in <mark>, scroll the current one into view. Call it once from a
// component's init; it registers on mount and cleans the DOM up on unmount.
import { MATCH_CAP } from "./find";
import {
  clearHighlights,
  highlightMatches,
  scrollMarkIntoView,
  setCurrent,
} from "./find-dom";
import { find, type FindAdapter } from "./find.svelte";

export function registerDomFind(
  root: () => HTMLElement | null | undefined,
  opts: {
    /** Read the reactive content here so a preview that loads late re-registers. */
    deps?: () => unknown;
    note?: string;
  } = {},
): void {
  $effect(() => {
    opts.deps?.();
    const el = root();
    if (!el) return;

    let marks: HTMLElement[] = [];
    const adapter: FindAdapter = {
      search(query) {
        marks = highlightMatches(el, query);
        return {
          total: marks.length,
          capped: marks.length >= MATCH_CAP,
          note: opts.note,
        };
      },
      reveal(index) {
        scrollMarkIntoView(setCurrent(marks, index));
      },
      clear() {
        marks = [];
        clearHighlights(el);
      },
    };

    find.register(adapter);
    return () => {
      clearHighlights(el);
      find.unregister(adapter);
    };
  });
}

/** A surface with nothing to search (an image). The bar says "No results". */
export function registerEmptyFind(note: string): void {
  $effect(() => {
    const adapter: FindAdapter = {
      search: () => ({ total: 0, note }),
      reveal: () => {},
      clear: () => {},
    };
    find.register(adapter);
    return () => find.unregister(adapter);
  });
}
