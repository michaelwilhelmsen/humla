# Humla — Reddit Marketing System

A multi-layered set of routines for marketing Humla on Reddit. Built around four loops:

1. **Karma loop** — daily, build authentic standing in target subs
2. **Research + drafting loop** — weekly, monitor competition + produce launch-quality posts
3. **Lead loop** — daily, surface high-intent threads where Humla genuinely solves the asker's problem
4. **Reply loop** — daily, watch for new replies on Michael's tracked comments and draft follow-ups

Each loop is a markdown spec in `routines/`. Each is wired up as a **Local Routine** in Claude Desktop (Routines → New routine → Local), pointing at this folder so the local Reddit MCP and project context are available.

## Account

- Reddit username: `u/tremendousquotes`
- Github repo: `https://github.com/michaelwilhelmsen/humla` (public, SSH origin)
- Other product on the same account: Tremendous Quotes (iOS) — separate marketing
- Active subs already with positive karma: r/Wordpress, r/ClaudeAI, r/ClaudeCode, r/sideprojects, r/SideProject, r/iosapps, r/buildinpublic, r/AiNoteTaker

## Identity (the frame for all engagement)

Humla is for **developers and consultants who want to own their meeting data**. Not a SaaS pitch, not a "10x productivity" tool. The community Humla resonates with values:

- Local-first and BYO-key over hosted convenience
- Apple Silicon as a real compute platform
- No bots, no meeting-join URLs, no vendor lock-in
- Open data formats (SQLite, plain markdown)

Every comment and post should reinforce this identity, not just promote the product.

## Sub priority + posting rules (verified from each sub's rules JSON)

| Sub | Karma gate | Promo rule | Status for Humla |
|---|---|---|---|
| r/macapps | **10 local** | 1 dev post / 30 days; flair required; `[OS]` prefix if open source | needs ~2 more local karma |
| r/AiNoteTaker | none published | niche, low traffic, high intent | unlocked, already established |
| r/SideProject | none | very lenient | unlocked |
| r/sideprojects (lowercase, separate sub) | none | lenient | unlocked |
| r/buildinpublic | none | lenient | unlocked |
| r/ClaudeCode | none | natural fit (Humla built with Claude Code) | unlocked |
| r/ClaudeAI | none | natural fit | unlocked |
| r/LocalLLaMA | none published | 1/10 rule; **no LLM-generated content** — hand-written only | unlocked but high bar |
| r/MacOS | none published | self-promo **Saturdays UTC only**; needs MAS or reputable GitHub repo | Saturdays only |
| r/ObsidianMD | none published | first post = promo → instant ban | engagement-only until history exists |
| r/privacy | n/a | **no self-promo, ban without warning** | engagement-only forever |
| r/consulting | n/a | no self-promo (Rule 5) | engagement-only forever |
| r/selfhosted | n/a | apps must be self-hosted server-style | likely off-topic |

## Folder layout

```
marketing/reddit/
├── README.md              # this file
├── subreddits.md          # central registry — single source of truth for sub list
├── lib/                   # shared helpers used by every routine
│   └── fetch.py           # Reddit JSON scraper — replaces Reddit_MCP_Buddy
├── routines/              # the loop specs (committed to git)
│   ├── karma-builder.md
│   ├── lead-finder.md
│   ├── reply-watcher.md
│   ├── research-and-drafts.md
│   └── historical-scan.md
├── karma/                 # daily karma-builder output (gitignored)
├── research/              # weekly research output (gitignored)
├── drafts/                # weekly post drafts (gitignored)
├── leads/                 # daily lead-finder output (gitignored)
└── intel/                 # competitor intel + Open Recorder asset library (gitignored)
```

## Reddit access — `lib/fetch.py`

All four routines call Reddit through `marketing/reddit/lib/fetch.py`. We used the `Reddit_MCP_Buddy` MCP until Reddit's policy change blocked the auth path the MCP relied on; the helper hits reddit.com's `.json` endpoints directly with a UA string and a small on-disk cache.

Command surface (run from the repo root):

