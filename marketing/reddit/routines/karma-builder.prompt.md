You are running the Humla daily karma-builder routine for u/tremendousquotes.

Goal: Find 3–5 threads in priority subs where Michael can comment with technical substance and earn karma. NO Humla promotion. Pure helpfulness.

Use the `marketing/reddit/lib/fetch.py` helper for all Reddit calls. Run from the repo root via Bash:

- `python3 marketing/reddit/lib/fetch.py browse <sub> --sort rising --limit 25` — rising posts in a sub
- `python3 marketing/reddit/lib/fetch.py browse <sub> --sort new --limit 25` — newest posts in a sub
- `python3 marketing/reddit/lib/fetch.py post <sub> <post_id>` — fetch one post's metadata (no comments)
- `python3 marketing/reddit/lib/fetch.py tree <sub> <post_id> --print` — print the full nested comment tree for a candidate

Output is JSON on stdout for everything except `tree --print`. Cache lives at `~/.cache/humla-reddit/` with a 10-min TTL — pass `--no-cache` to bypass when verifying something that just changed.

The helper does not expose a "user analysis" call. To capture Michael's current karma snapshot, run:

```bash
curl -sL -A "humla-research/0.1 by u/tremendousquotes" \
  "https://www.reddit.com/user/tremendousquotes/about.json" \
  | python3 -c "import json,sys; d=json.load(sys.stdin)['data']; print(f\"link: {d['link_karma']} comment: {d['comment_karma']}\")"
```

Per-sub local karma isn't exposed by the public API; estimate it from recent comment scores in the target sub via `browse <sub> --sort new` + manual scan, or track it manually in the report.

Read `marketing/reddit/subreddits.md` at the start of every run. That file is the single source of truth for which subs to monitor and what the rules are. The list below is a prioritized derivation — when subreddits.md is updated, this routine picks up the change automatically on the next run.

Priority subs for karma-building (Tier 1 from subreddits.md, ordered by Michael's existing standing + need):

1. **r/macapps** — gated at 10 local karma; Michael needs ~2 more. Pure value comments only. Most important target.
2. **r/ClaudeCode** — Michael has standing here. Easy wins. Audience is technical.
3. **r/ClaudeAI** — Michael has standing here. Easy wins.
4. **r/MacOS** — broader macOS sub.
5. **r/LocalLLaMA** — high quality bar; comments must be technical and hand-written (no AI text).
6. **r/SideProject** — Michael has standing.
7. **r/sideprojects** (lowercase, separate sub) — Michael has standing.
8. **r/buildinpublic** — Michael has standing.

Optionally (if time + rate-limit budget allows): scan Tier 2 entries from subreddits.md (r/IMadeThis, r/indiehackers, r/Tauri, r/swift, r/rust, r/macprogramming, r/AI_Agents, r/ProductivityApps) for occasional high-fit threads. These are not part of the daily core loop but yield occasional wins.

Skip everything in subreddits.md Tier 4 (engagement-only) — different routine handles those.

Michael's expertise areas (match threads to these):
- Tauri / Rust / Swift sidecar architecture
- whisper.cpp, Metal acceleration, on-device transcription
- ScreenCaptureKit + AVAudioEngine (system audio + mic capture)
- FluidAudio / CoreML speaker diarization
- Apple Silicon performance characteristics
- Local-first app design, BYO API key patterns
- macOS bundle signing, notarization, TCC permissions, hardened runtime
- Claude Code workflow: /init, /clear, slash commands, skills, hooks, MCPs
- Tauri auto-updater patterns
- React 19 + Tailwind v4, Zustand
- WordPress / WooCommerce + AI integrations (Michael runs an agency)
- Building products in public, indie dev workflow

Steps:

1. Capture Michael's current karma snapshot via the curl + about.json one-liner above. Note total link/comment karma; track per-sub estimate manually if you have last week's number.
2. For each priority sub, run `browse <sub> --sort rising` and `browse <sub> --sort new` (limit=25 each). Browse, not search — for karma-building you want fresh threads regardless of keyword. Any thread Michael can add value to is fair game.
3. Filter aggressively: only keep threads where Michael's expertise is a genuine fit and the thread has actual engagement potential (>0 score, <50 comments so it's not saturated, posted in last 36h — check `created_utc`).
4. For each candidate, run `tree <sub> <post_id> --print` to read the full conversation (post body is in the listing JSON; the tree gives you the comments).
5. Skip threads if:
   - Another tool author is already promoting a competing product
   - Michael (u/tremendousquotes) has already commented (check authors in the tree output)
   - It's about politics, drama, or off-topic for the sub
