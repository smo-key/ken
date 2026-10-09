// Pure helpers for the Ingest screen (the library inbox, white box 8b): the
// one-line count of what a source wrote and what waits, page names as a row
// says them, staging's line, and what a Raw/ row says and offers.
import type { IngestOverview, IngestWriteKind, RawState } from "./api";

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** "2 pages, 1 escalation written, 1 on my day · 3 wait" — what was written
 *  from a source and what waits, in one line. */
export function writtenLine(written: IngestWriteKind[], waiting: number): string {
  const count = (...kinds: IngestWriteKind[]) => written.filter((k) => kinds.includes(k)).length;
  const parts: string[] = [];
  const pages = count("edit", "page");
  const ideas = count("idea");
  const escalations = count("escalation");
  const tasks = count("task");
  if (pages > 0) parts.push(plural(pages, "page"));
  if (ideas > 0) parts.push(plural(ideas, "idea"));
  if (escalations > 0) parts.push(plural(escalations, "escalation"));
  let line = parts.length > 0 ? `${parts.join(", ")} written` : "";
  if (tasks > 0) line = line ? `${line}, ${tasks} on my day` : `${tasks} on my day`;
  if (!line) line = "nothing written";
  if (waiting > 0) line += ` · ${waiting} ${waiting === 1 ? "waits" : "wait"}`;
  return line;
}

/** A wiki page as a row names it: `Current/Project.md` → "Current › Project". */
export function pageName(path: string): string {
  return path
    .replace(/\.md$/i, "")
    .split("/")
    .filter((p) => p.length > 0)
    .join(" › ");
}

/** The line under the page edits: staging held none, or how many. */
export function stagingLine(held: number): string {
  return held === 0 ? "Every page edit went through staging; none was held." : `${held} held, below.`;
}

/** How a source in Raw/ reads in its row. */
export function rawStateLabel(state: RawState): string {
  switch (state) {
    case "queued":
      return "in the queue";
    case "transcribing":
      return "transcribing";
    case "waiting":
      return "waiting for a transcript";
    case "reading":
      return "reading";
    case "read":
      return "read";
    case "failed":
      return "could not read";
  }
}

/** What a Raw/ row offers: Try again after a failure or a wait, Remove
 *  (to the system trash) whenever Ken is not on it. A read source has its
 *  card instead. */
export function rawActions(state: RawState): ("retry" | "remove")[] {
  switch (state) {
    case "failed":
    case "waiting":
      return ["retry", "remove"];
    case "queued":
      return ["remove"];
    default:
      return [];
  }
}

/** Read now: something is queued and no pass is running. */
export function canReadNow(overview: Pick<IngestOverview, "raw" | "running"> | null): boolean {
  return !!overview && !overview.running && overview.raw.some((r) => r.state === "queued");
}

/** A pass is on, or something in Raw/ is moving: the screen keeps reading. */
export function ingestBusy(overview: Pick<IngestOverview, "raw" | "running"> | null): boolean {
  return (
    !!overview &&
    (overview.running || overview.raw.some((r) => r.state === "queued" || r.state === "reading" || r.state === "transcribing"))
  );
}