```bash
python3 marketing/reddit/lib/fetch.py browse <sub> --sort {hot|new|rising|top|controversial} [--time week] [--limit 25]
python3 marketing/reddit/lib/fetch.py search-sub <sub> "<query>" [--sort new] [--time week] [--limit 25]
python3 marketing/reddit/lib/fetch.py search "<query>" [--sort new] [--time week] [--limit 25]
python3 marketing/reddit/lib/fetch.py post <sub> <post_id>
python3 marketing/reddit/lib/fetch.py tree <sub> <post_id> [--depth 10] [--limit 200] [--print]
```

- Output is JSON on stdout (pipe to `jq`), except `tree --print` which prints an indented human-readable tree.
- Cache lives at `~/.cache/humla-reddit/` with a 10-minute TTL. Pass `--no-cache` (top-level) to bypass.
- The helper exposes `browse_subreddit`, `search_subreddit`, `search_reddit`, `get_post_with_comments`, and `walk_comments` as Python functions if you want to import it from a longer script — see the docstring at the top of `fetch.py`.

Practical Reddit unauth limit is ~60 req/min per IP. The cache + sequential calls keep us well under that for normal routine runs.

`marketing/.gitignore` keeps the dynamic outputs local. The specs are versioned so you can tune them and see what changed.

## Tracker + reply-watcher (the feedback loop)

`marketing/reddit/intel/tracker.md` is the single record of everything Michael has posted/commented on Reddit through these routines. **It's auto-populated** — the reply-watcher routine fetches Michael's last 50 Reddit comments daily, cross-references against the tracker (skip if already there) and against recent surfaced files in `karma/`, `leads/`, `drafts/`, `intel/` (to identify which routine surfaced the thread), and appends new rows with the right prefix (`K###`, `L###`, `E###`, etc.).

If the new comment is a follow-up to one of Michael's existing tracked comments (Reddit `parent_id` matches), the watcher updates the parent row's Status to `engaged` instead of duplicating.

After auto-population, the same routine walks each active row's thread for new child comments under Michael's reply and drafts follow-ups for any new ones. Output goes to `leads/follow-ups-YYYY-MM-DD.md`.

**Status flow:**
- `waiting` → posted, no replies yet
- `replied` → someone replied; reply-watcher surfaced a follow-up draft for review
- `engaged` → Michael posted his follow-up; watching for more
- `closed` → conversation ended (asker thanked, picked another tool)
- `archived` → 14 days inactive; auto-set by reply-watcher

The tracker is gitignored (it contains live activity records); the reply-watcher routine spec in `routines/reply-watcher.md` is committed.

## Single source of truth: subreddits.md

