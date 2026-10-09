# Proposal: inference-routing

## Why

Ken already has two engines and uses both, but which one runs a given job
is an accident of where the code was written rather than a decision anyone
made or can change:

| job | engine today | why |
| --- | --- | --- |
| per-file extraction | local model | `extraction_worker` calls `generate_json` |
| knowledge model | Claude Code CLI | `build_knowledge_model(&binary, …)` |
| quick answers | local, **falling back** to Claude on error | `quick_answer` |
| deep research, ingests | Claude Code CLI | `assistant::oneshot` |

That split is broadly right, and for a reason worth stating out loud:
**Claude has tool access and the local model does not.** Claude Code can
open files, grep, and follow a thread across a repo; the local model only
ever sees the prompt it is handed. So the real axis is not "better vs
cheaper", it is "can go and look" vs "answers about what it was given".

Two things follow, and neither is available today.

**Thinking models are rejected rather than supported.** The Advanced
Language entry is plain `Qwen3-8B`, a hybrid reasoning model that emits a
reasoning block before answering. `generate_json` cannot parse that, so
choosing the tier Ken itself labels "smarter answers" produces
`no JSON object found in the model output` on nearly every file — measured
at **47 errors to 1 success** on a real corpus. The reasoning is not
noise to be tolerated; for interactive work it is the most interesting
thing on screen, and Ken already streams tokens to the UI
(`quick-answer-delta`) so it could show it.

**The engine is not a choice.** A user who wants Claude to answer, or who
wants everything to stay local and offline, cannot say so. Claude appears
only as a silent fallback when the local model errors.

## What Changes

- **Reasoning blocks are understood, not fatal.** A leading reasoning span
  is separated from the answer before parsing. `generate_json` stops
  failing on thinking models, and the reasoning becomes available to the
  caller rather than being discarded at the boundary.
- **Thinking is a per-call-site decision.** Background jobs ask for no
  reasoning: over 26,291 queued files nobody reads it and it is pure time.
  Interactive jobs ask for it and show it. The same model can do both —
  Qwen3 takes the instruction in the prompt — so this is a property of the
  job, not of the model, and it is what makes the Advanced tier usable.
- **Reasoning streams as reasoning.** The existing token stream gains a
  span kind, so the frontend can render the thinking in a quieter
  treatment that scrolls as it arrives and gives way to the answer, rather
  than the two being one undifferentiated string.
- **Engine becomes an explicit choice per job class**, defaulting to
  today's behaviour: local for bulk mechanical work, Claude for work that
  needs to explore. `Auto` stays the default and keeps the existing
  fall-back-on-error path; `Local` never leaves the machine; `Claude` is
  chosen when tool access is the point.
- **The choice is explained by capability, not by quality.** The UI says
  Claude can open and search your files while the local model answers from
  what it is given, because that is the difference that decides it.

## Capabilities

### New Capabilities
- `inference-routing`: reasoning-aware parsing and streaming, per-call-site
  thinking, and the per-job-class engine choice.

### Modified Capabilities
- none. Both engines and the token stream already exist; this makes the
  routing between them explicit and the reasoning legible.

## Impact

- `crates/ken-core`: `local_llm` gains reasoning-aware parsing and a
  thinking flag on its generate calls; a small engine-selection type;
  `model.rs` catalogue blurbs corrected for the Advanced entry.
- `src-tauri`: the delta event gains a span kind; `quick_answer` and the
  extraction/knowledge paths consult the selection instead of hard-coding
  an engine.
- Frontend: reasoning rendered distinctly and transiently; an engine
  setting per job class.
- Flags: no new flag. The engines are already both wired.
- Tests: output wrapped in a reasoning block parses; a background call
  requests no reasoning; `Local` never invokes the Claude binary; `Claude`
  never loads the local model; `Auto` reproduces today's behaviour exactly.

## Risks

- **Reasoning is model-specific in form.** Parsing must key on a
  configurable delimiter, and must degrade to "treat it all as answer"
  rather than truncating output it does not recognise.
- **Showing thinking invites reading it.** It should be transient by
  default and never mistaken for the answer, or it becomes the thing users
  quote back.
