// The clock relative times read ("3 min ago"). `timeAgo` reads `clock.now`
// when it is given no time, so a template that shows one moves on by
// itself: the clock ticks every 30 seconds once something has read it.

const TICK_MS = 30_000;

class Clock {
  #now = $state(Date.now());
  #started = false;

  /** Milliseconds since the epoch, at most a tick old. */
  get now(): number {
    if (!this.#started && typeof window !== "undefined") {
      this.#started = true;
      window.setInterval(() => (this.#now = Date.now()), TICK_MS);
    }
    return this.#now;
  }
}

export const clock = new Clock();
