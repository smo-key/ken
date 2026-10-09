/** Every id once: a repeat gets `#2`, `#3`. Two reads can name the same
 *  thing the same way (one finding per broken link on a page, a file of the
 *  same name in two repos), and a list keyed by a repeated id throws and
 *  stops the screen updating. */
export function uniqueIds<T extends { id: string }>(items: T[]): T[] {
  const seen = new Map<string, number>();
  return items.map((it) => {
    const n = (seen.get(it.id) ?? 0) + 1;
    seen.set(it.id, n);
    return n === 1 ? it : { ...it, id: `${it.id}#${n}` };
  });
}
