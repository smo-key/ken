<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { digest } from "../lib/digest.svelte";
  import { ingests } from "../lib/ingests.svelte";
  import { review } from "../lib/review.svelte";
  import { digestMarkdown } from "../lib/assist";
  import { isProjectLink, renderMarkdown } from "../lib/markdown";
  import { timeAgo } from "../lib/format";
  import ResearchModal from "../research/ResearchModal.svelte";

  onMount(() => void digest.init());

  let researchOpen = $state(false);
  let copied = $state(false);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;

  async function share() {
    if (!digest.digest) return;
    await navigator.clipboard.writeText(digestMarkdown(digest.digest));
    copied = true;
    if (copyTimer) clearTimeout(copyTimer);
    copyTimer = setTimeout(() => (copied = false), 1500);
  }

  function digestTime(epoch: number): string {
    return new Date(epoch * 1000).toLocaleTimeString(undefined, {
      hour: "numeric",
      minute: "2-digit",
    });
  }

  function chipLabel(relPath: string): string {
    return relPath.split("/").pop() || relPath;
  }

  /** Project-relative links in the digest prose open in Files. */
  function onDigestClick(e: MouseEvent) {
    const a = (e.target as HTMLElement).closest("a");
    if (a) {
      e.preventDefault();
      const href = a.getAttribute("href") ?? "";
      if (isProjectLink(href)) app.openInFiles(href);
    }
  }

  const blockedSlugs = $derived(
    Object.values(ingests.live)
      .filter((ev) => ev.status === "blocked")
      .map((ev) => ev.slug),
  );

  // Open Review items beyond what this screen already shows individually
  // (approvals and blocked runs above, failed files below).
  const otherReviewCount = $derived(
    review.items.filter(
      (i) => i.kind !== "approval" && i.kind !== "failed-file",
    ).length,
  );

  function openIngest(slug: string) {
    app.screen = "ingests";
    void ingests.select(slug);
  }

  const today = new Date().toLocaleDateString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
  });

  const indexedCount = $derived(app.files.filter((f) => f.status === "indexed").length);
</script>

