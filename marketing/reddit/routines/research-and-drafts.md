# Research + Drafts — Weekly

**Purpose:** Two-part weekly loop. **Mondays** = competitive research and topic selection. **Fridays** = draft 1 publishable post for the upcoming week.

**Cadence:**
- Mondays 9am Europe/Oslo — research scan
- Fridays 14:00 Europe/Oslo — draft

**Execution:** Two separate Claude Desktop **Local** Routines. Folder: humla project. Uses local Reddit MCP.

**Output:** Weekly intel goes to `marketing/reddit/research/`. Drafts go to `marketing/reddit/drafts/`.

---

## Monday Routine — Setup in Claude Desktop

1. Routines → New routine → **Local**
2. **Name:** `humla-research-monday`
3. **Description:** `Weekly Reddit competitive research scan — what worked in target subs, competitor activity, topic candidates for Friday's draft.`
4. **Instructions:** paste the Monday prompt below
5. **Select folder:** `~/Documents/Development/Claude Code/humla`
6. **Schedule:** Weekly → Monday at 09:00

### Monday prompt (paste this in)

```
You are running the Humla weekly research routine (Monday edition) for u/tremendousquotes.

Goal: Build situational awareness of what's working in target subs, what competitors are doing, and what topics Humla should engage with this week. NO drafting today — just intel.

Use the Reddit MCP (Reddit_MCP_Buddy).

Steps:

1. Top of the week in target subs (browse_subreddit, sort=top, time=week, limit=25):
   - r/macapps
   - r/LocalLLaMA
   - r/SideProject
   - r/MacOS
   - r/AiNoteTaker
   - r/ClaudeCode
   - r/ClaudeAI
   - r/buildinpublic

2. For each top post in r/macapps and r/AiNoteTaker, note:
   - Title pattern (formula: "[OS] X — does Y", "I built X because Y", "Why I switched from X to Y")
   - Length, formatting, image/video use, flair
   - Top comment sentiment

3. Competitor mentions — search across Reddit (last week) via search_reddit, sort=new, time=week:
   - "Granola"
   - "Otter.ai"
   - "Fathom"
   - "Fireflies"
   - "Jamie AI"
   - "tldv"
   - "Read.ai"
   - "Krisp Notes"
   - "meeting notes app"
   - "self hosted meeting"
   - "local meeting transcription"

4. New entrants — anyone launching a Granola/Otter alternative this week? Note their pitch.

5. Topic candidates for Humla's next post: 3 ideas, each tied to a thread or pattern from this week's data. For each:
   - Working title
   - Which sub fits best
   - Why this week (timeliness)
   - Suggested Open Recorder asset to include

Output: Write to marketing/reddit/research/YYYY-Www.md (ISO week):

# Reddit Research — Week of YYYY-MM-DD

## What worked in target subs this week

| Post | Sub | Score | Format/Hook | Has video/GIF? |
|---|---|---|---|---|
| ... | ... | ... | ... | ... |

Patterns observed: [2–3 bullets on what's resonating]

## Competitor activity

### Granola
- [thread] [link] — [1-line summary]

### Otter.ai
- ...

### New entrants
- [Name] — [pitch] — [thread]

## Hot threads to follow up on (mid-week reply candidates)

- [thread] [link] — comment opportunity: [1 sentence]

## Topic candidates for Friday's draft

1. **[Working title]** — [sub] — [why this week, 2 sentences]
   - Asset suggestion: [Open Recorder clip]
2. **[Working title]** — ...
3. **[Working title]** — ...

## Recommended pick for Friday

Pick #X because: [1–2 sentences]

End report.
```

---

## Friday Routine — Setup in Claude Desktop

1. Routines → New routine → **Local**
2. **Name:** `humla-draft-friday`
3. **Description:** `Weekly draft of one publishable Reddit post for next week, based on Monday's research file.`
4. **Instructions:** paste the Friday prompt below
5. **Select folder:** `~/Documents/Development/Claude Code/humla`
6. **Schedule:** Weekly → Friday at 14:00

### Friday prompt (paste this in)

