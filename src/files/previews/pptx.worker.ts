// Off-main-thread unzip for the .pptx previewer. A 50 MB deck's expensive work
// is the JSZip inflate of every part plus the decode of embedded media; running
// that on the webview main thread janks scroll and input. We do it here instead.
//
// DrawingML XML still has to be parsed with DOMParser, which WebKit workers do
// NOT expose, so this worker deliberately does NOT parse XML into a render model.
// It ships the (small) XML part strings back verbatim and streams each (large)
// media entry as a transferable ArrayBuffer; the component then runs the pure
// pptx.ts parser per slide on the main thread, yielding between slides so the
// light DOM parse never blocks either.
//
// The same extractZip routine backs a main-thread fallback (see PptxPreview) for
// the rare case a Worker can't be constructed, so it takes plain callbacks.

import JSZip from "jszip";

/** All XML/rels part strings, keyed by their zip path. */
export type PartFiles = Record<string, string>;

export interface ExtractCallbacks {
  /** Called once with every .xml/.rels part decoded to a string. */
  onParts: (files: PartFiles) => void | Promise<void>;
  /** Called per media entry with its raw bytes (transferable in the worker). */
  onMedia: (path: string, buffer: ArrayBuffer) => void | Promise<void>;
  /** Cooperative-cancel hook checked between entries. */
  shouldCancel?: () => boolean;
}

/**
 * Unzip a .pptx: decode every XML/rels part to a string (small, one batch) then
 * stream each ppt/media entry's bytes (large, one at a time). Awaits each media
 * callback so a main-thread caller can yield the event loop between entries.
 */
export async function extractZip(
  bytes: ArrayBuffer | Uint8Array,
  cb: ExtractCallbacks,
): Promise<void> {
  const zip = await JSZip.loadAsync(bytes);

  const files: PartFiles = {};
  const mediaPaths: string[] = [];
  for (const path of Object.keys(zip.files)) {
    const entry = zip.files[path];
    if (entry.dir) continue;
    if (/\.(xml|rels)$/i.test(path)) {
      files[path] = await entry.async("string");
    } else if (/^ppt\/media\//i.test(path)) {
      mediaPaths.push(path);
    }
    if (cb.shouldCancel?.()) return;
  }
  await cb.onParts(files);

  for (const path of mediaPaths) {
    if (cb.shouldCancel?.()) return;
    const buffer = await zip.files[path].async("arraybuffer");
    await cb.onMedia(path, buffer);
  }
}

// --- Worker entry point (only when actually running inside a Worker) ----------
// Importing this module on the main thread (for the fallback) must not register
// a message handler, so guard on the absence of `document`.
if (typeof document === "undefined" && typeof self !== "undefined") {
  // `self` is typed as Window under the DOM lib; in a worker it's a
  // DedicatedWorkerGlobalScope whose postMessage takes a transfer list.
  const ctx = self as unknown as {
    onmessage: ((e: MessageEvent) => void) | null;
    postMessage: (message: unknown, transfer?: Transferable[]) => void;
  };
  ctx.onmessage = async (e: MessageEvent) => {
    const bytes = e.data?.bytes as ArrayBuffer | undefined;
    if (!bytes) return;
    try {
      await extractZip(bytes, {
        onParts: (files) => {
          ctx.postMessage({ type: "parts", files });
        },
        onMedia: (path, buffer) => {
          // Transfer the buffer so the 50 MB of media is moved, not copied.
          ctx.postMessage({ type: "media", path, buffer }, [buffer]);
        },
      });
      ctx.postMessage({ type: "done" });
    } catch (err) {
      ctx.postMessage({ type: "error", message: String(err) });
    }
  };
}
