// Small pure helpers behind the AI assists (digest share, ⌘K quick answer).

/** Whether a ⌘K query deserves a quick answer: ≥3 words or a trailing "?". */
export function isQuestionQuery(query: string): boolean {
  const q = query.trim();
  if (!q) return false;
  if (q.endsWith("?")) return true;
  return q.split(/\s+/).length >= 3;
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
