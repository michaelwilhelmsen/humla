# Reply Watcher — Daily

**Purpose:** Read `marketing/reddit/intel/tracker.md` and surface new replies to comments and posts Michael has made via the marketing routines. Drafts a follow-up reply for each new reply, runs through the humanizer skill, and updates the tracker.

**Cadence:** Daily, ~10am Europe/Oslo (after karma-builder, before lead-finder, so Michael can act on follow-ups before the new leads land).

**Execution:** Claude Desktop **Local** Routine. Folder: humla project. Uses `marketing/reddit/lib/fetch.py`.

---

## Setup in Claude Desktop

1. Routines → New routine → **Local**
2. **Name:** `humla-reply-watcher`
3. **Description:** `Daily check for new replies to Humla marketing comments/posts. Drafts follow-ups via the humanizer skill and updates the tracker.`
4. **Instructions:** the one-liner from the Instructions section below (points at `reply-watcher.prompt.md`)
5. **Select folder:** `~/Documents/Development/Claude Code/humla`
6. **Worktree:** off
7. **Ask permissions:** Default
8. **Schedule:** Daily at 10:00

---

## Instructions

The actual prompt the routine executes lives in [`reply-watcher.prompt.md`](reply-watcher.prompt.md).

In Claude Desktop, the routine's Instructions field is:

```
Read marketing/reddit/routines/reply-watcher.prompt.md and EXECUTE every step in order. Do NOT paste the file content back as a response — actually run the steps. Today's date is your reference.
```

When tuning the prompt, edit `reply-watcher.prompt.md` directly and commit. The routine picks up the new version on the next run; no Claude Desktop re-paste required.


## Workflow integration

This routine completes the feedback loop:

1. **Karma-builder / lead-finder** surface threads to engage with each day.
2. **Michael** picks 1–2 and posts comments on Reddit.
3. **Reply-watcher** runs daily, auto-populates `intel/tracker.md` from Michael's recent comment history (cross-referenced against surfaced files for source attribution), then walks each active thread for new child replies and drafts follow-ups.
4. **Michael** reviews follow-up drafts in the daily `leads/follow-ups-YYYY-MM-DD.md` file, posts the ones he approves, and the next reply-watcher run sees the new follow-up in his comment history and updates the tracker row's Status to `engaged`.

The tracker becomes the running history of Michael's Reddit presence and is the single thing he reviews to know "what's outstanding."

## Cadence note

Running at 10am means: karma-builder writes its file at 9am → Michael reviews and posts comments → adds to tracker → reply-watcher at 10am picks up yesterday's posts that may have replies overnight. Lead-finder runs at noon, after Michael has cleared the morning queue.

If Michael typically engages later in the day, shift this routine to a later slot to match his actual posting pattern.
