// Chat drawer state: rows, transcripts, active chat, terminal mode.
import {
  api,
  type ChatRow,
  type EditProposal,
} from "./api";

/** A stored edit message's proposal, or null when it is not one. */
export function parseEditProposal(content: string): EditProposal | null {
  try {
    const p = JSON.parse(content) as EditProposal;
    return typeof p.base === "string" && typeof p.proposed === "string" ? p : null;
  } catch {
    return null;
  }
}
import {
  dropPending,
  nextTempId,
  optimisticUserMessage,
  reconcile,
  type TranscriptEntry,
} from "./chatEcho";
import { parseQuestionPayload } from "./chatQuestion";
import { app, forFocused } from "./app.svelte";
import { scope } from "./scope.svelte";

class ChatsStore {
  open = $state(false);
  rows = $state<ChatRow[]>([]);
  activeId = $state<string | null>(null);
  transcript = $state<TranscriptEntry[]>([]);
  /** chat id currently in terminal mode (at most one). */
  terminalId = $state<string | null>(null);
  sendError = $state<string | null>(null);
  /** The reply streaming into the open chat, until the whole of it lands. */
  draft = $state<string>("");

  get active(): ChatRow | null {
    return this.rows.find((r) => r.id === this.activeId) ?? null;
  }

  get needsInput(): boolean {
    return this.rows.some((r) => r.status === "needs_input");
  }

  /** Pinned first, then most recent — matches the backend ordering. */
  get ordered(): ChatRow[] {
    return this.rows;
  }

  private listening = false;

  /** The drawer calls this each time it opens: the listeners are added
   *  once (twice would stream every reply twice), the list read each time. */
  async init() {
    if (this.listening) return this.refresh();
    this.listening = true;
    await api.onChatUpdated((row) => {
      if (!this.ours(row.project_id)) return;
      const i = this.rows.findIndex((r) => r.id === row.id);
      if (row.archived) {
        if (i >= 0) this.rows = this.rows.toSpliced(i, 1);
        if (this.activeId === row.id) {
          this.activeId = this.rows[0]?.id ?? null;
          if (this.activeId) void this.select(this.activeId);
          else this.transcript = [];
        }
        return;
      }
      if (i >= 0) this.rows = this.rows.toSpliced(i, 1, row);
      else this.rows = [row, ...this.rows];
      this.resort();
      // A turn that ended (or broke) leaves nothing streaming.
      if (row.id === this.activeId && row.status !== "working") this.draft = "";
    });
    await api.onChatDelta((d) => {
      if (!this.ours(d.project_id)) return;
      if (d.chatId === this.activeId) this.draft += d.text;
    });
    await api.onChatMessage((msg) => {
      if (!this.ours(msg.project_id)) return;
      if (msg.chatId === this.activeId) {
        this.transcript = reconcile(this.transcript, msg);
        // The whole reply replaces what streamed of it.
        if (msg.role === "assistant") this.draft = "";
      }
    });
    await this.refresh();
  }

  private resort() {
    this.rows = [...this.rows].sort((a, b) => {
      if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;
      return b.lastActiveAt - a.lastActiveAt;
    });
  }

  /** The repo the listed chats belong to. Opening a citation in another
   *  repo moves the focus; the chat open here goes on in its own repo. */
  private projectId: string | null = null;

  private ours(projectId: string | null | undefined): boolean {
    const mine = this.projectId ?? app.focused;
    return !projectId || !mine || projectId === mine || forFocused(projectId);
  }

  async refresh() {
    if (!(await api.currentProject().catch(() => null))) return;
    this.projectId = app.focused;
    this.rows = await api.listChats();
    if (this.activeId && !this.rows.some((r) => r.id === this.activeId)) {
      this.activeId = this.rows[0]?.id ?? null;
    }
  }

  async select(id: string) {
    if (this.terminalId && this.terminalId !== id) {
      await this.exitTerminal();
    }
    this.activeId = id;
    this.draft = "";
    this.transcript = await api.chatTranscript(id).catch(() => []);
    // Ingest and research sessions open straight into the terminal —
    // that's where they live.
    const kind = this.rows.find((r) => r.id === id)?.kind;
    if (kind === "ingest" || kind === "research") {
      await this.enterTerminal();
    }
  }

  async newChat() {
    const row = await api.createChat();
    this.open = true;
    await this.select(row.id);
  }

  async send(text: string) {
    if (!this.activeId) return;
    this.sendError = null;
    const chatId = this.activeId;
    // Optimistically echo the user's message so it never vanishes; the backend's
    // chat-message event reconciles against this pending copy by content.
    const tempId = nextTempId();
    this.transcript = [
      ...this.transcript,
      optimisticUserMessage(chatId, text, Date.now(), tempId),
    ];
    // Attach the files the user has on screen as a weak, clearly-caveated hint
    // (the backend frames it as "not necessarily relevant"). Send all open tabs
    // plus which one is focused.
    const openFiles = app.fileTabs.map((t) => t.path);
    const focusedFile = app.openFile;
    // Working from the moment it is sent: the backend's own status follows,
    // and its "done" or "error" ends it.
    this.setStatus(chatId, "working");
    try {
      await api.sendChatMessage(chatId, text, openFiles, focusedFile, scope.chatScope);
    } catch (e) {
      // The send failed: pull the pending echo and show why, so the message
      // doesn't sit there looking sent.
      this.transcript = dropPending(this.transcript, tempId);
      this.sendError = String(e);
      this.setStatus(chatId, "error");
    }
  }

