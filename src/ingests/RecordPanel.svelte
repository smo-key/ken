<script lang="ts">
  // Record, as a way to add a source on Ingest: the microphone and, where
  // the machine can capture it, the system audio (the other side of a call).
  // Stop and the take is transcribed on this computer, saved to the team
  // wiki's Raw/ and read at once. The title bar shows a take that runs on.
  import { onMount } from "svelte";
  import { record, recordClock } from "../lib/record.svelte";
  import { systemAudioLabel } from "../lib/platform";
  import LevelMeter from "../record/LevelMeter.svelte";
  import PermissionNotice from "../record/PermissionNotice.svelte";
  import ModelDownloadDialog from "../files/previews/ModelDownloadDialog.svelte";
  import ProgressBar from "../lib/ProgressBar.svelte";
  import Mic from "@lucide/svelte/icons/mic";
  import Speaker from "@lucide/svelte/icons/volume-2";

  let { team }: { team: string | null } = $props();

  onMount(() => void record.init());

  const canCapture = $derived(record.micSupported || record.systemSupported);
  const canStart = $derived(
    ((record.micOn && record.micSupported) || (record.systemOn && record.systemSupported)) && record.modelReady,
  );
</script>

<div class="record">
  {#if !canCapture}
    <p class="warn">{record.support?.reason ?? "This computer has nothing Ken can record from."}</p>
  {:else}
    <div class="sources">
      {#if record.micSupported}
        <label class="source" class:on={record.micOn}>
          <input type="checkbox" bind:checked={record.micOn} disabled={record.recording} />
          <span class="src-icon"><Mic size={14} strokeWidth={1.75} /></span>
          <span class="src-body">
            <span class="src-name">Me <span class="src-sub">· the microphone</span></span>
            <LevelMeter level={record.micLevel} active={record.recording && record.micOn} />
          </span>
        </label>
      {/if}
      {#if record.systemSupported}
        <label class="source" class:on={record.systemOn}>
          <input type="checkbox" bind:checked={record.systemOn} disabled={record.recording} />
          <span class="src-icon"><Speaker size={14} strokeWidth={1.75} /></span>
          <span class="src-body">
            <span class="src-name">Them <span class="src-sub">· the system audio</span></span>
            <LevelMeter level={record.systemLevel} active={record.recording && record.systemOn} />
          </span>
        </label>
      {:else if record.support?.reason}
        <p class="note">{record.support.reason}</p>
      {/if}
    </div>

    {#if record.micOn && record.micSupported}
      <PermissionNotice
        status={record.micPerm}
        label="microphone"
        settingsUrl={record.micSettingsUrl}
        onRequest={() => void record.requestMic()}
      />
    {/if}
    {#if record.systemOn && record.systemSupported}
      <PermissionNotice
        status={record.screenPerm}
        label={systemAudioLabel()}
        settingsUrl={record.screenSettingsUrl}
        onRequest={() => void record.requestScreen()}
      />
    {/if}

    {#if record.micOn && record.devices.length > 1 && !record.recording}
      <label class="device">
        <span class="device-label">Microphone</span>
        <select bind:value={record.deviceId}>
          {#each record.devices as d (d.id)}
            <option value={d.id}>{d.name}</option>
          {/each}
        </select>
      </label>
    {/if}

    {#if record.modelStatus && !record.modelStatus.installed && !record.recording}
      <div class="model-gate">
        <p class="note">Recording needs a transcription model. It runs on this computer.</p>
        <ModelDownloadDialog status={record.modelStatus} compact onInstalled={() => void record.refreshModelStatus()} />
      </div>
    {/if}

    <div class="controls">
      <span class="clock" class:live={record.phase === "recording" && !record.transcribing}>
        {recordClock(record.elapsedMs)}
      </span>
      {#if !record.recording}
        <button class="btn btn-small btn-primary rec" disabled={!canStart || record.transcribing} onclick={() => void record.start(team)}>
          <span class="rec-dot"></span> Record
        </button>
      {:else if !record.transcribing}
        {#if record.phase === "recording"}
          <button class="btn btn-small" onclick={() => void record.pause()}>Pause</button>
        {:else}
          <button class="btn btn-small" onclick={() => void record.resume()}>Resume</button>
        {/if}
        <button class="btn btn-small btn-primary" onclick={() => void record.stop()}>Stop and save</button>
        <button class="btn btn-small btn-ghost" onclick={() => void record.cancel()}>Discard</button>
      {/if}
    </div>

    {#if !record.transcribing}
      <label class="keep">
        <input
          type="checkbox"
          checked={record.storage === "both"}
          onchange={(e) => (record.storage = e.currentTarget.checked ? "both" : "transcript")}
        />
        Keep the audio too, in Ken's app data on this computer
      </label>
    {/if}

    {#if record.transcribing}
      <ProgressBar
        pct={record.transcribePct}
        label={record.transcribePct === null ? "Transcribing…" : `Transcribing… ${record.transcribePct}%`}
      />
    {/if}
    {#if record.savedPath && !record.recording && !record.transcribing}
      <p class="note done">Saved to Raw. Ken reads it now.</p>
    {/if}
    {#if record.error}
      <p class="warn">{record.error}</p>
    {/if}
  {/if}
</div>

<style>
  .record {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .sources {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .source {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 9px;
    background: var(--surface);
    cursor: pointer;
  }
  .source.on {
    border-color: color-mix(in srgb, var(--accent) 40%, var(--border));
  }
  .source input {
    accent-color: var(--accent);
  }
  .src-icon {
    display: inline-flex;
    color: var(--ink-tertiary);
  }
  .src-body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .src-name {
    font-size: 12.5px;
    font-weight: 600;
  }
  .src-sub {
    font-weight: 400;
    color: var(--ink-tertiary);
  }
  .device {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
  }
  .device-label {
    color: var(--ink-tertiary);
  }
  .device select {
    flex: 1;
    min-width: 0;
    padding: 4px 24px 4px 8px;
    border-radius: 6px;
    border: 1px solid var(--border);
    background-color: var(--surface);
    background-position: right 6px center;
    background-size: 11px;
    color: var(--ink);
    font: inherit;
    font-size: 12px;
  }
  .model-gate {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 12px;
    border: 1px solid color-mix(in srgb, var(--accent) 30%, var(--border));
    border-radius: 9px;
    background: color-mix(in srgb, var(--accent) 5%, transparent);
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .clock {
    font-family: var(--font-mono);
    font-size: 17px;
    color: var(--ink-secondary);
    min-width: 52px;
  }
  .clock.live {
    color: var(--accent-deep);
  }
  .rec {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .rec-dot {
    width: 8px;
    height: 8px;
    border-radius: 4px;
    background: var(--surface);
  }
  .keep {
    display: flex;
    align-items: flex-start;
    gap: 7px;
    font-size: 12px;
    color: var(--ink-secondary);
    cursor: pointer;
  }
  .keep input {
    accent-color: var(--accent);
    margin-top: 3px;
  }
  .note {
    margin: 0;
    font-size: 12px;
    color: var(--ink-tertiary);
  }
  .note.done {
    color: var(--healthy-text);
  }
  .warn {
    margin: 0;
    font-size: 12px;
    color: var(--danger);
  }
</style>
