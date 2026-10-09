import { describe, expect, it } from "vitest";
import { pickMessage, pickedIds, sendableMessages, type Pick } from "./chatPick";

const none: Pick = { selected: new Set(), anchor: null };

describe("sendableMessages", () => {
  it("keeps the saved user and assistant turns", () => {
    const msgs = [
      { id: 1, role: "user" },
      { id: 2, role: "activity" },
      { id: 3, role: "assistant" },
      { id: 4, role: "tool" },
      { id: -1, role: "user" },
      { id: 5, role: "edit" },
    ];
    expect(sendableMessages(msgs).map((m) => m.id)).toEqual([1, 3]);
  });
});

describe("pickMessage", () => {
  const order = [10, 11, 12, 13, 14];

  it("takes and drops one message with a plain click", () => {
    const a = pickMessage(order, none, 12, false);
    expect([...a.selected]).toEqual([12]);
    expect(a.anchor).toBe(12);
    const b = pickMessage(order, a, 12, false);
    expect(b.selected.size).toBe(0);
  });

  it("takes a range with a shift-click, either way", () => {
    const a = pickMessage(order, none, 11, false);
    const b = pickMessage(order, a, 13, true);
    expect(pickedIds(order, b.selected)).toEqual([11, 12, 13]);
    const c = pickMessage(order, pickMessage(order, none, 14, false), 12, true);
    expect(pickedIds(order, c.selected)).toEqual([12, 13, 14]);
  });

  it("treats a shift-click with no anchor as a plain click", () => {
    const a = pickMessage(order, none, 13, true);
    expect(pickedIds(order, a.selected)).toEqual([13]);
  });
});

describe("pickedIds", () => {
  it("lists the picked ids in chat order", () => {
    expect(pickedIds([3, 1, 2], new Set([2, 3]))).toEqual([3, 2]);
  });
});
