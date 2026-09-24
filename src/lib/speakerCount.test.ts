import { describe, expect, it } from "vitest";
import { NEMOTRON_MAX_SPEAKERS } from "./diarizeEngine";
import { SPEAKER_COUNT_OPTIONS, speakerCountLabel } from "./speakerCount";

describe("speaker counts", () => {
  it("reach past what Nemotron 3 tells apart, since a count above it picks community-1", () => {
    const counts = SPEAKER_COUNT_OPTIONS.map((o) => Number(o.value)).filter((n) => n > 0);
    expect(Math.max(...counts)).toBeGreaterThan(NEMOTRON_MAX_SPEAKERS);
  });

  it("start with Auto, stored as 0", () => {
    expect(SPEAKER_COUNT_OPTIONS[0]).toEqual({ value: "0", label: "Auto" });
  });

  it("are labelled in the singular for one", () => {
    expect(speakerCountLabel(1)).toBe("1 speaker");
    expect(speakerCountLabel(9)).toBe("9 speakers");
  });
});
