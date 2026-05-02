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

Use the `marketing/reddit/lib/fetch.py` helper for all Reddit calls. Run from the repo root via Bash:

- `python3 marketing/reddit/lib/fetch.py browse <sub> --sort new --limit 25` — list a sub's newest posts
- `python3 marketing/reddit/lib/fetch.py search-sub <sub> "<query>" --time week --limit 25` — keyword search inside one sub (uses `restrict_sr=1`)
- `python3 marketing/reddit/lib/fetch.py search "<query>" --time week --limit 25` — Reddit-wide keyword search
- `python3 marketing/reddit/lib/fetch.py tree <sub> <post_id> --print` — print the full nested comment tree (the verification path that used to need a curl + python3 heredoc)
- `python3 marketing/reddit/lib/fetch.py tree <sub> <post_id>` — same, but JSON list of `{id, author, score, body, depth, num_replies}` for programmatic checks

Output is JSON on stdout for everything except `tree --print`. Pipe to `jq` if you want to filter without parsing in Python. Cache lives at `~/.cache/humla-reddit/` with a 10-min TTL — pass `--no-cache` to bypass when you're verifying something that just changed.

### Pre-flight check

Before any real work, confirm the helper is available and Reddit is reachable:

```bash
test -f marketing/reddit/lib/fetch.py && python3 marketing/reddit/lib/fetch.py browse AiNoteTaker --sort new --limit 1 > /dev/null && echo OK || echo FETCH_FAIL
```

If you see `FETCH_FAIL`, abort the routine with a one-line report explaining which precondition failed (file missing, Reddit returning 429, network down). Don't run a full scan against a broken helper.

## Search strategy

Two lessons from real runs:

1. **Reddit-wide keyword search produces mostly noise.** "granola" matches breakfast recipes; "meeting notes" matches every business thread. Reddit's `q=` does loose word-matching by default. Solution: scope every search to a specific high-fit sub via `search-sub`.
2. **A 24h window is brutal for this niche.** r/AiNoteTaker often goes 24–72h without a new ask. Solution: search with `--time week` (7 days), then post-filter to ≤72h (3 days). De-dup against prior days' leads files so a single thread doesn't get re-surfaced after Michael's already seen it.

### Read subreddits.md and README first

At the start of every run, read:
1. `marketing/reddit/subreddits.md` — single source of truth for sub list, per-sub query patterns, promo rules, status (locked / unlocked / unverified)
2. `marketing/reddit/README.md` — specifically the **"Pain point → Humla differentiator map"** section. When drafting Your Reply for a surfaced lead, lead with the pain point that matches the asker's framing, not with the product. The mapping tells you which Humla differentiator addresses each recurring pain.

If subreddits.md has new subs added since the last run, this routine picks them up automatically.

### Per-sub scoped searches

For each Tier 1 + Tier 2 sub in subreddits.md with `Status: unlocked`, run the helper's `search-sub <sub> "<query>" --sort new --time week --limit 25` for each query pattern listed in that sub's entry. For Tier 2 subs marked `Status: unverified`, first verify rules via the curl + json.tool pattern documented in subreddits.md before treating them as promo-allowed.

**r/ClaudeCode and r/ClaudeAI special handling**: skip keyword search for these; instead run `browse <sub> --sort new` and scan titles for transcription / meeting / dictation / Whisper / on-device AI questions. These subs' value is build-in-public threads where someone is asking about real-time transcription pipelines.

### Reddit-wide fallback (only if all per-sub searches return empty)

If after per-sub searches you have zero candidates, do a small set of Reddit-wide tightly-quoted searches as fallback:
- `"granola alternative"` (quoted, intent-bearing)
- `"alternative to otter.ai"` (quoted)
- `"local meeting transcription"` (quoted)
- `"no bot meeting notes"` (quoted)

### Time window

