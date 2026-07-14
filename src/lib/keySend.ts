// Composer key rule, extracted so it is test-covered (§9 audit): Enter sends,
// Shift+Enter inserts a newline. Any other key is not a send.
export function shouldSend(e: { key: string; shiftKey: boolean }): boolean {
  return e.key === "Enter" && !e.shiftKey;
}