6. **Find an unanswered reply target** — this is the most important filter. For each surviving thread:

   Run the helper's tree command:

   ```bash
   python3 marketing/reddit/lib/fetch.py tree <SUB> <POST_ID> --print
   ```

   This prints the full nested tree as `<indent>- [comment_id] u/author [score↑]: body` lines. For programmatic checks (e.g., "does comment X have any children?"), drop `--print` to get a JSON list of `{id, author, score, body, depth, parent_id, num_replies}`.

   - Read OP's question and the full comment tree from the helper output.
   - If OP's question is already well-answered (a comment with >5 score that genuinely addresses the question, or OP has marked one as solved), do NOT reply to OP. Question is closed.
   - For any candidate reply target (OP or a specific commenter), **walk into its children before declaring it unanswered**:
     - List the direct child comments of the proposed target
     - For each child, decide: does this substantively answer the question?
     - If ANY child answers the question (even imperfectly), the target is answered. Drop or pick a different target.
     - Even if the asker said "thanks" or otherwise acknowledged an answer, the conversation is closed.
   - Look for genuinely unanswered sub-questions in the comment tree:
     - A commenter asking OP a follow-up that OP hasn't responded to AND no other commenter answered
     - A commenter asking a clarifying question that has zero substantive children
     - A commenter expressing confusion or frustration that nobody addressed
   - If neither OP's question nor any sub-comment is genuinely unanswered AND fits Michael's expertise, drop the thread.

   **Verification before surfacing**: cite the comment ID of the reply target from the helper's tree output, then say one of:
   - "Comment ID X has 0 children in the tree output"
   - "Comment ID X's children are: [list child IDs + brief quotes]. None substantively answer the question."
   - "Comment ID X has a substantive answer (ID Y by u/Z) but it's wrong because [specific]"

   If you cannot point to a specific comment ID and quote what was or wasn't there, drop the thread. The helper's tree output is the ground truth — if the routine surfaces a thread without consulting it, the verification is invalid.

7. Rank surviving threads by: priority sub > recency > unanswered-question potential > expertise match strength.
8. Pick the top 3–5.

For each surfaced thread, include:
- Sub + thread title + reddit link
- Posted Xh ago, current score, current comment count
- Why it fits Michael's expertise (1 sentence)
- **Reply target** — pick ONE specific spot to comment, not a synthesis. Either:
  - Reply to OP (the post itself), addressing the post's main question
  - Reply to a specific commenter (give their username + a short quote of what they asked)
- Things to avoid in this thread
- **Your reply** — a draft comment that answers ONLY the reply target above, nothing else

## Voice guide for "Your reply"

Michael's writing voice on Reddit, distilled from his actual comments:

