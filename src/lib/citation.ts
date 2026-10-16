// Source citations in chat replies: a link to a file, maybe at a line or a
// heading, that opens the file there when clicked. Pure, so it can be tested
// without a DOM.

/** Where a citation points. `projectId` is set for a `ken://` address, which
 *  may name a workspace member other than the focused one. */
export interface Citation {
  projectId: string | null;
  path: string;
  line?: number;
  anchor?: string;
}

/** Parse a link a reply cites. Accepts `path`, `path#L12`, `path#L12-L20`,
 *  `path:12`, `path#heading-slug`, and `ken://<project-id>/path` with the same
 *  suffixes. `null` for a web link or anything else that is not a file. */
export function parseCitation(href: string): Citation | null {
  let raw = href.trim();
  if (!raw) return null;
  let projectId: string | null = null;
  if (raw.startsWith("ken://")) {
    const rest = raw.slice("ken://".length);
    const slash = rest.indexOf("/");
    if (slash <= 0 || slash === rest.length - 1) return null;
    projectId = rest.slice(0, slash);
    raw = rest.slice(slash + 1);
  } else if (/^[a-z][a-z0-9+.-]*:/i.test(raw) || raw.startsWith("//") || raw.startsWith("#")) {
    return null;
  }
  let path = raw;
  let line: number | undefined;
  let anchor: string | undefined;
  const hash = path.indexOf("#");
  if (hash >= 0) {
    const frag = decodeURIComponent(path.slice(hash + 1));
    path = path.slice(0, hash);
    const l = /^L(\d+)(?:-L?\d+)?$/i.exec(frag);
    if (l) line = Number(l[1]);
    else if (frag) anchor = frag;
  }
  const colon = /^(.*\S):(\d+)(?::\d+)?$/.exec(path);
  if (colon && line === undefined) {
    path = colon[1];
    line = Number(colon[2]);
  }
  path = decodeURIComponent(path).replace(/^\.\//, "").replace(/\\/g, "/");
  if (!path) return null;
  return { projectId, path, line, anchor };
}

/** In a Markdown file, what to scroll to for a source line: the line's own
 *  heading when it is one, else a phrase from the line to find in the
 *  rendered page, and the nearest heading above as the fallback. */
export function lineTarget(markdown: string, line: number): { heading?: string; phrase?: string } {
  const lines = markdown.split("\n");
  const at = Math.min(Math.max(line, 1), lines.length) - 1;
  const heading = (l: string) => /^\s{0,3}#{1,6}\s+(.+?)\s*#*\s*$/.exec(l)?.[1];
  const own = heading(lines[at] ?? "");
  if (own) return { heading: own };
  let above: string | undefined;
  for (let i = at - 1; i >= 0; i--) {
    const h = heading(lines[i]);
    if (h) {
      above = h;
      break;
    }
  }
  const plain = (lines[at] ?? "")
    .replace(/^\s*(?:[-*+]|\d+[.)]|>)\s+/, "")
    .replace(/[`*_~[\]]/g, "")
    .replace(/\(([^)]*)\)/g, "")
    .trim();
  const phrase = plain.length >= 4 ? plain.slice(0, 60) : undefined;
  return { heading: above, phrase };
}

/** A relative link whose first folder is another workspace repo's name
 *  (`Project Documents/SOW.md`, written from a chat started in the wiki)
 *  points into that repo; a project-relative path would look for it in the
 *  focused one. Longest name first, so a `Group/Child` member wins over its
 *  group. A `ken://` citation, or a path naming no other repo, is unchanged. */
export function inMember(
  c: Citation,
  members: { id: string | null; name: string }[],
  focused: string | null,
): Citation {
  if (c.projectId) return c;
  const lower = c.path.toLowerCase();
  const named = members
    .filter((m) => m.id && m.id !== focused)
    .sort((a, b) => b.name.length - a.name.length)
    .find((m) => lower.startsWith(m.name.toLowerCase() + "/"));
  return named ? { ...c, projectId: named.id, path: c.path.slice(named.name.length + 1) } : c;
}
