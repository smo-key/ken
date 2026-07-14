<script lang="ts">
  import { onMount } from "svelte";
  import type { UnlistenFn } from "@tauri-apps/api/event";
  import { api, type VideoTranscript, type ModelStatus } from "../../lib/api";
  import {
    formatCueTime,
    isTimed,
    parseVtt,
    type VttCue,
  } from "../../lib/vtt";
  import { registerDomFind } from "../../lib/find-dom.svelte";
  import PreviewLoading from "./PreviewLoading.svelte";
  import ModelDownloadDialog from "./ModelDownloadDialog.svelte";

  let { relPath }: { relPath: string } = $props();

  let videoEl = $state<HTMLVideoElement | null>(null);
  let transcriptPane = $state<HTMLDivElement | null>(null);

  // The asset-protocol URL from `media_src`; assumed to support range/seek so
  // native controls can scrub. Until the backend lands this rejects — we show a
  // plain error and the transcript pane still works.
  let videoUrl = $state<string | null>(null);
  let videoError = $state<string | null>(null);

  // "loading" is our own pre-fetch state; the other three mirror the backend.
  let status = $state<"loading" | "ready" | "generating" | "none">("loading");
  let cues = $state<VttCue[]>([]);
  let activeIdx = $state(-1);
  let generateError = $state<string | null>(null);
  // When the Whisper model is missing, offer an in-app download instead of a
  // raw blocker error; once installed we auto-continue to generation.
  let missingModel = $state<ModelStatus | null>(null);

  // Captions can't point at an external URL (CSP), so the VTT string is served
  // to the <track> as a same-doc blob. Kept in a ref so it's revoked on swap.
  let trackUrl: string | null = null;
  let trackUrlState = $state<string | null>(null);

  const timed = $derived(isTimed(cues));

  registerDomFind(() => transcriptPane, { deps: () => cues });

  function setCaptions(vtt: string | null) {
    if (trackUrl) URL.revokeObjectURL(trackUrl);
    trackUrl = null;
    if (vtt && vtt.trim()) {
      trackUrl = URL.createObjectURL(new Blob([vtt], { type: "text/vtt" }));
    }
    trackUrlState = trackUrl;
  }

  function applyTranscript(t: VideoTranscript) {
    status = t.status;
    cues = t.vtt ? parseVtt(t.vtt) : [];
    activeIdx = -1;
    setCaptions(t.vtt);
  }

  async function loadTranscript(rel: string) {
    try {
      const t = await api.videoTranscript(rel);
      if (rel !== relPath) return; // a newer file took over while we awaited
      applyTranscript(t);
    } catch {
      if (rel !== relPath) return;
      // Backend not live yet: degrade to "none" so the user still sees the pane.
      status = "none";
      cues = [];
      setCaptions(null);
    }
  }

  async function generate() {
    generateError = null;
    // Gate on the model first: if it's missing, show the downloader rather than
    // a raw "download it yourself" blocker. Other prerequisites (ffmpeg) still
    // surface through generateTranscript's error below.
    try {
      const model = await api.modelStatus();
      if (!model.installed) {
        missingModel = model;
        return;
      }
    } catch {
      // Status check unavailable — fall through and let generate report why.
    }
    try {
      await api.generateTranscript(relPath);
      status = "generating"; // Whisper runs in the background; index-updated wakes us
    } catch (e) {
      // Whisper prerequisites can be missing — surface it rather than swallow it.
      generateError = `${e}`;
    }
  }

  function onModelInstalled() {
    // The model just landed — drop the downloader and proceed automatically.
    missingModel = null;
    void generate();
  }

  function seek(cue: VttCue) {
    if (videoEl) videoEl.currentTime = cue.start;
  }

  function onTimeUpdate() {
    if (!videoEl || !timed) return;
    const t = videoEl.currentTime;
    // Prefer the cue whose window contains `t`; between cues, keep the last one
    // that has started so the highlight doesn't flicker off during pauses.
    let idx = -1;
    for (let i = 0; i < cues.length; i++) {
      if (t >= cues[i].start && (cues[i].end <= 0 || t < cues[i].end)) {
        idx = i;
        break;
      }
      if (cues[i].start <= t) idx = i;
    }
    if (idx === activeIdx) return;
    activeIdx = idx;
    if (idx >= 0) {
      transcriptPane
        ?.querySelector(`[data-cue="${idx}"]`)
        ?.scrollIntoView({ block: "nearest" });
    }
  }

  // Reload everything when the previewed file changes.
  $effect(() => {
    const rel = relPath;
    videoUrl = null;
    videoError = null;
    status = "loading";
    cues = [];
    activeIdx = -1;
    generateError = null;
    missingModel = null;

    api
      .mediaSrc(rel)
      .then((url) => {
        if (rel === relPath) videoUrl = url;
      })
      .catch((e) => {
        if (rel === relPath) videoError = `Couldn't load this video — ${e}.`;
      });
    loadTranscript(rel);
  });

  onMount(() => {
    let unlisten: UnlistenFn | undefined;
    // Whisper writes its .vtt into the index; the existing event tells us to swap
    // the "generating" placeholder for the real transcript.
    api
      .onIndexUpdated(() => {
        if (status === "generating") loadTranscript(relPath);
      })
      .then((u) => (unlisten = u));
    return () => {
      unlisten?.();
      if (trackUrl) URL.revokeObjectURL(trackUrl);
    };
  });
