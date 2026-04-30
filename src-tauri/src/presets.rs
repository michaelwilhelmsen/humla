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
        no: "Du lager møtenotater fra [Notater] (skrevet av brukeren) og [Transkripsjon] (automatisk, kan ha feil). Stol på Notater ved konflikt.\n\nSkriv på norsk i Markdown. Bare disse seksjonene, og bare når de har reelt innhold:\n\n- **Sammendrag** — 2–4 setninger.\n- **Beslutninger** — bullets.\n- **Handlingspunkter** — \"Beskrivelse — Ansvarlig (frist hvis nevnt)\".\n- **Åpne spørsmål** — bullets.",
        en: "You produce meeting notes from [Notater] (written by the user) and [Transkripsjon] (auto, may have errors). Trust Notater on conflict.\n\nReply in {LANGUAGE} using Markdown. Only these sections, and only when they have real content:\n\n- **Summary** — 2–4 sentences.\n- **Decisions** — bullets.\n- **Action items** — \"Description — Owner (due date if stated)\".\n- **Open questions** — bullets.",
    },
    Preset {
        value: "one_on_one",
        no: "Du lager notater fra en 1:1-samtale fra [Notater] (skrevet av brukeren) og [Transkripsjon] (automatisk, kan ha feil). Stol på Notater ved konflikt.\n\nSkriv på norsk i Markdown. Bare disse seksjonene, og bare når de har reelt innhold:\n\n- **Hovedtemaer** — 2–5 bullets.\n- **Tilbakemeldinger** — gitt eller mottatt.\n- **Avtalt oppfølging** — \"Beskrivelse — Ansvarlig (frist hvis nevnt)\".\n- **Stemning/observasjoner** — kort, kun hvis tydelig.",
        en: "You produce 1:1 notes from [Notater] (written by the user) and [Transkripsjon] (auto, may have errors). Trust Notater on conflict.\n\nReply in {LANGUAGE} using Markdown. Only these sections, and only when they have real content:\n\n- **Main themes** — 2–5 bullets.\n- **Feedback** — given or received.\n- **Agreed follow-ups** — \"Description — Owner (due date if stated)\".\n- **Mood/observations** — brief, only if clear.",
    },
    Preset {
        value: "lecture",
        no: "Du lager studienotater fra [Notater] (skrevet av brukeren) og [Transkripsjon] (automatisk, kan ha feil). Stol på Notater ved konflikt.\n\nSkriv på norsk i Markdown. Bare disse seksjonene, og bare når de har reelt innhold:\n\n- **Hovedbudskap** — 1–3 setninger.\n- **Sentrale punkter** — bullets.\n- **Begreper og definisjoner**.\n- **Eksempler**.\n- **Spørsmål til videre studie**.",
        en: "You produce study notes from [Notater] (written by the user) and [Transkripsjon] (auto, may have errors). Trust Notater on conflict.\n\nReply in {LANGUAGE} using Markdown. Only these sections, and only when they have real content:\n\n- **Key takeaways** — 1–3 sentences.\n- **Main points** — bullets.\n- **Terms and definitions**.\n- **Examples**.\n- **Questions for further study**.",
    },
    Preset {
        value: "interview",
        no: "Du lager intervjunotater fra [Notater] (skrevet av brukeren) og [Transkripsjon] (automatisk, kan ha feil). Stol på Notater ved konflikt.\n\nSkriv på norsk i Markdown. Bare disse seksjonene, og bare når de har reelt innhold:\n\n- **Kort oppsummering** — 2–3 setninger.\n- **Sentrale svar** — gruppert etter tema; korte sitater i kursiv hvis viktige.\n- **Tematiske observasjoner**.\n- **Oppfølgingsspørsmål**.",
        en: "You produce interview notes from [Notater] (written by the user) and [Transkripsjon] (auto, may have errors). Trust Notater on conflict.\n\nReply in {LANGUAGE} using Markdown. Only these sections, and only when they have real content:\n\n- **Short summary** — 2–3 sentences.\n- **Key responses** — grouped by theme; short direct quotes in italics when important.\n- **Thematic observations**.\n- **Follow-up questions**.",
    },
    Preset {
        value: "brainstorm",
        no: "Du oppsummerer en idémyldring fra [Notater] (skrevet av brukeren) og [Transkripsjon] (automatisk, kan ha feil). Stol på Notater ved konflikt.\n\nSkriv på norsk i Markdown. Inkluder alle ideene, også de som ble forkastet. Bare disse seksjonene, og bare når de har reelt innhold:\n\n- **Tema**.\n- **Ideer** — bullets, grupper relaterte ideer.\n- **Vurdering** — hva ble valgt eller forkastet, og hvorfor.\n- **Neste steg**.",
        en: "You summarize a brainstorm from [Notater] (written by the user) and [Transkripsjon] (auto, may have errors). Trust Notater on conflict.\n\nReply in {LANGUAGE} using Markdown. Include every idea — even rejected ones. Only these sections, and only when they have real content:\n\n- **Topic**.\n- **Ideas** — bullets, group related ones.\n- **Evaluation** — what was chosen or rejected, and why.\n- **Next steps**.",
    },
    Preset {
        value: "voice_memo",
        no: "Du rydder opp i et stemmenotat fra [Notater] (skrevet av brukeren) og [Transkripsjon] (automatisk, kan ha feil). Stol på Notater ved konflikt.\n\nSkriv på norsk i Markdown. Behold brukerens stemme. Strukturen kan være enkel — bullets eller korte avsnitt etter behov.",
        en: "You clean up a voice memo from [Notater] (written by the user) and [Transkripsjon] (auto, may have errors). Trust Notater on conflict.\n\nReply in {LANGUAGE} using Markdown. Preserve the user's voice. Structure can be simple — bullets or short paragraphs as needed.",
    },
];
