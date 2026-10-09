// Your day, the pure part: dates, ordering, and the short labels a task row
// shows. No runes and no IPC, so it tests on its own.
import type { DayEscalation, DayTask } from "./api";

/** A local calendar date as YYYY-MM-DD. */
export function isoDate(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

/** Today, local time, as YYYY-MM-DD. */
export function localToday(now: Date = new Date()): string {
  return isoDate(now);
}

/** Parse YYYY-MM-DD as a local date (not UTC midnight). Null if malformed. */
export function parseIsoDate(s: string | null | undefined): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(s ?? "");
  if (!m) return null;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  return Number.isNaN(d.getTime()) ? null : d;
}

/** The date part of a stored timestamp (`2026-10-01`, `2026-10-01T09:40:00`,
 *  `2026-10-01 09:40`), or null. */
export function datePart(stamp: string | null | undefined): string | null {
  const m = /^(\d{4}-\d{2}-\d{2})/.exec(stamp ?? "");
  return m ? m[1] : null;
}

/** The HH:MM of a stored timestamp, or null when it carries no time. */
export function timePart(stamp: string | null | undefined): string | null {
  const m = /^\d{4}-\d{2}-\d{2}[T ](\d{2}):(\d{2})/.exec(stamp ?? "");
  return m ? `${m[1]}:${m[2]}` : null;
}

const DAY_MS = 86_400_000;

/** Whole days from `today` to `date`, both YYYY-MM-DD. */
function daysBetween(today: string, date: string): number | null {
  const a = parseIsoDate(today);
  const b = parseIsoDate(date);
  if (!a || !b) return null;
  return Math.round((b.getTime() - a.getTime()) / DAY_MS);
}

/** The short target label: the weekday for today and the six days after
 *  ("Wed"), the month and day otherwise ("Oct 14"), with the year when it
 *  is not this year ("Jan 3, 2027"). Empty for no target. */
export function shortTarget(target: string | null | undefined, today: string = localToday()): string {
  const d = parseIsoDate(target);
  if (!d) return "";
  const diff = daysBetween(today, target!);
  if (diff !== null && diff >= 0 && diff <= 6) {
    return d.toLocaleDateString("en-US", { weekday: "short" });
  }
  const sameYear = parseIsoDate(today)?.getFullYear() === d.getFullYear();
  return d.toLocaleDateString("en-US", {
    month: "short",
    day: "numeric",
    ...(sameYear ? {} : { year: "numeric" }),
  });
}

/** The long form, for the task panel: "Wed, Oct 1". */
export function longTarget(target: string | null | undefined): string {
  const d = parseIsoDate(target);
  if (!d) return "";
  return d.toLocaleDateString("en-US", { weekday: "short", month: "short", day: "numeric" });
}

/** True when the target is before today and the task is still open. */
export function isOverdue(task: Pick<DayTask, "state" | "target">, today: string = localToday()): boolean {
  return task.state === "open" && !!task.target && task.target < today;
}

/** A done task stays listed for the rest of the day it was done, then
 *  leaves. The day it was done is the date of its last update; one with no
 *  update date stays (the backend already dropped older ones). */
export function visibleToday(task: Pick<DayTask, "state" | "updated">, today: string = localToday()): boolean {
  if (task.state !== "done") return true;
  const day = datePart(task.updated);
  return !day || day >= today;
}

/** Other, in order: open tasks by target date ascending, then open tasks
 *  with no target, then done ones. Ties keep the order they came in. Done
 *  tasks from before today are dropped. */
export function orderTasks<T extends Pick<DayTask, "state" | "target" | "updated">>(
  tasks: T[],
  today: string = localToday(),
): T[] {
  const rank = (t: T) => (t.state === "done" ? 2 : t.target ? 0 : 1);
  return tasks
    .map((t, i) => ({ t, i }))
    .filter(({ t }) => visibleToday(t, today))
    .sort((a, b) => {
      const r = rank(a.t) - rank(b.t);
      if (r !== 0) return r;
      if (rank(a.t) === 0 && a.t.target !== b.t.target) return a.t.target! < b.t.target! ? -1 : 1;
      return a.i - b.i;
    })
    .map(({ t }) => t);
}

const WEEKDAYS = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"] as const;
const WEEKDAY_NAMES: Record<string, string> = {
  mon: "Monday",
  tue: "Tuesday",
  wed: "Wednesday",
  thu: "Thursday",
  fri: "Friday",
  sat: "Saturday",
  sun: "Sunday",
};

/** The weekday key (`mon`..`sun`) of a YYYY-MM-DD date. */
export function weekdayKey(date: string): string {
  const d = parseIsoDate(date);
  return WEEKDAYS[(d ?? new Date()).getDay()];
}

function ordinal(n: number): string {
  const rem100 = n % 100;
  if (rem100 >= 11 && rem100 <= 13) return `${n}th`;
  return `${n}${["th", "st", "nd", "rd"][n % 10] ?? "th"}`;
}

/** How a repeat reads in a row's detail line: "Every day", "Weekdays",
 *  "Tuesdays", "Monthly on the 14th". Null for a one-off. */
export function repeatText(repeat: string | null | undefined): string | null {
  if (!repeat) return null;
  if (repeat === "daily") return "Every day";
  if (repeat === "weekdays") return "Weekdays";
  const weekly = /^weekly:(mon|tue|wed|thu|fri|sat|sun)$/.exec(repeat);
  if (weekly) return `${WEEKDAY_NAMES[weekly[1]]}s`;
  const monthly = /^monthly:(\d{1,2})$/.exec(repeat);
  if (monthly) return `Monthly on the ${ordinal(Number(monthly[1]))}`;
  return repeat;
}

