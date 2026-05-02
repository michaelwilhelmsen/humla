# Lead Finder — Daily

**Purpose:** Surface threads where someone is **actively asking for what Humla solves** — high-intent buying signals across Reddit, in the last 24h. Michael then engages on the ones that fit, with proper disclosure.

**Cadence:** Daily, 12pm Europe/Oslo.

**Execution:** Claude Desktop **Local** Routine. Folder: humla project. Uses the local `marketing/reddit/lib/fetch.py` helper for all Reddit calls (Reddit's policy change made the MCP's auth path unusable; we hit reddit.com's `.json` endpoints directly with a UA string + on-disk cache).

**Difference vs karma-builder:** Karma-builder targets threads where Michael adds technical value with **no Humla mention**. Lead-finder targets threads where the asker is looking for a solution Humla provides — Humla can be mentioned (with disclosure) **only in subs that allow it**.

---

## Setup in Claude Desktop

1. Routines → New routine → **Local**
2. **Name:** `humla-lead-finder`
3. **Description:** `Daily Reddit lead-finder for Humla — surface high-intent threads where someone is actively asking for what Humla provides.`
4. **Instructions:** the one-liner from the Instructions section below (points at `lead-finder.prompt.md`)
5. **Select folder:** `~/Documents/Development/Claude Code/humla`
6. **Worktree:** off
7. **Ask permissions:** Default
8. **Schedule:** Daily at 12:00

---

## Instructions

The actual prompt the routine executes lives in [`lead-finder.prompt.md`](lead-finder.prompt.md).

In Claude Desktop, the routine's Instructions field is:

```
Read marketing/reddit/routines/lead-finder.prompt.md and EXECUTE every step in order. Do NOT paste the file content back as a response — actually run the steps. Today's date is your reference.
```

When tuning the prompt, edit `lead-finder.prompt.md` directly and commit. The routine picks up the new version on the next run; no Claude Desktop re-paste required.


## Open Recorder integration

When a surfaced lead has high intent and Humla solves the specific question, the report should suggest an **Open Recorder asset opportunity**. Reddit comments with embedded GIFs/video links convert dramatically better than text-only.

Useful Open Recorder clips to have pre-made and ready to drop into comments:

| Clip name | What it shows | When to use |
|---|---|---|
| `humla-mic-sys-parallel.gif` | A meeting starts, both mic and system audio waveforms appear independently in the UI; final transcript shows speaker labels | When asker mentions wanting to capture both sides without bots |
| `humla-offline-diarize.gif` | Stop recording → "Diarizing…" toast → Speaker 1 / Speaker 2 pills appear; airplane mode visible in menu bar | When asker mentions privacy / offline / no-cloud |
| `humla-byo-key.gif` | Settings → paste your own OpenAI key → all subsequent transcription uses your key | When asker mentions vendor lock-in or subscription fatigue |
| `humla-no-bot.gif` | Joining a meeting from Zoom/Meet, recording starts, no bot in attendee list | When asker complains about Otter/Fathom/Fireflies bot showing up |
| `humla-tauri-tray.gif` | Menu bar icon → start/stop, full app loads in <500ms | General "feels native" demo |

How to use Open Recorder for these:
1. Open Recorder is at https://github.com/imbhargav5/open-recorder (download the macOS build)
2. Use ScreenCaptureKit native capture path (default on macOS)
3. Record a single window of Humla, 15–30s
4. Smart zoom auto-tracks cursor — leave it on
5. Export as GIF (under 8 MB for direct Reddit upload) or MP4 + Imgur for Reddit
6. Save to `marketing/reddit/intel/assets/<clip-name>.gif`

The lead-finder routine should reference the asset library by checking `marketing/reddit/intel/assets/` for existing clips and surfacing the matching one. If no clip exists for the angle, mark `Asset opportunity: <description> — needs recording`.

## Engagement rules (encoded into the routine)

When Michael acts on a surfaced lead, the comment must:

1. **Open with the problem**, not the product. ("Yeah, the Granola pricing jump bit me too" not "I built a tool that...")
2. **Disclose maker status in the same comment.** ("I'm the dev of Humla, so take this with that grain of salt.")
3. **Address what they specifically asked.** Don't pitch features they didn't mention.
4. **Link to humla.no only if they engage.** First comment = no link unless explicitly asked.
5. **Skip if 3+ other tool authors are already in the thread.** Don't dogpile.
6. **No UTM tagging on links.** Plain humla.no.

## Daily review (Michael, ~10 min)

- [ ] Open today's leads file
- [ ] Pick 1–2 threads max (be selective)
- [ ] Write comment in your voice
- [ ] If asset clip suggested, check `marketing/reddit/intel/assets/` — drop it in if it exists, or note for next time
- [ ] Post, then add URL to `### Acted on:` section

## Weekly review (Sundays)

Audit which queries surfaced the highest-intent leads. Tune the query list in this file based on what worked.
