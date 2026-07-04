// Step 1 — Welcome. One screen, no carousel (design/ONBOARDING.md § Welcome):
// wordmark, one value-prop line, three quiet feature glyphs, the privacy line
// that earns the next step's permission prompts, and a single CTA.
import { Mic, FileText, Sparkles, ShieldCheck } from "lucide-react";
import type { StepContext } from "../types";

const FEATURES = [
  {
    icon: Mic,
    title: "Record",
    body: "Mic + meeting audio, captured together.",
  },
  {
    icon: FileText,
    title: "Transcribe",
    body: "On your Mac, or via an API.",
  },
  {
    icon: Sparkles,
    title: "Summarize",
    body: "Your notes + the transcript, fused.",
  },
];

export function WelcomeStep({ ctx }: { ctx: StepContext }) {
  return (
    <div className="w-full max-w-xl flex flex-col items-center text-center">
      {/* Wordmark */}
      <div className="text-[32px] font-bold tracking-[-0.03em] text-[var(--color-text-display)]">
        Humla
      </div>
      {/* Value prop */}
      <p className="mt-3 text-[16px] leading-relaxed text-[var(--color-text-muted)] max-w-md">
        Meeting notes that write themselves — you take notes, Humla records,
        transcribes, and summarizes alongside you.
      </p>

      {/* Three feature glyphs */}
      <div className="mt-10 grid grid-cols-1 sm:grid-cols-3 gap-3 w-full">
        {FEATURES.map((f) => {
          const Icon = f.icon;
          return (
            <div
              key={f.title}
              className="flex flex-col items-center text-center gap-2 rounded-[var(--radius)] border border-[var(--color-line)] bg-[var(--color-surface)] px-4 py-5"
            >
              <div className="text-[var(--color-accent-text)]">
                <Icon size={22} strokeWidth={1.7} />
              </div>
              <div className="text-sm font-semibold text-[var(--color-text)]">
                {f.title}
              </div>
              <div className="text-xs leading-snug text-[var(--color-text-muted)]">
                {f.body}
              </div>
            </div>
          );
        })}
      </div>

      {/* Privacy line */}
      <div className="mt-9 flex items-start gap-2 text-[13px] leading-relaxed text-[var(--color-text-muted)] max-w-md">
        <ShieldCheck
          size={16}
          strokeWidth={1.8}
          className="mt-0.5 shrink-0 text-[var(--color-success)]"
        />
        <span>
          Private by default — your notes and audio stay on your Mac unless you
          choose otherwise.
        </span>
      </div>

      {/* Single CTA */}
      <div className="mt-9">
        <button type="button" className="nd-btn nd-btn-primary" onClick={ctx.goNext}>
          Get started
        </button>
      </div>
    </div>
  );
}
