// Summary presets — labels + the prompt text used by Settings to seed the
// global custom prompt and as a reference for the per-note picker.
//
// Keep these in sync with src-tauri/src/presets.rs. The backend resolves the
// prompt at request time from the note's stored preset value, so this file
// is mostly UI labeling + the "custom" path's seed text.

export type PresetSpec = {
  value: string;
  label_no: string;
  label_en: string;
  prompt_no: string;
  prompt_en: string;
};

export const SUMMARY_PRESETS: PresetSpec[] = [
  {
    value: "meeting",
    label_no: "Møte",
    label_en: "Meeting",
    prompt_no: `Du lager møtenotater fra en automatisk transkribert samtale.

Kilder du får:
- [Notater] — det brukeren skrev under møtet (autoritativ kilde for navn, tall og beslutninger).
- [Transkripsjon] — automatisk generert fra lyden, kan inneholde feil.

Når transkripsjon og notater er i konflikt, stol på notatene.

Skriv på norsk i Markdown. Inkluder kun seksjoner som er reelt relevante — ikke skriv "Ingen identifisert".

- **Sammendrag** — 2–4 setninger som fanger essensen.
- **Beslutninger** — kun reelle beslutninger som ble tatt.
- **Handlingspunkter** — på formen "Beskrivelse — Ansvarlig (frist når oppgitt)".
- **Åpne spørsmål** — uavklarte ting som krever oppfølging.

Vær konkret og kort. Ikke gjenta deg selv. Ikke finn på detaljer som ikke står i kilden.`,
    prompt_en: `You produce meeting notes from an automatically transcribed conversation.

Sources you receive:
- [Notater] — what the user typed during the meeting (authoritative for names, numbers, and decisions).
- [Transkripsjon] — auto-generated from the audio, may contain errors.

When the transcript and notes conflict, trust the notes.

Reply in {LANGUAGE} using Markdown. Include only sections that are genuinely relevant — do not write "None identified".

- **Summary** — 2–4 sentences capturing the essence.
- **Decisions** — only real decisions that were made.
- **Action items** — formatted as "Description — Owner (due date when stated)".
- **Open questions** — unresolved items that need follow-up.

Be concrete and concise. Do not repeat yourself. Do not invent details that are not in the source.`,
  },
  {
    value: "one_on_one",
    label_no: "1:1-samtale",
    label_en: "1:1 conversation",
    prompt_no: `Du lager notater fra en 1:1-samtale.

Kilder: [Notater] (autoritativt) og [Transkripsjon] (kan ha feil). Stol på notatene ved konflikt.

Fokus: hva personen delte — bekymringer, ambisjoner, tilbakemeldinger og avtaler.

Skriv på norsk i Markdown. Bruk kun relevante seksjoner:
- **Hovedtemaer** — 2–5 bullets.
- **Tilbakemeldinger** — gitt eller mottatt.
- **Avtalt oppfølging** — "Beskrivelse — Ansvarlig (frist når oppgitt)".
- **Stemning/observasjoner** — kort, kun hvis tydelig.

Ikke spekuler. Hold konfidensielt språk.`,
    prompt_en: `You produce notes from a 1:1 conversation.

Sources: [Notater] (authoritative) and [Transkripsjon] (may have errors). Trust the notes when they conflict.

Focus: what the person shared — concerns, ambitions, feedback, and commitments.

Reply in {LANGUAGE} using Markdown. Use only relevant sections:
- **Main themes** — 2–5 bullets.
- **Feedback** — given or received.
- **Agreed follow-ups** — "Description — Owner (due date when stated)".
- **Mood/observations** — brief, only if clear.

Do not speculate. Use confidential, respectful phrasing.`,
  },
  {
    value: "lecture",
    label_no: "Foredrag",
    label_en: "Lecture",
    prompt_no: `Du lager studienotater fra et foredrag eller en presentasjon.

Kilder: [Notater] (autoritativt) og [Transkripsjon] (kan ha feil).

Mål: fange faktaene og argumentene slik at noen som ikke var til stede kan forstå dem.

Skriv på norsk i Markdown. Bruk kun relevante seksjoner:
- **Hovedbudskap** — 1–3 setninger.
- **Sentrale punkter** — bullets med korte forklaringer.
- **Begreper og definisjoner** — hvis relevant.
- **Eksempler** — hvis gitt.
- **Spørsmål til videre studie** — hvis nevnt.

Vær presis med tall og navn. Ikke finn på detaljer.`,
    prompt_en: `You produce study notes from a lecture or presentation.

Sources: [Notater] (authoritative) and [Transkripsjon] (may have errors).

Goal: capture the facts and arguments so someone who was not there can understand them.

Reply in {LANGUAGE} using Markdown. Use only relevant sections:
- **Key takeaways** — 1–3 sentences.
- **Main points** — bullets with brief explanations.
- **Terms and definitions** — if relevant.
- **Examples** — if given.
- **Questions for further study** — if mentioned.

Be precise with numbers and names. Do not invent details.`,
  },
  {
    value: "interview",
    label_no: "Intervju",
    label_en: "Interview",
    prompt_no: `Du lager notater fra et intervju.

Kilder: [Notater] (autoritativt) og [Transkripsjon] (kan ha feil).

Bevar Q&A-strukturen og intervjuobjektets stemme der det er mulig.

Skriv på norsk i Markdown:
- **Kort oppsummering** — 2–3 setninger.
- **Sentrale svar** — gruppert etter tema; korte sitater i kursiv hvis viktige.
- **Tematiske observasjoner** — mønstre i svarene.
- **Oppfølgingsspørsmål** — hvis intervjueren noterte noen.

Ikke parafrasér når et direkte sitat er klarere.`,
    prompt_en: `You produce notes from an interview.

Sources: [Notater] (authoritative) and [Transkripsjon] (may have errors).

Preserve the Q&A structure and the interviewee's voice where possible.

Reply in {LANGUAGE} using Markdown:
- **Short summary** — 2–3 sentences.
- **Key responses** — grouped by theme; short direct quotes in italics when important.
- **Thematic observations** — patterns across answers.
- **Follow-up questions** — if the interviewer noted any.

Do not paraphrase when a direct quote is clearer.`,
  },
  {
    value: "brainstorm",
    label_no: "Idémyldring",
    label_en: "Brainstorm",
    prompt_no: `Du oppsummerer en idémyldring.

Kilder: [Notater] (autoritativt) og [Transkripsjon] (kan ha feil).

Mål: fange alle ideene som ble nevnt — også de som ble forkastet.

Skriv på norsk i Markdown:
- **Tema** — hva ble brainstormet.
- **Ideer** — bullets, grupper relaterte ideer.
- **Vurdering** — hvilke ideer ble valgt eller forkastet, og hvorfor.
- **Neste steg** — hvis avtalt.

Ikke siler ideer på egen hånd. Inkluder alt.`,
    prompt_en: `You summarize a brainstorming session.

Sources: [Notater] (authoritative) and [Transkripsjon] (may have errors).

Goal: capture every idea mentioned — including ones that were rejected.

Reply in {LANGUAGE} using Markdown:
- **Topic** — what was being brainstormed.
- **Ideas** — bullets, group related ideas together.
- **Evaluation** — which ideas were chosen or rejected, and why.
- **Next steps** — if agreed.

Do not filter ideas yourself. Include everything.`,
  },
  {
    value: "voice_memo",
    label_no: "Stemmenotat",
    label_en: "Voice memo",
    prompt_no: `Du rydder opp i et stemmenotat.

Kilder: [Notater] (autoritativt) og [Transkripsjon] (kan ha feil). Bruk notatene til å rette transkripsjonsfeil.

Brukeren snakket fritt; oppgaven er å gjøre innholdet lesbart uten å miste detaljer.

Skriv på norsk i Markdown. Behold brukerens stemme. Strukturen kan være enkel — bullets eller korte avsnitt etter behov. Ikke legg til seksjoner som ikke trengs.

Mål: konsist, lesbart, trofast mot innholdet.`,
    prompt_en: `You clean up a voice memo.

Sources: [Notater] (authoritative) and [Transkripsjon] (may have errors). Use the notes to correct transcription mistakes.

The user spoke freely; the task is to make the content readable without losing details.

Reply in {LANGUAGE} using Markdown. Preserve the user's voice. Structure can be simple — bullets or short paragraphs as needed. Do not add sections that are not needed.

Goal: concise, readable, faithful to the content.`,
  },
];

const LANGUAGE_LABELS: Record<string, string> = {
  no: "Norwegian",
  en: "English",
  sv: "Swedish",
  da: "Danish",
  auto: "the same language as the input",
};

export function presetPromptForLang(preset: PresetSpec, lang: string): string {
  if (lang === "no") return preset.prompt_no;
  const label = LANGUAGE_LABELS[lang] ?? "English";
  return preset.prompt_en.replace("{LANGUAGE}", label);
}

export function presetLabelForLang(preset: PresetSpec, lang: string): string {
  return lang === "no" ? preset.label_no : preset.label_en;
}

export function presetByValue(value: string): PresetSpec | undefined {
  return SUMMARY_PRESETS.find((p) => p.value === value);
}
