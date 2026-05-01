# Humla — Reddit Marketing System

A multi-layered set of routines for marketing Humla on Reddit. Built around three loops:

1. **Karma loop** — daily, build authentic standing in target subs
2. **Research + drafting loop** — weekly, monitor competition + produce launch-quality posts
3. **Lead loop** — daily, surface high-intent threads where Humla genuinely solves the asker's problem

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
├── routines/              # the three loop specs (committed to git)
│   ├── karma-builder.md
│   ├── research-and-drafts.md
│   └── lead-finder.md
├── karma/                 # daily karma-builder output (gitignored)
├── research/              # weekly research output (gitignored)
├── drafts/                # weekly post drafts (gitignored)
├── leads/                 # daily lead-finder output (gitignored)
└── intel/                 # competitor intel + Open Recorder asset library (gitignored)
```

`marketing/.gitignore` keeps the dynamic outputs local. The specs are versioned so you can tune them and see what changed.

## Execution: Local Routines (Claude Desktop)

All three loops run as **Local Routines** in Claude Desktop:

- Routines tab → New routine → **Local** (not Remote)
- Local routines run on your machine while it's awake
- They have full access to your local MCPs (Reddit_MCP_Buddy, Obsidian, etc.)
- Output writes directly to your filesystem — no git roundtrip needed
- "Worktree" off for these (we want the writes to land in the main tree)

The exact field values for each routine are in the corresponding `routines/*.md` file under "Setup in Claude Desktop".

### Schedule summary

| Routine | When | UI schedule |
|---|---|---|
| `humla-karma-builder` | daily 9am | Daily at 09:00 |
| `humla-lead-finder` | daily 12pm | Daily at 12:00 |
| `humla-research-monday` | Mondays 9am | Weekly → Monday 09:00 |
| `humla-draft-friday` | Fridays 2pm | Weekly → Friday 14:00 |

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
