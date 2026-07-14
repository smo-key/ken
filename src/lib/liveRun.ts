import type { IngestEvent } from "./api";

export type Tone = "busy" | "attention" | "muted" | "healthy" | "danger";

/** A caption for a transient live event (queued/waiting/running). */
export function liveCaption(
  ev: Pick<IngestEvent, "status"> &
    Partial<Pick<IngestEvent, "activity" | "detail" | "elapsedSecs" | "etaSecs">>,
): { label: string; tone: Tone } {
  switch (ev.status) {
    case "running": {
      const act = ev.activity?.trim();
      const el = ev.elapsedSecs != null ? ` · ${ev.elapsedSecs}s` : "";
      return { label: act ? `${act}${el}` : `running…${el}`, tone: "busy" };
    }
    case "queued":
      return { label: `queued — starts in ${ev.etaSecs ?? 0}s`, tone: "attention" };
    case "waiting":
      return { label: ev.detail ?? "waiting…", tone: "muted" };
    case "blocked":
      return { label: "blocked on you", tone: "attention" };
    default:
      return { label: String(ev.status), tone: "muted" };
  }
}

/** "3s" until `deadlineMs`, or "now" once reached. Pure for testing. */
export function countdownFrom(deadlineMs: number, nowMs = Date.now()): string {
  const s = Math.ceil((deadlineMs - nowMs) / 1000);
  return s > 0 ? `${s}s` : "now";
}
