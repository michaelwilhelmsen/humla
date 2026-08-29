// A populated note library for the visual-check harness — one note per shape
// the grid has to render: summarized with a client, recorded but unsummarized,
// typed-only, untitled, a one-line voice memo, and a note filed nowhere.
// Unit tests build their own minimal notes; this exists so a card can be judged
// against realistic length and density.
import type { Client, Folder, Note } from "../lib/ipc";
import { makeNote } from "./fixtures";

const HOUR = 3_600_000;
const DAY = 24 * HOUR;

export const DEMO_FOLDERS: Folder[] = [
  { id: "f1", name: "Kunder", created_at: 0, updated_at: 0 },
  { id: "f2", name: "Produkt", created_at: 0, updated_at: 0 },
  { id: "f3", name: "Rekruttering", created_at: 0, updated_at: 0 },
];

export const DEMO_CLIENTS: Client[] = [
  { id: "c1", name: "Nordvik AS", created_at: 0, updated_at: 0 },
  { id: "c2", name: "Sparebanken Vest", created_at: 0, updated_at: 0 },
];

function turns(pairs: [string, string][], repeat = 6): string {
  const out: string[] = [];
  for (let r = 0; r < repeat; r++) {
    for (const [who, what] of pairs) out.push(`${who}: ${what}`);
  }
  return out.join("\n");
}

