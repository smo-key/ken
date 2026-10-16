/**
 * Links in the Markdown editor that Milkdown leaves inert: `[[wiki links]]`
 * (text to it), ordinary links to other pages or the web, and a repo's name
 * in code (`AUR-Team`) in a page that lists the team's repos. Each is shown
 * as a link and a click follows it. Clicks are taken on the DOM event, as
 * the heading links are, before the editor places its cursor.
 */
import { Plugin, PluginKey } from "@milkdown/kit/prose/state";
import { Decoration, DecorationSet } from "@milkdown/kit/prose/view";
import { $prose } from "@milkdown/kit/utils";

const LINK = /\[\[([^\]\n]+?)\]\]/g;

/** The page a link names: `[[Name]]`, `[[Name|label]]`, `[[Name#Heading]]`
 *  or `[[Folder/Name]]`. A bare name is the page of that file name, in the
 *  linking page's folder first, then the shortest path; a path is matched
 *  from the repo's root, then from the linking page's folder. */
export function resolveWikiLink(target: string, fromPath: string, files: string[]): string | null {
  const name = target.split("|")[0].split("#")[0].trim().replace(/\\/g, "/");
  if (!name) return null;
  const pages = files.filter((f) => /\.(md|markdown)$/i.test(f));
  const lower = (s: string) => s.toLowerCase();
  const noExt = (s: string) => s.replace(/\.(md|markdown)$/i, "");
  const dir = fromPath.includes("/") ? fromPath.slice(0, fromPath.lastIndexOf("/")) : "";
  if (name.includes("/")) {
    const want = lower(noExt(name)).replace(/^\.?\//, "");
    const fromDir = lower(dir ? `${dir}/${want}` : want);
    return (
      pages.find((p) => lower(noExt(p)) === want) ??
      pages.find((p) => lower(noExt(p)) === fromDir) ??
      pages.find((p) => lower(noExt(p)).endsWith(`/${want}`)) ??
      null
    );
  }
  const want = lower(noExt(name));
  const same = pages.filter((p) => lower(noExt(p.split("/").pop() ?? p)) === want);
  if (same.length === 0) return null;
  return (
    same.find((p) => lower(p.slice(0, Math.max(0, p.lastIndexOf("/")))) === lower(dir)) ??
    [...same].sort((a, b) => a.split("/").length - b.split("/").length || a.length - b.length)[0]
  );
}

/** A relative link (`../Current/Team.md#roles`, `Team.md`) as a path in
 *  the repo, from the linking page's folder; null for a web or mail link. */
export function resolveRelativeHref(href: string, fromPath: string): string | null {
  if (/^[a-z][a-z0-9+.-]*:/i.test(href)) return null;
  const path = decodeURIComponent(href.split("#")[0].split("?")[0]).replace(/\\/g, "/");
  if (!path) return null;
  const parts = path.startsWith("/") ? [] : fromPath.split("/").slice(0, -1);
  for (const p of path.replace(/^\//, "").split("/")) {
    if (p === "" || p === ".") continue;
    if (p === "..") parts.pop();
    else parts.push(p);
  }
  return parts.join("/") || null;
}

/** The link at offset `at` of `text`, if one covers it. */
export function linkAt(text: string, at: number): string | null {
  for (const m of text.matchAll(LINK)) {
    const start = m.index ?? 0;
    if (at >= start && at <= start + m[0].length) return m[1];
  }
  return null;
}

export type LinkClick =
  | { kind: "wiki"; target: string }
  | { kind: "href"; href: string }
  | { kind: "repo"; name: string };

const key = new PluginKey("ken-wiki-links");

/** The plugin. `open` follows a link and says whether it did; `isRepo`
 *  says whether a name in code is one of the workspace's repos. */
export function wikiLinkPlugin(open: (link: LinkClick) => boolean, isRepo: (name: string) => boolean) {
  return $prose(
    () =>
      new Plugin({
        key,
        props: {
          decorations(state) {
            const decos: Decoration[] = [];
            state.doc.descendants((node, pos) => {
              if (!node.isText || !node.text) return;
              for (const m of node.text.matchAll(LINK)) {
                const from = pos + (m.index ?? 0);
                decos.push(Decoration.inline(from, from + m[0].length, { class: "ken-wikilink", "data-target": m[1] }));
              }
              const inCode = node.marks.some((mk) => /code/i.test(mk.type.name));
              if (inCode && isRepo(node.text.trim())) {
                decos.push(Decoration.inline(pos, pos + node.text.length, { class: "ken-repolink", "data-repo": node.text.trim() }));
              }
            });
            return DecorationSet.create(state.doc, decos);
          },
          handleDOMEvents: {
            click: (view, event) => {
              if (event.button !== 0) return false;
              const el = event.target as HTMLElement | null;
              let link: LinkClick | null = null;
              const wiki = el?.closest?.(".ken-wikilink") as HTMLElement | null;
              const repo = el?.closest?.(".ken-repolink") as HTMLElement | null;
              const anchor = el?.closest?.("a") as HTMLAnchorElement | null;
              const href = anchor?.getAttribute("href") ?? "";
              if (wiki?.dataset.target) link = { kind: "wiki", target: wiki.dataset.target };
              else if (repo?.dataset.repo) link = { kind: "repo", name: repo.dataset.repo };
              else if (anchor && href && !href.startsWith("#")) link = { kind: "href", href };
              else {
                // The text under the pointer, should the decoration's own
                // element not be the click's target.
                const at = view.posAtCoords({ left: event.clientX, top: event.clientY });
                if (at) {
                  const $pos = view.state.doc.resolve(at.pos);
                  const node = $pos.parent.maybeChild($pos.index());
                  const target = node?.isText && node.text ? linkAt(node.text, $pos.textOffset) : null;
                  if (target) link = { kind: "wiki", target };
                }
              }
              if (!link || !open(link)) return false;
              event.preventDefault();
              return true;
            },
          },
        },
      }),
  );
}
