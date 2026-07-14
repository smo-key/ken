// A tolerant WebVTT cue parser, kept pure so the player, the transcript pane
// and the tests all read cues the same way. It intentionally accepts more than
// the spec: on-device Whisper output, docx-derived paragraphs with no timing,
// and files missing the `WEBVTT` header all have to render rather than throw.

export interface VttCue {
  /** Cue start, in seconds. 0 for untimed (docx-derived) cues. */
  start: number;
  /** Cue end, in seconds. 0 for untimed cues. */
  end: number;
  /** Cue body; multiple source lines joined with "\n". */
  text: string;
}

// `HH:MM:SS.mmm` or the spec's short `MM:SS.mmm`; a comma fraction (SRT-style)
// is accepted so a mislabeled transcript still lands on the right second.
const TIMESTAMP = /^(?:(\d+):)?(\d{1,2}):(\d{2})[.,](\d{1,3})$/;

function parseTimestamp(raw: string): number | null {
  const m = raw.trim().match(TIMESTAMP);
  if (!m) return null;
  const hours = m[1] ? Number(m[1]) : 0;
  const minutes = Number(m[2]);
  const seconds = Number(m[3]);
  const millis = Number(m[4].padEnd(3, "0"));
  return hours * 3600 + minutes * 60 + seconds + millis / 1000;
}

export function parseVtt(input: string): VttCue[] {
  const cues: VttCue[] = [];
  const text = input.replace(/^﻿/, "").replace(/\r\n?/g, "\n");

  for (const block of text.split(/\n[ \t]*\n/)) {
    const lines = block.split("\n");
    // Drop leading/trailing blank lines a lax split can leave behind.
    while (lines.length && !lines[0].trim()) lines.shift();
    while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
    if (!lines.length) continue;

    const first = lines[0].trim();
    // Header and non-cue blocks carry no transcript text.
    if (first.startsWith("WEBVTT")) continue;
    if (first === "NOTE" || first.startsWith("NOTE ")) continue;
    if (first === "STYLE" || first === "REGION") continue;

    const timingIdx = lines.findIndex((l) => l.includes("-->"));
    if (timingIdx === -1) {
      // No timing line: a docx-derived paragraph. Render it, but with no seek.
      const body = lines.join("\n").trim();
      if (body) cues.push({ start: 0, end: 0, text: body });
      continue;
    }

    const [rawStart, rawRest] = lines[timingIdx].split("-->");
    // The end token may be followed by cue settings (align/position/…).
    const rawEnd = (rawRest ?? "").trim().split(/\s+/)[0] ?? "";
    const body = lines
      .slice(timingIdx + 1)
      .join("\n")
      .trim();
    if (!body) continue; // nothing to show for an empty cue

    cues.push({
      start: parseTimestamp(rawStart) ?? 0,
      end: parseTimestamp(rawEnd) ?? 0,
      text: body,
    });
  }

  return cues;
}

/** True when at least one cue is placed on the timeline (has seek/highlight). */
export function isTimed(cues: VttCue[]): boolean {
  return cues.some((c) => c.end > 0 || c.start > 0);
}

/** A cue timestamp for the transcript list: M:SS, or H:MM:SS past an hour. */
export function formatCueTime(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = String(s).padStart(2, "0");
  if (h > 0) return `${h}:${String(m).padStart(2, "0")}:${ss}`;
  return `${m}:${ss}`;
}
