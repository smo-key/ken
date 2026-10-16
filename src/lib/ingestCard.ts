// Pure helpers for the Ingest screen (the library inbox, white box 8b): page
// names as a row says them, a risk's text, and what a Raw/ row says and offers.
import type { IngestOverview, IngestProposal, RawSource, RawState } from "./api";

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** The line or two ingest wrote for a waiting risk or issue (its entry's
 *  text), read from the proposal; empty for anything else. */
export function raidDetail(payload: string | null): string {
  try {
    const raid = (JSON.parse(payload ?? "{}") as { raid?: { text?: unknown } }).raid;
    return typeof raid?.text === "string" ? raid.text.trim() : "";
  } catch {
    return "";
  }
}

/** A wiki page as a row names it: `Current/Project.md` → "Current › Project". */
export function pageName(path: string): string {
  return path
    .replace(/\.md$/i, "")
    .split("/")
    .filter((p) => p.length > 0)
    .join(" › ");
}

/** A held page edit's title: the page it changes ("Current › Project"),
 *  "Retire …" when it retires the page. Read from the page, so an edit held
 *  before titles named the page ("Held: Current/Project.md from …") reads
 *  the same. */
export function heldTitle(p: Pick<IngestProposal, "title" | "page">): string {
  const name = pageName(p.page);
  return /^(held: )?retire /i.test(p.title) ? `Retire ${name}` : name;
}

/** The one line a proposal's card shows under its title: who made a ruling
 *  or an action, what ingest wrote for a risk or issue; nothing for a held
 *  edit, whose title is its page. */
export function proposalShort(p: Pick<IngestProposal, "kind" | "payload" | "who">): string {
  if (p.kind === "risk" || p.kind === "issue") return raidDetail(p.payload) || (p.who ?? "");
  if (p.kind === "ruling" || p.kind === "ticket") return p.who ?? "";
  return "";
}

/** How a source in Raw/ reads in its row: "Queued", "Transcribing 40%". */
export function rawStatus(r: Pick<RawSource, "state" | "detail">): string {
  switch (r.state) {
    case "queued":
      return "Queued";
    case "transcribing": {
      const m = r.detail?.match(/(\d+)%/);
      return m ? `Transcribing ${m[1]}%` : "Transcribing";
    }
    case "reading":
      return "Reading";
    case "waiting":
      return "Waits for a transcript";
    case "failed":
      return "Couldn't read";
    case "read":
      return "Read";
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
