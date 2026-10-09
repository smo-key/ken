import { describe, expect, it } from "vitest";
import { uniqueIds } from "./uniqueIds";

describe("uniqueIds", () => {
  it("numbers repeats and keeps the first as it was", () => {
    const items = [{ id: "a" }, { id: "b" }, { id: "a" }, { id: "a" }];
    expect(uniqueIds(items).map((i) => i.id)).toEqual(["a", "b", "a#2", "a#3"]);
    expect(uniqueIds(items)[0]).toBe(items[0]);
  });
});
