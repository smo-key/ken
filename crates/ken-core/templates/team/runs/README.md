# runs/

Run records: which gate ran, when, with what exit code, and the path to its log.

This is the only data that did not exist before the tool. Everything else in
this repository is readable and editable without it.

Each record carries what a resume needs: the runner, its session handle, the
ticket id, the step, and the worktree. Recording *which* runner rather than
assuming one is what lets another kind exist later.

A lane writes `started` on the way in and exactly one terminal entry — `pass`,
`fail`, `escalate`, `re-triage` — on the way out. A `started` with nothing after it
and no live session behind it is an **interrupted** lane: resume it rather than
restart it, keep its worktree, and re-run its gate. A resumed lane never
inherits the green from the run that died.
