import { describe, expect, it } from "vitest";
import { applyChoices, describeHunk, hunksOf } from "./editHunks";

const base = "# Save\n\nSaves are files.\nOne per region.\n\n## Format\n\nJSON.\n";
const proposed = "# Saving\n\nSaves are files.\nOne per region.\n\n## Format\n\nBinary, then JSON.\nVersioned.\n";

describe("edit hunks", () => {
  it("splits an edit into its changes, in file order, with where each starts", () => {
    const h = hunksOf(base, proposed);
    expect(h.length).toBe(2);
    expect(h[0]).toMatchObject({ oldStart: 1, removed: ["# Save"], added: ["# Saving"], before: [] });
    expect(h[1]).toMatchObject({ oldStart: 8, removed: ["JSON."], added: ["Binary, then JSON.", "Versioned."] });
    expect(h[1].before).toEqual(["", "## Format", ""]);
  });

  it("accepting all gives the proposal, none gives the file, some give some", () => {
    expect(applyChoices(base, proposed, [true, true])).toBe(proposed);
    expect(applyChoices(base, proposed, [false, false])).toBe(base);
    expect(applyChoices(base, proposed, [false, true])).toBe(
      "# Save\n\nSaves are files.\nOne per region.\n\n## Format\n\nBinary, then JSON.\nVersioned.\n",
    );
  });

  it("a new file is one change, all added", () => {
    const h = hunksOf("", "# New\nText.\n");
    expect(h).toHaveLength(1);
    expect(h[0]).toMatchObject({ removed: [], added: ["# New", "Text."] });
    expect(describeHunk(h[0])).toBe('at line 1, adding 2 line(s): "# New"');
  });
});
