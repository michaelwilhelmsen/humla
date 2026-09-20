export type Release = {
  version: string;
  date: string;
  title: string;
  summary: string;
  visual?: "filters" | "privacy" | "activity" | "updates";
  paragraphs: string[];
  highlights: string[];
  issues: { title: string; reference: string; status: "Open" | "Closed"; url: string }[];
};

export const releases: Release[] = [
  {
    version: "0.63.0",
    date: "2026-09-20",
    title: "Claude joins the lineup",
    summary: "Summaries and chat with Anthropic, using your own API key.",
    paragraphs: [
      "You can now pick Anthropic as the provider for AI summaries and for chat over your notes. Add your Claude API key once under Settings, choose a model, and summaries stream into the panel just as they do with OpenAI.",
      "Model pickers are live for both cloud providers: Humla asks the API for the models your key can use, so new releases show up without waiting for an app update. Chat on Anthropic searches your notes by keyword, since Claude has no embeddings endpoint.",
    ],
    highlights: [
      "Choose Anthropic for summaries, per note or as the default.",
      "Chat over your notes with Claude, tools and citations included.",
      "OpenAI and Anthropic model lists come straight from your account.",
    ],
    issues: [
      { title: "Anthropic API as a summary (and chat) provider", reference: "#183", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/183" },
    ],
  },
  {
    version: "0.62.0",
    date: "2026-09-15",
    title: "A home for what’s new",
    summary: "The latest from Humla, right on your home screen.",
    visual: "updates",
    paragraphs: [
      "Your home screen now shows the three latest releases, with the newest at the top. Open any entry for a closer look at the changes, illustrated with previews of the features.",
      "Related GitHub issues link directly to the work behind each release. Settings also gets a quieter look, with borderless panels, a sidebar that matches the app, and even padding.",
    ],
    highlights: [
      "Read detailed release notes without leaving Humla.",
      "Explore feature previews and related GitHub issues.",
      "Enjoy a cleaner, more consistent Settings panel.",
    ],
    issues: [],
  },
  {
    version: "0.61.0",
    issues: [],
    date: "2026-09-15",
    title: "Find the right note, faster",
    summary: "A little more order for a growing library.",
    visual: "filters",
    paragraphs: [
      "Your notes add up. Now you can narrow your library by what a note contains, which client it belongs to, and who created it — without remembering its title.",
      "The filter bar is available in All notes and inside folders. Combine filters to find exactly what you need, then clear them to see the full picture again.",
    ],
    highlights: [
      "Filter by notes, recordings, transcripts, or summaries.",
      "Narrow by client or, in a team workspace, by author.",
      "Find unfiled notes in All notes with the No folder filter.",
    ],
  },
  {
    version: "0.60.0",
    issues: [
      { title: "A note can be private inside a workspace", reference: "#191", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/191" },
    ],
    date: "2026-09-14",
    title: "A little space for private notes",
    summary: "Keep a note to yourself, even in a team workspace.",
    visual: "privacy",
    paragraphs: [
      "Some notes are just for you. You can now keep a note private inside a shared workspace, so personal thoughts and early drafts can live alongside your team’s work.",
      "The note’s author controls its visibility. Notes that only one person can access also skip the shared recording lock, keeping personal recording sessions straightforward.",
    ],
    highlights: [
      "Choose whether a workspace note is private or shared.",
      "Only the author can change a note’s visibility.",
      "Permanently deleting a note also removes its version history.",
    ],
  },
  {
    version: "0.59.0",
    issues: [
      { title: "Note cards show no live activity (recording / transcribing / summarizing)", reference: "#190", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/190" },
    ],
    date: "2026-09-11",
    title: "Know what’s happening at a glance",
    summary: "Live activity, right on your note cards.",
    visual: "activity",
    paragraphs: [
      "You no longer need to open a note to see whether Humla is still working on it. Note cards now show activity as it happens, so you can keep browsing while a recording or processing step is underway.",
      "A recording card also shows elapsed capture time. Its timer follows the recording itself, so navigating away and coming back won’t reset the clock.",
    ],
    highlights: [
      "See active recording and processing states on note cards.",
      "Follow elapsed recording time from your library.",
      "Return from an unfiled note directly to All notes.",
    ],
  },
];

export const latestReleases = [...releases]
  .sort((a, b) => b.date.localeCompare(a.date) || b.version.localeCompare(a.version, undefined, { numeric: true }))
  .slice(0, 3);
