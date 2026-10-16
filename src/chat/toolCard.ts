import type { ToolCard } from "../lib/api";

/** A `tool` message's content as a card, or null when it isn't one. A card
 *  still running when its chat is no longer working never got its result
 *  (the session ended first), so it shows as done rather than spinning. */
export function parseToolCard(content: string, chatWorking = true): ToolCard | null {
  try {
    const c = JSON.parse(content) as Partial<ToolCard>;
    if (typeof c.summary !== "string" || typeof c.name !== "string") return null;
    const status = c.status === "error" || c.status === "done" ? c.status : "running";
    return {
      toolUseId: typeof c.toolUseId === "string" ? c.toolUseId : "",
      name: c.name,
      summary: c.summary,
      status: status === "running" && !chatWorking ? "done" : status,
      result: typeof c.result === "string" ? c.result : undefined,
    };
  } catch {
    return null;
  }
}

/** One hit a search card lists: where it is and, for code, the symbol it
 *  holds. */
export interface CardHit {
  place: string;
  symbol: string | null;
}

/** The hits in the result of Ken's `search` tool, from lines like
 *  `1. [code · service · 2026-10-01] repo:src/a.ts:12 `saveWorld` (ken://…)`:
 *  the place a person cites and, for a code hit, its symbol (knowledge-core
 *  decision 26). The `next:` line under a code hit and the other tools'
 *  results (`read`, `code_imports`, `related`) hold no hits. Empty for any
 *  other result. */
export function cardHits(result: string | undefined): CardHit[] {
  if (!result) return [];
  const out: CardHit[] = [];
  for (const line of result.split(/\r?\n/)) {
    const m = /^\d+\. (?:\[[^\]]*\] )?(.+?)(?: `([^`]+)`)? \(ken:\/\//.exec(line);
    if (m) out.push({ place: m[1], symbol: m[2] ?? null });
  }
  return out;
}
