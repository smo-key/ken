// Pure path logic for ingest folder pickers. The native dialog hands back an
// ABSOLUTE path; recipes store PROJECT-RELATIVE paths — so every chosen folder
// must be proven inside the project and rewritten relative to its root. Kept
// dependency-free (no Node `path`) so it runs in the browser bundle and is
// trivially testable.

export type RelResult = { ok: true; rel: string } | { ok: false; error: string };

// Treat both separators as equivalent and drop trailing slashes so root and
// chosen path compare on the same footing regardless of OS.
function normalize(p: string): string {
  return p.replace(/\\/g, "/").replace(/\/+$/, "");
}

/**
 * Rewrite an absolute folder path as a path relative to the project root,
 * rejecting anything that isn't strictly inside (or equal to) the root.
 * The root itself yields an empty relative path.
 */
export function toProjectRelative(root: string, abs: string): RelResult {
  const r = normalize(root);
  const a = normalize(abs);
  if (a === r) return { ok: true, rel: "" };
  // Boundary check with the separator prevents "/p" matching a sibling "/p-2".
  if (a.startsWith(r + "/")) return { ok: true, rel: a.slice(r.length + 1) };
  return { ok: false, error: "Choose a folder inside this project." };
}

/**
 * Compose a single-mode output path from a chosen folder (already relative) and
 * a bare document name. A folder dialog can't name a file, so the name is typed
 * separately and must not smuggle in path segments.
 */
export function composeSingleOutput(relFolder: string, name: string): RelResult {
  const n = name.trim();
  if (!n) return { ok: false, error: "Enter a document name." };
  if (/[/\\]/.test(n))
    return { ok: false, error: "Use a document name without folders." };
  const named = n.includes(".") ? n : `${n}.md`;
  const folder = normalize(relFolder);
  return { ok: true, rel: folder ? `${folder}/${named}` : named };
}