/** A ticket link in a named repo, `<repo>/<ID>`, split; null for anything
 *  else. `repos`: the workspace's member names, so `repo/file.md` or a
 *  nested repo name is not mistaken for one. */
export function splitTicketLink(link: string, repos: readonly string[]): { repo: string; id: string } | null {
  const lower = link.toLowerCase();
  const repo = [...repos]
    .sort((a, b) => b.length - a.length)
    .find((r) => r && lower.startsWith(r.toLowerCase() + "/"));
  if (!repo) return null;
  const id = link.slice(repo.length + 1);
  if (!id || /[/.\s]/.test(id)) return null;
  return { repo: link.slice(0, repo.length), id };
}

/** How a link reads: a ken:// address by its path, `<repo>/<ID>` by its
 *  id, anything else as is. */
export function linkLabel(link: string, repos: readonly string[] = []): string {
  if (link.startsWith("ken://")) {
    const rest = link.slice("ken://".length);
    const slash = rest.indexOf("/");
    return slash === -1 ? rest : rest.slice(slash + 1);
  }
  return splitTicketLink(link, repos)?.id ?? link;
}

/** The line under a task's title: "from dee · ATT-014 · Tuesdays", or
 *  "note" with nothing else; "done 09:40" once done. */
export function taskDetail(
  task: Pick<DayTask, "state" | "from" | "links" | "repeat" | "updated">,
  repos: readonly string[] = [],
): string {
  if (task.state === "done") {
    const at = timePart(task.updated);
    return at ? `done ${at}` : "done";
  }
  const parts: string[] = [];
  if (task.from) parts.push(`from ${task.from}`);
  if (task.links.length > 0) parts.push(linkLabel(task.links[0], repos));
  const rep = repeatText(task.repeat);
  if (rep) parts.push(rep);
  return parts.length > 0 ? parts.join(" · ") : "note";
}

/** A ticket row's detail: "att-opmodel · 2 tasks, 1 done", the tasks part
 *  left out when none link to it. */
export function ticketDetail(t: { repo: string; linkedTasks: number; linkedDone: number }): string {
  if (t.linkedTasks === 0) return t.repo;
  const n = `${t.linkedTasks} task${t.linkedTasks === 1 ? "" : "s"}`;
  return `${t.repo} · ${n}, ${t.linkedDone} done`;
}

/** An escalation row's detail: "from dee · ATT-014 · blocks the release ·
 *  att-wiki", each part left out when the file does not say it. */
export function escalationDetail(e: Pick<DayEscalation, "raisedBy" | "ticket" | "blocks" | "raised" | "repo">): string {
  const parts: string[] = [];
  if (e.raisedBy) parts.push(`from ${e.raisedBy}`);
  if (e.ticket) parts.push(e.ticket);
  if (e.blocks) parts.push(`blocks ${e.blocks}`);
  const raised = datePart(e.raised);
  if (raised) parts.push(`raised ${raised}`);
  if (e.repo) parts.push(e.repo);
  return parts.join(" · ");
}

/** The ticket id of a path in a repo, when it is a ticket file:
 *  `tickets/ATT-014.md` or one folder deeper. Null otherwise. */
export function ticketIdForPath(relPath: string | null | undefined): string | null {
  const m = /^tickets\/(?:[^/]+\/)?([^/]+)\.md$/i.exec(relPath ?? "");
  return m ? m[1] : null;
}

/** The repeat choices the task panel offers, for a task targeted on
 *  `anchor` (or today): never, every day, weekdays, weekly on that
 *  weekday, monthly on that day of the month. */
export function repeatChoices(anchor: string): { value: string; label: string }[] {
  const wd = weekdayKey(anchor);
  const dom = parseIsoDate(anchor)?.getDate() ?? 1;
  return [
    { value: "", label: "never" },
    { value: "daily", label: "every day" },
    { value: "weekdays", label: "weekdays" },
    { value: `weekly:${wd}`, label: `weekly on ${WEEKDAY_NAMES[wd]}` },
    { value: `monthly:${dom}`, label: `monthly on the ${ordinal(dom)}` },
  ];
}

/** "Mon 14:02", "Oct 1 14:02", or the date alone when no time is stored. */
export function stampLabel(stamp: string | null | undefined, today: string = localToday()): string {
  const day = datePart(stamp);
  if (!day) return "";
  const time = timePart(stamp);
  const date = day === today ? "today" : shortTarget(day, today) || day;
  const diff = daysBetween(today, day);
  // A past date within the last week reads as its weekday too.
  const label =
    day !== today && diff !== null && diff < 0 && diff >= -6
      ? (parseIsoDate(day)?.toLocaleDateString("en-US", { weekday: "short" }) ?? day)
      : date;
  return time ? `${label} ${time}` : label;
}

/** What a task sent to a teammate carries in its body: the description,
 *  then its target, repeat and links, one per line. The inbox has no
 *  fields for those. */
export function sendBody(task: Pick<DayTask, "description" | "target" | "repeat" | "links">): string {
  const lines: string[] = [];
  if (task.target) lines.push(`Target: ${task.target}`);
  const rep = repeatText(task.repeat);
  if (rep) lines.push(`Repeat: ${rep}`);
  if (task.links.length > 0) lines.push(`Links: ${task.links.join(", ")}`);
  const desc = task.description.trim();
  if (lines.length === 0) return desc;
  return desc ? `${desc}\n\n${lines.join("\n")}` : lines.join("\n");
}
