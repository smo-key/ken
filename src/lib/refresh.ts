// How the stores read the backend again after its events. Indexing sends
// `index-updated` many times a second across a workspace's repos, so two
// rules keep the screens current without flooding the backend:
//
// - `coalesce`: the first event of a burst sets when the read happens; later
//   ones do not push it back. A steady stream still gets a read every so
//   often, instead of none until the stream stops.
// - `singleFlight`: one read at a time. A call made while a read runs asks
//   for one more read after it. No read is thrown away because a newer one
//   started: when events came faster than a slow read returned, dropping the
//   older answer meant none ever landed.
//
// A read applies its answer when the team and workspace it was for are
// still the open ones; when they changed, the change asked for another read.

export function singleFlight(read: () => Promise<void>): () => Promise<void> {
  let running: Promise<void> | null = null;
  let again = false;
  return () => {
    if (running) {
      again = true;
      return running;
    }
    running = (async () => {
      try {
        do {
          again = false;
          try {
            await read();
          } catch (e) {
            console.error(e);
          }
        } while (again);
      } finally {
        running = null;
      }
    })();
    return running;
  };
}

/** Run `fn` once, `ms` after the first call of a burst. A call asking for a
 *  shorter wait brings the run forward; a longer one changes nothing. */
export function coalesce(fn: () => void, ms: number): (wait?: number) => void {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let due = 0;
  return (wait = ms) => {
    const at = Date.now() + wait;
    if (timer !== null) {
      if (due <= at) return;
      clearTimeout(timer);
    }
    due = at;
    timer = setTimeout(() => {
      timer = null;
      fn();
    }, wait);
  };
}