</script>

<div class="wrap">
  <div class="split">
    <div class="stage">
      {#if videoError}
        <div class="note error">{videoError}</div>
      {:else if videoUrl}
        <!-- svelte-ignore a11y_media_has_caption — captions are attached below when a transcript exists -->
        <video
          bind:this={videoEl}
          class="player"
          controls
          preload="metadata"
          src={videoUrl}
          ontimeupdate={onTimeUpdate}
        >
          {#if trackUrlState}
            <track
              kind="captions"
              label="Captions"
              srclang="en"
              src={trackUrlState}
              default
            />
          {/if}
        </video>
      {:else}
        <PreviewLoading label="Loading video…" />
      {/if}
    </div>

    <div class="transcript" bind:this={transcriptPane}>
      {#if status === "loading"}
        <PreviewLoading label="Loading transcript…" />
      {:else if status === "generating"}
        <PreviewLoading
          label="Transcribing this video…"
          detail="On-device transcription is running in the background. The transcript and captions will appear here when it finishes."
        />
      {:else if status === "none"}
        <div class="note">
          <p class="lead">No transcript</p>
          {#if missingModel}
            <p>
              Generating a transcript needs the on-device speech model. Download
              it once and Ken will transcribe this video right after.
            </p>
            <ModelDownloadDialog
              status={missingModel}
              onInstalled={onModelInstalled}
            />
          {:else}
            <button class="generate" onclick={generate}
              >Generate transcript</button
            >
          {/if}
          {#if generateError}
            <p class="error">{generateError}</p>
          {/if}
        </div>
      {:else if cues.length === 0}
        <div class="note">This transcript has no text.</div>
      {:else if timed}
        <ul class="cues">
          {#each cues as cue, i (i)}
            <li>
              <button
                class="cue"
                class:active={i === activeIdx}
                data-cue={i}
                onclick={() => seek(cue)}
              >
                <span class="time">{formatCueTime(cue.start)}</span>
                <span class="text">{cue.text}</span>
              </button>
            </li>
          {/each}
        </ul>
      {:else}
        <div class="cues plain">
          {#each cues as cue, i (i)}
            <p class="para" data-cue={i}>{cue.text}</p>
          {/each}
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .wrap {
    flex: 1;
    min-height: 0;
    display: flex;
    container-type: inline-size;
  }
  .split {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .stage {
    flex: 1 1 60%;
    min-width: 0;
    min-height: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--sunken);
    padding: 20px;
  }
  .player {
    max-width: 100%;
    max-height: 100%;
    border-radius: 6px;
    box-shadow: var(--shadow-card);
    background: black;
  }
  .transcript {
    flex: 1 1 40%;
    min-width: 0;
    min-height: 0;
    max-width: 480px;
    overflow-y: auto;
    border-left: 1px solid var(--border);
    background: var(--paper);
    padding: 16px;
  }
  /* On a narrow pane, stack rather than crush both halves. */
  @container (max-width: 640px) {
    .split {
      flex-direction: column;
    }
    .stage {
      flex: 0 0 auto;
      max-height: 55%;
    }
    .transcript {
      flex: 1 1 auto;
      max-width: none;
      border-left: none;
      border-top: 1px solid var(--border);
    }
  }
  .cues {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .cue {
    display: flex;
    gap: 10px;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    border-radius: 6px;
    padding: 6px 8px;
    cursor: pointer;
    color: var(--ink-secondary);
    font: inherit;
    font-size: 13px;
    line-height: 1.5;
  }
  .cue:hover {
    background: var(--sunken);
  }
  .cue.active {
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    color: var(--ink);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .time {
    flex: none;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-tertiary);
    padding-top: 1px;
    font-variant-numeric: tabular-nums;
  }
  .cue.active .time {
    color: var(--accent);
  }
  .text {
    flex: 1;
    min-width: 0;
    white-space: pre-wrap;
  }
  .plain {
    gap: 12px;
  }
  .para {
    margin: 0;
    font-size: 13.5px;
    line-height: 1.7;
    color: var(--ink-secondary);
  }
  .note {
    color: var(--ink-tertiary);
    font-size: 13px;
    padding: 12px 4px;
    line-height: 1.6;
  }
  .note .lead {
    color: var(--ink-secondary);
    font-weight: 500;
    margin: 0 0 6px;
  }
  .note p {
    margin: 0 0 6px;
  }
  .note.error,
  .error {
    color: var(--danger);
  }
  .generate {
    margin-top: 4px;
    padding: 7px 14px;
    border: 1px solid var(--border-strong);
    border-radius: 8px;
    background: var(--accent);
    color: white;
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }
  .generate:hover {
    background: var(--accent-hover);
  }
</style>