- API search uses `time=week` (7 days)
- Post-filter to threads created within the last **7 days** (use the full week — Humla's niche is thin, narrower windows produce empty days)
- De-dup is what prevents re-surfacing: read the last 7 days of leads files in `marketing/reddit/leads/` AND `marketing/reddit/intel/_seen-permalinks.txt`, drop any candidate whose permalink appears in either source. Append today's surfaced permalinks to `_seen-permalinks.txt` so they don't re-surface tomorrow.

Net effect: each thread has roughly one chance to surface, on its first day inside the 7-day window. After that it's de-duped out unless something changes (e.g., Michael deletes the leads file or removes the entry from _seen-permalinks.txt to deliberately re-surface).

### Intent post-filter (apply to every candidate)

After collecting candidates, drop any that don't have an intent marker. Intent markers are organized by category — a thread matching ANY one is enough to keep, but matching multiple is a stronger signal.

**Generic asking:**
- `?`, "looking for", "any recommendations", "anyone know", "anyone tried", "alternative", "switch from", "moving from", "suggestions", "best for", "should I", "what do you use"

**Frustration-driven (high intent — they've already tried something):**
- "frustrated with", "tired of", "fed up", "sick of", "hate that", "annoying"
- "stopped using", "cancelled", "had to leave"

**Price shock (the Notion / Granola / Otter pattern — paywall hit):**
- "too expensive", "couldn't justify", "out of my budget", "can't afford"
- "trial ended", "free trial ran out", "trial is over"
- "team plan", "subscription paywall", "$X/month", "$X for", "pricing changed"
- "not paying that much", "for that price"

**FOSS / local-first / privacy (Humla's natural fit):**
- "FOSS", "open source", "self-hosted desktop", "BYO key", "bring your own"
- "local-first", "offline", "on-device", "no cloud", "without uploading"
- "privacy", "where does my data go", "data residency", "EU data", "GDPR"

**No-bot pain (the Otter/Fathom/Fireflies frustration):**
- "no bot", "without a bot", "no meeting bot", "without joining the meeting"
- "bot showed up", "bot joined uninvited", "Otter showed up"

**Speaker identification pain (the wall-of-text complaint):**
- "wall of text", "who said what", "speaker identification", "speaker labels", "diarization"
- "couldn't tell who", "no speakers", "all one speaker"

**In-person specifically (Humla's mic-only diarization is a direct match):**
- "in person", "in-person meeting", "physical meeting", "around the table", "no laptop on the table"

Drop announcement posts ("Introducing X", "I built Y", "X v2 is out") — those are competitor launches, not buying intent. Note them in the audit trail and in `intel/competitor-activity.md` for the research routine, then drop from the leads list.

Filter:

- Drop posts that are reviews/comparisons/listicles (not asking, just informing)
- Drop posts with score < -3 or upvote_ratio < 0.5
- Drop NSFW
- Drop posts where u/tremendousquotes is already in the comments
- Drop posts in r/privacy or r/consulting (no promo allowed there — surface as engagement-only instead)
- Drop posts in r/selfhosted unless the asker explicitly wants a self-hosted server (Humla is local-desktop, not server)

De-dup against recent days + bootstrap list:
- List the last 7 days of files in `marketing/reddit/leads/` (today minus 1 through today minus 7).
- Also read `marketing/reddit/intel/_seen-permalinks.txt` if it exists (populated by the historical-scan routine, then maintained daily by this routine).
- For each candidate post, check if its permalink appears in any of those sources. If yes, drop — already seen.
- Append today's surfaced permalinks to `_seen-permalinks.txt` (deduplicated) so future runs don't re-surface them either.

Empty days are good days:
- It's normal and expected to surface 0–2 leads on most days. The market doesn't generate high-intent meeting-notes threads at a constant rate.
- If after filtering there are 0 promo-allowed leads, the report should say so honestly. Do NOT pad with low-intent threads (intent score < 5) just to have something to surface.
- An empty leads file with a clear audit trail is more useful than a noisy one — it preserves Michael's commenting time for actually-good threads.

## Classify the thread before deciding if it's still open

This is the most important filter, and it differs from the karma-builder's "answered = stop" rule. Lead-finder is about *recommendations*, not unique answers.

**Walk the full comment tree first** using the helper:

```bash
python3 marketing/reddit/lib/fetch.py tree <SUB> <POST_ID> --print
```

This prints the full nested tree as `<indent>- [comment_id] u/author [score↑]: body` lines. For programmatic checks, drop `--print` to get JSON.

### Type A — Single-answer question

The asker has a specific problem with a correct/incorrect answer:
- "How do I capture system audio on Mac?" → there's a right answer (ScreenCaptureKit + AVAudioEngine setup)
- "Is using DMGKit safe with my Apple Developer account?" → yes/no with specifics
- "Why is my whisper.cpp slower on M1 vs M2?" → factual answer

**Rule for Type A: if the question is well-answered (a comment with >5 score that addresses the question, or OP marked it solved, or OP said "thanks"), drop the thread. The question is closed; another comment is noise.**

### Type B — Recommendation request (this is where Humla lives)

The asker wants a *tool* or *option*, and the thread is inherently multi-answer:
- "What's the best meeting notes app for Mac?"
- "Any FOSS alternatives to Granola?"
- "Looking for a meeting recorder that doesn't need a bot"
- "What do you use for transcribing in-person meetings?"

These threads stay valid for as long as they're visible. Adding a recommendation that wasn't yet named — *especially one with a different angle* — is genuinely valuable to the asker and to anyone Googling the same question later.

**Rule for Type B: do NOT drop just because answers exist.** Instead, check whether the existing recommendations cover Humla's specific angle:

- If existing answers are all cloud-based and asker wanted local-first → Humla is a different angle. Surface.
- If existing answers all need a meeting bot and asker wanted no bot → Humla is a different angle. Surface.
- If existing answers are all paid SaaS and asker mentioned price concerns → Humla's BYO-key model is a different angle. Surface.
- If Humla has already been named in the existing answers → drop. Don't dogpile.
- If 3+ tools matching Humla's angle have already been recommended (e.g., another local-first Mac app + another BYO-key option + another no-bot tool) → drop. The angle is well-covered.

### Acknowledgment is also nuanced

For Type A, "thanks" closes the thread.

For Type B, "thanks" to one specific tool (e.g., "Thanks, I'll try Otter") closes that branch but doesn't close the thread — other readers will still see Humla in the comments. If OP explicitly says "I picked X, thread closed" or similar, drop. Otherwise treat the thread as still open for new options.

### How to classify

Read OP's title and body. Strong signals:

- **Type A**: question word + specific technical setup ("how do I", "why does", "what's wrong with"), single-answer phrasing
- **Type B**: "what do you use", "any alternatives", "looking for a tool", "best X for Y", "recommendations for", listing of requirements, mentioning specific competitors as a starting point

If ambiguous, default to Type B — Humla's niche is mostly recommendation threads.

### Verification before surfacing

Cite the reply target's comment ID from the helper's tree output. Then say one of:

**For Type A:**
- "Type A: comment ID X has 0 substantive children — question genuinely unanswered"
- "Type A: comment ID X has answer Y but it's wrong because [specific]"

**For Type B:**
- "Type B recommendation thread. Existing recommendations: [list product names from comments]. None cover Humla's [specific differentiator] angle."
- "Type B recommendation thread. Existing recommendation in comment Z covers cloud option; OP mentioned wanting local-first, which Humla addresses."

If you can't point to specific comment IDs and quote either evidence of unansweredness (Type A) or evidence of an uncovered angle (Type B), drop the thread.

Score intent (0–10):
- +3 if asking a direct question ("does anyone know X?", "looking for Y")
- +2 if naming a specific competitor as a frustration ("Granola is too expensive", "Otter sends my data to OpenAI")
- +2 if mentioning macOS specifically
- +2 if the thread matches a cluster in `marketing/reddit/intel/recurring-asks.md` (read it at the start of the run; do simple keyword overlap between the thread title/body and each cluster's "Example phrasings" + "Common pain point" — overlap with any cluster = +2)
- +1 if mentioning "local" / "privacy" / "offline" / "own data"
- +1 if mentioning a use case Humla nails (1:1s, consulting calls, recurring meetings, in-person)
- +1 if posted by an account with reasonable history (not <7 days old)

Only surface threads scoring ≥ 5.

**Thin-day fallback**: if after applying all filters fewer than 2 leads emerge with score ≥ 5, lower the threshold to ≥ 3 for the rest of this run. Note in the audit trail which threads were surfaced under the lowered threshold so Michael can calibrate. Do not lower below 3.

For each surfaced thread:
- Verify which sub it's in
- Check the sub's promo rules from marketing/reddit/README.md
- Decide: "promo allowed" / "engagement-only"

## Voice guide for "Your reply" (apply this to every draft)

Michael's writing voice on Reddit:

- **Length: 1–3 sentences.** Usually 2, often 1. No essays.
- **One reply target only.** Answer the OP's question OR one specific commenter's question — never synthesize across multiple commenters. That's an AI tell. Pick the one thing you'd say if you were skimming the thread.
- **Only reply where there's actual value.** Two cases to handle differently:
  - *Single-answer threads (Type A):* if the question is already well-answered, skip. Showing up to repeat the same answer is noise.
  - *Recommendation threads (Type B):* answers existing in the thread don't close the thread — these are inherently multi-answer. Add Humla *only* if it represents a meaningfully different angle from what's already named (different cost model, different data-residency story, no-bot vs bot, etc.). If Humla is just another name in the same category, skip. If 3+ tools matching Humla's angle have already been listed, skip.
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
- His Reddit comment history — fetch the latest 25 comments via `curl -sL -A "humla-research/0.1 by u/tremendousquotes" "https://www.reddit.com/user/tremendousquotes/comments.json?limit=25" | python3 -m json.tool`
- His public repos (humla, git-timetrack)

If you can't verify a claim, drop the experience phrasing and reframe as opinion. Better to write "probably" than to fabricate "I've shipped this for months."

## Humanizer pass (mandatory)

After drafting each reply, run it through the `humanizer` skill before finalizing.

Steps:
1. Draft the reply per the voice guide above.
2. Invoke the humanizer skill: `Skill humanizer` with the draft + voice calibration samples (below) + instruction "humanize this Reddit reply, keep it 1–3 sentences max, preserve Humla disclosure if present."
3. Use the humanizer's final rewrite. If it adds length, trim back to cap.

**Voice calibration samples** (real recent comments by u/tremendousquotes — paste verbatim):

```
- "Skip making it read all the files. Use /init to create a decent CLAUDE.md and point it to the files you want to work with."
- "We've slowly started integrating ai automations. Most importantly we're seeing the need to use a good model that can plan accurately and have good vision."
- "I definitely think it's worth it, and I only maxed out session limits occasionally with heavy use. /clear often, and give good prompts and it will last you 🙂👍🏼"
- "I only use API for products and integrations. Use the pro/max plan subscription for your personal use"
- "I have the same issue. My product also criticizes Donald Trump, so every post gets flagged / banned automatically 😅"
- "I'm building Humla which is open source + local models. In a pretty decent shape now, but working on it actively."
- "The grind doesn't truly start until you hit \"Submit for Review\"."
```

The 1–3 sentence cap from this routine takes precedence over any humanizer suggestion to add length or structure. Disclosure for Humla mentions stays intact through the humanizer pass.

## Quick anti-AI checklist (spot-check after humanizer)

- "actually" / "essentially" / "fundamentally" / "the real question is" → cut
- "underscoring" / "highlighting" / "ensuring" / "reflecting" → cut
- "It's not just X — it's Y" → cut
- "Great question!" / "Hope this helps" → delete
- Tailing negations → rewrite as real clauses
- Em-dashes → periods, commas, parentheses
- Copula avoidance ("serves as") → "is" / "has"
- Tutorial tone → peer tone

---

Promo-allowed vs engagement-only is determined by reading subreddits.md (Tier 1+2 = promo-allowed; Tier 3 = case-by-case based on `Promo rules` field; Tier 4 = engagement-only). Always check subreddits.md for the current classification — do not hardcode here.

Special cases to remember:
- **r/macapps**: promo-allowed only after 10 local karma reached (current account: 0). Until then, surface as engagement-only.
- **r/MacOS**: promo-allowed Saturdays UTC only.
- **r/LocalLLaMA**: 1/10 rule, hand-write everything (no AI text).
- **r/ObsidianMD**: promo-banned for first-post accounts → engagement-only until real history exists.
- **r/privacy**, **r/consulting**, **r/productivity**, **r/BuyFromEU**: engagement-only forever.

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
- **Thread type & why valid:** Either:
  - "Type A — question genuinely unanswered. [Quote evidence from tree: comment IDs + child counts]"
  - "Type B — recommendation thread. Existing options: [list]. Humla's angle ([differentiator]) not yet covered." Quote the existing recommendations from the tree.
  Drop the thread if neither form applies.
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
- Thin-day fallback used: yes/no (and threshold dropped to)

End report. Do NOT post comments.

---

### Empty-day report (write this format if 0 leads surfaced)

If after all filters and the thin-day fallback there are still 0 leads to surface, write the report in this format. Do NOT just write a blank file or skip writing — the empty-day audit trail is what tells Michael whether the routine ran cleanly or silently failed.

# Leads — YYYY-MM-DD

## Empty day

No high-intent threads surfaced today. This is normal — the niche genuinely doesn't generate buying-intent threads at a constant rate.

## What was checked

- Subs scanned: [list, with subscriber counts and "Status: ..." from subreddits.md]
- Total candidates returned by per-sub searches: N
- Total candidates after intent filter: M
- Total candidates after de-dup against last 7 days + _seen-permalinks.txt: K
- Total candidates after Type A/B classification: J
- Threshold used: 5 (default) or 3 (thin-day fallback)

## Closest near-misses (for tuning)

If any candidate scored 3–4 (below the threshold but worth noting), list the top 3 here with a one-line "almost" reason. This helps Michael spot patterns where the intent filter is too narrow.

- [thread] [sub] — score X/10 — almost: [1 sentence]

## Cluster cross-check

Did any threads from today partially match a recurring-asks cluster but fail other criteria? If yes, list which cluster + why it didn't make it. This helps identify whether the issue is the cluster definition or the intent markers.

End empty-day report.
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
