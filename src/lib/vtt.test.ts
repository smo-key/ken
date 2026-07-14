import { describe, expect, it } from "vitest";
import { formatCueTime, isTimed, parseVtt } from "./vtt";

describe("parseVtt", () => {
  it("parses a standard timed VTT", () => {
    const vtt = [
      "WEBVTT",
      "",
      "00:00:01.000 --> 00:00:04.000",
      "Hello there.",
      "",
      "00:00:04.500 --> 00:00:06.250",
      "General Kenobi.",
    ].join("\n");

    expect(parseVtt(vtt)).toEqual([
      { start: 1, end: 4, text: "Hello there." },
      { start: 4.5, end: 6.25, text: "General Kenobi." },
    ]);
  });

  it("ignores cue identifier lines before the timing line", () => {
    const vtt = [
      "WEBVTT",
      "",
      "intro",
      "00:00:00.000 --> 00:00:02.000",
      "Welcome.",
    ].join("\n");

    expect(parseVtt(vtt)).toEqual([{ start: 0, end: 2, text: "Welcome." }]);
  });

  it("joins multi-line cue text with newlines", () => {
    const vtt = [
      "WEBVTT",
      "",
      "00:00:10.000 --> 00:00:13.000",
      "First line",
      "second line",
    ].join("\n");

    expect(parseVtt(vtt)).toEqual([
      { start: 10, end: 13, text: "First line\nsecond line" },
    ]);
  });

  it("tolerates a missing WEBVTT header", () => {
    const vtt = ["00:00:01.000 --> 00:00:02.000", "No header here."].join("\n");

    expect(parseVtt(vtt)).toEqual([
      { start: 1, end: 2, text: "No header here." },
    ]);
  });

  it("accepts MM:SS.mmm timestamps without an hours field", () => {
    const vtt = ["WEBVTT", "", "01:05.500 --> 01:07.000", "Short form."].join(
      "\n",
    );

    expect(parseVtt(vtt)).toEqual([
      { start: 65.5, end: 67, text: "Short form." },
    ]);
  });

  it("carries cue timing settings without breaking the end time", () => {
    const vtt = [
      "WEBVTT",
      "",
      "00:00:01.000 --> 00:00:03.000 align:start position:0%",
      "Aligned.",
    ].join("\n");

    expect(parseVtt(vtt)).toEqual([{ start: 1, end: 3, text: "Aligned." }]);
  });

  it("skips NOTE comment blocks and normalizes CRLF", () => {
    const vtt = [
      "WEBVTT",
      "",
      "NOTE this is a comment",
      "",
      "00:00:00.000 --> 00:00:01.000",
      "Kept.",
    ].join("\r\n");

    expect(parseVtt(vtt)).toEqual([{ start: 0, end: 1, text: "Kept." }]);
  });

  it("treats a header-only or empty transcript as no cues", () => {
    expect(parseVtt("")).toEqual([]);
    expect(parseVtt("WEBVTT\n")).toEqual([]);
    expect(parseVtt("   \n  \n")).toEqual([]);
  });

  it("renders untimed (docx-derived) paragraphs as start==end==0 cues", () => {
    const vtt = [
      "WEBVTT",
      "",
      "First paragraph.",
      "",
      "Second paragraph.",
    ].join("\n");

    expect(parseVtt(vtt)).toEqual([
      { start: 0, end: 0, text: "First paragraph." },
      { start: 0, end: 0, text: "Second paragraph." },
    ]);
  });

  it("drops empty cue bodies rather than emitting blank rows", () => {
    const vtt = [
      "WEBVTT",
      "",
      "00:00:01.000 --> 00:00:02.000",
      "",
      "00:00:02.000 --> 00:00:03.000",
      "Real text.",
    ].join("\n");

    expect(parseVtt(vtt)).toEqual([
      { start: 2, end: 3, text: "Real text." },
    ]);
  });
});

describe("isTimed", () => {
  it("is true when any cue carries a non-zero timestamp", () => {
    expect(
      isTimed([{ start: 0, end: 0, text: "a" }, { start: 1, end: 2, text: "b" }]),
    ).toBe(true);
  });

  it("is false for an all-untimed transcript", () => {
    expect(isTimed([{ start: 0, end: 0, text: "a" }])).toBe(false);
    expect(isTimed([])).toBe(false);
  });
});

describe("formatCueTime", () => {
  it("uses M:SS below an hour", () => {
    expect(formatCueTime(65)).toBe("1:05");
    expect(formatCueTime(5)).toBe("0:05");
  });

  it("uses H:MM:SS at or above an hour", () => {
    expect(formatCueTime(3661)).toBe("1:01:01");
  });
});
