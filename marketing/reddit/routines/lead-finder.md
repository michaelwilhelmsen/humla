# Lead Finder — Daily

**Purpose:** Surface threads where someone is **actively asking for what Humla solves** — high-intent buying signals across Reddit, in the last 24h. Michael then engages on the ones that fit, with proper disclosure.

**Cadence:** Daily, 12pm Europe/Oslo.

**Execution:** Claude Desktop **Local** Routine. Folder: humla project. Uses local Reddit MCP.

**Difference vs karma-builder:** Karma-builder targets threads where Michael adds technical value with **no Humla mention**. Lead-finder targets threads where the asker is looking for a solution Humla provides — Humla can be mentioned (with disclosure) **only in subs that allow it**.

---

## Setup in Claude Desktop

1. Routines → New routine → **Local**
2. **Name:** `humla-lead-finder`
3. **Description:** `Daily Reddit lead-finder for Humla — surface high-intent threads where someone is actively asking for what Humla provides.`
4. **Instructions:** paste the prompt block below
5. **Select folder:** `~/Documents/Development/Claude Code/humla`
6. **Worktree:** off
7. **Ask permissions:** Default
8. **Schedule:** Daily at 12:00

---

## Instructions (paste this into the routine)

```
You are running the Humla daily lead-finder routine for u/tremendousquotes.

Goal: Find Reddit threads from the last 24h where someone is actively asking for what Humla provides, score them by intent strength, and surface the top candidates with engagement angles.

Use the Reddit MCP (Reddit_MCP_Buddy) for all queries.

High-intent query patterns (search each via mcp__Reddit_MCP_Buddy__search_reddit, sort=new, time=day, limit=25):

1. "alternative to granola"
2. "alternative to otter"
3. "alternative to fathom"
4. "alternative to fireflies"
5. "local meeting transcription"
6. "self hosted meeting notes"
7. "meeting recorder mac"
8. "no bot meeting notes"
9. "privacy meeting transcription"
10. "system audio transcription mac"
11. "record system audio mac"
12. "offline meeting notes"
13. "whisper meeting"
14. "meeting notes app mac"
15. "AI note taker mac"
16. "transcribe meetings privately"

Also check these specific subs for new posts (browse_subreddit, sort=new, limit=25):
- r/AiNoteTaker (Michael already commented here recently — keep an eye on follow-ups)
- r/macapps
- r/MacOS
- r/LocalLLaMA
- r/SideProject

Filter:

- Drop posts that are reviews/comparisons/listicles (not asking, just informing)
- Drop posts with score < -3 or upvote_ratio < 0.5
- Drop NSFW
- Drop posts where u/tremendousquotes is already in the comments (use get_post_details to check)
- Drop posts in r/privacy or r/consulting (no promo allowed there — surface as engagement-only instead)
- Drop posts in r/selfhosted unless the asker explicitly wants a self-hosted server (Humla is local-desktop, not server)

Score intent (0–10):
- +3 if asking a direct question ("does anyone know X?", "looking for Y")
- +2 if naming a specific competitor as a frustration ("Granola is too expensive", "Otter sends my data to OpenAI")
- +2 if mentioning macOS specifically
- +1 if mentioning "local" / "privacy" / "offline" / "own data"
- +1 if mentioning a use case Humla nails (1:1s, consulting calls, recurring meetings)
- +1 if posted by an account with reasonable history (not <7 days old)

Only surface threads scoring ≥ 5.

For each surfaced thread:
- Verify which sub it's in
- Check the sub's promo rules from marketing/reddit/README.md
- Decide: "promo allowed" / "engagement-only"

## Voice guide for "Your reply" (apply this to every draft)

Michael's writing voice on Reddit:

- **Length: 1–3 sentences.** Usually 2, often 1. No essays.
- **One reply target only.** Answer the OP's question OR one specific commenter's question — never synthesize across multiple commenters. That's an AI tell. Pick the one thing you'd say if you were skimming the thread.
- **Open with action or soft opinion**, not preamble: "Skip making it...", "I definitely think...", "We've slowly started..."
- **Frame as opinion, not declaration.** Use: "I've found...", "Worked for me to...", "I'd lean toward...", "My take is...", "Honestly, I'd just...", "From what I've seen...", "Probably..."
- **Lower confidence by default.** Hedge liberally. Readers trust hedged claims more than confident ones.
- **One specific detail, not three.** Pick the single most useful concrete thing. Skip enumerated lists.
- **Casual register.** Contractions everywhere. Trailing rhetorical questions ok. Occasional dry aside ok.
- **Occasional emoji at the END**: 🙂 👍🏼 😅 🔥. Max one per comment, often zero.
- **No em-dashes.** Use periods, commas, parentheses.
- **No bold/italic/headers** in the comment.
- **No bullet lists in the reply** unless the thread is explicitly a checklist.
- **For Humla mentions:** disclose maker status in the same sentence ("I'm building Humla, so take this with that grain of salt"). One mention only. No link unless asked.

## Don't invent experience (critical)

The reply must only claim experience Michael actually has. Verify against:
- `CLAUDE.md` in this repo (Michael's technical history)
- His Reddit comment history via user_analysis on tremendousquotes
- His public repos (humla, git-timetrack)

If you can't verify a claim, drop the experience phrasing and reframe as opinion. Better to write "probably" than to fabricate "I've shipped this for months."

## Anti-AI pass (run before writing each "Your reply")

- Cut: "actually" / "essentially" / "fundamentally" / "the real question is" / "at its core"
- Cut: "underscoring" / "highlighting" / "ensuring" / "reflecting" / "showcasing"
- Cut: "It's not just X — it's Y" structures
- Cut: "Great question!" / "Hope this helps" / "Let me know"
- Cut: tailing negations ("no guessing", "no friction")
- Cut: rule-of-three lists, copula avoidance ("serves as", "functions as")
- Read out loud — if a sentence feels like a press release, delete.

---

Promo-allowed subs (Humla mention with disclosure ok):
- r/macapps (BUT only after 10 local karma reached — until then, engagement-only)
- r/SideProject
- r/sideprojects
- r/buildinpublic
- r/AiNoteTaker
- r/MacOS (Saturdays UTC only)
- r/LocalLLaMA (1/10 rule, hand-write)
- r/ClaudeCode, r/ClaudeAI (Humla is Claude-Code-built — natural fit)

Engagement-only subs (NO Humla mention):
- r/privacy
- r/consulting
- r/productivity
- any sub not in the allowed list above

Output: Write the report to marketing/reddit/leads/YYYY-MM-DD.md (today's UTC date):

# Leads — YYYY-MM-DD

## Top intent: promo-allowed subs

### [Thread title]
- **Sub:** r/...
- **Link:** [reddit.com link]
- **Posted:** Xh ago • [score]↑ • [N] comments
- **Author:** u/[username] ([account age], [karma summary])
- **Intent score:** X/10
- **What they're asking:** [1 sentence]
- **Humla fit:** [which differentiator addresses their question]
- **Reply to:** [either "OP" + 1-line quote, or "u/username" + 1-line quote of their comment]
- **Asset opportunity:** [Open Recorder clip suggestion if applicable]
- **DON'T:** [things to avoid]

**Your reply:**
> [draft addressing ONLY the reply target above. Lead with their problem, not the product. One Humla mention with disclosure. 1–3 sentences. No link in the first comment unless they explicitly asked for tool names.]

(repeat)

## Engagement-only (no Humla mention)

### [Thread title]
- **Sub:** r/...
- **Link:** ...
- **Why surface:** valuable thread to comment on for visibility/karma without promo
- **Reply to:** [OP or u/username + quote]

**Your reply:**
> [draft addressing only the reply target. No Humla mention. 1–3 sentences.]

(repeat)

## Skipped (audit trail)

- [thread] [sub] — [reason: e.g., "in r/privacy, no promo"]

## Tally
- Total candidates evaluated: N
- Surfaced (promo-allowed): X
- Surfaced (engagement-only): Y
- Skipped: Z

End report. Do NOT post comments.
```

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
