// Optimistic user-message echo for the chat transcript. send() appends a pending
// copy immediately so the message never vanishes; the backend's own chat-message
// echo then reconciles against it (by content) so it is neither duplicated nor
// dropped. Pending entries carry always-negative ids so they never collide with
// real DB ids and key distinctly until reconciled.
import type { ChatMessage } from "./api";

export interface PendingMessage extends ChatMessage {
  pending: true;
}

export type TranscriptEntry = ChatMessage | PendingMessage;

export function isPending(m: TranscriptEntry): m is PendingMessage {
  return (m as PendingMessage).pending === true;
}

let seq = 0;
/** Unique, always-negative temp id (real DB ids are positive). */
export function nextTempId(): number {
  seq -= 1;
  return seq;
}

export function optimisticUserMessage(
  chatId: string,
  content: string,
  now: number,
  tempId: number,
): PendingMessage {
  return { id: tempId, chatId, role: "user", content, createdAt: now, pending: true };
}

/** Merge a backend chat-message event into the transcript:
 *  - a real id already present → ignore (a re-fired echo);
 *  - a pending user message with the same content → replace it in place;
 *  - otherwise → append. */
export function reconcile(
  transcript: TranscriptEntry[],
  incoming: ChatMessage,
): TranscriptEntry[] {
  if (transcript.some((m) => !isPending(m) && m.id === incoming.id)) {
    return transcript;
  }
  if (incoming.role === "user") {
    const i = transcript.findIndex(
      (m) => isPending(m) && m.role === "user" && m.content === incoming.content,
    );
    if (i >= 0) {
      const next = transcript.slice();
      next[i] = incoming;
      return next;
    }
  }
  return [...transcript, incoming];
}

/** Remove a pending message (its send failed). */
export function dropPending(
  transcript: TranscriptEntry[],
  tempId: number,
): TranscriptEntry[] {
  return transcript.filter((m) => !(isPending(m) && m.id === tempId));
}
