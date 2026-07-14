import { describe, expect, it } from "vitest";
import type { ChatMessage } from "./api";
import {
  dropPending,
  isPending,
  nextTempId,
  optimisticUserMessage,
  reconcile,
} from "./chatEcho";

const real = (id: number, role: ChatMessage["role"], content: string): ChatMessage => ({
  id,
  chatId: "c1",
  role,
  content,
  createdAt: 1000 + id,
});

describe("chat optimistic echo", () => {
  it("mints unique, always-negative temp ids", () => {
    const a = nextTempId();
    const b = nextTempId();
    expect(a).toBeLessThan(0);
    expect(b).toBeLessThan(0);
    expect(a).not.toBe(b);
  });

  it("marks an optimistic message pending", () => {
    const m = optimisticUserMessage("c1", "hello", 1234, nextTempId());
    expect(isPending(m)).toBe(true);
    expect(m.role).toBe("user");
    expect(m.content).toBe("hello");
  });

  it("replaces a pending echo with the backend user message (no dupe)", () => {
    const tid = nextTempId();
    let t: (ChatMessage | ReturnType<typeof optimisticUserMessage>)[] = [
      optimisticUserMessage("c1", "hello", 1234, tid),
    ];
    t = reconcile(t, real(7, "user", "hello"));
    expect(t).toHaveLength(1);
    expect(isPending(t[0])).toBe(false);
    expect(t[0].id).toBe(7);
  });

  it("ignores a duplicate backend echo of an already-reconciled message", () => {
    let t = reconcile([], real(7, "user", "hello"));
    t = reconcile(t, real(7, "user", "hello"));
    expect(t).toHaveLength(1);
  });

  it("appends assistant messages normally", () => {
    let t = reconcile([optimisticUserMessage("c1", "hi", 1, nextTempId())], real(8, "user", "hi"));
    t = reconcile(t, real(9, "assistant", "Hello!"));
    expect(t.map((m) => m.role)).toEqual(["user", "assistant"]);
  });

  it("only reconciles a pending message of the same content", () => {
    const t = reconcile(
      [optimisticUserMessage("c1", "first", 1, nextTempId())],
      real(5, "user", "different"),
    );
    // Pending 'first' stays; the unrelated backend user message appends.
    expect(t).toHaveLength(2);
    expect(isPending(t[0])).toBe(true);
    expect(t[1].content).toBe("different");
  });

  it("drops a pending message by temp id (send failure)", () => {
    const tid = nextTempId();
    const t = dropPending([optimisticUserMessage("c1", "oops", 1, tid)], tid);
    expect(t).toHaveLength(0);
  });
});
