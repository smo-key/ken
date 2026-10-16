# Decisions

Every ruling, in the words it was made in, added and never edited. A reversed ruling stays, marked `[SUPERSEDED BY D-nnn]`, and a narrowed one `[REFINED BY D-nnn]`; never renumber, because commits, tickets and code cite the numbers.

A ruling found in an ingested note waits on its card as a ready entry, and one click adds it here with the next number.

Entry format:

```
## D-001 · 2026-01-01 · topic

**The ruling, in one sentence.**

> "The words it was made in." (who made it, if known)

Why: the reason, in one line.
sources: [[Note Name]], repo:path:line
aliases: the other terms anyone might search for it by
supersedes: D-nnn, when it replaces an earlier ruling
refines: D-nnn, when it narrows one
```
