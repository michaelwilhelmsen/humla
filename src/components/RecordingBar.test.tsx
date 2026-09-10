import { describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import { ROW_STEPS, RecordingBar, noAudioWarning } from "./RecordingBar";
import { useRecordingStore, type ReplayRun } from "../lib/store";
import type { RecordingStatus } from "../lib/ipc";

// #174. The warning fired correctly and said nothing useful: a pair of
// headphones in another room held the macOS default input, and "check your
// microphone" pointed at the one device that wasn't the problem.
describe("noAudioWarning", () => {
  it("names the device it isn't hearing", () => {
    expect(noAudioWarning("AirPods Pro")).toBe(
      "No audio from AirPods Pro — check your input device in System Settings",
    );
  });

  it("falls back to the device-less copy when the name is unavailable", () => {
    // An older sidecar reports no device, and CoreAudio can fail to name one.
    // Naming nothing is better than naming a guess.
    for (const absent of [undefined, null, "", "   "]) {
      expect(noAudioWarning(absent)).toBe("No audio detected — check your microphone");
    }
  });

  it("trims a padded name rather than rendering the padding", () => {
    expect(noAudioWarning("  Studio Display Microphone  ")).toContain(
      "No audio from Studio Display Microphone —",
    );
  });

  it("clamps without splitting a character in half", () => {
    // Device names are user-authored, and people put emoji in them. Slicing by
    // UTF-16 code unit can cut a surrogate pair down the middle, which renders
    // as U+FFFD — a replacement glyph in a warning reads as a second bug.
    const msg = noAudioWarning(`${"a".repeat(38)}🎧🎧🎧`);
    expect(msg).not.toContain("\uFFFD");
    expect(msg).toContain("…");
    // No lone surrogate survived the clamp.
    for (const ch of msg) expect(ch.codePointAt(0)! & 0xf800).not.toBe(0xd800);
  });

  it("does not leave a space stranded before the ellipsis", () => {
    expect(noAudioWarning(`${"b".repeat(38)} tail`)).toContain("…");
    expect(noAudioWarning(`${"b".repeat(38)} tail`)).not.toContain(" …");
  });

  // MIRROR: these two vectors are pinned identically against Rust's
  // `clamp_device_name` (src-tauri/src/commands.rs, `device_name_tests`), which
  // clamps the same name for the device-change toast. The same device must not
  // read differently in the warning pill and the toast. Change both.
  it("clamps the mirrored vectors exactly as the Rust side does", () => {
    expect(noAudioWarning(`${"b".repeat(38)} tail`)).toBe(
      `No audio from ${"b".repeat(38)}… — check your input device in System Settings`,
    );
    expect(noAudioWarning(`${"a".repeat(38)}🎧🎧🎧`)).toBe(
      `No audio from ${"a".repeat(38)}🎧… — check your input device in System Settings`,
    );
  });

  it("clamps a pathologically long name so the pill keeps its shape", () => {
    // Device names are user-authored, so the length is not ours to trust. The
    // pill wraps, so an unclamped name costs height rather than overflowing —
    // but it still has to stop somewhere.
    const msg = noAudioWarning("M".repeat(200));
    expect(msg.length).toBeLessThan(120);
    expect(msg).toContain("…");
  });
});

// #177. The row — diagnostics pill, an optional busy pill, the timer/controls
// pill — is wider than the body column it centres in, and every pill in it is
// `shrink-0 whitespace-nowrap`, so nothing absorbs the squeeze. It gives way in
// steps instead (see `ROW_STEPS`). Whether each step actually fits is a
// question only the harness can answer — jsdom pins every box to 0 — so what is
// pinned here is what must survive a step: the readout each hidden label
// carried, and the ordering between the two arrangements of the row.
describe("the recording bar's degradation ladder", () => {
  function seed(opts: { summarizing?: boolean; paused?: boolean } = {}) {
    useRecordingStore.setState({
      status: { noteId: "n1", phase: opts.paused ? "paused" : "recording" },
      summarizing: opts.summarizing ? { n1: true } : {},
      micHeard: true,
      activeSince: null,
      activeAccumMs: 0,
      diag: {
        noteId: "n1",
        micFrames: 16000 * 14,
        sysFrames: 16000 * 21,
        chunks: 3,
        micPeak: 0.4,
        sysPeak: 0.2,
        inputDevice: "MacBook Pro-mikrofon",
      },
    });
  }

  it("keeps the whole diagnostics readout reachable once its numbers are hidden", () => {
    // The compact step hides the seconds and the chunk count in CSS — the
    // meters stay, since "is it hearing me" is the reason the pill exists. The
    // numbers must not simply vanish: the pill names them, the way the note
    // toolbar's `title` carries every label it drops.
    seed();
    render(<RecordingBar noteId="n1" />);
    expect(screen.getByTitle("mic 14s · sys 21s · 3 chunks")).toBeInTheDocument();
  });

  it("names the busy pill so it still says what it is as a bare spinner", () => {
    // At the tightest step "Summarizing…" is a spinner and nothing else. An
    // unlabelled spinner is a mystery, so the pill carries its own name.
    seed({ summarizing: true });
    render(<RecordingBar noteId="n1" />);
    expect(screen.getByRole("status", { name: "Summarizing…" })).toBeInTheDocument();
  });

  it("degrades earlier in every step when a busy pill shares the row", () => {
    // A summary running during a recording puts a third pill in the same row,
    // so every step has to fire at a wider column than it does without one.
    // Pinning the ordering rather than the literals: the numbers are measured
    // and will move, but `tight` firing later than `roomy` is always the bug.
    const px = (cls: string) => {
      const m = /@max-\[(\d+)px\]/.exec(cls);
      if (!m) throw new Error(`no threshold in ${cls || "(empty)"}`);
      return Number(m[1]);
    };
    for (const key of ["detail", "pill", "pausedWord"] as const) {
      expect(px(ROW_STEPS.tight[key])).toBeGreaterThan(px(ROW_STEPS.roomy[key]));
    }
    // The busy label and the stop's frozen timer are the two steps the roomy
    // arrangement has no use for: with no third pill beside them, both fit.
    for (const key of ["busyLabel", "stopTimer"] as const) {
      expect(ROW_STEPS.roomy[key]).toBe("");
      expect(px(ROW_STEPS.tight[key])).toBeGreaterThan(0);
    }
  });

  it("reaches for the cheapest step first and the bare spinner last", () => {
    // Within an arrangement the numbers are measured, so their order is mostly
    // the arithmetic's to decide — `tight` drops the diagnostics pill before
    // the word PAUSED because ~50px cannot close a 60px gap. Two ends of the
    // ladder are not negotiable, though: the seconds and chunk count go first,
    // because they cost the least of anything in the row, and a busy pill is
    // stripped to a bare spinner only after the diagnostics pill has already
    // gone. Whether each step then FITS is the sweep's question
    // (`scripts/measure-recording-bar.js`) — jsdom pins every box to 0.
    const px = (cls: string) => Number(/@max-\[(\d+)px\]/.exec(cls)?.[1] ?? 0);
    for (const arrangement of [ROW_STEPS.roomy, ROW_STEPS.tight]) {
      expect(px(arrangement.detail)).toBeGreaterThan(px(arrangement.pausedWord));
      expect(px(arrangement.detail)).toBeGreaterThan(px(arrangement.pill));
      expect(px(arrangement.detail)).toBeGreaterThan(px(arrangement.busyLabel));
      expect(px(arrangement.pill)).toBeGreaterThanOrEqual(px(arrangement.busyLabel));
    }
  });
});

// #182. The tail of the transcript appends live through the stop, so the row
// reports a real fraction — and must never invent one for the diarize pass,
// which reports no progress at all.
describe("the bar after stop (#182)", () => {
  function seedStop(patch: Partial<RecordingStatus> = {}) {
    useRecordingStore.setState({
      status: { noteId: "n1", phase: "stopping", ...patch },
      summarizing: {},
      diag: null,
      micHeard: true,
    });
  }

  it("reports the drain as the fraction of the chunks that have landed", () => {
    seedStop({ pending: 4, done: 1 });
    render(<RecordingBar noteId="n1" />);
    const bar = screen.getByRole("progressbar", { name: "Finishing transcript…" });
    expect(bar).toHaveAttribute("aria-valuenow", "25");
    expect(bar).toHaveAttribute("aria-valuemax", "100");
  });

  it("reaches 100% when the last pending chunk lands", () => {
    seedStop({ pending: 4, done: 4 });
    render(<RecordingBar noteId="n1" />);
    expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "100");
  });

  it("is full immediately when nothing was in flight at stop", () => {
    // A stop with an empty drain must not read as 0% work done — there is no
    // work. The backend emits `{pending: 0, done: 0}`; an older one emits
    // neither field, and both mean the same thing here.
    for (const patch of [{ pending: 0, done: 0 }, {}]) {
      seedStop(patch);
      const { unmount } = render(<RecordingBar noteId="n1" />);
      expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "100");
      unmount();
    }
  });

  it("never shows a fraction while speakers are being identified", () => {
    // The diarize sidecar emits no progress. A `progressbar` with no
    // `aria-valuenow` is ARIA's own indeterminate, which is exactly the truth.
    seedStop({ phase: "diarizing", pending: undefined, done: undefined });
    render(<RecordingBar noteId="n1" />);
    const bar = screen.getByRole("progressbar", { name: "Identifying speakers…" });
    expect(bar).not.toHaveAttribute("aria-valuenow");
    expect(bar).toHaveAttribute("aria-busy", "true");
    expect(screen.getByText("Identifying speakers…")).toBeInTheDocument();
  });

  it("retires the old busy pills for the two phases it now covers", () => {
    seedStop({ pending: 2, done: 0 });
    render(<RecordingBar noteId="n1" />);
    expect(screen.queryByRole("status", { name: "Stopping…" })).toBeNull();
  });

  it("keeps the elapsed reading through the drain, frozen where the stop left it", () => {
    vi.useFakeTimers();
    try {
      useRecordingStore.setState({
        status: { noteId: "n1", phase: "recording" },
        summarizing: {},
        diag: null,
        micHeard: true,
      });
      render(<RecordingBar noteId="n1" />);
      act(() => void vi.advanceTimersByTime(3_000));
      expect(screen.getByText("0:03")).toBeInTheDocument();
      act(() =>
        useRecordingStore.setState({
          status: { noteId: "n1", phase: "stopping", pending: 2, done: 0 },
        }),
      );
      // The controls go — there is nothing left to pause or stop — but the
      // length of the recording is already final and stays on screen.
      expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
      act(() => void vi.advanceTimersByTime(5_000));
      expect(screen.getByText("0:03")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("says nothing about a capture running on another note", () => {
    seedStop({ noteId: "other", pending: 4, done: 1 });
    render(<RecordingBar noteId="n1" />);
    expect(screen.queryByRole("progressbar")).toBeNull();
  });
});

// #146. Pressing Transcribe replays retained audio through local Whisper for
// minutes at a stretch; the bar is where that says how far it has got. The
// fraction is audio position over the whole run, which the backend has already
// clamped and made monotonic — so what is pinned here is the arithmetic this
// side does and the label it picks.
describe("the bar during a deferred transcription's replay (#146)", () => {
  function seedReplay(run: Partial<ReplayRun> = {}, noteId = "n1") {
    useRecordingStore.setState({
      status: { noteId: null, phase: "idle" },
      summarizing: {},
      diag: null,
      transcribing: { [noteId]: { startedAt: 1, ...run } },
    });
  }

  it("reports the run's audio position as the fraction", () => {
    seedReplay({ doneMs: 45_000, totalMs: 180_000 });
    render(<RecordingBar noteId="n1" />);
    const bar = screen.getByRole("progressbar", { name: "Transcribing…" });
    expect(bar).toHaveAttribute("aria-valuenow", "25");
  });

  it("names the take it is on, but only when there is more than one", () => {
    seedReplay({ doneMs: 45_000, totalMs: 180_000, take: 2, takes: 3 });
    const { unmount } = render(<RecordingBar noteId="n1" />);
    expect(
      screen.getByRole("progressbar", { name: "Transcribing take 2 of 3…" }),
    ).toBeInTheDocument();
    unmount();
    // "take 1 of 1" is a number about nothing.
    seedReplay({ doneMs: 45_000, totalMs: 180_000, take: 1, takes: 1 });
    render(<RecordingBar noteId="n1" />);
    expect(screen.getByRole("progressbar", { name: "Transcribing…" })).toBeInTheDocument();
  });

  it("is full rather than empty when the run hasn't reported its length", () => {
    // Unknown is not zero, and a zero-width track beside a live label reads as
    // stuck. Both shapes the backend can produce mean the same thing here.
    for (const run of [{}, { doneMs: 0, totalMs: 0 }]) {
      seedReplay(run);
      const { unmount } = render(<RecordingBar noteId="n1" />);
      expect(screen.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "100");
      unmount();
    }
  });

  it("says nothing about a replay running on another note", () => {
    seedReplay({ doneMs: 45_000, totalMs: 180_000 }, "other");
    render(<RecordingBar noteId="n1" />);
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  it("draws no bar for the stop of a capture that deferred its transcription", () => {
    // That stop dispatched nothing and lands on idle in a few hundred
    // milliseconds, so a bar drawn for it appears and vanishes without ever
    // measuring anything. `deferred` is the discriminator, NOT an empty drain:
    // a live take with zero pending chunks is a real full bar.
    for (const phase of ["stopping", "diarizing"] as const) {
      useRecordingStore.setState({
        status: { noteId: "n1", phase, deferred: true },
        summarizing: {},
        diag: null,
        transcribing: {},
      });
      const { unmount } = render(<RecordingBar noteId="n1" />);
      expect(screen.queryByRole("progressbar")).toBeNull();
      unmount();
    }
  });
});
