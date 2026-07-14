/**
 * Pure date-partition logic for the Timeline screen.
 *
 * "Today" is passed in as a yyyy-mm-dd string so callers stay deterministic and
 * testable — the real clock lives only in the Svelte component.
 */

/**
 * Where an event's date sits relative to today.
 * `present` also covers ambiguous partial dates (e.g. `2026`, `2026-07`) whose
 * known parts match today: we can't prove they are strictly future, so they
 * stay visible instead of being collapsed away.
 */
export type TimelinePlacement = "future" | "present" | "past" | "undated";

/** Parse a best-effort yyyy-mm-dd into numeric parts, or null if it has no year. */
function dateParts(date: string): number[] | null {
  // A year is the minimum we need to place an event; keep leading numeric
  // components and stop at the first blank/non-numeric one. Note Number("")
  // is 0, so blank components must be rejected explicitly.
  const clean: number[] = [];
  for (const raw of date.split("-")) {
    const n = Number(raw);
    if (raw.trim() === "" || !Number.isFinite(n)) break;
    clean.push(n);
  }
  return clean.length > 0 ? clean : null;
}

export function classifyDate(date: string, today: string): TimelinePlacement {
  const parts = dateParts(date);
  if (parts === null) return "undated";
  const todayParts = dateParts(today) ?? [];
  // Compare only the parts the event actually specifies. A direction is proven
  // only when a known component differs; equal-through-known stays "present".
  for (let i = 0; i < parts.length; i++) {
    const t = todayParts[i] ?? 0;
    if (parts[i] > t) return "future";
    if (parts[i] < t) return "past";
  }
  return "present";
}

export interface TimelineGroups<T> {
  /** Provably after today; collapsed by default in the UI. */
  future: T[];
  /** Past + today + ambiguous partials, in input order; always shown. */
  visible: T[];
  /** No parseable date; shown at the far (oldest) end, never collapsed. */
  undated: T[];
  /** True when at least one event has a usable date (drives the today marker). */
  hasDated: boolean;
}

/**
 * Partition already-sorted events (the store delivers them newest-first) into
 * render groups, preserving input order within each group.
 */
export function partitionTimeline<T extends { date: string }>(
  events: readonly T[],
  today: string,
): TimelineGroups<T> {
  const future: T[] = [];
  const visible: T[] = [];
  const undated: T[] = [];
  for (const ev of events) {
    const placement = classifyDate(ev.date, today);
    if (placement === "future") future.push(ev);
    else if (placement === "undated") undated.push(ev);
    else visible.push(ev);
  }
  return {
    future,
    visible,
    undated,
    hasDated: future.length > 0 || visible.length > 0,
  };
}
