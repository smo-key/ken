// Live recording state (Svelte 5 runes). One recorder at a time. Record is a
// way to add a source on Ingest: the transcript goes to the team wiki's Raw/
// and is read at once. The title bar shows a running take from any screen.
import {
  api,
  type AudioDevice,
  type ModelStatus,
  type PermissionStatus,
  type RecordPhase,
  type RecordSupport,
} from "./api";

/** A transcript-progress path that belongs to a recording, not a video:
 *  the older `Recordings/` folder, or `<date> Recording.md` in Raw/. */
export function isRecordingPath(relPath: string): boolean {
  return relPath.startsWith("Recordings/") || /(^|\/)[^/]*Recording\.md$/.test(relPath);
}

/** "3:12" for 192 000 ms. */
export function recordClock(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(s / 60);
  return `${m}:${(s % 60).toString().padStart(2, "0")}`;
}

class RecordStore {
  phase = $state<RecordPhase>("idle");
  elapsedMs = $state(0);
  micOn = $state(true);
  systemOn = $state(false);
  devices = $state<AudioDevice[]>([]);
  deviceId = $state<string | null>(null);
  micLevel = $state(0);
  systemLevel = $state(0);
  micPerm = $state<PermissionStatus>("notDetermined");
  screenPerm = $state<PermissionStatus>("notDetermined");
  micSettingsUrl = $state("");
  screenSettingsUrl = $state("");
  /** What this machine can capture; null until the first read. */
  support = $state<RecordSupport | null>(null);
  /** The transcript only, unless the person keeps the audio too (in Ken's
   *  app data, never in a repo). */
  storage = $state<"transcript" | "audio" | "both">("transcript");
  transcribing = $state(false);
  /** 0–100 while the post-stop transcription runs; null until the first sample. */
  transcribePct = $state<number | null>(null);
  savedPath = $state<string | null>(null);
  error = $state<string | null>(null);
  // Readiness of the recommended/selected transcription model. Null until the
  // first check resolves; drives the up-front gate so we never let the user
  // record a transcript we can't produce.
  modelStatus = $state<ModelStatus | null>(null);

  private clock: ReturnType<typeof setInterval> | null = null;
  private baseElapsed = 0;
  private baseAt = 0;
  private listening = false;

  get recording() {
    return this.phase === "recording" || this.phase === "paused";
  }

  /** The transcription model is on disk and recording can produce a transcript. */
  get modelReady() {
    return this.modelStatus?.installed ?? false;
  }

  /** The microphone can be captured here (assumed until record_support says). */
  get micSupported() {
    return this.support?.mic ?? true;
  }

  /** The system audio can be captured here (assumed until record_support says). */
  get systemSupported() {
    return this.support?.system ?? true;
  }

  /** Read what the machine can do again (the Record panel, each time it
   *  opens), and follow the recorder's events. */
  async init() {
    this.devices = await api.recordInputDevices().catch(() => []);
    if (!this.deviceId && this.devices[0]) this.deviceId = this.devices[0].id;
    this.support = await api.recordSupport().catch(() => this.support);
    if (!this.systemSupported && !this.recording) this.systemOn = false;
    if (!this.micSupported && !this.recording) this.micOn = false;
    await this.refreshPermissions();
    await this.refreshModelStatus();
    await this.listen();
  }

  /** Follow the recorder's events, once: the shell does this at start so the
   *  title bar shows a take from any screen. Reads nothing from the machine. */
  async listen() {
    if (this.listening) return;
    this.listening = true;
    // A download finishing (100% sample, emitted only after a verified install)
    // flips readiness so the gate clears without a manual refresh.
    await api.onModelDownloadProgress((ev) => {
      if (ev.total > 0 && ev.downloaded >= ev.total) void this.refreshModelStatus();
    });
    await api.onRecordLevel((ev) => {
      if (ev.source === "mic") this.micLevel = ev.rms;
      else this.systemLevel = ev.rms;
    });
    await api.onRecordState((ev) => {
      this.phase = ev.phase;
      this.micOn = ev.mic || this.micOn;
      this.systemOn = ev.system || this.systemOn;
      this.baseElapsed = ev.elapsedMs;
      this.baseAt = performance.now();
      this.elapsedMs = ev.elapsedMs;
      if (ev.phase === "recording") this.startClock();
      else this.stopClock();
      if (ev.phase === "idle") {
        this.micLevel = 0;
        this.systemLevel = 0;
      }
    });
    await api.onRecordTranscribing(() => {
      // Recording has ended, but the backend emits no `record-state` phase
      // change at stop — only the terminal `Idle` after finishing. Stop the
      // elapsed clock here so it freezes at the true duration instead of
      // ticking on through transcription.
      this.stopClock();
      // `record-transcribing` fires unconditionally at stop, before the backend
      // examines the storage choice. For an audio-only recording no transcript
      // is produced and the finish path goes straight to `record-saved`, so
      // gating on `storage !== "audio"` prevents a misleading "Transcribing…"
      // flash. record-saved/record-error remain the authoritative resolvers.
      this.transcribePct = null;
      if (this.storage !== "audio") this.transcribing = true;
    });
    await api.onTranscriptProgress((ev) => {
      // The transcript lands in the team wiki, which need not be the focused
      // repo, so the path decides; a video transcription never drives this bar.
      if (this.transcribing && isRecordingPath(ev.relPath)) {
        this.transcribePct = ev.pct;
      }
    });
    await api.onRecordSaved((ev) => {
      this.transcribing = false;
      this.transcribePct = null;
      this.savedPath = ev.relPath;
    });
    await api.onRecordError((ev) => {
      this.transcribing = false;
      this.transcribePct = null;
      this.error = ev.message;
    });
  }

  async refreshModelStatus() {
    this.modelStatus = await api.modelStatus().catch(() => null);
  }

  async refreshPermissions() {
    const p = await api.recordPermissions().catch(() => null);
    if (!p) return;
    this.micPerm = p.mic;
    this.screenPerm = p.screen;
    this.micSettingsUrl = p.micSettingsUrl;
    this.screenSettingsUrl = p.screenSettingsUrl;
  }

  private startClock() {
    this.stopClock();
    this.clock = setInterval(() => {
      this.elapsedMs = this.baseElapsed + (performance.now() - this.baseAt);
    }, 200);
  }
  private stopClock() {
    if (this.clock) clearInterval(this.clock);
    this.clock = null;
  }

  /** Start a take for `team`; its transcript goes to that team wiki's Raw/. */
  async start(team: string | null = null) {
    this.error = null;
    this.savedPath = null;
    // Up-front gate: without the transcription model on disk the recording would
    // only fail after Stop, wasting the take. Refuse and let the UI prompt the
    // download instead.
    if (!this.modelReady) {
      this.error = "Download a transcription model first, so Ken can make a transcript.";
      return;
    }
    await api.recordStart(this.micOn, this.systemOn, this.deviceId, team).catch((e) => {
      this.error = String(e);
    });
    await this.refreshPermissions();
  }
  async pause() {
    await api.recordPause();
  }
  async resume() {
    await api.recordResume();
  }
  async stop() {
    await api.recordStop(this.storage);
  }
  async cancel() {
    await api.recordCancel();
  }
  async requestMic() {
    await api.recordRequestPermission("mic");
    setTimeout(() => void this.refreshPermissions(), 800);
  }
  async requestScreen() {
    await api.recordRequestPermission("screen");
    setTimeout(() => void this.refreshPermissions(), 800);
  }
}

export const record = new RecordStore();
