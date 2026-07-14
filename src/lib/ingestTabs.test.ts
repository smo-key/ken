import { describe, it, expect } from "vitest";
import { routesToIngest } from "./ingests.svelte";
import { routesToAutomation } from "./automations.svelte";

describe("event routing", () => {
  it("ingest events route to the ingests store", () => {
    expect(routesToIngest({ kind: "ingest" } as any)).toBe(true);
  });
  it("automation events do not", () => {
    expect(routesToIngest({ kind: "automation" } as any)).toBe(false);
  });
  it("automation events route to automations", () => {
    expect(routesToAutomation({ kind: "automation" } as any)).toBe(true);
    expect(routesToAutomation({ kind: "ingest" } as any)).toBe(false);
  });
});
