# Design: inference-routing

## Context

Everything needed is already present and only needs joining up:

- `local_llm::generate_stream(prompt, priority, on_token)` streams tokens,
  and `quick_answer` already forwards each piece to the frontend as a
  `quick-answer-delta` event. The transport for a live reasoning display
  exists.
- `local_llm::generate_json` is the strict path that thinking models break.
- `runner::discover_claude()` + `assistant::oneshot` is the Claude path,
  used by ingests, research and the knowledge model.
- `quick_answer` already contains a local-then-Claude fallback, so both
  engines are reachable from one call site — it is just not a choice.
- The two-priority scheduler (`Interactive` / `Background`) already exists
  and already makes background work yield.

## Goals / Non-Goals

- **Goals**: thinking models work; reasoning is visible where a person is
  watching and skipped where nobody is; the engine is selectable per job
  class with today's behaviour as the default.
- **Non-Goals**: a general provider plugin system; remote providers other
  than the Claude CLI Ken already drives; per-request model switching;
  changing what any job actually does.

## Decisions

### D1. Reasoning is separated at the boundary, once

One function turns raw model output into `(reasoning, answer)`. Every
caller then gets what it wants: `generate_json` parses only the answer,
the streaming path can label spans, and nothing else needs to know a
thinking model exists.

Unrecognised output is all answer. A parser that guesses wrong must lose
the reasoning, never the answer — truncating a good response because a
delimiter moved is far worse than showing a little extra text.

### D2. Thinking is asked for per call site, not configured per model

Extraction over 26,291 files gains nothing from reasoning nobody reads,
and pays for it in time on every file. A quick answer gains a great deal.
Same model, different request — Qwen3 takes the instruction in the prompt.

So `Priority::Background` requests no reasoning and `Interactive` requests
it. That also rescues the Advanced tier: the reason `Qwen3-8B` failed was
not that it is a thinking model, but that it was asked to think in the one
context where the output had to be strict JSON and nobody was watching.

Rejected: a global "thinking on/off" setting. It forces one answer to two
unrelated questions, and the right answer differs per job.

### D3. The stream carries span kinds

`on_token` gains a span kind (`Reasoning` | `Answer`) so the delta event
can say which it is. The frontend renders reasoning quietly and
transiently — present while it arrives, giving way to the answer — which
is the behaviour being asked for, and it is a rendering decision once the
transport carries the distinction.

Rejected: emitting reasoning as a separate event type. It would reorder
against the answer stream and lose the interleaving.

### D4. Engine choice is per job class, with capability as the criterion

Job classes, because per-request choice is noise and one global switch is
too blunt:

| class | default | why |
| --- | --- | --- |
| extraction | Local | mechanical, per-file, tens of thousands of them |
| knowledge model | Claude | reads across a corpus; needs tools |
| quick answers | Auto | local first, Claude when it fails — today's behaviour |
| chat / research / ingests | Claude | exploratory; needs tools |

`Auto` = today. `Local` = never leaves the machine, and is the honest
setting for someone who wants offline or private. `Claude` = chosen when
tool access is the point.

**The UI must explain this by capability.** Claude can open and search the
files; the local model answers from what it is handed. Framing it as
"better vs cheaper" would push people to Claude for bulk extraction, which
is the one place it is clearly the wrong tool.

### D5. Fix the catalogue as well as the parser

Even with D1 and D2, the Advanced blurb ("smarter answers, needs more
memory") does not warn that this entry reasons and the Recommended one
does not. Either say so, or curate a non-thinking build for that tier. A
catalogue that offers a trap is a bug even when the parser survives it.

## Risks / Trade-offs

- **Delimiters vary by model.** Keep the marker configurable per catalogue
  entry rather than hard-coding one family's convention.
- **`Local` can promise privacy Ken must keep.** If a job class is set to
  `Local`, that path must not silently fall back to Claude — the fallback
  is `Auto`'s behaviour and must not leak into `Local`.
- **Two engines, two failure modes.** Errors must say which engine failed
  and what the alternative is, or a broken local model reads as "Ken is
  broken".
