import { afterEach, describe, expect, it, vi } from "vitest";
import { coalesce, singleFlight } from "./refresh";

describe("coalesce", () => {
  afterEach(() => vi.useRealTimers());

  it("a steady stream of calls still runs every so often", () => {
    vi.useFakeTimers();
    let runs = 0;
    const queue = coalesce(() => runs++, 250);
    // A call every 100 ms for a second: a reset-on-each-call debounce would
    // never run.
    for (let i = 0; i < 10; i++) {
      queue();
      vi.advanceTimersByTime(100);
    }
    expect(runs).toBeGreaterThanOrEqual(3);
  });

  it("runs once for a burst, and a shorter wait brings it forward", () => {
    vi.useFakeTimers();
    let runs = 0;
    const queue = coalesce(() => runs++, 3000);
    queue();
    queue();
    queue(400);
    vi.advanceTimersByTime(400);
    expect(runs).toBe(1);
    vi.advanceTimersByTime(3000);
    expect(runs).toBe(1);
  });
});

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((r) => (resolve = r));
  return { promise, resolve };
}

describe("singleFlight", () => {
  it("runs one read at a time, and one more after a burst", async () => {
    const gates: ReturnType<typeof deferred>[] = [];
    let started = 0;
    const run = singleFlight(async () => {
      started++;
      const g = deferred();
      gates.push(g);
      await g.promise;
    });
    const first = run();
    run();
    run();
    const last = run();
    expect(started).toBe(1);
    gates[0].resolve();
    await Promise.resolve();
    await Promise.resolve();
    expect(started).toBe(2);
    gates[1].resolve();
    await first;
    await last;
    expect(started).toBe(2);
  });

  it("keeps going after a read fails, and runs again later", async () => {
    let calls = 0;
    const errors = console.error;
    console.error = () => {};
    const run = singleFlight(async () => {
      calls++;
      if (calls === 1) throw new Error("offline");
    });
    await run();
    await run();
    console.error = errors;
    expect(calls).toBe(2);
  });

  it("starts the read at once when none is running", () => {
    let started = 0;
    const run = singleFlight(async () => {
      started++;
    });
    void run();
    expect(started).toBe(1);
  });
});
