# Research + Drafts — Weekly

**Purpose:** Two-part weekly loop. **Mondays** = competitive research and topic selection. **Fridays** = draft 1 publishable post for the upcoming week.

**Cadence:**
- Mondays 9am Europe/Oslo — research scan
- Fridays 14:00 Europe/Oslo — draft

**Execution:** Two separate Claude Desktop **Local** Routines. Folder: humla project. Uses the local `marketing/reddit/lib/fetch.py` helper for all Reddit calls (Reddit's policy change made the MCP's auth path unusable; we hit reddit.com's `.json` endpoints directly with a UA string + on-disk cache).

**Output:** Weekly intel goes to `marketing/reddit/research/`. Drafts go to `marketing/reddit/drafts/`.

---

## Monday Routine — Setup in Claude Desktop

1. Routines → New routine → **Local**
2. **Name:** `humla-research-monday`
3. **Description:** `Weekly Reddit competitive research scan — what worked in target subs, competitor activity, topic candidates for Friday's draft.`
4. **Instructions:** paste the Monday prompt below
5. **Select folder:** `~/Documents/Development/Claude Code/humla`
6. **Schedule:** Weekly → Monday at 09:00

### Monday prompt

The Monday research prompt lives in [`research-monday.prompt.md`](research-monday.prompt.md).

Claude Desktop Instructions field for `humla-research-monday`:

```
Read marketing/reddit/routines/research-monday.prompt.md and EXECUTE every step in order. Do NOT paste the file content back as a response — actually run the steps. Today's date is your reference for "this week".
```


---

## Friday Routine — Setup in Claude Desktop

1. Routines → New routine → **Local**
2. **Name:** `humla-draft-friday`
3. **Description:** `Weekly draft of one publishable Reddit post for next week, based on Monday's research file.`
4. **Instructions:** paste the Friday prompt below
5. **Select folder:** `~/Documents/Development/Claude Code/humla`
6. **Schedule:** Weekly → Friday at 14:00

### Friday prompt

The Friday draft prompt lives in [`draft-friday.prompt.md`](draft-friday.prompt.md).

Claude Desktop Instructions field for `humla-draft-friday`:

```
Read marketing/reddit/routines/draft-friday.prompt.md and EXECUTE every step in order. Do NOT paste the file content back as a response — actually run the steps. Today's date is your reference for "this week".
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
