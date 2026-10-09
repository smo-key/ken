---
id: {{KEY}}-001      # project key + number. The filename is this id.
kind: ticket          # document kind
status: todo          # todo | in-progress | blocked | in-review | testing | done | cancelled
type: {{type}}        # work type: bug | feature | design | spike | process — sets the human touchpoints
size: {{size}}        # XS | S | M | L | XL | design | spike | audit — proposed by the signals, confirmed at G0
scope:                # the declared write surface — G8, the Drift check, checks it against the diff
  - {{path/to/area}}
verify:               # acceptance as runnable commands — G2 and close-out read these
  - {{command}}
assignee: {{person}}  # or empty: unclaimed
workflow:             # overrides only; everything else comes from the preset the size selects
  # build: { effort: high, isolation: worktree }
# ---- below here is this team's overlay, not the method's ----
---

## What and Why

One paragraph. Written so somebody with no memory of the conversation can act
on it — which, on a team with agents, is literally who picks it up.

## Evidence

The log line, the reproduction, the measurement.

## What Not to Do

The wrong turns already taken and why they failed. Usually the most valuable
paragraph in the ticket, because wrong turns are what gets repeated.

## How You Would Know It Worked

The observable success condition, named before the work starts.

## thread

<!-- Append only. Nothing is edited or removed; a correction is a later entry.
     Three kinds, and you may add a fourth but not remove these:

       machine  2026-01-01 14:31  gate    G2 fast suite — exit 1, 3 failing — runs/2026-01-01/g2-1431.log
       status   2026-01-01 15:22  status  in-progress -> in-review — G2 green
       human    2026-01-01 15:40  {{handle}}  the second failure was already failing before the lane started — its own ticket, {{KEY}}-002
       machine  2026-01-01 16:02  escalate  which wins, the config file or the flag? — to {{handle}}
       human    2026-01-01 16:20  {{handle}}  answered: the flag wins — into the context pack

     Machine entries carry facts, never verdicts — an exit code, a count, a path.
     An answered escalation is not done until the answer is in the context pack,
     or in the decisions log if it is a ruling; the Answer drift check reads
     these entries at close.
     Every lane writes `started` on the way in and one terminal entry on the way
     out, so a `started` with nothing after it is an interrupted lane: resume it,
     keep the worktree, and re-run its gate rather than inheriting the old green. -->
