import { describe, expect, it } from "vitest";
import { base64, mimeForExtension, mimeForPath } from "./mime";

describe("mimeForExtension", () => {
  it("maps the common image types", () => {
    expect(mimeForExtension("png")).toBe("image/png");
    expect(mimeForExtension("jpg")).toBe("image/jpeg");
    expect(mimeForExtension("jpeg")).toBe("image/jpeg");
    expect(mimeForExtension("gif")).toBe("image/gif");
    expect(mimeForExtension("bmp")).toBe("image/bmp");
    expect(mimeForExtension("svg")).toBe("image/svg+xml");
  });

  // These three were absent from ImagePreview's own copy of the map, so an
  // asset that inlined fine in the HTML preview rendered broken here. The
  // shared map closes that gap by construction.
  it("maps the modern image types the image preview used to miss", () => {
    expect(mimeForExtension("webp")).toBe("image/webp");
    expect(mimeForExtension("avif")).toBe("image/avif");
    expect(mimeForExtension("ico")).toBe("image/x-icon");
  });

  it("maps the web font types the html preview inlines", () => {
    expect(mimeForExtension("woff")).toBe("font/woff");
    expect(mimeForExtension("woff2")).toBe("font/woff2");
    expect(mimeForExtension("ttf")).toBe("font/ttf");
    expect(mimeForExtension("otf")).toBe("font/otf");
    expect(mimeForExtension("eot")).toBe("application/vnd.ms-fontobject");
  });

  it("is case-insensitive and unknown extensions are undefined", () => {
    expect(mimeForExtension("PNG")).toBe("image/png");
    expect(mimeForExtension("xyz")).toBeUndefined();
  });
});

describe("mimeForPath", () => {
  it("derives the type from a path's extension", () => {
    expect(mimeForPath("notes/assets/logo.PNG")).toBe("image/png");
    expect(mimeForPath("f.woff2")).toBe("font/woff2");
    expect(mimeForPath("noext")).toBeUndefined();
  });
});

describe("base64", () => {
  it("encodes a known byte array, from either buffer or view", () => {
    // "Man" → TWFu is the canonical RFC 4648 example.
    const bytes = new Uint8Array([0x4d, 0x61, 0x6e]);
    expect(base64(bytes)).toBe("TWFu");
    expect(base64(bytes.buffer)).toBe("TWFu");
  });

  it("survives inputs larger than the 0x8000 chunk boundary", () => {
    const big = new Uint8Array(0x8000 * 2 + 5).fill(0x41); // 'A'
    const encoded = base64(big);
    // Round-trips back to the same bytes.
    const bin = atob(encoded);
    expect(bin.length).toBe(big.length);
    expect(bin.charCodeAt(0)).toBe(0x41);
    expect(bin.charCodeAt(bin.length - 1)).toBe(0x41);
  });
});
