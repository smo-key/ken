// Small pure helpers behind the AI assists (digest share, ⌘K quick answer).

/** Whether a ⌘K query deserves a quick answer: ≥3 words or a trailing "?". */
export function isQuestionQuery(query: string): boolean {
  const q = query.trim();
  if (!q) return false;
  if (q.endsWith("?")) return true;
  return q.split(/\s+/).length >= 3;
}

/**
 * While a quick answer streams in, hide the trailing `SOURCES:` line (which the
 * model emits last and which the final `quick-answer` event turns into source
 * chips) — including a partially-typed one — so the card shows only the answer.
 */
export function stripStreamingBody(text: string): string {
  // Cut from the last line that looks like the start of a (possibly partial)
  // "SOURCES:" marker onward. Match a prefix of "SOURCES:" at a line start.
  const lines = text.split("\n");
  while (lines.length > 0) {
    const last = lines[lines.length - 1].trimStart();
    const isSourcesPrefix = "SOURCES:".startsWith(last) || last.startsWith("SOURCES:");
    if (last !== "" && isSourcesPrefix) {
      lines.pop();
    } else {
      break;
    }
  }
  return lines.join("\n").trimEnd();
}

/** The digest as markdown for the share action. */
export function digestMarkdown(digest: {
  date: string;
  body: string;
  sources: string[];
}): string {
  const lines = [`**Ken digest — ${digest.date}**`, "", digest.body];
  if (digest.sources.length > 0) {
    lines.push("", `Sources: ${digest.sources.join(", ")}`);
  }
  return lines.join("\n");
}