export function demoNotes(now = Date.now()): Note[] {
  return [
    makeNote({
      id: "n1",
      title: "Nordvik — kickoff for migreringsprosjektet",
      folder_id: "f1",
      client_id: "c1",
      created_at: now - 2 * HOUR,
      summary:
        "Nordvik vil flytte hele arkivet over før nyttår, men mangler folk til å kvalitetssikre dataene selv.\n\n" +
        "- Migreringen deles i tre puljer, første pulje 15. september\n" +
        "- Vi tar kvalitetssikringen de første fire ukene, deretter overtar de\n" +
        "- Prisen holdes uendret så lenge volumet er under 40 000 dokumenter\n" +
        "- Kari sender over dagens felt-mapping innen fredag",
      transcript: turns([
        ["Michael", "Så hovedspørsmålet er egentlig om dere klarer å kvalitetssikre selv underveis."],
        ["Kari Nordvik", "Vi har to som kan bidra, men ikke fulltid. Det er der jeg tror det knekker."],
        ["Ola Berg", "Vi kan ta de første fire ukene, så overtar dere når mønsteret sitter."],
        ["Kari Nordvik", "Da må vi ha en klar definisjon på hva som er godkjent."],
      ], 9),
      body: "<p>Kickoff Nordvik</p><p>Husk å spørre om felt-mapping</p>",
    }),
    makeNote({
      id: "n2",
      title: "Standup",
      folder_id: "f2",
      created_at: now - 5 * HOUR,
      transcript: turns([
        ["Speaker 1", "Jeg ble ferdig med importen i går kveld, den ligger på main nå."],
        ["Speaker 2", "Fint. Jeg tar diarisering videre i dag."],
        ["Speaker 3", "Jeg er blokkert på designtokens, trenger en avklaring."],
      ], 3),
    }),
    makeNote({
      id: "n3",
      title: "Tanker om prisingen",
      created_at: now - 7 * HOUR,
      body:
        "<p>Per sete er lettere å forklare enn flat pris, men gjør småteam dyrere.</p>" +
        "<p>Kanskje 3 gratis seter og så per sete over det? Må regne på hva det gjør med de 12 workspacene vi har nå.</p>" +
        "<p>Sjekk hva Steno tar.</p>",
    }),
    makeNote({
      id: "n4",
      title: "1:1 med Hege",
      folder_id: "f3",
      created_at: now - DAY - 3 * HOUR,
      summary:
        "Hege trives, men savner mer eierskap til transkripsjonsdelen.\n\n" +
        "- Hun tar over hele STT-området fra oktober\n" +
        "- Vi ser på kurs i CoreML til vinteren\n" +
        "- Neste 1:1 om tre uker",
      transcript: turns([
        ["Michael", "Hvordan har de siste ukene vært, egentlig?"],
        ["Hege", "Bra, men jeg føler jeg bare fikser ting andre har begynt på."],
      ], 14),
    }),
    makeNote({
      id: "n5",
      title: "Sparebanken Vest — demo og innvendinger",
      folder_id: "f1",
      client_id: "c2",
      created_at: now - DAY - 6 * HOUR,
      summary:
        "Demoen gikk bra helt til spørsmålet om hvor lyden lagres.\n\n" +
        "- De krever at ingenting forlater maskinen; lokal Whisper løser det\n" +
        "- Compliance vil ha et skriftlig svar før de går videre\n" +
        "- Ny demo med deres egne opptak neste uke",
      transcript: turns([
        ["Michael", "Alt dere ser her kjører lokalt, ingenting går ut av maskinen."],
        ["Trond Haugen", "Men transkripsjonen da? Den går vel til en leverandør?"],
        ["Michael", "Den kan gjøre det, men den trenger ikke. Dere velger per språk."],
        ["Anne Lie", "Compliance kommer til å be om det svaret skriftlig."],
        ["Trond Haugen", "Da tar vi en runde til med våre egne opptak."],
      ], 8),
    }),
    makeNote({
      id: "n6",
      title: "",
      created_at: now - DAY - 9 * HOUR,
      transcript: turns([["Speaker 1", "Husk å ringe regnskapsfører om mva-oppgaven før fredag."]], 2),
    }),
    makeNote({
      id: "n7",
      title: "Produktmøte — hva slipper vi i 0.54",
      folder_id: "f2",
      created_at: now - 3 * DAY,
      summary:
        "Vi kutter kortvisningen fra 0.54 og tar den i 0.55 i stedet.\n\n" +
        "- MCP-serveren er klar og går ut nå\n" +
        "- Redigering av transkripsjon trenger én runde til med testing\n" +
        "- Ingen skjemaendringer i denne slippen",
      transcript: turns([
        ["Michael", "Spørsmålet er om kortvisningen er ferdig nok til å gå ut nå."],
        ["Hege", "Den ser bra ut, men den er ikke testet på små vinduer."],
        ["Ola Berg", "Da tar vi den i neste. MCP er uansett den store nyheten."],
        ["Hege", "Enig. Jeg vil ikke slippe noe vi må rulle tilbake."],
      ], 7),
    }),
    makeNote({
      id: "n8",
      title: "Intervju — senior iOS",
      folder_id: "f3",
      created_at: now - 4 * DAY,
      transcript: turns([
        ["Speaker 1", "Kan du fortelle om et prosjekt du er stolt av?"],
        ["Speaker 2", "Ja, vi bygget om hele synkroniseringslaget på tre måneder."],
        ["Speaker 3", "Hva var det vanskeligste med det?"],
      ], 11),
    }),
    makeNote({
      id: "n9",
      title: "Workshop: hva skal Humla ikke være",
      folder_id: "f2",
      created_at: now - 5 * DAY,
      summary:
        "Vi ble enige om tre ting produktet aldri skal gjøre.\n\n" +
        "- Ingen serverside-logging av spørsmål\n" +
        "- Ingen lyd som forlater maskinen uten at brukeren har slått det på\n" +
        "- Ingen abonnement som låser gamle notater",
      transcript: turns([
        ["Michael", "La oss begynne med det vi ikke skal gjøre."],
        ["Hege", "Ingen logging av det folk spør om. Det er hele poenget."],
        ["Ola Berg", "Og ingen lyd ut av maskinen uten et eksplisitt valg."],
        ["Kari Nordvik", "Jeg vil legge til: gamle notater må alltid være tilgjengelige."],
        ["Trond Haugen", "Enig, ellers er det bare enda en SaaS."],
        ["Anne Lie", "Kan vi skrive det ned som prinsipper?"],
      ], 6),
    }),
    makeNote({
      id: "n10",
      title: "Notater fra konferansen",
      created_at: now - 6 * DAY,
      body:
        "<p>Tre foredrag verdt å huske:</p><p>Diarisering på enhet er nærmere enn jeg trodde.</p>" +
        "<p>Alle snakker om agenter, ingen om hvor dataene ligger.</p>",
    }),
    makeNote({
      id: "n11",
      title: "Nordvik — oppfølging etter puljen",
      folder_id: "f1",
      client_id: "c1",
      created_at: now - 9 * DAY,
      summary:
        "Første pulje gikk gjennom med 1,2 % avvik, godt under grensen.\n\n" +
        "- Avvikene er nesten alle i ett felt: saksbehandler\n" +
        "- Kari tar en manuell runde på de 340 dokumentene\n" +
        "- Pulje to starter mandag",
      transcript: turns([
        ["Michael", "Vi endte på 1,2 prosent avvik, det er godt innenfor."],
        ["Kari Nordvik", "Men nesten alt ligger i saksbehandlerfeltet."],
      ], 10),
    }),
    makeNote({
      id: "n12",
      title: "Talenotat — ide om folder-chat",
      created_at: now - 11 * DAY,
      transcript: turns([
        ["Speaker 1", "Hvis chat kan avgrenses til en mappe, blir kundemapper til et minne per kunde."],
      ], 4),
    }),
    makeNote({
      id: "n13",
      title: "Retro august",
      folder_id: "f2",
      created_at: now - 14 * DAY,
      summary:
        "For mye halvferdig arbeid i parallell.\n\n" +
        "- Maks to store ting i gang samtidig\n" +
        "- Vi skriver ADR før vi begynner, ikke etter",
      transcript: turns([
        ["Michael", "Vi hadde fem ting i gang samtidig i august."],
        ["Hege", "Og ingen av dem ble ferdige før den siste uka."],
        ["Ola Berg", "Maks to. Ellers husker ingen hva som er på vent."],
      ], 9),
    }),
    makeNote({
      id: "n14",
      title: "Gammel skisse til onboarding",
      created_at: now - 22 * DAY,
      body: "<p>Fire steg: mikrofon, språk, modell, første notat.</p>",
    }),
  ];
}
