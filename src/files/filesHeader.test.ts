import { describe, expect, it } from "vitest";
import { showMarkAllViewed, showUnreadFilter } from "./filesHeader";

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

describe("mark all as viewed visibility", () => {
  it("shows the button when anything is unread", () => {
    expect(showMarkAllViewed(1)).toBe(true);
  });
  it("hides the button once nothing is unread", () => {
    expect(showMarkAllViewed(0)).toBe(false);
  });
});
