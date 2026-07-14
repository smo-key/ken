<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { digest } from "../lib/digest.svelte";
  import { ingests } from "../lib/ingests.svelte";
  import { review } from "../lib/review.svelte";
  import { digestMarkdown } from "../lib/assist";
  import { isProjectLink, renderMarkdown } from "../lib/markdown";
  import { homeRecents, recentlyOpened } from "../lib/recent";
  import HomeSearch from "./HomeSearch.svelte";
  import HomeStatus from "./HomeStatus.svelte";
  import RecentFiles from "./RecentFiles.svelte";
  import ContextMenu, { openContextMenu } from "../lib/ui/ContextMenu.svelte";
  import Check from "@lucide/svelte/icons/check";
  import Copy from "@lucide/svelte/icons/copy";
  import BellOff from "@lucide/svelte/icons/bell-off";
  import Eye from "@lucide/svelte/icons/eye";

  // Right-click a "Needs a look" row to ignore that file's issues (per-user,
  // never synced) or open it in Files.
  function failedRowMenu(e: MouseEvent, relPath: string) {
    e.preventDefault();
    openContextMenu(e.clientX, e.clientY, [
      { label: "Open in Files", icon: Eye, onSelect: () => app.openInFiles(relPath) },
      "separator",
      { label: "Ignore this file", icon: BellOff, onSelect: () => void app.ignoreFile(relPath) },
    ]);
  }

  onMount(() => void digest.init());

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

  // Own history when there is one, most-recently-modified files otherwise;
  // empty (a project with no files at all) renders no section.
  const recents = $derived(homeRecents(app.recents, app.files));
  const recentsFromHistory = $derived(
    recentlyOpened(app.recents, app.files).length > 0,
  );

  const hasWaiting = $derived(
    ingests.pending.length > 0 ||
      blockedSlugs.length > 0 ||
      otherReviewCount > 0,
  );
  const hasNeedsLook = $derived(app.failedFiles.length > 0);

  // Reading order: the day, then what Ken wrote, then what wants a human, then
  // the ways in. Three of these sections are conditional, so the entrance
  // stagger is indexed off the sections actually rendered — a hard-coded --d
  // would leave a dead beat wherever a section is absent.
  const sections = $derived([
    "masthead",
    "digest",
    ...(hasWaiting ? ["waiting"] : []),
    ...(hasNeedsLook ? ["needs-look"] : []),
    "search",
    ...(recents.length > 0 ? ["recents"] : []),
    "status",
  ]);
  const delay = (id: string) => sections.indexOf(id);
</script>

<div class="wrap">
  <div class="inner">
    <!-- Masthead: the day, set like a paper's dateline -->
    <header class="masthead rise" style="--d: {delay('masthead')}">
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
    <section class="digest rise" style="--d: {delay('digest')}">
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

    {#if hasWaiting}
      <section class="rise" style="--d: {delay('waiting')}">
        <div class="overline amber">Waiting on you</div>
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

    {#if hasNeedsLook}
      <section class="rise" style="--d: {delay('needs-look')}">
        <div class="overline">Needs a look</div>
        <div class="group">
          {#each app.failedFiles.slice(0, 5) as f (f.relPath)}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="row" oncontextmenu={(e) => failedRowMenu(e, f.relPath)}>
              <span class="rdot"></span>
              <div class="rtext">
                <strong>{f.relPath.split("/").pop()}</strong> couldn't be indexed —
                {f.error ?? "unknown reason"}. It's still findable by name.
              </div>
              <button
                class="btn btn-small ghost"
                title="Ignore this file's issues (only for you)"
                onclick={() => void app.ignoreFile(f.relPath)}
              >
                Ignore
              </button>
              <button class="btn btn-small" onclick={() => app.openInFiles(f.relPath)}>View</button>
            </div>
          {/each}
        </div>
      </section>
    {/if}

    <!-- The page's primary action: opens the ⌘K palette, which owns search. -->
    <section class="find rise" style="--d: {delay('search')}">
      <HomeSearch />
    </section>

    {#if recents.length > 0}
      <section class="rise" style="--d: {delay('recents')}">
        <RecentFiles rows={recents} fromHistory={recentsFromHistory} />
      </section>
    {/if}

    <!-- Footer: a few at-a-glance stats and where team-sync stands -->
    <section class="status rise" style="--d: {delay('status')}">
      <HomeStatus />
    </section>

  </div>
</div>

<ContextMenu />

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

  /* ── Search: the page's one loud thing ────────────────── */
  .find {
    margin-top: 40px;
  }

  /* Footer spacing; the tiles/sync visual language lives in HomeStatus. */
  .status {
    margin-top: 36px;
  }

  /* ── Grouped attention lists ──────────────────────────── */
  section {
    margin-top: 36px;
  }
  /* Typography lives on the global `.overline`; only the spacing is local. */
  .overline {
    margin-bottom: 8px;
  }
  .overline.amber {
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
  /* Secondary "Ignore" action reads quieter than the primary "View". */
  .btn.ghost {
    background: transparent;
    color: var(--ink-tertiary);
  }
  .btn.ghost:hover {
    color: var(--ink);
  }
</style>
