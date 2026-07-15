<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { record } from "../lib/record.svelte";
  import LevelMeter from "../record/LevelMeter.svelte";
  import PermissionNotice from "../record/PermissionNotice.svelte";
  import ModelDownloadDialog from "../files/previews/ModelDownloadDialog.svelte";
  import Mic from "@lucide/svelte/icons/mic";
  import Speaker from "@lucide/svelte/icons/volume-2";

  onMount(() => void record.init());

  function clock(ms: number): string {
    const s = Math.floor(ms / 1000);
    const m = Math.floor(s / 60);
    const rem = s % 60;
    return `${m}:${rem.toString().padStart(2, "0")}`;
  }

  const canStart = $derived(
    (record.micOn || record.systemOn) && record.modelReady,
  );

  function openSaved() {
    if (record.savedPath) app.openInFiles(record.savedPath);
  }
</script>

<div class="screen">
  <div class="inner">
    <header>
      <h1>Record</h1>
      <p class="lead">
        Capture a meeting or a call. Ken keeps a private transcript — nothing
        you say leaves your Mac.
      </p>
    </header>

    <section class="sources">
      <label class="source" class:on={record.micOn}>
        <input type="checkbox" bind:checked={record.micOn} disabled={record.recording} />
        <span class="src-icon"><Mic size={15} strokeWidth={1.75} /></span>
        <span class="src-body">
          <span class="src-name">Me</span>
          <span class="src-sub">Your microphone</span>
          <LevelMeter level={record.micLevel} active={record.recording && record.micOn} />
        </span>
      </label>

      <label class="source" class:on={record.systemOn}>
        <input type="checkbox" bind:checked={record.systemOn} disabled={record.recording} />
        <span class="src-icon"><Speaker size={15} strokeWidth={1.75} /></span>
        <span class="src-body">
          <span class="src-name">Them</span>
          <span class="src-sub">System audio — the other side of a call</span>
          <LevelMeter level={record.systemLevel} active={record.recording && record.systemOn} />
        </span>
      </label>
    </section>

    {#if record.micOn}
      <PermissionNotice
        status={record.micPerm}
        label="microphone"
        settingsUrl={record.micSettingsUrl}
        onRequest={() => void record.requestMic()}
      />
    {/if}
    {#if record.systemOn}
      <PermissionNotice
        status={record.screenPerm}
        label="system audio (Screen Recording)"
        settingsUrl={record.screenSettingsUrl}
        onRequest={() => void record.requestScreen()}
      />
    {/if}

    {#if record.micOn && record.devices.length > 1 && !record.recording}
      <div class="device">
        <span class="device-label">Microphone</span>
        <select bind:value={record.deviceId}>
          {#each record.devices as d (d.id)}
            <option value={d.id}>{d.name}</option>
          {/each}
        </select>
      </div>
    {/if}

    {#if record.modelStatus && !record.modelStatus.installed && !record.recording}
      <div class="model-gate">
        <p class="model-gate-lead">
          Recording needs the on-device speech model. Download it once and Ken
          will transcribe your recordings — nothing leaves your Mac.
        </p>
        <ModelDownloadDialog
          status={record.modelStatus}
          onInstalled={() => void record.refreshModelStatus()}
        />
      </div>
    {/if}

    <section class="controls">
      <div class="clock" class:live={record.phase === "recording" && !record.transcribing}>
        {clock(record.elapsedMs)}
      </div>

      {#if !record.recording}
        <button class="rec" disabled={!canStart} onclick={() => void record.start()}>
          <span class="rec-dot"></span> Record
        </button>
      {:else if !record.transcribing}
        {#if record.phase === "recording"}
          <button class="btn" onclick={() => void record.pause()}>Pause</button>
        {:else}
          <button class="btn" onclick={() => void record.resume()}>Resume</button>
        {/if}
        <button class="btn btn-primary" onclick={() => void record.stop()}>Stop &amp; save</button>
        <button class="btn btn-quiet" onclick={() => void record.cancel()}>Discard</button>
      {/if}
    </section>

    {#if record.recording && !record.transcribing}
      <div class="storage">
        <span class="storage-label">Keep</span>
        <div class="seg">
          <button class:sel={record.storage === "transcript"} onclick={() => (record.storage = "transcript")}>Transcript</button>
          <button class:sel={record.storage === "audio"} onclick={() => (record.storage = "audio")}>Audio</button>
          <button class:sel={record.storage === "both"} onclick={() => (record.storage = "both")}>Both</button>
        </div>
      </div>
    {/if}

    {#if record.transcribing}
      <p class="status">Transcribing on your Mac…</p>
    {/if}
    {#if record.savedPath}
      <p class="status done">
        Saved. <button class="link" onclick={openSaved}>Open transcript</button>
      </p>
    {/if}
    {#if record.error}
      <p class="status err">{record.error}</p>
    {/if}
  </div>
</div>

<style>
  .screen {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 40px;
  }
  .inner {
    max-width: 560px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 20px;
  }
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 28px;
    font-weight: 500;
  }
  .lead {
    margin: 6px 0 0;
    font-family: var(--font-serif);
    font-size: 15px;
    line-height: 1.6;
    color: var(--ink-secondary);
  }
  .sources {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .source {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 14px 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    background: var(--surface);
    cursor: pointer;
  }
  .source.on {
    border-color: color-mix(in srgb, var(--accent) 40%, var(--border));
  }
  .source input {
    margin-top: 2px;
  }
  .src-icon {
    color: var(--ink-tertiary);
    margin-top: 1px;
  }
  .src-body {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1;
    min-width: 0;
  }
  .src-name {
    font-weight: 600;
    font-size: 13.5px;
  }
  .src-sub {
    font-size: 12px;
    color: var(--ink-tertiary);
  }
  .device {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12.5px;
  }
  .device-label {
    color: var(--ink-tertiary);
  }
  .device select {
    flex: 1;
    padding: 6px 8px;
    border-radius: 8px;
    border: 1px solid var(--border);
    background: var(--surface);
    font-size: 12.5px;
  }
  .model-gate {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 14px 16px;
    border: 1px solid color-mix(in srgb, var(--accent) 30%, var(--border));
    border-radius: var(--radius-card);
    background: color-mix(in srgb, var(--accent) 5%, transparent);
  }
  .model-gate-lead {
    margin: 0;
    font-size: 12.5px;
    line-height: 1.6;
    color: var(--ink-secondary);
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .clock {
    font-family: var(--font-mono);
    font-size: 22px;
    color: var(--ink-secondary);
    min-width: 72px;
  }
  .clock.live {
    color: var(--accent-deep);
  }
  .rec {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 10px 18px;
    border-radius: 22px;
    border: none;
    background: var(--accent);
    color: var(--surface);
    font-size: 13.5px;
    font-weight: 600;
    cursor: pointer;
  }
  .rec:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .rec-dot {
    width: 9px;
    height: 9px;
    border-radius: 5px;
    background: var(--surface);
  }
  .btn {
    padding: 9px 15px;
    border-radius: 20px;
    border: 1px solid var(--border);
    background: var(--surface);
    font-size: 13px;
    cursor: pointer;
  }
  .btn-primary {
    background: var(--accent);
    color: var(--surface);
    border-color: transparent;
    font-weight: 600;
  }
  .btn-quiet {
    color: var(--ink-tertiary);
  }
  .storage {
    display: flex;
    align-items: center;
    gap: 12px;
    font-size: 12.5px;
  }
  .storage-label {
    color: var(--ink-tertiary);
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
  }
  .seg button {
    padding: 6px 12px;
    border: none;
    background: var(--surface);
    font-size: 12.5px;
    cursor: pointer;
    color: var(--ink-secondary);
  }
  .seg button.sel {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent-deep);
    font-weight: 600;
  }
  .status {
    font-size: 12.5px;
    color: var(--ink-secondary);
  }
  .status.done {
    color: var(--healthy);
  }
  .status.err {
    color: var(--danger);
  }
  .link {
    border: none;
    background: none;
    color: var(--accent);
    font-weight: 600;
    text-decoration: underline;
    cursor: pointer;
    font-size: 12.5px;
  }
</style>
