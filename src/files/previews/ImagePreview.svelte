<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { api, type OcrRegion } from "../../lib/api";
  import { MATCH_CAP, findTextMatches } from "../../lib/find";
  import { find, type FindAdapter } from "../../lib/find.svelte";
  import { registerEmptyFind } from "../../lib/find-dom.svelte";
  import { mimeForPath } from "../../lib/mime";
  import PreviewLoading from "./PreviewLoading.svelte";

  let { relPath }: { relPath: string } = $props();

  let url = $state<string | null>(null);
  let error = $state<string | null>(null);

  let overlay: HTMLDivElement | undefined = $state();
  let regions: OcrRegion[] = [];
  /** True once we know there is OCR text to search; drives which adapter runs. */
  let hasText = $state(false);
  let boxes: HTMLElement[] = [];

  function clearBoxes() {
    for (const box of boxes) box.remove();
    boxes = [];
  }

  function draw(bbox: [number, number, number, number], current: boolean): HTMLElement | null {
    if (!overlay) return null;
    const [x, y, w, h] = bbox;
    // bbox is normalized, top-left origin: position with percentages so the
    // boxes track the image as it scales (max-width/max-height: 100%).
    const box = document.createElement("div");
    box.className = current ? "ocr-hit current" : "ocr-hit";
    box.style.left = `${x * 100}%`;
    box.style.top = `${y * 100}%`;
    box.style.width = `${w * 100}%`;
    box.style.height = `${h * 100}%`;
    overlay.appendChild(box);
    boxes.push(box);
    return box;
  }

  // A surface with no OCR text (still processing, non-macOS, or a blank image)
  // degrades to the empty adapter so the bar reads honestly.
  registerEmptyFind("No recognized text in this image.");

  $effect(() => {
    if (!hasText) return; // empty adapter above handles the no-text case
    const adapter: FindAdapter = {
      search(query) {
        clearBoxes();
        if (!query.trim()) return { total: 0 };

        // One box per hit, in region order — findTextMatches gives us the count
        // of occurrences in each region's text.
        const found: [number, number, number, number][] = [];
        for (let r = 0; r < regions.length && found.length < MATCH_CAP; r++) {
          const region = regions[r];
          const count = findTextMatches(
            region.text,
            query,
            MATCH_CAP - found.length,
          ).starts.length;
          for (let i = 0; i < count; i++) found.push(region.bbox);
        }

        for (const bbox of found) draw(bbox, false);
        return { total: found.length, capped: found.length >= MATCH_CAP };
      },
      reveal(index, opts) {
        boxes.forEach((box, i) => box.classList.toggle("current", i === index));
        if (opts?.scroll !== false) {
          boxes[index]?.scrollIntoView({ block: "center", inline: "nearest" });
        }
      },
      clear() {
        clearBoxes();
      },
    };
    find.register(adapter);
    return () => {
      clearBoxes();
      find.unregister(adapter);
    };
  });

  onMount(async () => {
    try {
      const bytes = await api.readFileBytes(relPath);
      const type = mimeForPath(relPath);
      url = URL.createObjectURL(
        type ? new Blob([bytes], { type }) : new Blob([bytes]),
      );
    } catch (e) {
      error = `Couldn't load this image — ${e}.`;
    }

    // OCR is independent of rendering — a failure here just means no find, not a
    // broken preview.
    try {
      const found = await api.getOcrRegions(relPath);
      if (found.length) {
        regions = found;
        hasText = true;
        find.refresh(); // regions arrived after the user already typed a query
      }
    } catch {
      // Non-macOS or still processing: leave the empty adapter in place.
    }
  });

  onDestroy(() => {
    if (url) URL.revokeObjectURL(url);
  });
</script>

<div class="scroll">
  {#if error}
    <div class="note error">{error}</div>
  {:else if url}
    <div class="frame">
      <img src={url} alt={relPath} />
      <div class="image-overlay" bind:this={overlay}></div>
    </div>
  {:else}
    <PreviewLoading label="Loading image…" />
  {/if}
</div>

<style>
  .scroll {
    flex: 1;
    min-height: 0;
    overflow: auto;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--sunken);
    padding: 24px;
  }
  .frame {
    position: relative;
    display: inline-flex;
    max-width: 100%;
    max-height: 100%;
  }
  img {
    max-width: 100%;
    max-height: 100%;
    border-radius: 6px;
    box-shadow: var(--shadow-card);
  }
  .image-overlay {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .image-overlay :global(.ocr-hit) {
    position: absolute;
    background: color-mix(in srgb, var(--needs-input) 45%, transparent);
    border-radius: 2px;
    mix-blend-mode: multiply;
  }
  .image-overlay :global(.ocr-hit.current) {
    background: color-mix(in srgb, var(--accent) 50%, transparent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .note {
    color: var(--ink-tertiary);
    font-size: 13px;
  }
  .note.error {
    color: var(--danger);
  }
</style>
