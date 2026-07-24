import { describe, it, expect } from "vitest";
import { relativeTime, conversationTitle } from "./chatSessions";

const NOW = new Date("2026-07-24T12:00:00").getTime();
const secs = (n: number) => NOW - n * 1000;
const mins = (n: number) => NOW - n * 60_000;
const hrs = (n: number) => NOW - n * 3_600_000;
const days = (n: number) => NOW - n * 86_400_000;

describe("relativeTime", () => {
  it("reads as 'just now' for the whole first minute (no 0m dead zone)", () => {
    expect(relativeTime(NOW, NOW)).toBe("just now");
    expect(relativeTime(secs(10), NOW)).toBe("just now");
    expect(relativeTime(secs(44), NOW)).toBe("just now");
    expect(relativeTime(secs(59), NOW)).toBe("just now");
  });

  it("counts minutes under an hour, starting at 60s", () => {
    expect(relativeTime(secs(60), NOW)).toBe("1m ago");
    expect(relativeTime(mins(5), NOW)).toBe("5m ago");
    expect(relativeTime(mins(59), NOW)).toBe("59m ago");
  });

  it("counts hours under a day", () => {
    expect(relativeTime(hrs(1), NOW)).toBe("1h ago");
    expect(relativeTime(hrs(23), NOW)).toBe("23h ago");
  });

  it("says 'yesterday' one calendar day back", () => {
    expect(relativeTime(days(1), NOW)).toBe("yesterday");
  });

  it("counts days within the past week", () => {
    expect(relativeTime(days(3), NOW)).toBe("3d ago");
    expect(relativeTime(days(6), NOW)).toBe("6d ago");
  });

  it("falls back to a locale date beyond a week", () => {
    const older = days(30);
    expect(relativeTime(older, NOW)).toBe(
      new Date(older).toLocaleDateString(undefined, { month: "short", day: "numeric" }),
    );
  });

  it("includes the year for a date in a different year", () => {
    const lastYear = new Date("2025-01-02T12:00:00").getTime();
    expect(relativeTime(lastYear, NOW)).toBe(
      new Date(lastYear).toLocaleDateString(undefined, {
        month: "short",
        day: "numeric",
        year: "numeric",
      }),
    );
  });

  it("never renders a future timestamp as negative", () => {
    expect(relativeTime(NOW + 5_000, NOW)).toBe("just now");
  });
});

describe("conversationTitle", () => {
  it("uses the stored title when present", () => {
    expect(conversationTitle({ title: "Kickoff questions" })).toBe("Kickoff questions");
  });

  it("trims surrounding whitespace", () => {
    expect(conversationTitle({ title: "  Trimmed  " })).toBe("Trimmed");
  });

  it("falls back to 'New chat' for an empty or whitespace title", () => {
    expect(conversationTitle({ title: "" })).toBe("New chat");
    expect(conversationTitle({ title: "   " })).toBe("New chat");
  });
});
