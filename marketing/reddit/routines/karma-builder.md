# Karma Builder — Daily

**Purpose:** Surface 3–5 threads where Michael (`u/tremendousquotes`) can comment with technical substance and earn local karma. **No Humla promo.** Pure helpfulness.

**Cadence:** Daily, 9am Europe/Oslo.

**Execution:** Claude Desktop **Local** Routine. Folder: humla project. Uses local Reddit MCP (`Reddit_MCP_Buddy`).

---

## Setup in Claude Desktop

1. Routines → New routine → **Local**
2. **Name:** `humla-karma-builder`
3. **Description:** `Daily Reddit karma-builder for u/tremendousquotes — surface threads where Michael can comment with technical substance, no Humla promo.`
4. **Instructions:** paste the prompt block below
5. **Select folder:** `~/Documents/Development/Claude Code/humla`
6. **Worktree:** off (we want output in the main tree, not a worktree)
7. **Ask permissions:** Default
8. **Schedule:** Daily at 09:00

---

## Instructions (paste this into the routine)

```
You are running the Humla daily karma-builder routine for u/tremendousquotes.

Goal: Find 3–5 threads in priority subs where Michael can comment with technical substance and earn karma. NO Humla promotion. Pure helpfulness.

Use the Reddit MCP (Reddit_MCP_Buddy) for all Reddit queries:
- mcp__Reddit_MCP_Buddy__browse_subreddit (sort=rising, sort=new) for each sub
- mcp__Reddit_MCP_Buddy__get_post_details on candidates to read top comments
- mcp__Reddit_MCP_Buddy__user_analysis on tremendousquotes once at start to get current karma

Priority subs (in this order):
1. r/macapps — gated at 10 local karma; Michael needs ~2 more. Pure value comments only.
2. r/ClaudeCode — Michael has standing here. Easy wins.
3. r/ClaudeAI — Michael has standing here. Easy wins.
4. r/MacOS — broader macOS sub.
5. r/LocalLLaMA — high quality bar; comments must be technical.
6. r/SideProject — Michael has standing.
7. r/sideprojects — Michael has standing (note: lowercase, separate sub).
8. r/buildinpublic — Michael has standing.

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

1. Run user_analysis on tremendousquotes to capture current karma snapshot.
2. For each priority sub, browse_subreddit with sort=rising and sort=new (limit=25 each).
3. Filter aggressively: only keep threads where Michael's expertise is a genuine fit and the thread has actual engagement potential (>0 score, <50 comments so it's not saturated, posted in last 36h).
4. For each candidate, fetch the post + top 5 comments via get_post_details to understand the conversation.
5. Skip threads if:
   - Another tool author is already promoting a competing product
   - The thread is already answered (top comment has >20 score and addresses the question)
   - Michael (u/tremendousquotes) has already commented (check authors of all comments)
   - It's about politics, drama, or off-topic for the sub
6. Rank surviving threads by: priority sub > recency > engagement potential > expertise match strength.
7. Pick the top 3–5.

For each surfaced thread, include:
- Sub + thread title + reddit link
- Posted Xh ago, current score, current comment count
- Why it fits Michael's expertise (1 sentence)
- The specific technical detail Michael should lead with (e.g., "you actually need ScreenCaptureKit, not AVAudioEngine, because system audio routes go through the screen capture API on macOS 13+")
- Comment angle: 2–3 sentences describing what to address. Do NOT write the full comment — Michael writes that.
- Things to avoid in this thread (e.g., "don't mention Humla even though it's tempting — this is karma-building")

Critical rules:
- NO Humla mention in any of these comments. Phase 0 is karma-only.
- Comments should be ≥50 words substantive. Single-sentence drive-bys waste the thread slot.
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
- **Lead with:** [specific technical detail]
- **Angle:** [2–3 sentences on what to say]
- **Don't:** [anything to avoid in this thread]

(repeat per surfaced thread)

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
```

## Open Recorder integration

Karma-builder is text-only. Skip Open Recorder for this routine.
