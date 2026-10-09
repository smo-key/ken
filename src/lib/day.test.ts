import { describe, expect, it } from "vitest";
import {
  datePart,
  escalationDetail,
  isOverdue,
  linkLabel,
  orderTasks,
  repeatChoices,
  repeatText,
  sendBody,
  shortTarget,
  splitTicketLink,
  taskDetail,
  ticketDetail,
  ticketIdForPath,
  timePart,
  visibleToday,
} from "./day";

type T = { id: string; state: "open" | "done"; target: string | null; updated: string | null };
const t = (id: string, state: "open" | "done", target: string | null, updated: string | null = "2026-10-01"): T => ({
  id,
  state,
  target,
  updated,
});

// 2026-10-01 is a Thursday.
const TODAY = "2026-10-01";

describe("orderTasks", () => {
  it("puts targets first, earliest first, then no target, then done", () => {
    const out = orderTasks(
      [
        t("done1", "done", "2026-09-30"),
        t("none1", "open", null),
        t("late", "open", "2026-10-09"),
        t("soon", "open", "2026-10-02"),
        t("none2", "open", null),
        t("overdue", "open", "2026-09-28"),
      ],
      TODAY,
    );
    expect(out.map((x) => x.id)).toEqual(["overdue", "soon", "late", "none1", "none2", "done1"]);
  });

  it("keeps the incoming order for equal targets", () => {
    const out = orderTasks([t("b", "open", "2026-10-03"), t("a", "open", "2026-10-03")], TODAY);
    expect(out.map((x) => x.id)).toEqual(["b", "a"]);
  });

  it("drops a task done before today and keeps one done today", () => {
    const out = orderTasks(
      [t("yesterday", "done", null, "2026-09-30T18:00:00"), t("today", "done", null, "2026-10-01T09:40:00")],
      TODAY,
    );
    expect(out.map((x) => x.id)).toEqual(["today"]);
  });

  it("hides yesterday's done task once the day turns", () => {
    const task = t("x", "done", null, "2026-10-01T23:59:00");
    expect(visibleToday(task, "2026-10-01")).toBe(true);
    expect(visibleToday(task, "2026-10-02")).toBe(false);
  });

  it("never hides an open task, however old", () => {
    expect(visibleToday(t("x", "open", null, "2020-01-01"), TODAY)).toBe(true);
  });

  it("keeps a done task with no update date", () => {
    expect(visibleToday(t("x", "done", null, null), TODAY)).toBe(true);
  });
});

describe("shortTarget", () => {
  it("is empty with no target", () => {
    expect(shortTarget(null, TODAY)).toBe("");
    expect(shortTarget("not a date", TODAY)).toBe("");
  });

  it("names the weekday for today and the next six days", () => {
    expect(shortTarget("2026-10-01", TODAY)).toBe("Thu");
    expect(shortTarget("2026-10-02", TODAY)).toBe("Fri");
    expect(shortTarget("2026-10-07", TODAY)).toBe("Wed");
  });

  it("gives month and day beyond a week, and for past dates", () => {
    expect(shortTarget("2026-10-08", TODAY)).toBe("Oct 8");
    expect(shortTarget("2026-10-14", TODAY)).toBe("Oct 14");
    expect(shortTarget("2026-09-28", TODAY)).toBe("Sep 28");
  });

  it("adds the year when it is not this year", () => {
    expect(shortTarget("2027-01-03", TODAY)).toBe("Jan 3, 2027");
  });
});

describe("isOverdue", () => {
  it("is an open task with a target before today", () => {
    expect(isOverdue({ state: "open", target: "2026-09-30" }, TODAY)).toBe(true);
    expect(isOverdue({ state: "open", target: TODAY }, TODAY)).toBe(false);
    expect(isOverdue({ state: "done", target: "2026-09-30" }, TODAY)).toBe(false);
    expect(isOverdue({ state: "open", target: null }, TODAY)).toBe(false);
  });
});

describe("timestamps", () => {
  it("reads the date and the time of a stored stamp", () => {
    expect(datePart("2026-10-01T09:40:12Z")).toBe("2026-10-01");
    expect(timePart("2026-10-01T09:40:12Z")).toBe("09:40");
    expect(timePart("2026-10-01 14:02")).toBe("14:02");
    expect(timePart("2026-10-01")).toBeNull();
    expect(datePart(null)).toBeNull();
  });
});