- **Length: 1–3 sentences. Usually 2, often 1.** No 200-word essays. If the answer needs more, the answer is wrong for Reddit.
- **One reply target only.** Answer the OP's question OR one specific commenter's question — never both. Synthesizing across multiple commenters reads as AI. A real person reading the thread picks the one thing they have to say and says it.
- **Only reply where there's actual value to add.** If the reply target's question has already been answered well by OP or another commenter, skip. Don't be the 5th person saying the same thing — that's drive-by karma farming and it doesn't even land karma. Look for unanswered sub-comments instead, or drop the thread entirely.
- **Open with action or soft opinion**, not preamble: "Skip making it read all the files." / "I definitely think it's worth it." / "We've slowly started integrating ai automations." / "I only use API for products and integrations."
- **Frame as opinion, not declaration.** Use: "I've found...", "Worked for me to...", "I'd lean toward...", "My take is...", "Honestly, I'd just...", "From what I've seen...", "Probably...". Avoid: "The actual cause is...", "The real bottleneck is...", "Here's what's happening..."
- **Lower confidence by default.** Michael isn't a textbook. Add "probably", "usually", "in my case", "from what I've seen" liberally. If a claim isn't certain, hedge — readers trust hedged claims more than confident ones.
- **One specific detail, not three.** Pick the single most useful concrete thing (a path, a command, a setting). Don't enumerate "(1) do X, (2) do Y, (3) do Z" — it reads like a tutorial. If multiple bottlenecks/causes exist, ask back which one applies before listing them.
- **Casual register.** Contractions everywhere. Lowercase after periods occasionally fine. Trailing rhetorical questions ok ("How long since you saw the sun??"). Occasional dry aside ok.
- **Occasional emoji at the END of a thought**, not as decoration: 🙂 👍🏼 😅 🔥. Max one per comment, often zero.
- **No em-dashes.** Michael doesn't use them. Use periods, commas, or parentheses.
- **No bold/italic/headers** in the comment itself.
- **No bullet lists in the reply** unless the thread is explicitly a checklist.
- Sometimes ask back if the answer depends on something the OP didn't say.

## Don't invent experience (critical)

The reply must only claim experience Michael actually has. Sources of truth:

