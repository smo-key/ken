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
  import Check from "@lucide/svelte/icons/check";
  import Copy from "@lucide/svelte/icons/copy";
  import Search from "@lucide/svelte/icons/search";

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
    <!-- Masthead: the day, set like a paper's dateline -->
    <header class="masthead rise" style="--d: 0">
      <h1>{today}</h1>
      <div class="rule">
        <span class="rule-label">Today's digest</span>
        {#if digest.digest}
          <span class="rule-meta">
            {digestTime(digest.digest.generatedAt)}
            <button class="share" onclick={share} title="Copy as markdown">
              {#if copied}<Check size={12} strokeWidth={2} /> copied
              {:else}<Copy size={12} strokeWidth={1.75} /> share{/if}
            </button>
          </span>
        {/if}
      </div>
    </header>

    <!-- The digest is the lead story: open serif prose, no chrome -->
    <section class="digest rise" style="--d: 1">
      {#if digest.digest}
        <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
        <div class="digest-body" onclick={onDigestClick}>
          {@html renderMarkdown(digest.digest.body)}
        </div>
        {#if digest.digest.sources.length > 0}
          <div class="sources">
            <span class="from">From</span>
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
        <p class="digest-quiet pulse">Ken is writing today's digest…</p>
      {:else if digest.claudeFound}
        <p class="digest-quiet">
          Ken writes you a morning digest — what changed, what's waiting —
          and it will appear here.
        </p>
        {#if digest.error}
          <p class="digest-error">Last try didn't finish — {digest.error}</p>
        {/if}
        <div class="digest-actions">
          <button class="btn" onclick={() => void digest.writeNow()}>Write it now</button>
        </div>
      {:else}
        <p class="digest-quiet">
          The daily digest needs Claude Code — once it's installed, Ken
          writes you one each morning.
        </p>
      {/if}
    </section>

    <!-- Quiet status strip: one line about the folder, plus the day's verbs -->
    <section class="status rise" style="--d: 2">
      <p class="status-line">
        {#if app.scanning}
          Ken is reading your folder for the first time — search lights up as
          files are indexed.
        {:else}
          Watching <strong>{app.project?.name}</strong> —
          {app.files.length} files, {indexedCount} searchable{app.failedFiles.length
            ? `, ${app.failedFiles.length} unreadable`
            : ""}{app.lastScanAt
            ? ` · updated ${timeAgo(Math.floor(app.lastScanAt / 1000))}`
            : ""}
        {/if}
      </p>
      <div class="status-actions">
        <button class="btn" onclick={() => (app.searchOpen = true)}>
          <Search size={13} strokeWidth={1.75} /> Search
          <span class="kbd">⌘K</span>
        </button>
        <button class="btn btn-ghost" onclick={() => (app.screen = "files")}>Browse files</button>
        <button class="btn btn-ghost" onclick={() => (researchOpen = true)}>Start research</button>
      </div>
    </section>

    {#if ingests.pending.length > 0 || blockedSlugs.length > 0 || otherReviewCount > 0}
      <section class="rise" style="--d: 3">
        <div class="section-label amber">Waiting on you</div>
        <div class="group">
          {#each ingests.pending as run (run.id)}
            <div class="row">
              <span class="rdot amber"></span>
              <div class="rtext">
                <strong>{run.slug}</strong> finished a big refresh —
                {run.summary ?? "review it before Ken writes it."}
              </div>
              <button class="btn btn-small" onclick={() => openIngest(run.slug)}>Review</button>
            </div>
          {/each}
          {#each blockedSlugs as slug (slug)}
            <div class="row">
              <span class="rdot amber"></span>
              <div class="rtext"><strong>{slug}</strong> is waiting on your input.</div>
              <button class="btn btn-small" onclick={() => openIngest(slug)}>Open</button>
            </div>
          {/each}
          {#if otherReviewCount > 0}
            <div class="row">
              <span class="rdot amber"></span>
              <div class="rtext">
                {otherReviewCount === 1
                  ? "One more thing is"
                  : `${otherReviewCount} more things are`} waiting in Review — documents
                going stale or things Ken couldn't handle alone.
              </div>
              <button class="btn btn-small" onclick={() => (app.screen = "review")}>Open Review</button>
            </div>
          {/if}
        </div>
      </section>
    {/if}

    {#if app.failedFiles.length > 0}
      <section class="rise" style="--d: 4">
        <div class="section-label">Needs a look</div>
        <div class="group">
          {#each app.failedFiles.slice(0, 5) as f (f.relPath)}
            <div class="row">
              <span class="rdot"></span>
              <div class="rtext">
                <strong>{f.relPath.split("/").pop()}</strong> couldn't be indexed —
                {f.error ?? "unknown reason"}. It's still findable by name.
              </div>
              <button class="btn btn-small" onclick={() => app.openInFiles(f.relPath)}>View</button>
            </div>
          {/each}
        </div>
      </section>
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
    padding: 44px 44px 56px;
  }
  .inner {
    max-width: 680px;
    margin: 0 auto;
  }

  /* Entrance: one staggered rise, then still. */
  .rise {
    animation: rise 0.45s cubic-bezier(0.16, 1, 0.3, 1) both;
    animation-delay: calc(var(--d) * 60ms);
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(7px);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .rise {
      animation: none;
    }
  }

  /* ── Masthead ─────────────────────────────────────────── */
  h1 {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 31px;
    font-weight: 500;
    letter-spacing: -0.01em;
  }
  .rule {
    display: flex;
    align-items: baseline;
    gap: 10px;
    margin-top: 14px;
    padding-bottom: 9px;
    border-bottom: 1px solid var(--border);
  }
  .rule-label {
    font-size: 11px;
    font-weight: 700;
    color: var(--accent-deep);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .rule-meta {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 10px;
    font-size: 11.5px;
    color: var(--ink-tertiary);
  }
  .share {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    border: none;
    background: transparent;
    padding: 0;
    font-size: 11.5px;
    font-family: inherit;
    color: var(--accent);
    cursor: pointer;
  }
  .share:hover {
    color: var(--accent-hover);
  }

  /* ── The lead: digest prose, serif, no box ────────────── */
  .digest {
    margin-top: 18px;
  }
  .digest-body {
    font-family: var(--font-serif);
    font-size: 16px;
    line-height: 1.8;
    color: var(--ink);
    max-width: 62ch;
  }
  .digest-body :global(p) {
    margin: 0 0 12px;
  }
  .digest-body :global(p:first-child) {
    font-size: 17.5px;
  }
  .digest-body :global(p:last-child) {
    margin-bottom: 0;
  }
  .digest-body :global(a) {
    color: var(--accent-deep);
    text-decoration-color: color-mix(in srgb, var(--accent) 40%, transparent);
    text-underline-offset: 2px;
  }
  .digest-quiet {
    margin: 0;
    font-family: var(--font-serif);
    font-size: 15.5px;
    line-height: 1.75;
    color: var(--ink-secondary);
    max-width: 58ch;
  }
  .digest-error {
    margin: 8px 0 0;
    font-size: 12px;
    color: var(--ink-tertiary);
  }
  .digest-actions {
    margin-top: 14px;
  }
  .pulse {
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
  .sources {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex-wrap: wrap;
    margin-top: 18px;
  }
  .from {
    font-size: 10.5px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.08em;
    text-transform: uppercase;
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

  /* ── Status strip: the folder, in one quiet line ──────── */
  .status {
    margin-top: 44px;
    display: flex;
    align-items: center;
    gap: 16px;
    flex-wrap: wrap;
    padding: 13px 16px;
    background: var(--sunken-2);
    border: 1px solid var(--border);
    border-radius: 10px;
  }
  .status-line {
    margin: 0;
    flex: 1;
    min-width: 240px;
    font-size: 13px;
    line-height: 1.6;
    color: var(--ink-secondary);
  }
  .status-line strong {
    color: var(--ink);
  }
  .status-actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .status-actions .btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  /* ── Grouped attention lists ──────────────────────────── */
  section {
    margin-top: 36px;
  }
  .status + section {
    margin-top: 28px;
  }
  .section-label {
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-tertiary);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    margin-bottom: 8px;
  }
  .section-label.amber {
    color: var(--needs-input-text);
  }
  .group {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-card);
    box-shadow: var(--shadow-card);
  }
  .row {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    padding: 13px 16px;
  }
  .row + .row {
    border-top: 1px solid var(--border);
  }
  .rdot {
    width: 7px;
    height: 7px;
    border-radius: 4px;
    background: var(--danger);
    margin-top: 6px;
    flex: none;
  }
  .rdot.amber {
    background: var(--needs-input);
  }
  .rtext {
    flex: 1;
    font-size: 13px;
    line-height: 1.6;
  }
</style>
