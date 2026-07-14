import { describe, it, expect } from "vitest";
import { liveCaption, countdownFrom } from "./liveRun";

describe("liveCaption", () => {
  it("running with activity shows the activity and elapsed", () => {
    const c = liveCaption({ status: "running", activity: "Read notes/a.md", elapsedSecs: 12 });
    expect(c.label).toContain("Read notes/a.md");
    expect(c.label).toContain("12s");
    expect(c.tone).toBe("busy");
  });
  it("queued shows a countdown seeded from etaSecs", () => {
    const c = liveCaption({ status: "queued", etaSecs: 8 });
    expect(c.label.toLowerCase()).toContain("starts in");
    expect(c.label).toContain("8s");
  });
  it("waiting shows the blocking run", () => {
    const c = liveCaption({ status: "waiting", detail: "waiting for people" });
    expect(c.label).toContain("waiting for people");
    expect(c.tone).toBe("muted");
  });
});

describe("countdownFrom", () => {
  it("floors at zero and formats seconds", () => {
    const now = 10_000;
    expect(countdownFrom(now + 3000, now)).toBe("3s");
    expect(countdownFrom(now - 5000, now)).toBe("now");
  });
});