`marketing/reddit/subreddits.md` is the registry every routine reads. It defines:
- Which subs to monitor, organized by Tier (1=core, 2=adjacent, 3=vertical, 4=engagement-only)
- Per-sub karma gates and promo rules (verified from each sub's rules JSON)
- Per-sub query patterns for the lead-finder
- Per-sub status (unlocked / locked-pending-karma / engagement-only / unverified)

When you discover a new relevant sub, add it to subreddits.md. The next routine run picks it up — no need to update each routine prompt separately.

Routines must read subreddits.md at the start of every run and respect the current data. If a sub is marked `Status: unverified`, the routine should fetch its rules JSON via curl and update the registry with the verified answer before treating it as promo-allowed.

## Execution: Local Routines (Claude Desktop)

All three loops run as **Local Routines** in Claude Desktop:

- Routines tab → New routine → **Local** (not Remote)
- Local routines run on your machine while it's awake
- They have full access to your local MCPs (Obsidian, etc.) and to `marketing/reddit/lib/fetch.py` via Bash
- Output writes directly to your filesystem — no git roundtrip needed
- "Worktree" off for these (we want the writes to land in the main tree)

The exact field values for each routine are in the corresponding `routines/*.md` file under "Setup in Claude Desktop".

### Schedule summary

| Routine | When | UI schedule |
|---|---|---|
| `humla-historical-scan` | quarterly (effective) | Weekly + 85-day skip-guard in prompt |
| `humla-karma-builder` | daily 9am | Daily at 09:00 |
| `humla-reply-watcher` | daily 10am | Daily at 10:00 |
| `humla-lead-finder` | daily 12pm | Daily at 12:00 |
| `humla-research-monday` | Mondays 9am | Weekly → Monday 09:00 |
| `humla-draft-friday` | Fridays 2pm | Weekly → Friday 14:00 |

**Daily flow (intentional ordering):**
- 09:00 → karma-builder writes today's karma targets
- 10:00 → reply-watcher checks tracker.md for new replies on yesterday's posts
- 11:00 → Michael reviews both, posts comments, copies suggested tracker entries
- 12:00 → lead-finder writes today's leads, fresh queue for the afternoon

Run `humla-historical-scan` first (manually) before enabling the daily routines — it populates `intel/_seen-permalinks.txt` for de-dup and gives the drafts routine pattern intel to work from.

All times are in your local timezone (Europe/Oslo). Local Routines respect local time, no UTC conversion needed.

## Open Recorder integration

Open Recorder (https://github.com/imbhargav5/open-recorder) is the recommended tool for marketing assets — short GIFs/videos that go into Reddit posts and high-intent comments. Reddit posts with embedded video consistently outperform text-only.

Pre-made clip library (target list, build over time):

| Clip | What it shows |
|---|---|
| `humla-mic-sys-parallel.gif` | Mic + system audio captured as separate streams |
| `humla-offline-diarize.gif` | Stop → Speaker labels appear, no network |
| `humla-byo-key.gif` | Settings → paste your own OpenAI key |
| `humla-no-bot.gif` | Recording during Zoom/Meet, no bot in attendee list |
| `humla-tauri-tray.gif` | Menu bar interaction, native feel |

Save to `marketing/reddit/intel/assets/` (gitignored). Lead-finder and drafts routines reference this library when suggesting comment/post assets.

## Account hygiene rules (encoded in routines)

1. **Never post Humla in r/privacy or r/consulting.** Comment value-add only.
2. **Never use AI-drafted text in r/LocalLLaMA or r/ObsidianMD.** Hand-write.
3. **Disclose Humla affiliation** when commenting in any sub if mentioning Humla.
4. **Wait until 10 local karma in r/macapps** before any Humla mention there. Until then: pure helpfulness on others' threads.
5. **Don't comment on the same thread twice in 24h.** No bumping.
6. **Skip threads where another tool author is already promoting their tool.** Don't hijack.
7. **No comment under 50 words.** Substance only.
8. **No UTM tags on links.** Plain humla.no.
9. **First comment never includes a link.** Reply with link only after the asker engages.

## Pain point → Humla differentiator map

When drafting a reply or post, lead with the pain point that matches the audience. The mapping below is sourced from the Bucket B clusters in `intel/recurring-asks.md` (refreshed every quarterly historical-scan).

| Pain point recurring on Reddit | Humla's response | Asset clip (Open Recorder) |
|---|---|---|
| "Subscription too expensive / hit team-plan paywall" (Notion AI, Granola, Otter) | BYO API key. You only pay for the tokens you use. No Humla subscription. | `humla-byo-key.gif` |
| "Can't justify $X/month for the AI features" | Free download, transcript stays on your machine. Cloud transcription optional. | `humla-byo-key.gif` |
| "Otter / Fathom / Fireflies bot showed up uninvited" | No bot. System-audio capture means Humla never has to join the meeting. | `humla-no-bot.gif` |
| "FOSS Granola alternative?" | Source-available on GitHub, local SQLite, plain markdown export. | `humla-tauri-tray.gif` (general native demo) |
| "Wall of text / can't tell who said what" | Offline diarization on stop. Speaker labels in colored pills. | `humla-offline-diarize.gif` |
| "Where does my meeting data go?" | Local SQLite. Optional cloud transcription via your own key. Airplane mode supported end-to-end. | `humla-offline-diarize.gif` (shows airplane mode) |
| "I want this on-device" / "Apple Silicon" | whisper-rs with Metal acceleration; FluidAudio CoreML for diarization. | `humla-offline-diarize.gif` |
| "Recording in-person meetings" / "around the table" | Mic-only mode auto-detects and diarizes the room. Same pipeline, different branch. | `humla-mic-sys-parallel.gif` (or in-person variant) |
| "EU data residency" / "AI Act voice rules" | Built in Norway. Local by default. No US-server dependency unless you opt in via your own OpenAI key. | `humla-offline-diarize.gif` (airplane mode visible) |

Update this table when historical-scan finds new clusters worth addressing.

## Findings tracker

When a research routine surfaces a strategic finding (a new competitor, a new high-value sub, a sustained narrative shift), capture it as an entry in this section so future tunings can reference what changed and why.

### 2026-05-02 — first historical-scan (60-day window)
- **407 threads inspected.** Bucket A=2, Bucket B=61 (clustered into 11 patterns), Bucket C=63.
- **r/AI_Agents promoted Tier 2 → Tier 1.** Produced both Bucket A leads on first scan. Verified-allowed via rules JSON, links go in comments not posts, 1/10 self-promo ratio.
- **Top recurring-ask clusters** (in priority order from `intel/recurring-asks.md`):
  1. FOSS Granola alternative
  2. No-bot AI notetaker
  3. Speaker identification / wall-of-text complaint
  4. Local Whisper for meetings
  5. Otter privacy alternative
- **Bucket C insight:** Rust+Tauri is the dominant stack across new entrants — Humla's stack is validated, not differentiating. Speaker-attribution IS differentiating. Cloud-tool fatigue (Otter spam, Fathom hallucinations, Granola flakiness) is a recurring complaint and a positioning hook.
- **Notion AI Meeting Notes** added as a tracked competitor. Personal pain point that drove Humla's existence: hit a $1000+ team-plan paywall after the trial. Added r/Notion to Tier 2 registry. "Too expensive / trial ended / team plan" added to lead-finder intent markers across the board.
- **Lead-finder intent markers expanded** based on Bucket B clusters: price shock (`too expensive`, `couldn't justify`, `team plan`, `trial ended`), FOSS framing (`FOSS`, `open source`, `BYO key`), no-bot pain (`no bot`, `bot showed up`), speaker pain (`wall of text`, `who said what`), in-person specifics (`around the table`, `no laptop`).
- **407 permalinks loaded into `intel/_seen-permalinks.txt`** — daily lead-finder de-dup primed.

### 2026-W18 (week of Apr 27)
- **New direct competitor: Myna** (u/heyAshwinn) — local-first Mac meeting notes, mic+sys audio, structured summaries, no account, no model download. Colliding with Humla on most axes. Differences worth verifying: <20MB install (likely Apple Speech APIs vs Humla's whisper-rs ~547MB), no diarization, no language coverage mention. Free 5 meetings/month.
- **New high-fit sub discovered: r/AiNoteTaker** — small but high intent, several open Humla-shaped asks. Promoted to Tier 1 in subreddits.md.
- **EU AI Act voice-data narrative** — r/BuyFromEU thread (305 upvotes) names Otter/Fathom/Fireflies as US-server liabilities. Humla's local-first + Norway angle aligns directly. r/BuyFromEU added to Tier 4 (engagement-only) — useful for context, off-topic for direct promo.
- **Top format in r/macapps**: Problem → Comparison → Solution prose with inline GIF or v.redd.it clip. 7 of top 10 had video. Pure-text only works when the news is the asset (Notepad++ port). Reinforces Open Recorder asset priority.
- **Format that works in r/ClaudeCode**: skill/tool sharing posts (humanizer hit 572 upvotes) outperform product pitches. "Built with Claude Code" angle is valid for Humla.
- **r/AiNoteTaker cadence**: goes 24–72h without new asks. Lead-finder's 72h window is right.

## Phase plan

**Phase 0 — Karma sprint (now → 10 local karma in r/macapps, ~2 weeks)**
- Daily karma-builder running, comment on 2–3 surfaced threads
- Use The App Pile megathread in r/macapps for first Humla mention (allowed without main-feed qualification)
- Land 1 lenient-sub post: r/SideProject "building Humla in public"

**Phase 1 — Establish (weeks 3–6)**
- Daily lead-finder running, target 1–2 high-intent comments per day
- Weekly research routine producing 1 publishable draft per week
- First main-feed r/macapps post once 10 local karma cleared
- Start building Open Recorder clip library

**Phase 2 — Compounding (week 7+)**
- Saturday r/MacOS post (with public GitHub repo + recorded demo)
- Sustained presence in r/LocalLLaMA + r/macapps
- Monthly "Humla update" post in r/SideProject + r/buildinpublic

## Metrics to watch (review weekly)

- Local karma in r/macapps (target: ≥10 by week 2, ≥50 by week 6)
- Total Reddit karma growth
- Number of high-intent threads commented on per week
- Replies to your comments (signal of actual engagement)
- humla.no traffic spikes correlated with comment activity (loose attribution, no UTMs)
- Star count on the GitHub repo (Reddit traffic typically pulls stars)
