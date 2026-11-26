import { beforeEach, describe, expect, it } from "vitest";
import { useRecordingStore } from "./store";

// #217: the reader marks a note as cleaning the moment it asks, and the pass's
// `clean_status` events carry it from there. A request that returns with no
// pass behind it must not leave the note cleaning, nor re-read a timeline
// nothing was written to.

const start = { active: true, done: 0, total: 0, error: null };

describe("clean request state", () => {
  beforeEach(() => {
    useRecordingStore.setState({ cleaning: {}, cleanErrors: {}, cleanedAt: {} });
  });

  it("settles a request no pass answered, without a re-read", () => {
    const { setCleaning, settleCleanRequest } = useRecordingStore.getState();
    setCleaning("n1", start);
    settleCleanRequest("n1", null);
    expect(useRecordingStore.getState().cleaning).toEqual({});
    expect(useRecordingStore.getState().cleanedAt).toEqual({});
  });

  it("keeps the reason a request failed", () => {
    const { setCleaning, settleCleanRequest } = useRecordingStore.getState();
    setCleaning("n1", start);
    settleCleanRequest("n1", "OpenAI API key not set");
    expect(useRecordingStore.getState().cleanErrors).toEqual({ n1: "OpenAI API key not set" });
  });

  it("leaves a pass the closing event already ended alone", () => {
    const { setCleaning, settleCleanRequest } = useRecordingStore.getState();
    setCleaning("n1", start);
    setCleaning("n1", { active: false, done: 0, total: 0, error: null });
    const cleanedAt = useRecordingStore.getState().cleanedAt;
    settleCleanRequest("n1", "late");
    expect(useRecordingStore.getState().cleanErrors).toEqual({});
    expect(useRecordingStore.getState().cleanedAt).toBe(cleanedAt);
  });
});
