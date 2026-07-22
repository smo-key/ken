// Live recording state (Svelte 5 runes). One recorder at a time.
import { api, type AudioDevice, type ModelStatus, type PermissionStatus, type RecordPhase } from "./api";

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
  storage = $state<"transcript" | "audio" | "both">("both");
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

  get recording() {
    return this.phase === "recording" || this.phase === "paused";
  }

  /** The transcription model is on disk and recording can produce a transcript. */
  get modelReady() {
    return this.modelStatus?.installed ?? false;
  }

  async init() {
    this.devices = await api.recordInputDevices().catch(() => []);
    if (!this.deviceId && this.devices[0]) this.deviceId = this.devices[0].id;
    await this.refreshPermissions();
    await this.refreshModelStatus();
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
      // Recording docs land under Recordings/; that prefix keeps a concurrent
      // video transcription from driving this bar.
      if (this.transcribing && ev.relPath.startsWith("Recordings/")) {
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

  async start() {
    this.error = null;
    this.savedPath = null;
    // Up-front gate: without the transcription model on disk the recording would
    // only fail after Stop, wasting the take. Refuse and let the UI prompt the
    // download instead.
    if (!this.modelReady) {
      this.error =
        "Download a transcription model below before recording, so Ken can make a transcript.";
      return;
    }
    await api.recordStart(this.micOn, this.systemOn, this.deviceId).catch((e) => {
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