  private setStatus(chatId: string, status: ChatRow["status"]) {
    const i = this.rows.findIndex((r) => r.id === chatId);
    if (i >= 0) this.rows = this.rows.toSpliced(i, 1, { ...this.rows[i], status });
  }

  /** Answer a pending AskUserQuestion card. Merges the answers into the local
   *  transcript entry first so the card flips to answered instantly; the
   *  backend re-emits the same message id with the authoritative content. */
  async answerQuestion(messageId: number, answers: Record<string, string>) {
    if (!this.activeId) return;
    this.sendError = null;
    const chatId = this.activeId;
    const i = this.transcript.findIndex((m) => m.id === messageId);
    if (i < 0) return;
    const before = this.transcript[i];
    const payload = parseQuestionPayload(before.content);
    if (payload) {
      const merged = { ...payload, answers: { ...(payload.answers ?? {}), ...answers } };
      this.transcript = this.transcript.toSpliced(i, 1, {
        ...before,
        content: JSON.stringify(merged),
      });
    }
    try {
      await api.answerChatQuestion(chatId, messageId, answers);
    } catch (e) {
      // Put the unanswered card back so the user can retry.
      const j = this.transcript.findIndex((m) => m.id === messageId);
      if (j >= 0) this.transcript = this.transcript.toSpliced(j, 1, before);
      this.sendError = String(e);
    }
  }

  /** Edits in the open chat still waiting for a decision, newest last. */
  get pendingEdits(): { messageId: number; proposal: EditProposal }[] {
    const out: { messageId: number; proposal: EditProposal }[] = [];
    for (const m of this.transcript) {
      if (m.role !== "edit") continue;
      const p = parseEditProposal(m.content);
      if (p && !p.decision) out.push({ messageId: m.id, proposal: p });
    }
    return out;
  }

  /** The waiting edit to a file, if any (the editor shows it over the page). */
  pendingEditFor(relPath: string): { messageId: number; proposal: EditProposal } | null {
    return this.pendingEdits.find((e) => e.proposal.relPath === relPath) ?? null;
  }

  /** Answer an edit: every change accepted, none, or some (Ken writes the
   *  accepted ones and tells Claude which it left out). Flips the card at
   *  once; the backend re-emits it with the decision it recorded. */
  async answerEdit(
    messageId: number,
    decision: "accepted" | "declined" | "partial",
    merged: string | null,
    declinedChanges: string[],
  ) {
    if (!this.activeId) return;
    this.sendError = null;
    const chatId = this.activeId;
    const i = this.transcript.findIndex((m) => m.id === messageId);
    if (i < 0) return;
    const before = this.transcript[i];
    const p = parseEditProposal(before.content);
    if (p) {
      this.transcript = this.transcript.toSpliced(i, 1, {
        ...before,
        content: JSON.stringify({ ...p, decision }),
      });
    }
    try {
      await api.answerEditProposal(chatId, messageId, decision, merged, declinedChanges);
    } catch (e) {
      const j = this.transcript.findIndex((m) => m.id === messageId);
      if (j >= 0) this.transcript = this.transcript.toSpliced(j, 1, before);
      this.sendError = String(e);
      throw e;
    }
  }

  async pin(id: string, pinned: boolean) {
    await api.setChatPinned(id, pinned);
  }

  /** Change the active chat's model. Applies to the next message/session. */
  async setModel(model: string | null) {
    if (!this.activeId) return;
    await api.setChatModel(this.activeId, model);
  }

  async archive(id: string) {
    if (this.terminalId === id) await this.exitTerminal();
    await api.archiveChat(id);
  }

  async enterTerminal() {
    if (!this.activeId) return;
    this.sendError = null;
    try {
      await api.enterTerminalMode(this.activeId);
      this.terminalId = this.activeId;
    } catch (e) {
      this.sendError = String(e);
    }
  }

  async exitTerminal() {
    if (!this.terminalId) return;
    const id = this.terminalId;
    this.terminalId = null;
    await api.leaveTerminalMode(id).catch(() => {});
    if (this.activeId === id) {
      this.transcript = await api.chatTranscript(id).catch(() => []);
    }
  }
}

export const chats = new ChatsStore();

export const SUGGESTED_PROMPTS = [
  "What changed in this project in the last week?",
  "Summarize what this project is about in a paragraph.",
  "Which open questions or decisions still need an owner?",
];
