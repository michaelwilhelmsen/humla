# OpenAI nye STT-modeller (annonsert ca. 2026-07-31)

> ## Rettelser etter primærkilde-verifisering (2026-07-31)
>
> Denne rapporten ble skrevet før prissiden og API-referansen ble hentet direkte. Følgende punkter er **siden bekreftet mot primærkilde** og overstyrer `[SANNSYNLIG]`-merkingen nedenfor:
>
> - **Pris — BEKREFTET** mot `developers.openai.com/api/docs/pricing`: `gpt-transcribe` **$0.0045/min**, `gpt-live-transcribe` **$0.017/min**, `gpt-4o-transcribe` $0.006/min, `whisper-1` $0.006/min. Advarselen i pkt. 6 om at tallene måtte verifiseres er dermed innfridd — tallene stemte.
> - **Streaming-event-skjema — BEKREFTET** mot API-referansen: `stream=true` sender `transcript.text.delta` (felt: `type`, `delta`, `logprobs`) og `transcript.text.done` (felt: `type`, `text`, `logprobs`, `usage`). Hullet i pkt. 3 er lukket.
> - **`languages` (flertall) erstatter `language` — BEKREFTET** ordrett i transkripsjonsguiden. Viktig tillegg fra lanseringsvideoen: språkhint er **valgfritt**, og modellen følger flerspråklig tale automatisk som standard. Å sende ett enkelt språk kan derfor motvirke code-switching. Se [#125](https://github.com/michaelwilhelmsen/humla/issues/125).
> - **WER-tallene (~40% → ~19%) — FORKASTET.** Lanseringsvideoen oppgir ingen WER- eller benchmark-tall overhodet. Tallene i pkt. 5 og sammendraget tilhører etter alt å dømme en tidligere modellgenerasjon og **skal ikke siteres** for disse modellene.
> - **Fortsatt uavklart:** om `gpt-transcribe` aksepterer `timestamp_granularities[]=word`. Dette avgjør om modellen kan bli ny standard — se #125.
>
> Lanseringsvideoens innhold er oppsummert i [`openai-stt-launch-video.md`](./openai-stt-launch-video.md).

## Sammendrag

- To nye modeller lansert ~28.-29. juli 2026: **`gpt-transcribe`** (fil/batch) og **`gpt-live-transcribe`** (lav-latens/live) [DOK/SANNSYNLIG, se pkt. 1]
- Ingen ordrette timestamps, SRT/VTT-subtitle-output, speaker-diarisering eller engelsk-oversettelse ved lansering på noen av de to nye modellene — de funksjonene er reservert `whisper-1` / `gpt-4o-transcribe-diarize` [SANNSYNLIG]
- Pris: `gpt-transcribe` $0.0045/min (billigere enn `gpt-4o-transcribe`s $0.006/min); `gpt-live-transcribe` $0.017/min (samme som `gpt-realtime-whisper`) [SANNSYNLIG — sekundærkilde, ikke funnet i primær prisside ennå]
- WER-forbedring rapportert: fra ~40% (whisper-1) til ~19% (gpt-transcribe) på Common Voice 22 språk (tredjepartsrapportering) [SANNSYNLIG]
- Kritisk for Humla: begge nye modeller mangler funksjoner Humla er avhengig av per språk-chunk (timestamps for diarize-alignment, SRT/VTT) — se konklusjon

## 1. Modell-ID-er

- `gpt-transcribe` — nytt, for ferdig innspilte filer / batch [DOK — developers.openai.com/api/docs/guides/transcription, hentet 2026-07-31]
- `gpt-live-transcribe` — nytt, for lav-latens/live STT [DOK — samme kilde]
- Eksisterende/legacy som fortsatt støttes: `gpt-4o-transcribe`, `gpt-4o-mini-transcribe`, `gpt-4o-transcribe-diarize` (speaker labeling), `whisper-1` (timestamps/SRT/VTT/oversettelse), `gpt-realtime-whisper` [DOK — samme kilde]
- Blogg: "Introducing next-generation audio models in the API" (openai.com/index/introducing-our-next-generation-audio-models/) og OpenAI Developer Community-tråd bekrefter navnene og lansering 28.-29. juli 2026 [SANNSYNLIG — sekundærkilder: Techgenyz, Gigazine, community.openai.com, ikke selv åpnet ennå]

## 2. REST-endepunkt /v1/audio/transcriptions

- `gpt-transcribe` støtter `POST /v1/audio/transcriptions` med multipart `file`-opplasting [DOK — developers.openai.com/api/reference/resources/audio/subresources/transcriptions/methods/create, hentet 2026-07-31]
- Dokumenterte `response_format`-verdier på selve referansesiden: `json`, `diarized_json`, `verbose_json` (`diarized_json` gjelder trolig kun `gpt-4o-transcribe-diarize`, ikke bekreftet for `gpt-transcribe` spesifikt) [DOK for verdiene som eksisterer i skjemaet; SANNSYNLIG for hvilken modell som støtter hvilken — siden manglet en fullstendig per-modell-tabell og henviste videre til `/llms.txt`]
- `text`/`srt`/`vtt` er IKKE bekreftet støttet for `gpt-transcribe`/`gpt-live-transcribe` — sekundærkilder (Techgenyz/Gigazine-oppsummering) sier eksplisitt at SRT/VTT-subtitle-output **ikke** støttes av de to nye modellene ved lansering, og er forbeholdt `whisper-1` [SANNSYNLIG — sekundærkilde, ikke funnet direkte bekreftet i referansen]
- `gpt-live-transcribe` bruker IKKE `/v1/audio/transcriptions` for sitt hovedbruk — den kobles via `/v1/realtime` (se pkt. 3); changelog sier eksplisitt at endepunktene er `v1/audio/transcriptions` (GPT Transcribe) og `v1/realtime` (GPT Live Transcribe) [DOK — developers.openai.com/api/docs/changelog, hentet 2026-07-31]

## 3. Streaming

## 3. Streaming (fil-upload stream=true, og Realtime API)

### (a) `stream=true` på `/v1/audio/transcriptions` (streamet svar på en fullført fil)
- Referansesiden viser `stream` som en boolsk parameter (eksempel med `true`) på create-transcription-endepunktet [DOK — API-referansen, hentet 2026-07-31], men eksakt SSE-event-skjema (delta-events, feltnavn) ble ikke funnet i det hentede utdraget — siden pekte videre til `/llms.txt` for full detalj [IKKE FUNNET — søkte etter fullt parametere-/event-skjema i referansen og i changelog, fikk kun oppsummert utdrag fra WebFetch, ikke rå-schema]
- Ikke bekreftet om `gpt-live-transcribe` i det hele tatt eksponeres via `/v1/audio/transcriptions` + `stream=true` — changelog knytter den modellen til `/v1/realtime` som sin primære vei (se over), så fil-upload-streaming er trolig `gpt-transcribe`s domene [SANNSYNLIG]

### (b) Realtime API (WebSocket/WebRTC) — `gpt-live-transcribe`
Kilde: [DOK — developers.openai.com/api/docs/guides/realtime-transcription, hentet 2026-07-31]

- Transport: WebSocket eller WebRTC (egne under-guider for tilkoblingsdetaljer, URL ikke gjengitt i utdraget) [DOK for at begge transporter finnes; IKKE FUNNET eksakt endepunkt-URL i det hentede utdraget]
- Sesjonsoppsett via `session.update`:
```json
{
  "type": "session.update",
  "session": {
    "type": "transcription",
    "audio": {
      "input": {
        "format": { "type": "audio/pcm", "rate": 24000 },
        "transcription": { "model": "gpt-live-transcribe" },
        "turn_detection": null
      }
    }
  }
}
```
- Sende lyd: `input_audio_buffer.append` (base64-kodet PCM)
- Committe en "turn": `input_audio_buffer.commit`
- Motta delvis tekst: `conversation.item.input_audio_transcription.delta` (fortløpende deltas fra `gpt-live-transcribe`)
- Motta ferdig transkripsjon for en committed turn: `conversation.item.input_audio_transcription.completed` — dette er der `gpt-transcribe` også kan brukes (som "final transcript of committed Realtime turns", jf. changelog-sitatet i pkt. 1)
- `delay`-parameter styrer latens/nøyaktighet-avveining: `minimal | low | medium | high | xhigh`
- Ingen ord-tidsstempler, speaker-labels eller confidence-scores fra `gpt-live-transcribe` [DOK — samme guide]

## 4. prompt og language

- `prompt` støttes på begge nye modeller: "Free-form context about the recording, such as its topic or setting" — dette er en friteksts kontekst-parameter, IKKE nødvendigvis samme semantikk som Whisper sin `initial_prompt`/fortsettelses-priming som Humla bruker i `prior_context` [DOK for at parameteren finnes — developers.openai.com/api/docs/guides/transcription og realtime-transcription-guiden; SANNSYNLIG at oppførselen er "kontekstbeskrivelse" snarere enn ordrett fortsettelse, basert på ordlyden i dokumentasjonen — ikke eksplisitt testet]
- `keywords`: literal termer som kan forekomme i lyden — tilsvarer Humlas `custom_vocabulary`-bruk av Deepgrams `keyterm`/`keywords`. Regel: ingen `<>`, linjeskift eller vognretur i verdiene [DOK — realtime-transcription-guiden]
- `language` (entall) er IKKE parameteren — dokumentasjonen bruker konsekvent **`languages`** (flertall), som tar en liste av ISO 639-1-koder (f.eks. `en`, `fr`), enkelte ISO 639-3-koder, og regionale zh-koder, for å angi *forventede* input-språk (ikke tvunget enkelt-språk som Whisper sin `language`-parameter) [DOK — developers.openai.com/api/docs/guides/transcription (WebFetch-utdrag) og realtime-transcription-guiden]
- Konsekvens for Humla: dagens `TranscribeCtx.language` (singular) og bias-modell må trolig mappes til en liste ved evt. adapter for disse modellene — se Hull og usikkerhet

## 5. Flerspråklighet og benchmarks (WER)

- Flerspråklighet: `languages`-parameteren lar deg oppgi flere forventede språk per økt/forespørsel; `gpt-transcribe` (dokumentert som "processes transcription after committed turns") inkluderer også detected-language-output [DOK for parameterens eksistens — se pkt. 4; SANNSYNLIG for "detected language output"-detaljen, kun observert i én WebFetch-oppsummering av realtime-transcription-guiden]
- WER: tredjepartsrapportering (Windowsreport, oppsummert via websøk, ikke selv åpnet som primærkilde) siterer en forbedring fra ca. 40 % (whisper-1) til ca. 19 % (gpt-transcribe) på Common Voice, 22 språk [SANNSYNLIG — sekundærkilde, tall ikke bekreftet mot en OpenAI-primærkilde med rå tabell]
- OpenAIs eget "Context Aware ASR"-benchmark (fra samme sekundærrapportering): kontekst (prompt) forbedret semantisk nøyaktighet fra 38,5 % → 44,6 % for `gpt-live-transcribe`, og fra 41,6 % → 45,2 % for `gpt-transcribe` [SANNSYNLIG — sekundærkilde, tallene er ikke funnet i en primær OpenAI-blogg/tabell i dette søket]
- Direkte sammenligning mot `gpt-4o-transcribe` på WER er IKKE funnet med tall — kun prisdifferansen ble funnet (se pkt. 6) [IKKE FUNNET — søkte etter "gpt-transcribe vs gpt-4o-transcribe WER benchmark"]

## 6. Pris

- `gpt-transcribe`: **$0,0045/min** lyd — lavere enn `gpt-4o-transcribe`s $0,006/min [SANNSYNLIG — sekundærkilde (Windowsreport-oppsummering via websøk); ikke bekreftet mot OpenAIs offisielle prisside i dette søket, budsjett brukt opp før den kunne hentes direkte]
- `gpt-live-transcribe`: **$0,017/min** — samme som `gpt-realtime-whisper` [SANNSYNLIG — samme sekundærkilde]
- ⚠️ Disse tallene bør verifiseres direkte mot https://openai.com/api/pricing/ før de brukes i et tilbud/budsjett — de er under 24 timer gamle på annonseringstidspunktet og kun sett gjennom en websøk-oppsummering, ikke en primær prisside

## 7. Ikke-støttede parametre

- **Ord-tidsstempler (`timestamp_granularities`), speaker-labels og confidence-scores**: eksplisitt IKKE støttet av `gpt-live-transcribe` [DOK — developers.openai.com/api/docs/guides/realtime-transcription]. For `gpt-transcribe` er dette ikke like eksplisitt bekreftet, men samme begrensning nevnes i sekundærkilder for begge nye modeller ved lansering [SANNSYNLIG]
- **SRT/VTT-subtitle-format og engelsk-oversettelse**: sekundærkilder sier disse er reservert `whisper-1` og ikke tilgjengelig i de to nye modellene ved lansering [SANNSYNLIG — ikke bekreftet ord-for-ord i primærkilden i dette søket]
- **Speaker-diarisering**: reservert `gpt-4o-transcribe-diarize`; ikke en del av `gpt-transcribe`/`gpt-live-transcribe` [DOK for at diarize er en egen modell — developers.openai.com/api/docs/guides/transcription; SANNSYNLIG at de to nye modellene mangler funksjonen, jf. sekundærkilder]
- **`temperature`**: ett websøk-treff antyder at temperature-parameteren fortsatt eksisterer i transcription-API-et generelt (0–1-skala, auto-økning ved 0), men om `gpt-transcribe`/`gpt-live-transcribe` faktisk *respekterer* verdien eller ignorerer den (slik nyere OpenAI-modeller ofte gjør) er IKKE bekreftet [IKKE FUNNET — søkte etter eksplisitt "temperature not supported gpt-transcribe", fikk bare generell API-beskrivelse tilbake]
- `response_format`-verdiene `text`, `srt`, `vtt` ble ikke funnet i det tilgjengelige skjema-utdraget for `gpt-transcribe` (kun `json`, `diarized_json`, `verbose_json` var synlige) — usikkert om disse tre eldre formatene i det hele tatt eksisterer som valg for denne modellen [IKKE FUNNET — API-referansesiden henviste videre til `/llms.txt` for komplett skjema, som ikke ble hentet innenfor søkebudsjettet]

## Hull og usikkerhet

- **`/llms.txt`** på developers.openai.com ble aldri hentet — API-referansesiden pekte dit for det fullstendige skjemaet for `/v1/audio/transcriptions` (alle enum-verdier for `response_format`, per-modell støttematrise). Bør hentes før implementasjon.
- Eksakt SSE/streaming-event-skjema for `stream=true` på selve `/v1/audio/transcriptions`-endepunktet (feltnavn på delta-events, om det er `transcript.text.delta` e.l.) ble ikke funnet — kun at parameteren finnes.
- Offisiell OpenAI-blogg ("Introducing next-generation audio models in the API") ga 403 Forbidden ved direkte henting — alt om lansering/WER/pris hviler på sekundærkilder (Techgenyz, Gigazine, Windowsreport, community.openai.com-tråd) oppsummert via WebSearch, IKKE lest i fulltekst av meg.
- Offisiell prisside (openai.com/api/pricing) ikke hentet direkte — prisene i pkt. 6 er annenhånds.
- Uklart om `gpt-transcribe` kan nås via streamet fil-upload OG om `gpt-live-transcribe` i det hele tatt har et REST-alternativ utenom Realtime API.
- `temperature`-støtte/-effekt på de to nye modellene ikke bekreftet.
- Om `text`/`srt`/`vtt` `response_format`-verdier eksisterer for `gpt-transcribe` er ikke bekreftet — kun `json`/`diarized_json`/`verbose_json` sett i utdraget.
- Søkebudsjett brukt: 6 av 12 tillatte web-operasjoner (2×WebFetch guide-side, 1×WebSearch annonsering, 1×WebFetch blogg [403, feilet], 1×WebFetch changelog, 1×WebFetch API-referanse, 1×WebFetch realtime-guide, 1×WebSearch param-detaljer — totalt 7 kall, godt innenfor budsjettet, men stanset for å prioritere skriving fremfor å jage `/llms.txt` og prissiden i tillegg).

## Konklusjon / anbefaling

For Humlas `stt::BatchSttAdapter` (OpenAI-adapteren i `stt/openai.rs`):

- **`gpt-transcribe` er en god kandidat til å erstatte `whisper-1`/`gpt-4o-transcribe` som default OpenAI batch-provider** på pris (billigere) og rapportert WER — MEN Humla bruker i dag Whisper-stil `prior_context` (siste ~150 ord) som `initial_prompt` for kontinuitet på tvers av chunks. `gpt-transcribe`s `prompt`-parameter er dokumentert som en løs "beskriv opptaket/settingen"-kontekst, ikke nødvendigvis en ordrett fortsettelses-primer. Dette bør A/B-testes før bytte — hallusinasjons-mitigeringen i `transcribe_chunk` (pkt. 4.5 i CLAUDE.md) er bygget rundt Whisper-semantikk og kan oppføre seg annerledes.
- **Ingen av de nye modellene støtter word-level timestamps eller SRT/VTT** — Humlas VAD-chunk-tilnærming med `start_ms` fra sidecar dekker timing-behovet uavhengig av provider, så dette er trolig ikke en blokkerende begrensning for dagens arkitektur (Humla bruker ikke providerens egne timestamps for diarize-alignment; det gjøres separat via `assign_speaker`).
- **`gpt-live-transcribe` (Realtime API) er ikke rett frem å plugge inn** i dagens arkitektur, som er batch/VAD-chunk-basert (ikke en løpende WebSocket-økt mot Whisper). Å ta i bruk den ville kreve en ny adapter-form utenfor `BatchSttAdapter`-trait'et — vurderes ikke som en umiddelbar prioritet.
- **Anbefaling**: legg `gpt-transcribe` til som en ny `ProviderConfig`-variant ved siden av eksisterende OpenAI-valg (samme mønster som whisper-1/gpt-4o-transcribe-familien), test kvalitet og hallusinasjonsatferd med Humlas faktiske `prior_context`-bruk på norsk/engelsk lydmateriale, og hent `/llms.txt` + den offisielle prissiden før det caps av som default. Ikke invester i `gpt-live-transcribe` før en eventuell fremtidig streaming-arkitektur er på roadmap.