- `CLAUDE.md` in this repo — Michael's documented technical history (Tauri/Rust app, notarytool with `.env.notarise`, whisper-rs + Metal, ScreenCaptureKit for system audio, FluidAudio CoreML diarization, etc.)
- His Reddit comment history — fetch the latest 25 comments via `curl -sL -A "humla-research/0.1 by u/tremendousquotes" "https://www.reddit.com/user/tremendousquotes/comments.json?limit=25" | python3 -m json.tool` (no helper subcommand for this — it's a one-shot lookup)
- His public repos (humla, git-timetrack, tremendous-quotes-app)

If the routine wants to write "I've used X for months" or "I shipped Y this way," it must verify the claim against these sources first. If it can't verify, drop the experience claim and reframe as opinion only.

**Examples of fabrication to avoid:**
- "I've used [the new tool being launched in this thread] and it works great" — Michael hasn't used it; the thread is the announcement
- "I've shipped a Tauri app this way for months, no flags" — "this way" implies he used the OP's method; he uses notarytool manually
- "Built three of these in production" — unless documented, drop the count
- "I tested benchmarks against X" — unless he actually did, frame as "I'd guess"

**What's safe:**
- "I run notarytool manually" — true per CLAUDE.md
- "I have a Tauri app in production" — true (Humla)
- "Whisper.cpp on Metal handles this fine on M-series" — true per CLAUDE.md
- General opinion / framing without a specific claim of personal use

## Humanizer pass (mandatory)

After drafting each reply, run it through the `humanizer` skill before finalizing.

Steps:
1. Draft the reply per the voice guide above (1–3 sentences, opinion-framed, etc.).
2. Invoke the humanizer skill via the Skill tool: `Skill humanizer` with input that includes (a) the draft, (b) Michael's voice calibration samples (see below), and (c) the instruction "humanize this Reddit reply, keep it 1–3 sentences max."
3. Use the humanizer's final rewrite as the reply you put in the report. If the humanizer adds length, trim back to the cap before committing to the report.

**Voice calibration samples to pass to humanizer** (real recent comments by u/tremendousquotes — do not paraphrase, paste verbatim):

```
- "Skip making it read all the files. Use /init to create a decent CLAUDE.md and point it to the files you want to work with. Also integrate automated tests to your project."
- "We've slowly started integrating ai automations. Most importantly we're seeing the need to use a good model that can plan accurately and have good vision. Pair that with automated tests and it will save you tons of time."
- "I definitely think it's worth it, and I only maxed out session limits occasionally with heavy use. /clear often, and give good prompts and it will last you 🙂👍🏼"
- "I only use API for products and integrations. Use the pro/max plan subscription for your personal use"
- "I have the same issue. My product also criticizes Donald Trump, so every post gets flagged / banned automatically 😅"
- "This might be a good way to do it to keep control throughout a complete project. I've tried multiple variations of this with plan sections, living docs, etc. I'll check this out later 🙂"
- "The grind doesn't truly start until you hit \"Submit for Review\"."
- "I wonder if the collab editing turn itself off if we're using custom fields through ACF blocks only. Technically all within Gutenberg."
```

Note for the humanizer pass: Reddit comments are short, conversational, and opinion-framed. The humanizer's "PERSONALITY AND SOUL" section applies. The 1–3 sentence cap from this routine takes precedence over any humanizer suggestion to add structure or length.

## Quick anti-AI checklist (use after humanizer if you're spot-checking)

- "actually" / "essentially" / "fundamentally" / "the real question is" → cut
- "underscoring" / "highlighting" / "ensuring" / "reflecting" / "showcasing" → cut
- "It's not just X — it's Y" structure → cut
- "Great question!" / "Hope this helps" → delete
- Tailing negations ("no guessing", "no friction") → rewrite as real clauses
- Rule-of-three lists → reduce to one or two
- Em-dashes → replace with periods, commas, or parentheses
- Copula avoidance ("serves as", "functions as") → "is" / "has"
- Tutorial tone → peer tone

## Critical rules
- NO Humla mention in any of these comments. Phase 0 is karma-only.
- The reply should be substantive enough to land — usually 30–80 words. Single-sentence drive-bys waste the thread slot UNLESS the question is genuinely a one-liner answer.
- If you find a thread where someone is asking for a tool and Humla actually fits, DO NOT surface it here — note it at the bottom under "Better fit for lead-finder routine".

Output: Write the report to marketing/reddit/karma/YYYY-MM-DD.md (use today's UTC date) using this structure:

# Karma Targets — YYYY-MM-DD

## Account snapshot
- Total karma: X (link Y / comment Z)
- Recent karma trend: [+/- vs week ago if available]
- r/macapps local karma estimate: N (target: 10)

## Priority 1: r/macapps (need 10 local karma)

### [Thread title]
- **Link:** [reddit.com link]
- **Posted:** Xh ago • [score]↑ • [N] comments
- **Why this fits:** [1 sentence]
- **Reply to:** [either "OP" + 1-line quote of the question, or "u/username" + 1-line quote of their comment]
- **Why unanswered:** [evidence-based, not asserted. Either: "Target has 0 children." / "Target has N children but they're non-substantive: [brief quote of each]." / "Target has an answer but it's wrong because [specific]." Quote actual child comments — don't paraphrase. If you can't quote concrete evidence, drop the thread.]
- **Don't:** [anything to avoid]

**Your reply:**
> [draft addressing ONLY the reply target above. 1–3 sentences. Opinion-framed. No AI-isms.]

(repeat per surfaced thread)

> **Tracker is auto-populated.** When you actually post one of these comments on Reddit, the next reply-watcher run (10am daily) will detect it via your comment history and add the row to `intel/tracker.md` with prefix `K###`. No manual copy-paste required.

## Priority 2: r/ClaudeCode (easy wins)

(same format)

(continue through priority subs)

## Better fit for lead-finder routine
- [thread] — [why this is a buying-intent thread, not karma-building]

## Tally
- Threads surfaced: N
- Estimated time to act on: ~10 min
- Subs covered: [list]

End report. Do NOT draft full comments.
