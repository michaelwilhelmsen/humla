# Karma Builder — Daily

**Purpose:** Surface 3–5 threads where Michael (`u/tremendousquotes`) can comment with technical substance and earn local karma. **No Humla promo.** Pure helpfulness.

**Scope vs lead-finder:** Karma-builder is intentionally Type A only — it surfaces single-answer technical questions where Michael adds *unique* value. If the question already has a good answer, drop. Recommendation threads ("what tool should I use?") are lead-finder's territory, not karma-builder's. Keep this routine narrow.

**Cadence:** Daily, 9am Europe/Oslo.

**Execution:** Claude Desktop **Local** Routine. Folder: humla project. Uses the local `marketing/reddit/lib/fetch.py` helper for all Reddit calls (Reddit's policy change made the MCP's auth path unusable; we hit reddit.com's `.json` endpoints directly with a UA string + on-disk cache).

---

## Setup in Claude Desktop

1. Routines → New routine → **Local**
2. **Name:** `humla-karma-builder`
3. **Description:** `Daily Reddit karma-builder for u/tremendousquotes — surface threads where Michael can comment with technical substance, no Humla promo.`
4. **Instructions:** the one-liner from the Instructions section below (points at `karma-builder.prompt.md`)
5. **Select folder:** `~/Documents/Development/Claude Code/humla`
6. **Worktree:** off (we want output in the main tree, not a worktree)
7. **Ask permissions:** Default
8. **Schedule:** Daily at 09:00

---

## Instructions

The actual prompt the routine executes lives in [`karma-builder.prompt.md`](karma-builder.prompt.md).

In Claude Desktop, the routine's Instructions field is:

```
Read marketing/reddit/routines/karma-builder.prompt.md and EXECUTE every step in order. Do NOT paste the file content back as a response — actually run the steps. Today's date is your reference.
```

When tuning the prompt, edit `karma-builder.prompt.md` directly and commit. The routine picks up the new version on the next run; no Claude Desktop re-paste required.


## Open Recorder integration

Karma-builder is text-only. Skip Open Recorder for this routine.
