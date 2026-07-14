import { describe, expect, it } from "vitest";
import { showUnreadFilter } from "./filesHeader";

describe("files header filter visibility", () => {
  it("shows the filter when anything is unread", () => {
    expect(showUnreadFilter(3, "all")).toBe(true);
  });
  it("shows the filter while the unread view is active, even at zero", () => {
    expect(showUnreadFilter(0, "unread")).toBe(true);
  });
  it("hides the filter when nothing is unread and the view is all", () => {
    expect(showUnreadFilter(0, "all")).toBe(false);
  });
});
