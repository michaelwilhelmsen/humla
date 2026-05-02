# Reply Watcher — Daily

**Purpose:** Read `marketing/reddit/intel/tracker.md` and surface new replies to comments and posts Michael has made via the marketing routines. Drafts a follow-up reply for each new reply, runs through the humanizer skill, and updates the tracker.

**Cadence:** Daily, ~10am Europe/Oslo (after karma-builder, before lead-finder, so Michael can act on follow-ups before the new leads land).

**Execution:** Claude Desktop **Local** Routine. Folder: humla project. Uses `marketing/reddit/lib/fetch.py`.

---

## Setup in Claude Desktop

1. Routines → New routine → **Local**
2. **Name:** `humla-reply-watcher`
3. **Description:** `Daily check for new replies to Humla marketing comments/posts. Drafts follow-ups via the humanizer skill and updates the tracker.`
4. **Instructions:** paste the prompt below
5. **Select folder:** `~/Documents/Development/Claude Code/humla`
6. **Worktree:** off
7. **Ask permissions:** Default
8. **Schedule:** Daily at 10:00

---

## Instructions (paste this in)

```
You are running the Humla daily reply-watcher routine for u/tremendousquotes.

Goal: Find new replies to Michael's marketing comments/posts (tracked in marketing/reddit/intel/tracker.md), draft follow-up responses where appropriate, and update the tracker.

Use marketing/reddit/lib/fetch.py for all Reddit calls.

## Pre-flight check

```bash
test -f marketing/reddit/lib/fetch.py && test -f marketing/reddit/intel/tracker.md && python3 marketing/reddit/lib/fetch.py browse AiNoteTaker --sort new --limit 1 > /dev/null && echo OK || echo PRECONDITION_FAIL
```

If PRECONDITION_FAIL, exit with a one-line report explaining which file is missing or whether Reddit is unreachable.

## Step 1 — Auto-populate tracker from comment history

Before reading the tracker for active rows, sweep Michael's recent Reddit activity and add any new comments that aren't yet tracked.

### 1a. Fetch recent comment history

```bash
UA="humla-research/0.1 by u/tremendousquotes"
curl -sL -A "$UA" "https://www.reddit.com/user/tremendousquotes/comments.json?limit=50" \
  | python3 -m json.tool