<div class="wrap">
  <div class="inner">
    <h1>{today}</h1>

    <div class="card">
      <div class="head">
        <span class="overline-accent">Today's digest</span>
        {#if digest.digest}
          <span class="when">
            {digestTime(digest.digest.generatedAt)} ·
            <button class="share" onclick={share}>{copied ? "Copied" : "share"}</button>
          </span>
        {/if}
      </div>
      {#if digest.digest}
<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
        <div class="digest-body" onclick={onDigestClick}>
          {@html renderMarkdown(digest.digest.body)}
        </div>
        {#if digest.digest.sources.length > 0}
          <div class="chips">
            {#each digest.digest.sources as source (source)}
              <button
                class="chip mono"
                title={source}
                onclick={() => app.openInFiles(source)}
              >
                {chipLabel(source)}
              </button>
            {/each}
          </div>
        {/if}
      {:else if digest.generating}
        <p class="pulse">Ken is writing today's digest…</p>
      {:else if digest.claudeFound}
        <p>Ken writes you a morning digest — it'll appear here.</p>
        {#if digest.error}
          <p class="digest-error">Last try didn't finish — {digest.error}</p>
        {/if}
        <div class="actions">
          <button class="btn" onclick={() => void digest.writeNow()}>Write it now</button>
        </div>
      {:else}
        <p>
          The daily digest needs Claude Code — once it's installed, Ken
          writes you one each morning.
        </p>
      {/if}
    </div>

    <div class="card">
      <div class="head">
        <span class="overline-accent">Your knowledge</span>
        {#if app.lastScanAt}
          <span class="when">updated {timeAgo(Math.floor(app.lastScanAt / 1000))}</span>
        {/if}
      </div>
      {#if app.scanning}
        <p>Ken is reading your folder for the first time — search lights up as files are indexed.</p>
      {:else}
        <p>
          Watching <strong>{app.project?.name}</strong> — {app.files.length}
          files known, {indexedCount} with searchable content{app.failedFiles.length
            ? `, ${app.failedFiles.length} couldn't be read`
            : ""}. Press <span class="kbd">⌘K</span> to search, or browse and edit
          anything in Files.
        </p>
      {/if}
      <div class="actions">
        <button class="btn" onclick={() => (app.searchOpen = true)}>Search knowledge</button>
        <button class="btn btn-ghost" onclick={() => (app.screen = "files")}>Browse files</button>
        <button class="btn btn-ghost" onclick={() => (researchOpen = true)}>Start research</button>
      </div>
    </div>

    {#if ingests.pending.length > 0 || blockedSlugs.length > 0 || otherReviewCount > 0}
      <div class="section-label">Waiting on you</div>
      {#each ingests.pending as run (run.id)}
        <div class="callout attention">
          <span class="cdot amber"></span>
          <div class="ctext">
            <strong>{run.slug}</strong> finished a big refresh —
            {run.summary ?? "review it before Ken writes it."}
          </div>
          <button class="btn btn-small" onclick={() => openIngest(run.slug)}>Review</button>
        </div>
      {/each}
      {#each blockedSlugs as slug (slug)}
        <div class="callout attention">
          <span class="cdot amber"></span>
          <div class="ctext"><strong>{slug}</strong> is waiting on your input.</div>
          <button class="btn btn-small" onclick={() => openIngest(slug)}>Open</button>
        </div>
      {/each}
      {#if otherReviewCount > 0}
        <div class="callout attention">
          <span class="cdot amber"></span>
          <div class="ctext">
            {otherReviewCount === 1
              ? "One more thing is"
              : `${otherReviewCount} more things are`} waiting in Review — documents
            going stale or things Ken couldn't handle alone.
          </div>
          <button class="btn btn-small" onclick={() => (app.screen = "review")}>Open Review</button>
        </div>
      {/if}
    {/if}

    {#if app.failedFiles.length > 0}
      <div class="section-label">Needs a look</div>
      {#each app.failedFiles.slice(0, 5) as f (f.relPath)}
        <div class="callout">
          <span class="cdot"></span>
          <div class="ctext">
            <strong>{f.relPath.split("/").pop()}</strong> couldn't be indexed —
            {f.error ?? "unknown reason"}. It's still findable by name.
          </div>
          <button class="btn btn-small" onclick={() => app.openInFiles(f.relPath)}>View</button>
        </div>
      {/each}
    {/if}

  </div>
</div>

{#if researchOpen}
  <ResearchModal close={() => (researchOpen = false)} />
{/if}

<style>
  .wrap {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    padding: 36px 44px;
  }
  .inner {
    max-width: 720px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  h1 {
    margin: 0 0 10px;
    font-family: var(--font-serif);
    font-size: 30px;
    font-weight: 500;
    letter-spacing: -0.01em;
  }
  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    padding: 22px 24px;
    box-shadow: var(--shadow-card);
  }
  .card.muted p {
    color: var(--ink-tertiary);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 9px;
    margin-bottom: 10px;
  }
  .overline-accent {
    font-size: 11.5px;
    font-weight: 700;
    color: var(--accent-deep);
    letter-spacing: 0.07em;
    text-transform: uppercase;
  }
  .when {
    margin-left: auto;
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  p {
    margin: 0;
    font-size: 14px;
    line-height: 1.7;
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-top: 14px;
  }
  .section-label {
    font-size: 11.5px;
    font-weight: 700;
    color: var(--accent-deep);
    letter-spacing: 0.07em;
    text-transform: uppercase;
    margin-top: 8px;
  }
  .callout {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    background: var(--surface);
    border: 1px solid rgba(163, 77, 63, 0.3);
    border-radius: 10px;
    padding: 14px 16px;
  }
  .cdot {
    width: 8px;
    height: 8px;
    border-radius: 4px;
    background: var(--danger);
    margin-top: 5px;
    flex: none;
  }
  .cdot.amber {
    background: var(--needs-input);
  }
  .callout.attention {
    border-color: rgba(168, 116, 44, 0.3);
  }
  .ctext {
    flex: 1;
    font-size: 13px;
    line-height: 1.6;
  }
  .card .overline {
    display: block;
    margin-bottom: 8px;
  }
  .share {
    border: none;
    background: transparent;
    padding: 0;
    font-size: 11.5px;
    font-family: inherit;
    color: var(--accent);
    cursor: pointer;
  }
  .share:hover {
    text-decoration: underline;
  }
  .digest-body {
    font-size: 14px;
    line-height: 1.7;
  }
  .digest-body :global(p) {
    margin: 0 0 8px;
  }
  .digest-body :global(p:last-child) {
    margin-bottom: 0;
  }
  .chips {
    display: flex;
    gap: 8px;
    margin-top: 14px;
    flex-wrap: wrap;
  }
  .chip {
    font-size: 11px;
    color: var(--ink-secondary);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 2px 8px;
    background: var(--sunken);
    cursor: pointer;
  }
  .chip:hover {
    border-color: var(--accent);
  }
  .pulse {
    color: var(--ink-tertiary);
    animation: digest-pulse 1.6s ease-in-out infinite;
  }
  @keyframes digest-pulse {
    0%,
    100% {
      opacity: 0.45;
    }
    50% {
      opacity: 1;
    }
  }
  .digest-error {
    margin-top: 8px;
    font-size: 12px;
    color: var(--ink-tertiary);
  }
</style>
