export type Release = {
  version: string;
  date: string;
  title: string;
  summary: string;
  visual?: "filters" | "privacy" | "activity" | "updates" | "speakers" | "webhook" | "export" | "two-factor" | "date" | "audio" | "copilot";
  paragraphs: string[];
  highlights: string[];
  issues: { title: string; reference: string; status: "Open" | "Closed"; url: string }[];
};

export const releases: Release[] = [
  {
    version: "0.72.1",
    date: "2026-10-29",
    title: "Humla opens on macOS Ventura and Sonoma again",
    summary: "Humla quit right after opening on Macs older than macOS Sequoia. This fixes it.",
    paragraphs: [
      "On macOS 13 Ventura and macOS 14 Sonoma, Humla quit as soon as it opened, with a “Humla quit unexpectedly” message. Part of the on-device transcription code needed something only macOS 15 Sequoia has. Humla now starts on macOS 13 and later again, and uses the newer feature only on Macs that have it.",
    ],
    highlights: [
      "Humla opens again on macOS 13 Ventura and macOS 14 Sonoma.",
    ],
    issues: [],
  },
  {
    version: "0.72.0",
    date: "2026-10-27",
    title: "Use your GitHub Copilot",
    summary: "Summaries and chat on the Copilot subscription you already have, no API key needed.",
    visual: "copilot",
    paragraphs: [
      "GitHub Copilot is now a provider for summaries, titles and chat over your notes. Pick it under Settings → Summary or Settings → Chat, sign in with GitHub by entering a short code, and choose a model. You get the models your Copilot plan includes, such as Claude and GPT, and the calls count against your Copilot plan instead of an API key.",
      "Humla reaches Copilot through GitHub's Copilot CLI. If it's already on your Mac, Humla finds it; if not, Settings downloads it for you (about 120 MB). Chat on Copilot searches your notes by keyword, as it does on Anthropic.",
      "Also fixed: chat on some newer OpenAI models, such as GPT-6.1 Sol, failed with an error about reasoning_effort. It works again.",
    ],
    highlights: [
      "Pick GitHub Copilot for summaries and chat.",
      "Sign in with GitHub, no API key to paste.",
      "Choose from the models your Copilot plan includes.",
    ],
    issues: [
      { title: "GitHub Copilot", reference: "#227", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/227" },
    ],
  },
  {
    version: "0.71.0",
    date: "2026-10-24",
    title: "Save a meeting’s audio",
    summary: "Save the recording behind a note as one audio file, wherever you want it.",
    visual: "audio",
    paragraphs: [
      "A note’s ⋯ menu now has Save audio…, right below Export…. It lists every recording on the note with its length, then saves them joined in order as one file, as M4A or WAV, wherever you pick. Use it to keep a meeting somewhere else, share it, or open it in another app.",
      "Humla only has audio to save when Keep recorded audio is on under Settings → Recording. If some of a note’s recordings aren’t stored on this Mac, the list marks them as missing and the file leaves them out.",
    ],
    highlights: [
      "Save a note’s audio from ⋯ → Save audio….",
      "Choose M4A or WAV.",
      "Every recording on the note is joined into one file, in order.",
    ],
    issues: [
      { title: "Button for save audio records from note", reference: "#224", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/224" },
    ],
  },
  {
    version: "0.70.0",
    date: "2026-10-20",
    title: "Date your notes",
    summary: "Change when a note happened, and imported recordings keep the day they were made.",
    visual: "date",
    paragraphs: [
      "Click the date under a note’s title to pick the day and time the meeting actually happened. The note moves to its place in your notes right away, and the change syncs to your other Macs and your team. Its title stays as it is.",
      "Imported recordings now get their date from the file. Humla reads the recording time that phones and Voice Memos save in the file, and otherwise uses the file’s own dates, so a recording from last spring no longer shows up as today’s meeting. Notes you imported before this update keep the import date, so set those by hand.",
    ],
    highlights: [
      "Click the date on a note to change it.",
      "Imported recordings are dated by when they were recorded.",
      "Notes can’t be dated in the future.",
    ],
    issues: [
      { title: "Feature request: edit old notes", reference: "#222", status: "Open", url: "https://github.com/michaelwilhelmsen/humla/issues/222" },
    ],
  },
  {
    version: "0.69.2",
    date: "2026-10-20",
    title: "Humla works in Claude Code again",
    summary: "The latest Claude Code connected to Humla but couldn’t see any of its tools. This fixes it.",
    paragraphs: [
      "If you search your notes from Claude Code, recent versions of Claude Code showed Humla as connected but found none of its tools, and reconnecting gave an error about missing fields. Claude Code now opens a connection a newer way, and Humla’s server didn’t handle it. 0.69.2 handles both the new way and the old one, so Claude Code, Codex and other clients can search and read your notes again.",
      "Once you’ve updated, run /mcp in Claude Code and reconnect humla.",
    ],
    highlights: [
      "Humla’s tools show up in Claude Code again.",
    ],
    issues: [],
  },
  {
    version: "0.69.1",
    date: "2026-10-18",
    title: "Humla opens again",
    summary: "Humla 0.69.0 closed right after opening if you use a team workspace. This fixes it.",
    paragraphs: [
      "If you use a team workspace, Humla 0.69.0 closed as soon as you opened it. That came from how the new sign-in code started up, and 0.69.1 fixes it. Two-factor authentication and everything else from 0.69.0 work as before.",
      "A Mac on 0.69.0 can’t update itself, since it closes before it gets the chance. Download 0.69.1 from the releases page and install it over the old one. Your notes and settings stay where they are.",
    ],
    highlights: [
      "Humla opens again for people in a team workspace.",
    ],
    issues: [],
  },
  {
    version: "0.69.0",
    date: "2026-10-18",
    title: "Two-factor authentication",
    summary: "Protect your Humla Cloud account with a code from your phone, and require it for a whole team.",
    visual: "two-factor",
    paragraphs: [
      "You can now turn on two-factor authentication for your Humla Cloud account. Signing in then asks for a 6-digit code from an authenticator app, like 1Password, Google Authenticator, Apple Passwords or Authy, as well as your password. Turn it on under Settings → Account. You get ten recovery codes when you do. Keep them somewhere safe: each one gets you in once if you lose your phone.",
      "Workspace owners and admins can require it of everyone in the workspace, from the workspace settings under Settings → Account, and see who already has it before they switch it on. Until a member sets it up, the workspace shows as locked in their switcher and nothing in it syncs to their Mac. Changes they made keep waiting, and sync once they have it.",
      "Humla also stays signed in now. It used to sign in again with your password every time it started. Now it keeps your session in your Mac’s Keychain and renews it, so a restart doesn’t ask for a code. If a session does end, Humla asks you to sign in again.",
    ],
    highlights: [
      "Turn on two-factor authentication under Settings → Account.",
      "Sign in with a code from your authenticator app, or a recovery code.",
      "Require two-factor authentication for a whole workspace.",
      "See which members have it before you require it.",
      "Humla stays signed in between restarts.",
    ],
    issues: [
      { title: "Two-factor authentication in the app: sign-in code, setup, workspace requirement", reference: "#221", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/221" },
    ],
  },
  {
    version: "0.68.1",
    date: "2026-10-16",
    title: "Summaries with GPT-6",
    summary: "Summaries work again when you pick a GPT-6 model.",
    paragraphs: [
      "If you chose one of OpenAI’s GPT-6 models for summaries, the summary failed with an error about temperature. Humla now handles GPT-6 the way it handles GPT-5. It stops sending the setting those models don’t accept, so summaries, chat and automatic titles all work with them.",
    ],
    highlights: [
      "Summaries work with GPT-6 models.",
      "Chat and automatic titles work with GPT-6 models too.",
    ],
    issues: [],
  },
  {
    version: "0.68.0",
    date: "2026-10-16",
    title: "Export to PDF",
    summary: "Save a note as a PDF, and plain text exports come out as clean text.",
    visual: "export",
    paragraphs: [
      "Export… in a note’s ⋯ menu now has a third format: PDF. You get a ready-to-share document with the title, the summary, the transcript and your notes in the sections you picked, set in Humla’s own typeface, with page numbers at the bottom. Headings, lists, tables and bold text from the summary carry over, and speaker names are in bold.",
      "Plain text exports are clean text now. They used to keep the summary’s Markdown, so you got lines full of ## and ** that only make sense in a Markdown editor. Now headings are plain lines, lists get proper bullets, and tables line up in columns. Markdown exports haven’t changed.",
    ],
    highlights: [
      "Export a note as a PDF with page numbers.",
      "Plain text exports no longer contain Markdown symbols.",
      "Lists, tables and headings stay readable in both formats.",
    ],
    issues: [
      { title: "Export utility: transcript + summary to files (Markdown, txt)", reference: "#18", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/18" },
    ],
  },
  {
    version: "0.67.1",
    date: "2026-10-16",
    title: "Moving a note to another workspace",
    summary: "Moving a note now takes you back to your list and tells you where it went.",
    paragraphs: [
      "When you moved the note you were reading to another workspace, Humla left you on it, even though it was no longer in the workspace you were in. Now you go back to the list you came from, and a short message says where the note went.",
      "A moved note also leaves its folder and client behind, since those belong to the old workspace. Before, it kept them and ended up in no folder at all in the new workspace, not even under No folder. You can file it again once it's there.",
    ],
    highlights: [
      "Moving a note to another workspace takes you back to your list.",
      "A message says which workspace the note moved to.",
      "A moved note drops its old folder and client, so you can file it again.",
    ],
    issues: [],
  },
  {
    version: "0.67.0",
    date: "2026-10-14",
    title: "Better auto-titles and webhook support",
    summary: "Titles that name the actual meeting, and a way to send a note to your own tools.",
    visual: "webhook",
    paragraphs: [
      "Humla now names a meeting after what most of it was about. Titles used to come from the first few minutes, and that’s usually small talk. Now Humla reads a sample from across the whole transcript, plus the summary when there is one. Regenerate title in a note’s ⋯ menu works the same way, and it’s greyed out until there’s something to read.",
      "You can now send a note to your own webhook, so a knowledge base, an agent or an automation like n8n or Zapier gets the meeting. Turn it on under Settings → General → Integrations, add the URL and any headers it needs, then choose Send to webhook… in a note’s ⋯ menu. Humla asks before every send and never sends anything on its own.",
      "The URL and headers stay in your Mac’s Keychain and aren’t synced. Each send is the whole note as JSON: your notes, the summary, the transcript with speaker names, and when it was recorded. Audio is never sent.",
      "Thanks to @jgontrum and @gYonder, whose pull requests started both of these.",
    ],
    highlights: [
      "Auto-titles read the whole meeting, not just its first minutes.",
      "Titles also use the summary when there is one.",
      "Send any note to your own webhook from the ⋯ menu.",
      "The webhook URL and headers stay in your Keychain.",
      "Audio is never sent to the webhook.",
    ],
    issues: [
      { title: "Generate note titles from the note's first paragraphs", reference: "#90", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/90" },
      { title: "Add opt-in note delivery via webhooks", reference: "#213", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/213" },
    ],
  },
  {
    version: "0.66.0",
    date: "2026-10-06",
    title: "Start quietly in the menu bar",
    summary: "Humla can now open when you log in, hidden in the menu bar and ready to record.",
    paragraphs: [
      "There’s a new Open at login setting under Settings → Recording. Turn it on and Humla starts with your Mac, straight into the menu bar, with no window and no Dock icon. Start a recording from the menu bar icon whenever a meeting begins.",
      "When you want the window, open Humla from Finder or Spotlight, or pick Open from the menu bar icon. Opening Humla yourself always shows the window as before; only the login launch stays hidden.",
      "If Humla is still running from the disk image or a Downloads copy macOS has moved aside, the setting asks you to move it to Applications first, since that copy won’t be there at your next login.",
    ],
    highlights: [
      "Open at login, hidden in the menu bar.",
      "No window or Dock icon until you ask for one.",
      "Finder, Spotlight or the menu bar’s Open bring the window back.",
    ],
    issues: [
      { title: "Feature Request: Start Humla Minimized / Hidden on macOS", reference: "#207", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/207" },
    ],
  },
  {
    version: "0.65.2",
    date: "2026-10-02",
    title: "Local chat answers again",
    summary: "Chat on local models no longer comes back empty, and your notes now have times in MCP.",
    paragraphs: [
      "If you chat with your notes through a local server like mlx_lm, some models came back with “returned an empty response”. The server was cutting replies off at a few hundred words, and models that think before they answer used all of that thinking. Humla now asks for a much higher limit, so the answer has room to arrive. Local summaries had the same cap and get the same fix. If a reply is still cut short, the message now says the model hit its output limit instead of calling it empty.",
      "Apps that read your notes over MCP now get the time of each note, not just the date, in your local time with its UTC offset. A note from just after midnight also stops showing up under the day before, and date filters follow your own midnight instead of UTC’s.",
    ],
    highlights: [
      "Local chat models get room to finish their answer.",
      "Local summaries no longer stop early on a server’s default limit.",
      "MCP shows each note’s local time and UTC offset.",
      "MCP date filters follow your local midnight.",
    ],
    issues: [
      { title: "I am unable to chat with my notes. Error - model returned an empty response", reference: "#210", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/210" },
      { title: "Feature Request: Expose the meeting start time (local time + timezone) via MCP", reference: "#208", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/208" },
    ],
  },
  {
    version: "0.65.1",
    date: "2026-09-27",
    title: "Nemotron 3 on macOS 27",
    summary: "The switch to Nemotron 3 now finishes on macOS 27 and on newer Macs.",
    paragraphs: [
      "On macOS 27, and on some newer Macs, switching to Nemotron 3 stopped right at the end with an error about an “output backing”. The download itself had finished. What failed was the last step, where Humla runs the model once to get it ready. This version moves to an updated build of the model that gets through that step.",
      "If you already use Nemotron 3, or a new install tried to set it up and couldn’t, Humla fetches the updated model in the background the first time you open this version: another 193 MB and a couple of minutes of setup, once. Until it’s ready, new recordings get their speaker labels from Community-1.",
      "If the switch from the home screen failed for you, try it again from the card there. And when setup does fail, the message now says which part went wrong, so a problem preparing the model no longer reads like a problem with your connection.",
      "It’s also harder to lose speaker labels now. If identifying speakers fails on one engine, Humla tries the other one straight away instead of leaving the transcript unlabelled. And after a macOS update, Humla gets Nemotron 3 ready again when it starts, so that setup never lands at the end of a meeting. If that fails, Settings says why and lets you try again.",
    ],
    highlights: [
      "The Nemotron 3 upgrade finishes on macOS 27 and on newer Macs.",
      "Existing Nemotron 3 installs update the model once, in the background.",
      "A failed setup says whether the download or the preparation went wrong.",
      "If one speaker engine fails, the other takes over, so the labels aren’t lost.",
      "After a macOS update, Nemotron 3 gets ready again at launch, not after a meeting.",
    ],
    issues: [
      { title: "Nemotron 3 upgrade fails with a CoreML output-backing error, reported as a download failure", reference: "#203", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/203" },
    ],
  },
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
      { title: "Make Nemotron 3 the default diarization engine (replaces Sortformer)", reference: "#193", status: "Closed", url: "https://github.com/michaelwilhelmsen/humla/issues/193" },
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