```

For each comment in the response, extract:
- `id` (Reddit base36 comment id)
- `permalink` (path; prefix with https://reddit.com to get full URL)
- `link_permalink` (URL of the parent thread)
- `subreddit`
- `body` (first 200 chars enough)
- `created_utc`
- `parent_id` (e.g., `t3_abc456` for top-level on post, `t1_xyz789` for reply to another comment)

### 1b. Build the set of already-tracked comment URLs

Read marketing/reddit/intel/tracker.md, parse the table rows, collect the My URL values into a set.

### 1c. Classify each comment

For each comment fetched in 1a, in time order (oldest first):

- **Already tracked** — comment.permalink is in the tracked-URL set → skip.

- **Follow-up to a tracked comment** — `parent_id` starts with `t1_`, and the parent comment's permalink is in the tracked-URL set. To construct the parent's permalink, take `link_permalink` and append the parent comment id (strip the `t1_` prefix from `parent_id`). If that URL matches a tracker row's My URL:
  - Update that row's Status: anything → `engaged` (Michael posted his follow-up)
  - Update Last check to today
  - Do NOT add a new row
  - Continue

- **New entry** — neither of the above. Add a new tracker row with these field derivations:

  - **Source + ID prefix**: search the union of these files for the comment's parent thread URL (`link_permalink`):
    - Files modified in last 30 days under `marketing/reddit/karma/` → source=`karma-builder`, prefix=`K`
    - `marketing/reddit/leads/*.md` (excluding `follow-ups-*.md`) — if the thread appears under `## Top intent: promo-allowed subs` → source=`lead-finder`, prefix=`L`, humla=`yes`; if under `## Engagement-only` → source=`lead-finder`, prefix=`E`, humla=`no`
    - `marketing/reddit/drafts/*.md` → source=`drafts`, prefix=`D`
    - `marketing/reddit/intel/historical-scan-*.md` → source=`historical-scan`, prefix=`H`
    - `marketing/reddit/intel/recurring-asks.md` → source=`recurring-asks`, prefix=`R`
    - **No match** in any file → source=`manual`, prefix=`M` (Michael commented on a thread the routines never surfaced)

  - **Humla mention** (column `Humla?`): if `body.lower()` contains `humla` → `yes`, else `no`. (For prefix-`L` entries this should be yes by definition; double-check.)

  - **ID number**: scan tracker.md for the highest existing number with this prefix, add 1. So if `K003` is the highest K, the new row is `K004`.

  - **Status**: `waiting` (it's freshly posted)

  - **Last check**: today's UTC date

  - **Notes**: 1-line summary. Pull from the body's first sentence (truncate to ~100 chars), or from the surfaced entry's "What they're asking" field if you can find it. If no source matched (manual entry), use the body excerpt.

### 1d. Write the rows

Append all new rows to the tracker table at the bottom of marketing/reddit/intel/tracker.md. Preserve formatting (one row per line, pipe-delimited).

If you updated existing rows in the follow-up case (1c, t1_ parent), edit those rows in place — change Status and Last check columns only.

Log a one-line summary in the day's follow-up report:
- "Auto-added N new tracker rows. Updated M existing rows (follow-ups posted)."

## Step 2 — Check each active row for new replies

Re-read marketing/reddit/intel/tracker.md (it includes the new rows you just added). For each row where Status is in {waiting, replied, engaged}, check the thread for new activity. Skip rows with Status = closed or archived.

For each active tracker row:

1. Extract the Thread URL and parse out subreddit + post_id.
2. Run `python3 marketing/reddit/lib/fetch.py tree <SUB> <POST_ID> --print` to get the full nested tree.
3. Find Michael's comment in the tree (match by URL or by author=tremendousquotes within the relevant branch).
4. List all child comments of Michael's comment. For each child:
   - Has it been seen before? (Check Last check date in tracker — anything newer is a new reply.)
   - Is the child by Michael himself? (Skip — that's his own follow-up; the auto-populate step in 1c already handled it.)

For each row, produce:
- "No new replies since last check" — update Last check to today, no other action
- "N new replies" — list the new replies and proceed to Step 3

## Step 3 — Triage new replies

For each new reply, decide one of:

A. **Worth a follow-up** — substantive question, genuine engagement, or a misunderstanding to clarify
B. **Acknowledgment-only** — "thanks", "good point", emoji-only. Don't reply. Mark Status = closed.
C. **Hostile / off-topic / argument bait** — disengage. Mark Status = closed and add a "do not engage" note.
D. **Auto-archive** — last activity ≥14 days old. Mark Status = archived.

## Step 4 — Draft follow-ups (Type A only)

For each new reply categorized as Type A, draft a follow-up using the same voice guide as lead-finder.md and karma-builder.md:

- 1–3 sentences
- Opinion-framed, hedged confidence
- One specific detail
- No em-dashes
- Casual register
- If Humla is being mentioned in the conversation: keep maker disclosure intact, no link unless they explicitly ask

Then run the draft through the humanizer skill. Use the same voice calibration samples documented in lead-finder.md.

For the calibration block, paste these verbatim recent comments by u/tremendousquotes:

- "Skip making it read all the files. Use /init to create a decent CLAUDE.md and point it to the files you want to work with."
- "We've slowly started integrating ai automations. Most importantly we're seeing the need to use a good model that can plan accurately and have good vision."
- "I definitely think it's worth it, and I only maxed out session limits occasionally with heavy use. /clear often, and give good prompts and it will last you 🙂👍🏼"
- "I only use API for products and integrations. Use the pro/max plan subscription for your personal use"
- "I have the same issue. My product also criticizes Donald Trump, so every post gets flagged / banned automatically 😅"
- "I'm building Humla which is open source + local models. In a pretty decent shape now, but working on it actively."

The 1–3 sentence cap takes precedence over any humanizer suggestion to add length.

## Step 5 — Write the daily report

Write marketing/reddit/leads/follow-ups-YYYY-MM-DD.md (today's UTC date, NOT the leads file — separate name so they don't collide):

# Follow-ups — YYYY-MM-DD

## Active rows checked: N

## New replies surfaced: M

### [Tracker ID] — [thread title]
- **Original comment:** [Michael's URL]
- **New reply by:** u/[username] (Xh ago)
- **Reply text:**
  > [verbatim quote of the new reply, max 200 chars]
- **Triage:** A (follow-up worthwhile) / B (ack-only) / C (do not engage) / D (auto-archive)

**Your follow-up:**
> [draft follow-up, 1–3 sentences, post-humanizer]

(repeat per new reply)

## Tracker updates applied

List the tracker rows you updated and what changed:
- C003: status waiting → replied, last check 2026-05-02 → 2026-05-03
- C007: auto-archived (no activity since 2026-04-15)

## Empty day

If there were 0 active rows OR 0 new replies on any of them, write:

"No new replies today. N active rows checked, all current as of YYYY-MM-DD."

## Step 6 — Update tracker.md

For each active row you checked, update the Last check column to today's date. For rows where you found new activity, update the Status column accordingly:
- waiting → replied (someone replied for the first time)
- replied → engaged (you posted a follow-up; mark this manually if Michael actually posts the draft)
- engaged → closed (asker thanked, conversation died, etc.)
- any → archived (no new activity in 14 days)

End report. Do NOT post follow-ups automatically — Michael reviews and posts manually, then updates Status to engaged on the next routine run.
```

## Workflow integration

This routine completes the feedback loop:

1. **Karma-builder / lead-finder** surface threads to engage with, including a "Suggested tracker entry" line ready to paste.
2. **Michael** picks 1–2, posts comments, copies the suggested entries into `intel/tracker.md`.
3. **Reply-watcher** runs daily, finds replies, drafts follow-ups, updates statuses.
4. **Michael** reviews follow-up drafts in the daily `leads/follow-ups-YYYY-MM-DD.md` file, posts the ones he approves, marks the tracker row Status = engaged.

The tracker becomes the running history of Michael's Reddit presence and is the single thing he reviews to know "what's outstanding."

## Cadence note

Running at 10am means: karma-builder writes its file at 9am → Michael reviews and posts comments → adds to tracker → reply-watcher at 10am picks up yesterday's posts that may have replies overnight. Lead-finder runs at noon, after Michael has cleared the morning queue.

If Michael typically engages later in the day, shift this routine to a later slot to match his actual posting pattern.
