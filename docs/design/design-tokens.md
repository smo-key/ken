# Ken — "Paper & Ink" Design Tokens

Distilled from the Claude Design project "Ken: Team Knowledge Manager"
(claude.ai/design/p/051a5dee-d439-477b-9d0a-ea943f484534, files
`Ken Design System.dc.html` and `Ken Prototype v2.dc.html`). The full
prototype markup is saved verbatim at `docs/design/ken-prototype-v2.dc.html`
and is the authoritative visual reference.

**Voice:** calm, editorial, constrained. The interface recedes so knowledge
reads like a well-set document. One accent, warm neutrals, generous
whitespace.

## Copy rule

A screen shows names, values and verbs. Taken from the SR page how-to
(Shattered-Realms-Docs, `Engine/How-tos/UI/Building an SR page.md` §3).
The rule is about visible text. Tooltips are where detail goes (rule 7).

1. **No how-to text.** No sentence on a screen that explains how to use it.
2. **A subtitle holds a count or an id.** Nothing else: never a sentence,
   never a path.
3. **No file paths.** A line that says what an action writes may stay.
4. **An empty list reads "none".** An empty screen gets a short title
   ("No file open"), at most one clause of fact, never a how-to. Use
   `EmptyState` (`src/lib/ui/EmptyState.svelte`); its title is quiet, the
   body face in the muted ink, never louder than the content.
5. **Labels, not sentences.** "Controls", "Recent", "no changes".
6. **A control shows its state itself.** A checkbox, a radio or a badge
   says it; a sentence does not repeat it.
7. **Tooltips may carry detail.** A path, a shortcut, what a count means,
   why a control is disabled, what a hint or icon does. A tooltip never
   repeats the visible label it sits on: "Add it to the decisions log" on
   a button reading "Add to the decisions log" goes. (Chris, 2026-10-15:
   "Tool tips are fine, just content everywhere is not the move.")

Every screen has one title, in `ScreenHeader` (`src/lib/ui/ScreenHeader.svelte`):
the title, an optional count or id, and on the right the screen's actions and
its count ("4 of 4"). No lede under it.

Background work stays out of sight: one short status line, and only where it
blocks.

## Color

### Neutrals — paper
| token | hex | use |
|---|---|---|
| `paper` | `#F6F4EF` | app background |
| `surface` | `#FDFCFA` | cards, inputs, editor canvas |
| `sunken` | `#EFECE4` | title bar, hover fills, wells |
| `sunken-2` | `#FBFAF6` | secondary panels (inbox, drawer) |
| `border` | `#E4E0D7` | hairline borders |
| `border-strong` | `#D8D3C8` | control borders |
| `window` | `#EBE8E0` | outermost window background |

### Neutrals — ink
| token | hex | use |
|---|---|---|
| `ink` | `#211E19` | primary text, "gold standard" doc glyphs |
| `ink-secondary` | `#57524A` | secondary text |
| `ink-tertiary` | `#8B857A` | captions, placeholders |
| `terminal-bg` | `#1C1916` | terminal panels (text `#C9C3B7`, prompt `#B08968`, dim `#6B655C`, warn `#D9A441`) |

### Accent & status
| token | hex | use |
|---|---|---|
| `accent` (clay) | `#8A5A44` | active nav, primary buttons, links, selection. Hover `#7A4E3A`, deep `#6F4534`, border `#6F4534` |
| `needs-input` | `#A8742C` | text pair `#8A5E20` |
| `healthy/done` | `#5A7A5E` | text pair `#47614B` |
| `conflict/error` | `#A34D3F` | |
| highlight (search match) | `rgba(217,164,65,0.25)` | |

Status colors: 10–14% opacity for fills, full strength for text and dots.
Clay marks the active nav item, primary buttons, links, selection — nothing
else. `::selection` is `#E8DCCB`.

### Syntax (the Files code viewer)
`--syntax-keyword` (accent ink), `--syntax-string` (ok), `--syntax-number`
(attention ink), `--syntax-comment` (ink 3), `--syntax-type` (info),
`--syntax-meta` (ink 2), and the danger colour for invalid code. Each is a
token above, so the dark theme brings its own. Colour only: no weight or
slant, which could move the text off the caret.

## Typography
- **Source Serif 4** (400/500/600): document titles and page headers.
  display 32/500, title 22–24/500, page h1 28–30/500, letter-spacing -0.01em.
- **System sans**: all UI. heading 15/600, body 14 (lh 1.6), small 13/13.5,
  caption 12 & 11.5, overline 11–11.5/700 uppercase tracking 0.07–0.08em.
- **IBM Plex Mono** (400/500): paths, commands, terminal, machine-generated
  labels, kbd chips.
- Editor prose: 14.5px / 1.75.

## Spacing, shape, elevation
- Spacing scale: 4 / 8 / 12 / 16 / 24 / 32 / 48.
- Radius: 6–8 controls · 10–12 cards · 14 overlays.
- Shadows: cards `0 1px 3px rgba(33,30,25,.06)`; overlays
  `0 24px 64px rgba(33,30,25,.3)`; drawer (narrow overlay mode)
  `-12px 0 32px rgba(33,30,25,.14)`.

## Core components (see prototype for exact markup)
- **Buttons** h34 (small h28): primary clay; secondary surface+border;
  ghost transparent; destructive `rgba(163,77,63,…)` tinted.
- **Status badge**: pill h24, 6px dot, tinted fill, 12/600 text.
- **Inputs** h36, focus `border-color:#8A5A44` +
  `0 0 0 3px rgba(138,90,68,0.12)`; mono for paths.
- **File glyphs**: dog-eared rectangle (`border-radius:6px 12px 6px 6px`)
  with mono extension label; per-type tints (XLSX green, DOCX blue
  `#3F5E8C`, PDF red, PPTX amber); Ken-maintained structured docs in full
  ink with light text — "the gold standard".
- **Callouts**: tinted card, 8px dot, bold lead sentence, action button
  right.
- **List rows**: glyph + title/caption + trailing badge; selected row
  `rgba(138,90,68,0.06–0.1)`.

## App layout (Prototype v2)

The prototype as drawn. Since `knowledge-core` there is no Map, and the
Timeline is the Log, the project's RAID register
(`docs/knowledge-core/DECISIONS.md`, decision 37).

- Title bar h52 on `sunken`: traffic lights, project switcher pill (with
  sync dot), centered global search (⌘K, "Search — Ken answers questions
  inline…"), Chats toggle button (clay-tinted, unread dot).
- Left nav rail w64: Home ▦ / Files ▤ / Review ☑ (count badge) /
  Ingests ⧉ / Map ⌗ / Timeline ◷, Settings ⚙ pinned bottom. Active =
  `rgba(138,90,68,0.12)` + `#6F4534`. Attention dot 7px clay.
- Chat drawer right: 372px docked ≥1140px window width, else 340px overlay.
  Tabs across top, transcript on `paper`, reply box with "/ for terminal"
  hint, structured questions as stacked option buttons.
- Screens: Home (digest + waiting-on-you), Files (tree 264px + full-bleed
  editor, 720px measure), Review (inbox 272px + detail), Ingests (list
  272px + definition: Sources / Instruction / Output / Rules / Recent
  runs), Map (SVG graph, dashed = mentioned-but-unconnected), Timeline
  (search-filterable event stream, category chips, "View as of…"),
  Settings (MCP server card, Sync & collaboration, Global ingest rules).
- Search overlay: 640px, top-aligned 96px, quick AI answer card first
  (clay-tinted), then FTS matches with highlight, footer kbd hints
  (↵ open · esc close · ⌘↵ continue in chat).
