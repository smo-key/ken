// Short notices at the corner of the window: an action that failed, or one
// that finished somewhere the person cannot see. Each one leaves on its own.
// No IPC here, so any store can import it.

export type ToastTone = "info" | "error";

export interface Toast {
  id: number;
  text: string;
  tone: ToastTone;
}

/** How long a notice stays, in ms. A failure stays longer: it has a reason to read. */
export const TOAST_MS = { info: 4000, error: 8000 } as const;

/** An error as a sentence: `Error: ` and the like taken off. */
export function errorText(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e);
  return raw.replace(/^(Error|TypeError):\s*/, "").trim();
}

/** "Could not update the wiki: git is not installed". */
export function failure(what: string, e: unknown): string {
  const why = errorText(e);
  return why ? `${what}: ${why}` : what;
}

class ToastStore {
  items = $state<Toast[]>([]);
  private next = 1;

  show(text: string, tone: ToastTone = "info"): number {
    const id = this.next++;
    this.items = [...this.items, { id, text, tone }];
    setTimeout(() => this.dismiss(id), TOAST_MS[tone]);
    return id;
  }

  /** A failed action: `what` it was, then why. */
  error(what: string, e?: unknown): number {
    return this.show(e === undefined ? what : failure(what, e), "error");
  }

  dismiss(id: number) {
    this.items = this.items.filter((t) => t.id !== id);
  }
}

export const toast = new ToastStore();
