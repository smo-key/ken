# Tasks: inference-routing

Task 1 is the bug fix and unblocks the Advanced tier on its own. Task 2
makes thinking cheap where nobody reads it. Tasks 3-4 are the feature.

## 1. ken-core — reasoning-aware parsing

- [ ] 1.1 One function: raw output -> `(reasoning, answer)`, with the
  delimiter configurable rather than hard-coded to one family's markers.
- [ ] 1.2 Unrecognised output is **all answer**. Never truncate a response
  because a delimiter moved; losing the reasoning is acceptable, losing
  the answer is not.
- [ ] 1.3 `generate_json` parses the answer span only. This alone turns
  the measured 47-errors-to-1-success on `Qwen3-8B` into a working path.
- [ ] 1.4 Tests: output with a reasoning block parses; output without one
  is unchanged; an unterminated reasoning block still yields the answer;
  a delimiter appearing inside a JSON string is not treated as a marker.

## 2. ken-core — thinking per call site

- [ ] 2.1 Generate calls take "reasoning wanted" as a parameter.
  `Priority::Background` requests none; `Interactive` requests it.
- [ ] 2.2 Extraction therefore asks for no reasoning — over 26,291 queued
  files that is the difference between a long job and an unusable one.
- [ ] 2.3 Confirm the request actually suppresses reasoning for the
  catalogue's models rather than only hiding it, and record what was
  observed. Suppressing the tokens saves the time; hiding them does not.
- [ ] 2.4 Tests: a background call requests no reasoning; an interactive
  call does; both parse.

## 3. Streaming spans

- [ ] 3.1 `on_token` gains a span kind (`Reasoning` | `Answer`).
- [ ] 3.2 The delta event carries the kind. Reasoning and answer must stay
  in arrival order — one stream, labelled, not two streams.
- [ ] 3.3 Frontend renders reasoning quietly and transiently: visible
  while it arrives, giving way to the answer, never mistakable for it.
- [ ] 3.4 Tests: spans arrive in order; a stream with no reasoning renders
  exactly as today.

## 4. Engine choice per job class

- [ ] 4.1 An `Engine` selection (`Auto` | `Local` | `Claude`) per job
  class: extraction, knowledge model, quick answers, chat/research/ingests.
  Persisted alongside the model selection, which is already machine-wide.
- [ ] 4.2 Defaults reproduce today exactly: extraction Local, knowledge
  model Claude, quick answers Auto, chat/research/ingests Claude.
- [ ] 4.3 **`Local` must never fall back to Claude.** The fallback is
  `Auto`'s behaviour. If someone selects Local for privacy or offline use,
  a silent hop to Claude breaks a promise Ken made.
- [ ] 4.4 `Claude` must not load the local model at all — no VRAM, no
  warm-up.
- [ ] 4.5 Errors name the engine that failed and what the alternative is.
- [ ] 4.6 Tests: each setting routes where it says; Local never invokes the
  binary; Claude never loads a GGUF; Auto reproduces the current
  local-then-Claude path including its error fallback.

## 5. Catalogue honesty

- [ ] 5.1 The Advanced Language blurb must say that entry reasons and the
  Recommended one answers directly — or curate a non-thinking build for
  that tier. Offering a trap is a bug even once the parser survives it.
- [ ] 5.2 Explain the engine choice by **capability**: Claude can open and
  search your files; the local model answers from what it is given. Not
  "better vs cheaper", which would push people to Claude for bulk
  extraction — the one place it is clearly wrong.

## 6. Verification

- [ ] 6.1 With the Advanced model selected, extraction completes at a
  normal success rate. Record the before and after: 47 errors to 1 success
  is the number this change has to beat.
- [ ] 6.2 A quick answer shows reasoning arriving, then the answer, and the
  reasoning does not persist as part of it.
- [ ] 6.3 With every class set to Local, no Claude process is spawned for
  a full extraction and answer cycle.
- [ ] 6.4 With defaults unchanged, behaviour is indistinguishable from
  before this change.
