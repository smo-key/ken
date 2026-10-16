# Templates

What Ken lays down. `wiki/` is the knowledge base: one repo holding the library, the decisions log, the RAID register (`Current/RAID.md`, the risks and issues), the tickets, `log.md` and `.ken/knowledge.json`. `code/CLAUDE.md` is the short pointer Ken proposes for a code repo the knowledge base covers; a person adds it, and Ken never overwrites one.

The method these follow is the Knowledge page of Ways of Working (`docs/manual/docs-system.html`); this folder is the one copy of the templates, and the method links here.

## What Ken Fills

At set-up Ken fills `{{workspace_name}}`, `{{wiki_repo}}`, `{{KEY}}`, the repo table on Start Here, the repos in `.ken/knowledge.json`, and every `updated:` date. The blanks in `Templates/` keep their placeholders: a page is copied from one when something writes it, so no page ships that only holds placeholders.

## Frontmatter

Seven keys: `title`, `aliases`, `status` (`draft` · `current` · `superseded`), `updated`, `sources` (each `repo@sha:path` or `[[Note Name]]`), `checked` (the date and commit the page was last re-read against its sources at, `checked: 2026-10-13 f00218c`, written by the check that read it) and, on a retired page, `superseded_by` (the page that replaces it, a path or a `[[link]]`), plus an optional `reviewed_by`. A Research page is `current` too: its section makes it a record. A retired page is `status: superseded` with `superseded_by:` naming its successor, and a hit on it says so (`doc · superseded · 2026-07-21 · superseded by Design/New.md`); an older page's `replaced_by` reads the same. Tickets keep their own keys (`id`, `status`, `type`, `assignee`).
