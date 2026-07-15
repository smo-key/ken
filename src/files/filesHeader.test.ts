import { describe, expect, it } from "vitest";
import { isMarkAllEnabled, showUnreadFilter } from "./filesHeader";

describe("files header filter visibility", () => {
  it("shows the filter when anything is unread", () => {
    expect(showUnreadFilter(3, "all")).toBe(true);
  });
  it("shows the filter while the unread view is active, even at zero", () => {
    expect(showUnreadFilter(0, "unread")).toBe(true);
  });
  it("still shows the filter when nothing is unread and the view is all", () => {
    // The filter is always visible now so the user can flip to Unread anytime.
    expect(showUnreadFilter(0, "all")).toBe(true);
  });
});

describe("mark all as viewed enablement", () => {
  it("enables the button when anything is unread", () => {
    expect(isMarkAllEnabled(1)).toBe(true);
  });
  it("disables the button once nothing is unread", () => {
    expect(isMarkAllEnabled(0)).toBe(false);
  });
});
