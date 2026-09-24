// The counts a note's speaker chip offers. The list reaches past what Nemotron 3
// tells apart, because a count above that sends the note to community-1.
const MAX_SPEAKER_COUNT = 12;

export function speakerCountLabel(n: number): string {
  return n === 1 ? "1 speaker" : `${n} speakers`;
}

/** Picker values are strings, so "0" stands in for Auto (no count). */
export const SPEAKER_COUNT_OPTIONS = [
  { value: "0", label: "Auto" },
  ...Array.from({ length: MAX_SPEAKER_COUNT }, (_, i) => ({
    value: String(i + 1),
    label: speakerCountLabel(i + 1),
  })),
];
