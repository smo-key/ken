// Chat drawer state: rows, transcripts, active chat, terminal mode.
import {
  api,
  type ChatRow,
} from "./api";
import {
  dropPending,
  nextTempId,
  optimisticUserMessage,
  reconcile,
  type TranscriptEntry,
} from "./chatEcho";
import { app } from "./app.svelte";

class ChatsStore {
  open = $state(false);
  rows = $state<ChatRow[]>([]);
  activeId = $state<string | null>(null);
  transcript = $state<TranscriptEntry[]>([]);
  /** chat id currently in terminal mode (at most one). */
  terminalId = $state<string | null>(null);
  sendError = $state<string | null>(null);

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

  async init() {
    await api.onChatUpdated((row) => {
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
    });
    await api.onChatMessage((msg) => {
      if (msg.chatId === this.activeId) {
        this.transcript = reconcile(this.transcript, msg);
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

  async refresh() {
    if (!(await api.currentProject().catch(() => null))) return;
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
    try {
      await api.sendChatMessage(chatId, text, openFiles, focusedFile);
    } catch (e) {
      // The send failed: pull the pending echo and show why, so the message
      // doesn't sit there looking sent.
      this.transcript = dropPending(this.transcript, tempId);
      this.sendError = String(e);
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