describe("row text", () => {
  const base = { state: "open" as const, from: null, links: [] as string[], repeat: null, updated: null };

  it("says note when a task has nothing else", () => {
    expect(taskDetail(base)).toBe("note");
  });

  it("joins sender, first link and repeat", () => {
    expect(
      taskDetail({ ...base, from: "dee", links: ["Project Docs/Pilot checklist.docx", "ATT-014"], repeat: "weekly:tue" }),
    ).toBe("from dee · Project Docs/Pilot checklist.docx · Tuesdays");
  });

  it("shows when a done task was done", () => {
    expect(taskDetail({ ...base, state: "done", updated: "2026-10-01T09:40:00" })).toBe("done 09:40");
    expect(taskDetail({ ...base, state: "done", updated: "2026-10-01" })).toBe("done");
  });

  it("reads a ken:// link by its path", () => {
    expect(linkLabel("ken://abc123/src/retry.test.ts")).toBe("src/retry.test.ts");
    expect(linkLabel("ATT-014")).toBe("ATT-014");
  });

  it("words each repeat", () => {
    expect(repeatText(null)).toBeNull();
    expect(repeatText("daily")).toBe("Every day");
    expect(repeatText("weekdays")).toBe("Weekdays");
    expect(repeatText("monthly:1")).toBe("Monthly on the 1st");
    expect(repeatText("monthly:22")).toBe("Monthly on the 22nd");
    expect(repeatText("monthly:13")).toBe("Monthly on the 13th");
  });

  it("offers repeats anchored on the target", () => {
    const values = repeatChoices("2026-10-14").map((c) => c.value);
    expect(values).toEqual(["", "daily", "weekdays", "weekly:wed", "monthly:14"]);
  });

  it("leaves the tasks part out of a ticket with none linked", () => {
    expect(ticketDetail({ repo: "att-opmodel", linkedTasks: 0, linkedDone: 0 })).toBe("att-opmodel");
    expect(ticketDetail({ repo: "att-opmodel", linkedTasks: 2, linkedDone: 1 })).toBe("att-opmodel · 2 tasks, 1 done");
    expect(ticketDetail({ repo: "x", linkedTasks: 1, linkedDone: 0 })).toBe("x · 1 task, 0 done");
  });
});

describe("ticketIdForPath", () => {
  it("matches tickets/<ID>.md and one folder deeper", () => {
    expect(ticketIdForPath("tickets/ATT-014.md")).toBe("ATT-014");
    expect(ticketIdForPath("tickets/done/ATT-009.md")).toBe("ATT-009");
  });

  it("ignores anything else", () => {
    expect(ticketIdForPath("tickets/a/b/ATT-1.md")).toBeNull();
    expect(ticketIdForPath("docs/tickets/ATT-1.md")).toBeNull();
    expect(ticketIdForPath("tickets/ATT-1.txt")).toBeNull();
    expect(ticketIdForPath(null)).toBeNull();
  });
});

describe("ticket links in a repo", () => {
  const repos = ["att-opmodel", "clients/att-wiki", "att-wiki"];

  it("splits <repo>/<ID> and shows the id", () => {
    expect(splitTicketLink("att-opmodel/ATT-014", repos)).toEqual({ repo: "att-opmodel", id: "ATT-014" });
    expect(splitTicketLink("clients/att-wiki/W-2", repos)).toEqual({ repo: "clients/att-wiki", id: "W-2" });
    expect(linkLabel("att-opmodel/ATT-014", repos)).toBe("ATT-014");
  });

  it("leaves a bare id, a file path and an unknown repo as they are", () => {
    expect(linkLabel("ATT-014", repos)).toBe("ATT-014");
    expect(linkLabel("att-opmodel/docs/plan.md", repos)).toBe("att-opmodel/docs/plan.md");
    expect(linkLabel("att-opmodel/README.md", repos)).toBe("att-opmodel/README.md");
    expect(linkLabel("other/ATT-014", repos)).toBe("other/ATT-014");
    expect(linkLabel("att-opmodel/ATT-014")).toBe("att-opmodel/ATT-014");
  });
});

describe("escalationDetail", () => {
  it("names who raised it, its ticket, what it blocks, when and where", () => {
    expect(
      escalationDetail({ raisedBy: "dee", ticket: "ATT-014", blocks: "the release", raised: "2026-09-30T10:00", repo: "att-team" }),
    ).toBe("from dee · ATT-014 · blocks the release · raised 2026-09-30 · att-team");
  });
  it("leaves out what the file does not say", () => {
    expect(escalationDetail({ raisedBy: "", ticket: null, blocks: null, raised: null, repo: "att-team" })).toBe("att-team");
  });
});

describe("sendBody", () => {
  it("adds target, repeat and links under the description", () => {
    expect(
      sendBody({ description: "Check the totals.", target: "2026-10-02", repeat: "weekly:tue", links: ["ATT-014", "a/b.md"] }),
    ).toBe("Check the totals.\n\nTarget: 2026-10-02\nRepeat: Tuesdays\nLinks: ATT-014, a/b.md");
  });

  it("is the description alone when there is nothing else", () => {
    expect(sendBody({ description: "Hi", target: null, repeat: null, links: [] })).toBe("Hi");
    expect(sendBody({ description: "", target: "2026-10-02", repeat: null, links: [] })).toBe("Target: 2026-10-02");
  });
});
