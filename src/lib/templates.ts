// Bundled ingest templates. "Use template" copies one into the project as an
// ordinary recipe — no ongoing link.
import type { IngestMode, IngestRefresh } from "./api";

export interface IngestTemplate {
  id: string;
  name: string;
  description: string;
  output: string;
  mode: IngestMode;
  refresh: IngestRefresh;
  instruction: string;
}

export const TEMPLATES: IngestTemplate[] = [
  {
    id: "people",
    name: "People",
    description: "A directory of every person in the project's knowledge",
    output: "knowledge/People.md",
    mode: "single",
    refresh: "on-change",
    instruction:
      "Extract every person mentioned across the sources. For each person record: name, role or title, team or organization, what they own or are responsible for, and which documents they appear in. Merge obvious duplicates (nicknames, initials); when two names might be the same person, keep them separate and note it under Open questions.",
  },
  {
    id: "requirements",
    name: "Requirements",
    description: "One gold-standard requirements document",
    output: "knowledge/Requirements.md",
    mode: "single",
    refresh: "on-change",
    instruction:
      "Maintain the single authoritative requirements document for this project. Capture every requirement stated or implied in the sources, grouped by area, each with: a stable identifier, the requirement in one clear sentence, its current status (proposed / agreed / cut), and the source document it came from. When sources conflict, prefer the newest and record the conflict under Open questions.",
  },
  {
    id: "decisions",
    name: "Decision log",
    description: "Every decision, when it was made, and why",
    output: "knowledge/Decisions.md",
    mode: "single",
    refresh: "on-change",
    instruction:
      "Maintain a chronological decision log. For each decision: date, what was decided (one sentence), who made or owns it, the reasoning, and the source document. Include reversals as new entries that reference the original — never rewrite history.",
  },
  {
    id: "glossary",
    name: "Glossary",
    description: "Project terms, acronyms, and code names explained",
    output: "knowledge/Glossary.md",
    mode: "single",
    refresh: "on-change",
    instruction:
      "Maintain an alphabetical glossary of the project's terms: acronyms, code names, internal tools, vendor names, and domain jargon. Each entry gets a one-or-two sentence plain-language definition and, where useful, the document where it's best explained. Only include terms that actually appear in the sources.",
  },
  {
    id: "meeting-digest",
    name: "Meeting notes digest",
    description: "One page per meeting: outcomes, owners, open items",
    output: "meetings/",
    mode: "collection",
    refresh: "on-change",
    instruction:
      "For each meeting found in the sources, maintain one digest document named by date and topic. Each digest: attendees, decisions made, action items with owners, and open questions. Keep each digest under a page — link to the raw notes rather than repeating them.",
  },
  {
    id: "faq",
    name: "FAQ",
    description: "Answers to the questions the team keeps asking",
    output: "knowledge/FAQ.md",
    mode: "single",
    refresh: "on-change",
    instruction:
      "Maintain a frequently-asked-questions document. Derive the questions from what the sources discuss repeatedly or answer implicitly — dates, owners, scope, process. Write each answer in one or two sentences with a pointer to the source document. Remove questions whose answers no longer appear in the sources.",
  },
  {
    id: "risks",
    name: "Risks",
    description: "A living register of risks and their mitigations",
    output: "knowledge/Risks.md",
    mode: "single",
    refresh: "on-change",
    instruction:
      "Maintain a risk register. For each risk stated or implied in the sources: a short name, what could go wrong, likelihood and impact (low/medium/high, your judgment from the sources), the mitigation if one is mentioned, and the owner if known. Mark risks that no longer appear in current sources as retired rather than deleting them.",
  },
];
