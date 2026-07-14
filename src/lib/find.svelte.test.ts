// The find controller's contract with an ASYNC adapter (a PDF reads pdf.js text
// per page, so its search is a promise). A slow search must never land its count
// after a newer one has already answered — the regression that showed a stale or
// zero total in the PDF viewer. These drive the controller directly; the
// generation guard is the pure core here, no DOM or Svelte effect needed.
import { afterEach, describe, expect, it } from "vitest";
import { find, type FindAdapter } from "./find.svelte";

/** An adapter whose searches resolve only when the test says so, in any order. */
function deferredAdapter() {
  const pending: Array<{ query: string; resolve: (total: number) => void }> = [];
  const adapter: FindAdapter = {
    search(query) {
      return new Promise((resolve) => {
        pending.push({ query, resolve: (total) => resolve({ total }) });
      });
    },
    reveal() {},
    clear() {},
  };
  return { adapter, pending };
}

/** Like deferredAdapter, but records every reveal so a test can assert the
    controller accented (and, when asked, scrolled to) the current match. */
function recordingAdapter() {
  const pending: Array<{ query: string; resolve: (total: number) => void }> = [];
  const reveals: Array<{ index: number; scroll: boolean }> = [];
  const adapter: FindAdapter = {
    search(query) {
      return new Promise((resolve) => {
        pending.push({ query, resolve: (total) => resolve({ total }) });
      });
    },
    reveal(index, opts) {
      reveals.push({ index, scroll: opts?.scroll !== false });
    },
    clear() {},
  };
  return { adapter, pending, reveals };
}

// The query deliberately outlives close() (⌘F must survive tab switches), so
// reset it between tests to keep them independent.
afterEach(() => {
  find.close();
  find.query = "";
});

describe("find controller with an async adapter", () => {
  it("applies a search result once it resolves", async () => {
    const { adapter, pending } = deferredAdapter();
    find.register(adapter);
    find.show();
    find.setQuery("cat");

    expect(pending).toHaveLength(1);
    pending[0].resolve(3);
    await Promise.resolve();
    await Promise.resolve();

    expect(find.total).toBe(3);
  });

  it("lets the newest query win even if an older search resolves last", async () => {
    const { adapter, pending } = deferredAdapter();
    find.register(adapter);
    find.show();

    find.setQuery("ca"); // older
    find.setQuery("cats"); // newer, supersedes
    expect(pending.map((p) => p.query)).toEqual(["ca", "cats"]);

    // Resolve the NEWER one first, then the stale older one afterwards.
    pending[1].resolve(4);
    pending[0].resolve(99); // stale — must be ignored
    await Promise.resolve();
    await Promise.resolve();

    expect(find.total).toBe(4);
  });

  it("does not strand the total at zero when a re-registered adapter answers", async () => {
    // Mimics a preview that registers before its content is ready (total 0), then
    // re-registers once loaded and answers with real hits.
    const empty = deferredAdapter();
    find.register(empty.adapter);
    find.show();
    find.setQuery("cat");
    empty.pending[0].resolve(0); // content not ready yet
    await Promise.resolve();
    expect(find.total).toBe(0);

    const ready = deferredAdapter();
    find.register(ready.adapter); // re-registration runs the query again
    expect(ready.pending).toHaveLength(1);
    ready.pending[0].resolve(5);
    await Promise.resolve();
    await Promise.resolve();

    expect(find.total).toBe(5);
  });
});

describe("find controller refresh reveals the current match", () => {
  it("jumps to the first match when a refresh finds hits after content loads", async () => {
    // The PDF/XLSX case: the query is typed before the searchable content exists,
    // so the first search finds nothing. When the content lands, refresh() must
    // count, accent AND scroll to the current match — not just update the counter.
    const { adapter, pending, reveals } = recordingAdapter();
    find.register(adapter);
    find.show();
    find.setQuery("cat");
    pending[0].resolve(0); // content not ready yet
    await Promise.resolve();
    await Promise.resolve();
    expect(reveals).toHaveLength(0); // nothing to reveal at zero hits

    find.refresh(); // content finished loading
    expect(pending).toHaveLength(2);
    pending[1].resolve(3);
    await Promise.resolve();
    await Promise.resolve();

    expect(find.total).toBe(3);
    expect(reveals).toEqual([{ index: 0, scroll: true }]);
  });

  it("re-applies the accent without scrolling on an edit-driven refresh", async () => {
    // Typing in an editor with find open re-searches; the accent must reappear on
    // the current match, but the view must NOT be yanked around under the cursor.
    const { adapter, pending, reveals } = recordingAdapter();
    find.register(adapter);
    find.show();
    find.setQuery("cat");
    pending[0].resolve(3);
    await Promise.resolve();
    await Promise.resolve();
    reveals.length = 0; // drop the initial reveal from the search itself

    find.refresh(false); // the user just typed a character
    pending[1].resolve(3);
    await Promise.resolve();
    await Promise.resolve();

    expect(reveals).toEqual([{ index: 0, scroll: false }]);
  });
});
