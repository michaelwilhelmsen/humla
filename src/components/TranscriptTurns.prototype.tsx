/**
 * PROTOTYPE — throwaway. Not imported by the app; only by the mock harness
 * (`?case=transcript-turns`), which `index.html` never builds.
 *
 * Question: how should a turn say who spoke? Four variants of the transcript
 * reader on the harness's existing panel-sized container, switchable via
 * `?variant=`. A is what ships today (a coloured dot, no name anywhere) and is
 * here as the control; B, C and D each give the name a different shape.
 *
 * The sample is deliberately a FIVE-speaker meeting, because
 * `speakerColorMap` cycles four colours — so the first and fifth speakers
 * carry the same blue, which is the complaint behind #176. With a name on
 * every turn that collision stops mattering; with only a dot it is the whole
 * identity of the turn. Short interjections are in the sample too: they are
 * what makes a per-turn title expensive.
 *
 * Throwaway rules in force: no virtualization, no editing, no playback, no
 * tests. Judge layout and density, nothing else.
 */
import { useEffect, useState } from "react";
import { SPEAKER_COLORS, speakerColorMap } from "./SpeakerLabels";

type Turn = { id: number; label: string; text: string; at: string };

// A five-speaker meeting: two named people, three the diarizer never got a
// name for. Order here is order of first appearance, which is what decides
// colour — so `You` (index 0) and `Speaker 5` (index 4) are both blue.
const SAMPLE: Turn[] = [
  { id: 1, label: "You", text: "Skal vi ta gjennomgangen nå, eller venter vi på Hege?", at: "0:04" },
  { id: 2, label: "Speaker 2", text: "Hun er på vei. Vi kan starte med tallene imens.", at: "0:09" },
  { id: 3, label: "You", text: "Greit.", at: "0:13" },
  {
    id: 4,
    label: "Speaker 2",
    text: "Forrige kvartal endte på 2,4 millioner, som er omtrent elleve prosent over budsjett. Det meste av det kommer fra to kontrakter som ble signert i mars, så jeg vil ikke lese for mye inn i trenden ennå.",
    at: "0:15",
  },
  { id: 5, label: "Hege", text: "Beklager, jeg er her.", at: "0:41" },
  { id: 6, label: "You", text: "Vi er akkurat i gang med kvartalstallene.", at: "0:43" },
  {
    id: 7,
    label: "Hege",
    text: "Perfekt. Jeg har notatene klare, og tallene fra forrige kvartal ligger i arket jeg delte i går.",
    at: "0:46",
  },
  { id: 8, label: "Speaker 4", text: "Ja.", at: "0:53" },
  {
    id: 9,
    label: "Hege",
    text: "Det viktigste derfra er at marginen holder seg, selv med de to nye ansettelsene.",
    at: "0:55",
  },
  { id: 10, label: "Speaker 5", text: "Har vi tatt høyde for lisenskostnadene i det?", at: "1:04" },
  { id: 11, label: "Hege", text: "Ikke ennå. Jeg legger dem inn før fredag.", at: "1:08" },
  { id: 12, label: "Speaker 4", text: "Mm.", at: "1:12" },
  { id: 13, label: "You", text: "Bra — da starter vi der neste gang.", at: "1:14" },
];

const LABELS = Array.from(new Set(SAMPLE.map((t) => t.label)));
const COLORS = speakerColorMap(LABELS);
const colorOf = (label: string) => COLORS.get(label) ?? SPEAKER_COLORS[0];

/** Runs of consecutive turns by the same speaker — what B and D group on. */
function runs(turns: Turn[]): Turn[][] {
  const out: Turn[][] = [];
  for (const t of turns) {
    const last = out[out.length - 1];
    if (last && last[0].label === t.label) last.push(t);
    else out.push([t]);
  }
  return out;
}

