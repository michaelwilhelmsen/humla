export type Release = {
  version: string;
  date: string;
  title: string;
  summary: string;
  visual?: "filters" | "privacy" | "activity" | "updates" | "speakers";
  paragraphs: string[];
  highlights: string[];
  issues: { title: string; reference: string; status: "Open" | "Closed"; url: string }[];
};

export const releases: Release[] = [
  {
    version: "0.65.0",
    date: "2026-09-24",
    title: "Sharper speaker labels",
    summary: "Nemotron 3 tells up to eight voices apart, and call echo no longer adds any.",
    visual: "speakers",
    paragraphs: [
      "Humla now labels speakers with NVIDIA’s Nemotron 3, which tells up to eight voices apart and counts them itself. A meeting left on Auto used to come back as a single speaker whenever one person did most of the talking; now it doesn’t need the speaker count to get it right. It runs entirely on your Mac, on the Neural Engine.",
      "New installs get it straight away. If you’ve been using Humla already, nothing changes until you choose it: a card on the home screen offers the switch, a 193 MB download plus about two minutes of one-time setup. Community-1 stays in Settings, and takes over by itself for any note set to more than eight speakers.",
      "Calls played through your speakers come out better too. The mic hears the other side of the call a second time, as an echo, and Humla used to count that echo as extra people in the room. It’s now taken out before speakers are identified, and stopping after a long call finishes much sooner.",
    ],
    highlights: [
      "Nemotron 3 labels up to eight speakers and counts them itself.",
      "Switch from the home screen when it suits you; nothing changes until you do.",
      "Echo from calls on your speakers no longer shows up as extra voices.",
      "The retired Sortformer engine and its models are cleaned up.",
    ],
    issues: [
      { title: "Make Nemotron 3 the default diarization engine (replaces Sortformer)", reference: "#193", status: "Open", url: "https://github.com/michaelwilhelmsen/humla/issues/193" },
      { title: "Stop speaker echo from becoming extra voices on the mic", reference: "#196", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/196" },
      { title: "Speed up the echo pass and cut its memory on long takes", reference: "#200", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/200" },
    ],
  },
  {
    version: "0.64.0",
    date: "2026-09-21",
    title: "Keep a chat on one client",
    summary: "Pin a conversation to one client, or to one person’s speech.",
    paragraphs: [
      "Chat already lets you choose how much of your library a conversation can reach. Now you can also narrow it to a single client, or to the passages where one particular person was speaking. Set it once and every answer in that thread follows it — no need to repeat yourself each time you ask.",
      "Active filters sit just above the message box as small tags, so you can always see what a conversation is working from, and remove one with a single click.",
    ],
    highlights: [
      "Pin a conversation to one client.",
      "Narrow it to what one person said.",
      "See and clear active filters above the message box.",
    ],
    issues: [
      { title: "Chat: pin a conversation to one Client", reference: "#115", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/115" },
    ],
  },
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
