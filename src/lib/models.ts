// Offline models, the pure part: the tier a model row names, the size it
// shows, the device the models run on, and the search-by-meaning line.
import type { EmbeddingState, GpuInfo, ModelCategory, ModelStatus } from "./api";

export function tierLabel(tier: ModelStatus["tier"]): string {
  switch (tier) {
    case "light":
      return "Light";
    case "recommended":
      return "Recommended";
    case "best":
      return "Best";
    case "advanced":
      return "Advanced";
  }
}

/** "146 MB", "1.6 GB"; "" for nothing. */
export function modelSize(bytes: number | null | undefined): string {
  if (!bytes || bytes <= 0) return "";
  const mb = bytes / (1024 * 1024);
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.round(mb)} MB`;
}

/** The catalog's models for one use. */
export function modelsFor(models: readonly ModelStatus[], category: ModelCategory): ModelStatus[] {
  return models.filter((m) => m.category === category);
}

/** "Runs on: NVIDIA RTX PRO 2000 (Vulkan)", "Runs on: CPU". */
export function runsOnLine(gpu: GpuInfo | null): string {
  if (!gpu) return "";
  if (!gpu.useGpu) return "Runs on: CPU (the graphics card is off)";
  if (gpu.backend === "cpu" || !gpu.device) return "Runs on: CPU";
  return `Runs on: ${gpu.device} (${gpu.backend === "metal" ? "Metal" : "Vulkan"})`;
}

/** Where search by meaning is: re-reading after a switch, partly read,
 *  read, or no model yet. */
export function embeddingLine(s: EmbeddingState | null): string {
  if (!s) return "";
  if (s.rebuilding) {
    return `Re-reading for meaning: ${s.embedded} of ${s.total}. Keyword search works meanwhile.`;
  }
  if (!s.model) return "No model yet, so search is by keyword.";
  if (s.total > 0 && s.embedded < s.total) return `${s.embedded} of ${s.total} read for meaning.`;
  return "Everything indexed is read for meaning.";
}

/** 0–100 while a re-read runs, else null. */
export function embeddingPct(s: EmbeddingState | null): number | null {
  if (!s || !s.rebuilding || s.total <= 0) return null;
  return Math.round((s.embedded / s.total) * 100);
}
