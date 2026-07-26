import { describe, it, expect } from "vitest";
import {
  relativeTime,
  conversationTitle,
  conversationRows,
  usageTone,
  liveChatErrorCopy,
  groundingLikelyTruncated,
  GROUNDING_CHAR_BUDGET,
  GROUNDING_TEXT_BUDGET,
} from "./chatSessions";

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

describe("usageTone", () => {
  it("stays default below 70%", () => {
    expect(usageTone(0, 100)).toBe("default");
    expect(usageTone(69, 100)).toBe("default");
  });

  it("warns from 70% to 89%", () => {
    expect(usageTone(70, 100)).toBe("warning");
    expect(usageTone(89, 100)).toBe("warning");
  });

  it("goes danger at 90% and once used reaches or passes the cap", () => {
    expect(usageTone(90, 100)).toBe("danger");
    expect(usageTone(100, 100)).toBe("danger");
    expect(usageTone(5, 3)).toBe("danger");
  });

  it("is default for a non-positive cap (total edge)", () => {
    expect(usageTone(0, 0)).toBe("default");
    expect(usageTone(2, 0)).toBe("default");
  });
});

describe("liveChatErrorCopy", () => {
  const owner = { isOwner: true, ownerName: "Ada" };
  const member = { isOwner: false, ownerName: "Ada" };

  it("gives the owner a fix path and the member an ask-owner line for a rejected key", () => {
    expect(liveChatErrorCopy("byok_key_invalid", owner)).toMatch(/Organization → Workspace chat/);
    expect(liveChatErrorCopy("byok_key_invalid", member)).toMatch(/ask Ada/);
  });

  it("distinguishes provider quota from the managed add-on upsell", () => {
    const o = liveChatErrorCopy("byok_provider_quota", owner)!;
    const m = liveChatErrorCopy("byok_provider_quota", member)!;
    expect(o).toMatch(/out of quota/);
    expect(m).toMatch(/ask Ada/);
    // Never the managed add-on's upsell wording.
    expect(o).not.toMatch(/add-on/);
    expect(m).not.toMatch(/add-on/);
  });

  it("treats an unavailable key as transient for everyone", () => {
    expect(liveChatErrorCopy("byok_key_unavailable", owner)).toMatch(/try again shortly/);
    expect(liveChatErrorCopy("byok_key_unavailable", member)).toMatch(/try again shortly/);
  });

  it("returns null for reasons outside the taxonomy (caller falls back)", () => {
    expect(liveChatErrorCopy("quota_exhausted", owner)).toBeNull();
    expect(liveChatErrorCopy("chat_not_activated", owner)).toBeNull();
    expect(liveChatErrorCopy(undefined, owner)).toBeNull();
    expect(liveChatErrorCopy("", owner)).toBeNull();
  });
});

describe("groundingLikelyTruncated", () => {
  const parts = (bodyText = "", transcript = "", summary = "") => ({
    bodyText,
    transcript,
    summary,
  });

  it("is false for an empty note", () => {
    expect(groundingLikelyTruncated(parts())).toBe(false);
  });

  it("is false right up to the budget and true just past it", () => {
    const at = "x".repeat(GROUNDING_TEXT_BUDGET);
    expect(groundingLikelyTruncated(parts(at))).toBe(false);
    expect(groundingLikelyTruncated(parts(at + "x"))).toBe(true);
  });

  it("leaves room for the block's scaffold, so it fires before the raw budget", () => {
    // The backend measures the assembled block (preamble + section headers), so
    // note text alone can be under 24k and still truncate.
    const justUnderRaw = "x".repeat(GROUNDING_CHAR_BUDGET - 1);
    expect(justUnderRaw.length).toBeLessThan(GROUNDING_CHAR_BUDGET);
    expect(groundingLikelyTruncated(parts(justUnderRaw))).toBe(true);
  });

  it("sums all three sources rather than checking the largest", () => {
    // No single source is over budget, but together they are — this is the case
    // a per-field check would miss.
    const third = "x".repeat(Math.ceil(GROUNDING_TEXT_BUDGET / 3) + 10);
    expect(groundingLikelyTruncated(parts(third))).toBe(false);
    expect(groundingLikelyTruncated(parts(third, third, third))).toBe(true);
  });
});


describe("conversationRows", () => {
  const rows = (
    convos: { id: string; title: string; updatedAt: number }[],
    activeId: string | null = null,
  ) => conversationRows(convos, activeId, NOW);

  it("orders most-recent first regardless of the order given", () => {
    const out = rows([
      { id: "a", title: "Oldest", updatedAt: NOW - 5 * 86_400_000 },
      { id: "b", title: "Newest", updatedAt: NOW - 60_000 },
      { id: "c", title: "Middle", updatedAt: NOW - 3 * 3_600_000 },
    ]);
    expect(out.map((r) => r.id)).toEqual(["b", "c", "a"]);
    expect(out.map((r) => r.description)).toEqual(["1m ago", "3h ago", "5d ago"]);
  });

  it("does not mutate the caller's array", () => {
    const input = [
      { id: "a", title: "A", updatedAt: 1 },
      { id: "b", title: "B", updatedAt: 2 },
    ];
    rows(input);
    expect(input.map((c) => c.id)).toEqual(["a", "b"]);
  });

  it("falls back to a label for an untitled conversation", () => {
    expect(rows([{ id: "a", title: "   ", updatedAt: NOW }])[0].label).toBe("New chat");
  });

  it("marks exactly the active row, and none when there is no active id", () => {
    const convos = [
      { id: "a", title: "A", updatedAt: 2 },
      { id: "b", title: "B", updatedAt: 1 },
    ];
    expect(rows(convos, "b").map((r) => r.active)).toEqual([false, true]);
    expect(rows(convos, null).every((r) => !r.active)).toBe(true);
    // An id that isn't in the list marks nothing rather than defaulting to first.
    expect(rows(convos, "gone").every((r) => !r.active)).toBe(true);
  });
});
