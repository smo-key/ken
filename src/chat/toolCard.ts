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
