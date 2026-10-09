import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import type { DayState, DayTask } from "./api";

const listeners: { dayChanged?: () => void; indexUpdated?: () => void } = {};

vi.mock("./api", () => ({
  memberLeaf: (name: string) => name.split("/").pop() ?? name,
  api: {
    onDayChanged: vi.fn(async (cb: () => void) => {
      listeners.dayChanged = cb;
    }),
    onTeamDigestGenerating: vi.fn(async () => {}),
    onTeamDigestUpdated: vi.fn(async () => {}),
    onTeamDigestError: vi.fn(async () => {}),
    onKnowledgeModelState: vi.fn(async () => {}),
    onIndexUpdated: vi.fn(async (cb: () => void) => {
      listeners.indexUpdated = cb;
    }),
    claudeDoctor: vi.fn(async () => ({ found: true })),
    dayState: vi.fn(async () => ({ tickets: [], tasks: [], me: { name: null, email: null }, hasTickets: false })),
    teamDigest: vi.fn(async () => null),
    teamDigestWriting: vi.fn(async () => false),
    indexHealth: vi.fn(async () => ({ indexed: 0, total: 0, queued: 0, failed: 0 })),
    dayTaskUpdate: vi.fn(),
    ticketTasks: vi.fn(async () => []),
  },
}));

vi.mock("./app.svelte", () => ({
  app: { workspace: { id: "ws1", members: [] }, registry: [], screen: "home" },
}));

vi.mock("./scope.svelte", () => ({ scope: { team: null } }));

import { day, REFRESH_DEBOUNCE_MS } from "./day.svelte";
import { api } from "./api";

const flush = async () => {
  for (let i = 0; i < 5; i++) await Promise.resolve();
};

function task(over: Partial<DayTask> = {}): DayTask {
  return {
    id: "t1",
    title: "Write it",
    state: "open",
    target: null,
    repeat: null,
    links: [],
    from: null,
    description: "",
    updatedBy: null,
    updated: null,
    created: null,
    relPath: "tasks/t1.md",
    inbox: null,
    ...over,
  };
}

function withTasks(tasks: DayTask[]): DayState {
  return { tickets: [], tasks, me: { name: null, email: null }, hasTickets: false };
}

beforeAll(async () => {
  await day.init();
  await flush();
});

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("refresh on events", () => {
  it("reads once after a burst of day-changed and index-updated", async () => {
    const states = vi.mocked(api.dayState).mock.calls.length;
    const health = vi.mocked(api.indexHealth).mock.calls.length;
    listeners.dayChanged!();
    listeners.dayChanged!();
    listeners.indexUpdated!();
    listeners.dayChanged!();
    vi.advanceTimersByTime(REFRESH_DEBOUNCE_MS - 1);
    expect(vi.mocked(api.dayState).mock.calls.length).toBe(states);
    vi.advanceTimersByTime(1);
    expect(vi.mocked(api.dayState).mock.calls.length).toBe(states + 1);
    expect(vi.mocked(api.indexHealth).mock.calls.length).toBe(health + 1);
  });

  it("leaves health alone after day-changed alone", async () => {
    const health = vi.mocked(api.indexHealth).mock.calls.length;
    listeners.dayChanged!();
    vi.advanceTimersByTime(REFRESH_DEBOUNCE_MS);
    expect(vi.mocked(api.indexHealth).mock.calls.length).toBe(health);
  });
});

describe("a failed update", () => {
  it("puts back the fields it set", async () => {
    day.state = withTasks([task({ title: "Write it", target: "2026-10-02" })]);
    vi.mocked(api.dayTaskUpdate).mockRejectedValueOnce("offline");
    await expect(day.update("t1", { title: "Write it all" })).rejects.toBe("offline");
    expect(day.state!.tasks[0].title).toBe("Write it");
    expect(day.state!.tasks[0].target).toBe("2026-10-02");
  });

  it("keeps a newer value, and fields it did not set", async () => {
    day.state = withTasks([task()]);
    let reject!: (e: unknown) => void;
    vi.mocked(api.dayTaskUpdate).mockReturnValueOnce(new Promise((_, r) => (reject = r)));
    const pending = day.update("t1", { title: "Mine", description: "Notes" });
    expect(day.state!.tasks[0].title).toBe("Mine");
    // A read lands while the patch is out: a new title and a target.
    day.state = withTasks([task({ title: "Theirs", description: "Notes", target: "2026-10-05" })]);
    reject("offline");
    await expect(pending).rejects.toBe("offline");
    const t = day.state!.tasks[0];
    expect(t.title).toBe("Theirs");
    expect(t.description).toBe("");
    expect(t.target).toBe("2026-10-05");
  });
});

describe("a new day", () => {
  it("is noticed on focus when the midnight timer ran late", async () => {
    vi.setSystemTime(new Date(2026, 9, 1, 23, 0));
    day.checkDay();
    await flush();
    expect(day.today).toBe("2026-10-01");
    const states = vi.mocked(api.dayState).mock.calls.length;
    // Asleep through midnight: the timer has not fired.
    vi.setSystemTime(new Date(2026, 9, 2, 8, 0));
    window.dispatchEvent(new Event("focus"));
    expect(day.today).toBe("2026-10-02");
    await flush();
    expect(vi.mocked(api.dayState).mock.calls.length).toBe(states + 1);
  });

  it("does nothing on focus the same day", () => {
    vi.setSystemTime(new Date(2026, 9, 2, 9, 0));
    day.checkDay();
    const states = vi.mocked(api.dayState).mock.calls.length;
    window.dispatchEvent(new Event("focus"));
    expect(vi.mocked(api.dayState).mock.calls.length).toBe(states);
  });
});

describe("the panel", () => {
  it("shows a task opened from a ticket that is not in today's list", () => {
    day.state = withTasks([]);
    const t = task({ id: "t9", state: "done", updated: "2026-09-20" });
    day.openTask("t9", { task: t, ticket: { projectId: "p1", ticketId: "ATT-014" } });
    expect(day.panelTask?.id).toBe("t9");
  });

  it("closes when the task is gone from its ticket", async () => {
    vi.useRealTimers();
    day.state = withTasks([]);
    day.openTask("t9", { task: task({ id: "t9" }), ticket: { projectId: "p1", ticketId: "ATT-014" } });
    vi.mocked(api.ticketTasks).mockResolvedValueOnce([]);
    await day.resolvePanel();
    expect(day.panel).toBeNull();
  });
});
