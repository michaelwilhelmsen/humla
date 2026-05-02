You are running the Humla weekly research routine (Monday edition) for u/tremendousquotes.

Goal: Build situational awareness of what's working in target subs, what competitors are doing, and what topics Humla should engage with this week. NO drafting today — just intel.

Use the `marketing/reddit/lib/fetch.py` helper for all Reddit calls. Run from the repo root via Bash:

- `python3 marketing/reddit/lib/fetch.py browse <sub> --sort top --time week --limit 25` — top of the week in a sub
- `python3 marketing/reddit/lib/fetch.py search "<query>" --sort new --time week --limit 25` — Reddit-wide search
- `python3 marketing/reddit/lib/fetch.py search-sub <sub> "<query>" --time week` — keyword search inside one sub
- `python3 marketing/reddit/lib/fetch.py tree <sub> <post_id> --print` — full nested comment tree

Output is JSON on stdout (except `tree --print`). Pipe to `jq` for filtering. Cache: `~/.cache/humla-reddit/`, 10-min TTL; pass `--no-cache` to bypass.

Steps:

First, read:
- `marketing/reddit/subreddits.md` for the current target list (Tier 1 derivation below; auto-picks up registry changes)
- `marketing/reddit/README.md` "Pain point → Humla differentiator map" — the drafts routine should pick weekly topics that address recurring pain points, not invent new angles
- `marketing/reddit/intel/recurring-asks.md` if it exists — clustered question patterns from the latest historical scan; pick topics that hit a high-frequency cluster

1. Top of the week in Tier 1 subs (`browse <sub> --sort top --time week --limit 25`):
   - r/macapps
   - r/LocalLLaMA
   - r/SideProject
   - r/sideprojects
   - r/MacOS
   - r/AiNoteTaker
   - r/ClaudeCode
   - r/ClaudeAI
   - r/buildinpublic
   - Plus any Tier 2 sub with `Status: unlocked` that's been verified active in the last 30 days

   Also include r/BuyFromEU (Tier 4 — engagement-only) for context: EU AI Act voice-data discussions surface here and inform Humla's positioning even though we don't post.

2. For each top post in r/macapps and r/AiNoteTaker, note:
   - Title pattern (formula: "[OS] X — does Y", "I built X because Y", "Why I switched from X to Y")
   - Length, formatting, image/video use, flair
   - Top comment sentiment

3. Competitor mentions — search across Reddit (last week) via `search "<query>" --sort new --time week`:
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