```
You are running the Humla weekly draft routine (Friday edition).

Goal: Take Monday's research and produce 1 publishable Reddit post draft, ready for Michael to review, edit, and ship next week.

Use the Reddit MCP for any verification searches needed.

Steps:

1. Read this week's research file in marketing/reddit/research/YYYY-Www.md (find latest by listing the directory).
2. Pick the recommended topic (or pivot if something more timely emerged this week — quick search to verify).
3. Decide target sub. Verify rules from marketing/reddit/README.md:
   - r/macapps: needs 10 local karma. Use [App] flair. Disclose maker. Open source? Prefix [OS].
   - r/SideProject: any time, lenient.
   - r/LocalLLaMA: must be hand-written, 1/10 rule. Likely skip if drafting AI-assisted.
   - r/MacOS: Saturday UTC only.
   - r/AiNoteTaker: niche but high intent. Disclose.
   - r/ClaudeCode: natural fit (Humla built with Claude Code).
4. Draft the post:
   - Title: ≤80 chars, hooks with the differentiator (parallel mic+sys, offline diarization, no bots, BYO key)
   - Body: 200–500 words, plain prose, no bullet-point soup, no emoji, no marketing copy
   - Lead with the problem
   - Specific technical detail in the first 2 sentences
   - Honest about what Humla isn't (macOS-only, requires API key for cloud option, etc.)
   - End with a soft CTA (link to humla.no, open to feedback)

Critical:
- This is a DRAFT for Michael to review. He will rewrite in his voice. Don't ship-ready.
- Mark all assumptions with [TODO: verify] inline.
- No claims about benchmarks, accuracy, or comparisons unless Michael has measured them.
- Specify the Open Recorder clip(s) to include with the post.

Output: Write to marketing/reddit/drafts/YYYY-Www-[slug].md:

---
target_sub: r/...
flair: ...
publish_window: [date or "any"]
status: draft
asset_clips:
  - [clip filename]: [what it shows]
---

# Draft: [working title]

## Title
[≤80 chars]

## Body

[200–500 words plain prose]

## Open Recorder shot list

For this post, record the following clips with Open Recorder (~/Applications/Open Recorder.app or equivalent):

1. **[clip name]** — Setup: [what to show]. Duration: ~15s. Export as: GIF if <8MB else MP4. Save to `marketing/reddit/intel/assets/[slug].gif`.
2. ...

Tips for these clips:
- Use ScreenCaptureKit native capture (default on macOS, hides cursor cleanly)
- Keep smart zoom ON for cursor tracking
- Cursor smoothing ON
- Solid neutral background (#0F0F0F or wallpaper) for upload-friendliness
- Trim aggressively in the editor — Reddit attention span is 5s

## Notes for Michael
- [voice/tone note]
- [things to verify]
- [things to remove if too marketing-y]
- [alternative angles to consider]

## Self-promo disclosure
"I'm the dev — happy to answer questions"-style line to include in body or first comment.

End. Save the file.
```

---

## Sunday review (Michael, 30 min)

- Read this week's draft + Monday's research
- Rewrite the draft in your voice (don't ship Claude's prose verbatim)
- Verify any TODOs
- Check for AI-tells: no em-dashes, no "I'd like to share", no "in this post we'll...", no triple-bulletpoint walls
- Record the Open Recorder clips listed in the shot list. Save them to `marketing/reddit/intel/assets/`.
- Decide publish slot:
  - r/MacOS → next Saturday (UTC)
  - r/macapps → any day, but use [App] flair and the right format
  - r/SideProject / r/sideprojects → any day
  - r/AiNoteTaker → any day
  - r/ClaudeCode → any day, anchor on "built this with Claude Code" angle

## Open Recorder usage notes

Open Recorder (https://github.com/imbhargav5/open-recorder) is the recommended tool for Reddit-bound clips because:

- Native ScreenCaptureKit capture on macOS = zero compositor overhead, clean recording
- Apple-style smart zoom that auto-tracks cursor activity (your demo zooms into the right spot without you setting waypoints)
- Cursor smoothing + click bounce animation = looks polished
- One-click GIF export at right size for Reddit's 100MB upload cap (target <8MB for inline)
- Editor lets you trim and add zoom regions after the fact

For Reddit specifically:
- Upload GIF directly to Reddit if <100MB (Reddit converts to v.redd.it)
- For longer demos, MP4 → upload to streamable.com or imgur, link in post
- Avoid Imgur for new accounts (they shadowban based on referrer); v.redd.it is safest
