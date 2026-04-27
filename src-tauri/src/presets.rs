// Summary presets, mirrored from the frontend so the backend can resolve a
// preset → prompt without round-tripping. Norwegian and English versions per
// preset; English uses {LANGUAGE} which we substitute at request time.
//
// Keep these in sync with src/pages/Settings.tsx::SUMMARY_PRESETS.

pub fn prompt(preset: &str, lang: &str) -> String {
    let entry = ALL.iter().find(|p| p.value == preset).unwrap_or(&ALL[0]);
    if lang == "no" {
        entry.no.to_string()
    } else {
        let label = match lang {
            "sv" => "Swedish",
            "da" => "Danish",
            "auto" => "the same language as the input",
            _ => "English",
        };
        entry.en.replace("{LANGUAGE}", label)
    }
}

struct Preset {
    value: &'static str,
    no: &'static str,
    en: &'static str,
}

const ALL: &[Preset] = &[
    Preset {
        value: "meeting",
        no: "Du lager møtenotater fra en automatisk transkribert samtale.\n\nKilder du får:\n- [Notater] — det brukeren skrev under møtet (autoritativ kilde for navn, tall og beslutninger).\n- [Transkripsjon] — automatisk generert fra lyden, kan inneholde feil.\n\nNår transkripsjon og notater er i konflikt, stol på notatene.\n\nSkriv på norsk i Markdown. Inkluder kun seksjoner som er reelt relevante — ikke skriv \"Ingen identifisert\".\n\n- **Sammendrag** — 2–4 setninger som fanger essensen.\n- **Beslutninger** — kun reelle beslutninger som ble tatt.\n- **Handlingspunkter** — på formen \"Beskrivelse — Ansvarlig (frist når oppgitt)\".\n- **Åpne spørsmål** — uavklarte ting som krever oppfølging.\n\nVær konkret og kort. Ikke gjenta deg selv. Ikke finn på detaljer som ikke står i kilden.",
        en: "You produce meeting notes from an automatically transcribed conversation.\n\nSources you receive:\n- [Notater] — what the user typed during the meeting (authoritative for names, numbers, and decisions).\n- [Transkripsjon] — auto-generated from the audio, may contain errors.\n\nWhen the transcript and notes conflict, trust the notes.\n\nReply in {LANGUAGE} using Markdown. Include only sections that are genuinely relevant — do not write \"None identified\".\n\n- **Summary** — 2–4 sentences capturing the essence.\n- **Decisions** — only real decisions that were made.\n- **Action items** — formatted as \"Description — Owner (due date when stated)\".\n- **Open questions** — unresolved items that need follow-up.\n\nBe concrete and concise. Do not repeat yourself. Do not invent details that are not in the source.",
    },
    Preset {
        value: "one_on_one",
        no: "Du lager notater fra en 1:1-samtale.\n\nKilder: [Notater] (autoritativt) og [Transkripsjon] (kan ha feil). Stol på notatene ved konflikt.\n\nFokus: hva personen delte — bekymringer, ambisjoner, tilbakemeldinger og avtaler.\n\nSkriv på norsk i Markdown. Bruk kun relevante seksjoner:\n- **Hovedtemaer** — 2–5 bullets.\n- **Tilbakemeldinger** — gitt eller mottatt.\n- **Avtalt oppfølging** — \"Beskrivelse — Ansvarlig (frist når oppgitt)\".\n- **Stemning/observasjoner** — kort, kun hvis tydelig.\n\nIkke spekuler. Hold konfidensielt språk.",
        en: "You produce notes from a 1:1 conversation.\n\nSources: [Notater] (authoritative) and [Transkripsjon] (may have errors). Trust the notes when they conflict.\n\nFocus: what the person shared — concerns, ambitions, feedback, and commitments.\n\nReply in {LANGUAGE} using Markdown. Use only relevant sections:\n- **Main themes** — 2–5 bullets.\n- **Feedback** — given or received.\n- **Agreed follow-ups** — \"Description — Owner (due date when stated)\".\n- **Mood/observations** — brief, only if clear.\n\nDo not speculate. Use confidential, respectful phrasing.",
    },
    Preset {
        value: "lecture",
        no: "Du lager studienotater fra et foredrag eller en presentasjon.\n\nKilder: [Notater] (autoritativt) og [Transkripsjon] (kan ha feil).\n\nMål: fange faktaene og argumentene slik at noen som ikke var til stede kan forstå dem.\n\nSkriv på norsk i Markdown. Bruk kun relevante seksjoner:\n- **Hovedbudskap** — 1–3 setninger.\n- **Sentrale punkter** — bullets med korte forklaringer.\n- **Begreper og definisjoner** — hvis relevant.\n- **Eksempler** — hvis gitt.\n- **Spørsmål til videre studie** — hvis nevnt.\n\nVær presis med tall og navn. Ikke finn på detaljer.",
        en: "You produce study notes from a lecture or presentation.\n\nSources: [Notater] (authoritative) and [Transkripsjon] (may have errors).\n\nGoal: capture the facts and arguments so someone who was not there can understand them.\n\nReply in {LANGUAGE} using Markdown. Use only relevant sections:\n- **Key takeaways** — 1–3 sentences.\n- **Main points** — bullets with brief explanations.\n- **Terms and definitions** — if relevant.\n- **Examples** — if given.\n- **Questions for further study** — if mentioned.\n\nBe precise with numbers and names. Do not invent details.",
    },
    Preset {
        value: "interview",
        no: "Du lager notater fra et intervju.\n\nKilder: [Notater] (autoritativt) og [Transkripsjon] (kan ha feil).\n\nBevar Q&A-strukturen og intervjuobjektets stemme der det er mulig.\n\nSkriv på norsk i Markdown:\n- **Kort oppsummering** — 2–3 setninger.\n- **Sentrale svar** — gruppert etter tema; korte sitater i kursiv hvis viktige.\n- **Tematiske observasjoner** — mønstre i svarene.\n- **Oppfølgingsspørsmål** — hvis intervjueren noterte noen.\n\nIkke parafrasér når et direkte sitat er klarere.",
        en: "You produce notes from an interview.\n\nSources: [Notater] (authoritative) and [Transkripsjon] (may have errors).\n\nPreserve the Q&A structure and the interviewee's voice where possible.\n\nReply in {LANGUAGE} using Markdown:\n- **Short summary** — 2–3 sentences.\n- **Key responses** — grouped by theme; short direct quotes in italics when important.\n- **Thematic observations** — patterns across answers.\n- **Follow-up questions** — if the interviewer noted any.\n\nDo not paraphrase when a direct quote is clearer.",
    },
    Preset {
        value: "brainstorm",
        no: "Du oppsummerer en idémyldring.\n\nKilder: [Notater] (autoritativt) og [Transkripsjon] (kan ha feil).\n\nMål: fange alle ideene som ble nevnt — også de som ble forkastet.\n\nSkriv på norsk i Markdown:\n- **Tema** — hva ble brainstormet.\n- **Ideer** — bullets, grupper relaterte ideer.\n- **Vurdering** — hvilke ideer ble valgt eller forkastet, og hvorfor.\n- **Neste steg** — hvis avtalt.\n\nIkke siler ideer på egen hånd. Inkluder alt.",
        en: "You summarize a brainstorming session.\n\nSources: [Notater] (authoritative) and [Transkripsjon] (may have errors).\n\nGoal: capture every idea mentioned — including ones that were rejected.\n\nReply in {LANGUAGE} using Markdown:\n- **Topic** — what was being brainstormed.\n- **Ideas** — bullets, group related ideas together.\n- **Evaluation** — which ideas were chosen or rejected, and why.\n- **Next steps** — if agreed.\n\nDo not filter ideas yourself. Include everything.",
    },
    Preset {
        value: "voice_memo",
        no: "Du rydder opp i et stemmenotat.\n\nKilder: [Notater] (autoritativt) og [Transkripsjon] (kan ha feil). Bruk notatene til å rette transkripsjonsfeil.\n\nBrukeren snakket fritt; oppgaven er å gjøre innholdet lesbart uten å miste detaljer.\n\nSkriv på norsk i Markdown. Behold brukerens stemme. Strukturen kan være enkel — bullets eller korte avsnitt etter behov. Ikke legg til seksjoner som ikke trengs.\n\nMål: konsist, lesbart, trofast mot innholdet.",
        en: "You clean up a voice memo.\n\nSources: [Notater] (authoritative) and [Transkripsjon] (may have errors). Use the notes to correct transcription mistakes.\n\nThe user spoke freely; the task is to make the content readable without losing details.\n\nReply in {LANGUAGE} using Markdown. Preserve the user's voice. Structure can be simple — bullets or short paragraphs as needed. Do not add sections that are not needed.\n\nGoal: concise, readable, faithful to the content.",
    },
];