// ---- A: what ships today ---------------------------------------------------
// A 5px dot in a gutter and nothing else. The name exists only in the dot's
// native tooltip and in the chip strip above, so at five speakers two of these
// rows are the same blue and the reader cannot tell them apart.
function VariantA() {
  return (
    <div className="text-sm leading-relaxed text-[var(--color-text-muted)]">
      {SAMPLE.map((t) => (
        <div
          key={t.id}
          className="group flex items-start gap-1 px-2 py-1 rounded transition-colors hover:bg-[var(--color-pill-hover)]"
        >
          <div className="relative w-3 shrink-0 self-stretch">
            <span
              className="nd-speaker-dot"
              title={t.label}
              style={{ background: colorOf(t.label), left: 0, top: "calc(0.5lh - 5px)" }}
            />
          </div>
          <div className="flex-1">{t.text}</div>
        </div>
      ))}
    </div>
  );
}
VariantA.variantName = "Today — gutter dot, no name";

// ---- B: the name as a turn title -------------------------------------------
// The user's proposal. One title per RUN of consecutive turns, not per turn —
// a title above every "Ja." would spend two lines saying one word. Colour
// drops to a dot beside the name, so it decorates an identity the text already
// carries rather than being the identity.
function VariantB() {
  return (
    <div className="text-sm leading-relaxed text-[var(--color-text-muted)]">
      {runs(SAMPLE).map((run) => (
        <div key={run[0].id} className="px-2 pt-3 first:pt-0 pb-1">
          <div className="flex items-baseline gap-1.5 mb-0.5">
            <span
              className="inline-block w-[7px] h-[7px] rounded-full shrink-0 self-center"
              style={{ background: colorOf(run[0].label) }}
            />
            <span className="text-[13px] font-semibold text-[var(--color-text)]">
              {run[0].label}
            </span>
            <span className="text-[11px] text-[var(--color-text-disabled)] tabular-nums">
              {run[0].at}
            </span>
          </div>
          {run.map((t) => (
            <div
              key={t.id}
              className="py-1 rounded transition-colors hover:bg-[var(--color-pill-hover)]"
            >
              {t.text}
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}
VariantB.variantName = "Turn title — name above the run";

// ---- C: the name as an inline lead-in --------------------------------------
// Script/interview shape: `Hege:` opens the paragraph in the speaker's colour
// and the text continues on the same line. Costs no vertical space at all, so
// the transcript stays as dense as today — but the name competes with the
// first words of the sentence for the eye, and a long name pushes the text in.
function VariantC() {
  return (
    <div className="text-sm leading-relaxed text-[var(--color-text-muted)]">
      {runs(SAMPLE).map((run) =>
        run.map((t, i) => (
          <div
            key={t.id}
            className="px-2 py-1 rounded transition-colors hover:bg-[var(--color-pill-hover)]"
          >
            {i === 0 && (
              <span className="font-semibold" style={{ color: colorOf(t.label) }}>
                {t.label}:{" "}
              </span>
            )}
            {t.text}
          </div>
        )),
      )}
    </div>
  );
}
VariantC.variantName = "Inline lead-in — Name: text";

// ---- D: the name in its own column -----------------------------------------
// Screenplay shape: a fixed left column carries the name on speaker change and
// stays empty otherwise, so every turn's text starts at the same x. Best
// scanning of the four — you read the who and the what in separate columns —
// and the most expensive: 84px of a 420px panel is a fifth of the width, and a
// name longer than the column has to truncate.
function VariantD() {
  return (
    <div className="text-sm leading-relaxed text-[var(--color-text-muted)]">
      {runs(SAMPLE).map((run) => (
        <div key={run[0].id} className="pt-2 first:pt-0">
          {run.map((t, i) => (
            <div
              key={t.id}
              className="grid grid-cols-[84px_1fr] gap-2 px-2 py-1 rounded transition-colors hover:bg-[var(--color-pill-hover)]"
            >
              <div className="text-right truncate">
                {i === 0 && (
                  <span
                    className="text-[12px] font-medium"
                    style={{ color: colorOf(t.label) }}
                    title={t.label}
                  >
                    {t.label}
                  </span>
                )}
              </div>
              <div>{t.text}</div>
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}
VariantD.variantName = "Name column — screenplay";

// ---- E: the hybrid the other four argue toward ------------------------------
// A's gutter dot, C's inline name — but the name in ordinary ink rather than in
// the speaker's colour, so colour goes back to being a scanning aid and the
// NAME is the identity. That inversion is the whole point: at five speakers two
// dots are the same blue, and it stops mattering the moment the name is there.
// Reads at today's density, and the raw palette colours never have to work as
// text (the warm gold is too light for that — see `--color-accent-text`).
function VariantE() {
  return (
    <div className="text-sm leading-relaxed text-[var(--color-text-muted)]">
      {runs(SAMPLE).map((run) =>
        run.map((t, i) => (
          <div
            key={t.id}
            className="group flex items-start gap-1 px-2 py-1 rounded transition-colors hover:bg-[var(--color-pill-hover)]"
          >
            <div className="relative w-3 shrink-0 self-stretch">
              {i === 0 && (
                <span
                  className="nd-speaker-dot"
                  title={t.label}
                  style={{ background: colorOf(t.label), left: 0, top: "calc(0.5lh - 5px)" }}
                />
              )}
            </div>
            <div className="flex-1">
              {i === 0 && (
                <span className="font-semibold text-[var(--color-text)]">{t.label}: </span>
              )}
              {t.text}
            </div>
          </div>
        )),
      )}
    </div>
  );
}
VariantE.variantName = "Dot + inline name in ink";

const VARIANTS = { A: VariantA, B: VariantB, C: VariantC, D: VariantD, E: VariantE } as const;
type VariantKey = keyof typeof VARIANTS;
const KEYS = Object.keys(VARIANTS) as VariantKey[];

/** Floating switcher: arrows, the current variant's name, ←/→ keys, URL-stable. */
function PrototypeSwitcher({
  current,
  onPick,
}: {
  current: VariantKey;
  onPick: (k: VariantKey) => void;
}) {
  const step = (d: number) => onPick(KEYS[(KEYS.indexOf(current) + d + KEYS.length) % KEYS.length]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = document.activeElement;
      if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) return;
      if (el instanceof HTMLElement && el.isContentEditable) return;
      if (e.key === "ArrowLeft") step(-1);
      if (e.key === "ArrowRight") step(1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });
  // Deliberately NOT token-styled: the bar must not read as part of the design
  // being judged.
  return (
    <div
      style={{
        position: "fixed",
        bottom: 16,
        left: "50%",
        transform: "translateX(-50%)",
        display: "flex",
        alignItems: "center",
        gap: 12,
        padding: "8px 12px",
        borderRadius: 999,
        background: "#111",
        color: "#fff",
        font: "500 12px/1.2 ui-sans-serif, system-ui",
        boxShadow: "0 6px 24px rgba(0,0,0,.35)",
        zIndex: 100,
      }}
    >
      <button onClick={() => step(-1)} style={{ color: "#fff", padding: "0 6px" }} aria-label="Previous variant">←</button>
      <span style={{ minWidth: 230, textAlign: "center" }}>
        {current} — {VARIANTS[current].variantName}
      </span>
      <button onClick={() => step(1)} style={{ color: "#fff", padding: "0 6px" }} aria-label="Next variant">→</button>
    </div>
  );
}

export function TranscriptTurnsPrototype() {
  const initial = (new URLSearchParams(location.search).get("variant") ?? "A").toUpperCase();
  const [variant, setVariant] = useState<VariantKey>(
    (KEYS as string[]).includes(initial) ? (initial as VariantKey) : "A",
  );
  function pick(k: VariantKey) {
    setVariant(k);
    const url = new URL(location.href);
    url.searchParams.set("variant", k);
    history.replaceState(null, "", url);
  }
  const Current = VARIANTS[variant];
  return (
    <>
      <div className="flex-1 min-h-0 overflow-y-auto">
        <Current />
      </div>
      {/* The state under judgement, printed: which speakers exist, and which
          two of them the four-colour cycle has collided. */}
      <div className="mt-3 pt-2 border-t border-[var(--color-line)] text-[11px] text-[var(--color-text-disabled)]">
        {LABELS.length} speakers · {LABELS[0]} and {LABELS[4]} share a colour
      </div>
      {import.meta.env.DEV && <PrototypeSwitcher current={variant} onPick={pick} />}
    </>
  );
}
