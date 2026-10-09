import { describe, expect, it } from "vitest";
import { embeddingLine, embeddingPct, modelSize, modelsFor, runsOnLine, tierLabel } from "./models";
import type { EmbeddingState, ModelStatus } from "./api";

const state = (over: Partial<EmbeddingState> = {}): EmbeddingState => ({
  model: "nomic-embed-text-v1.5",
  dims: 768,
  embedded: 10,
  total: 10,
  rebuilding: false,
  device: "CPU",
  ...over,
});

describe("tierLabel and modelSize", () => {
  it("name the tier and the size", () => {
    expect(tierLabel("light")).toBe("Light");
    expect(tierLabel("best")).toBe("Best");
    expect(modelSize(146 * 1024 * 1024)).toBe("146 MB");
    expect(modelSize(1.6 * 1024 * 1024 * 1024)).toBe("1.6 GB");
    expect(modelSize(null)).toBe("");
  });
});

describe("modelsFor", () => {
  it("keeps one category", () => {
    const m = (id: string, category: ModelStatus["category"]) => ({ id, category }) as ModelStatus;
    expect(modelsFor([m("a", "embedding"), m("b", "transcription"), m("c", "embedding")], "embedding").map((x) => x.id)).toEqual([
      "a",
      "c",
    ]);
  });
});

describe("runsOnLine", () => {
  it("names the card and its backend, or the CPU", () => {
    expect(runsOnLine({ device: "NVIDIA RTX PRO 2000", backend: "vulkan", useGpu: true })).toBe(
      "Runs on: NVIDIA RTX PRO 2000 (Vulkan)",
    );
    expect(runsOnLine({ device: "Apple M3", backend: "metal", useGpu: true })).toBe("Runs on: Apple M3 (Metal)");
    expect(runsOnLine({ device: null, backend: "cpu", useGpu: true })).toBe("Runs on: CPU");
    expect(runsOnLine({ device: "NVIDIA", backend: "vulkan", useGpu: false })).toBe("Runs on: CPU (the graphics card is off)");
    expect(runsOnLine(null)).toBe("");
  });
});

describe("embeddingLine", () => {
  it("says where search by meaning is", () => {
    expect(embeddingLine(state({ rebuilding: true, embedded: 40, total: 200 }))).toBe(
      "Re-reading for meaning: 40 of 200. Keyword search works meanwhile.",
    );
    expect(embeddingLine(state({ model: null }))).toBe("No model yet, so search is by keyword.");
    expect(embeddingLine(state({ embedded: 3, total: 10 }))).toBe("3 of 10 read for meaning.");
    expect(embeddingLine(state())).toBe("Everything indexed is read for meaning.");
    expect(embeddingLine(null)).toBe("");
  });
  it("gives a percent only while re-reading", () => {
    expect(embeddingPct(state({ rebuilding: true, embedded: 50, total: 200 }))).toBe(25);
    expect(embeddingPct(state())).toBeNull();
  });
});
