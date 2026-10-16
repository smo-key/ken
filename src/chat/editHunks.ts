// A proposed edit as a git-style list of changes, each accepted or declined
// on its own, and the file text that results. Pure, so it can be tested
// without a DOM.
import { diffLines } from "diff";

/** One change: lines taken out and lines put in, with a little of the
 *  unchanged text around it and where it starts in the old file. */
export interface Hunk {
  id: number;
  /** 1-based line in the old file where the change starts. */
  oldStart: number;
  /** 1-based line in the new file where the change starts. */
  newStart: number;
  removed: string[];
  added: string[];
  before: string[];
  after: string[];
}

type Part = { value: string; added?: boolean; removed?: boolean };

function lines(value: string): string[] {
  const out = value.split("\n");
  if (out.length > 1 && out[out.length - 1] === "") out.pop();
  return out;
}

/** The parts of the diff, unchanged runs and change runs alternating: a
 *  change run is every removed and added part between two unchanged ones. */
function runs(base: string, proposed: string): ({ same: string } | { removed: string; added: string })[] {
  const out: ({ same: string } | { removed: string; added: string })[] = [];
  for (const p of diffLines(base, proposed) as Part[]) {
    const last = out[out.length - 1];
    if (!p.added && !p.removed) {
      out.push({ same: p.value });
    } else if (last && !("same" in last)) {
      if (p.added) last.added += p.value;
      else last.removed += p.value;
    } else {
      out.push({ removed: p.removed ? p.value : "", added: p.added ? p.value : "" });
    }
  }
  return out;
}

/** The changes of an edit, in file order, with `context` unchanged lines each side. */
export function hunksOf(base: string, proposed: string, context = 3): Hunk[] {
  const parts = runs(base, proposed);
  const hunks: Hunk[] = [];
  let oldLine = 1;
  let newLine = 1;
  parts.forEach((part, i) => {
    if ("same" in part) {
      const n = lines(part.same).length;
      oldLine += n;
      newLine += n;
      return;
    }
    const prev = parts[i - 1];
    const next = parts[i + 1];
    const removed = part.removed ? lines(part.removed) : [];
    const added = part.added ? lines(part.added) : [];
    hunks.push({
      id: hunks.length,
      oldStart: oldLine,
      newStart: newLine,
      removed,
      added,
      before: prev && "same" in prev ? lines(prev.same).slice(-context) : [],
      after: next && "same" in next ? lines(next.same).slice(0, context) : [],
    });
    oldLine += removed.length;
    newLine += added.length;
  });
  return hunks;
}

/** The file with only the accepted changes: `accepted[i]` for hunk i. */
export function applyChoices(base: string, proposed: string, accepted: boolean[]): string {
  let i = 0;
  let out = "";
  for (const part of runs(base, proposed)) {
    if ("same" in part) {
      out += part.same;
    } else {
      out += accepted[i] ? part.added : part.removed;
      i++;
    }
  }
  return out;
}

/** A one-line description of a change, for telling Claude what was declined. */
export function describeHunk(h: Hunk): string {
  const first = (h.removed[0] ?? h.added[0] ?? "").trim();
  const quote = first.length > 70 ? first.slice(0, 70) + "…" : first;
  const what =
    h.removed.length === 0 ? `adding ${h.added.length} line(s)` : h.added.length === 0 ? `removing ${h.removed.length} line(s)` : `rewriting ${h.removed.length} line(s)`;
  return `at line ${h.oldStart}, ${what}: "${quote}"`;
}
