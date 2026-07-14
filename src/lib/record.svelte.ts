// Live recording state (Svelte 5 runes). One recorder at a time.
import { api, type AudioDevice, type PermissionStatus, type RecordPhase } from "./api";

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
  savedPath = $state<string | null>(null);
  error = $state<string | null>(null);

  private clock: ReturnType<typeof setInterval> | null = null;
  private baseElapsed = 0;
  private baseAt = 0;

  get recording() {
    return this.phase === "recording" || this.phase === "paused";
  }

  async init() {
    this.devices = await api.recordInputDevices().catch(() => []);
    if (!this.deviceId && this.devices[0]) this.deviceId = this.devices[0].id;
    await this.refreshPermissions();
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
      if (this.storage !== "audio") this.transcribing = true;
    });
    await api.onRecordSaved((ev) => {
      this.transcribing = false;
      this.savedPath = ev.relPath;
    });
    await api.onRecordError((ev) => {
      this.transcribing = false;
      this.error = ev.message;
    });
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
