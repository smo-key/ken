// Picking chat messages to send to Ingest: only the person's and Ken's
// turns, a click to take or drop one, a shift-click for every message from
// the last one clicked. Pure, so it tests on its own.

/** The messages of a chat Ingest can take: saved user and assistant turns
 *  (a message still being sent has a negative id). */
export function sendableMessages<T extends { id: number; role: string }>(messages: readonly T[]): T[] {
  return messages.filter((m) => (m.role === "user" || m.role === "assistant") && m.id > 0);
}

export interface Pick {
  selected: Set<number>;
  /** The last message clicked: where a shift-click range starts. */
  anchor: number | null;
}

/** A click on message `id`. Plain: take it, or drop it when taken.
 *  Shift: take every message from the anchor to it, in `order`. */
export function pickMessage(order: readonly number[], current: Pick, id: number, shift: boolean): Pick {
  const selected = new Set(current.selected);
  const from = current.anchor === null ? -1 : order.indexOf(current.anchor);
  const to = order.indexOf(id);
  if (shift && from !== -1 && to !== -1) {
    const [a, b] = from <= to ? [from, to] : [to, from];
    for (const m of order.slice(a, b + 1)) selected.add(m);
    return { selected, anchor: id };
  }
  if (selected.has(id)) selected.delete(id);
  else selected.add(id);
  return { selected, anchor: id };
}

/** The picked ids in chat order, for `ingest_add_chat`. */
export function pickedIds(order: readonly number[], selected: ReadonlySet<number>): number[] {
  return order.filter((id) => selected.has(id));
}
