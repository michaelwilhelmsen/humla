You are running the Humla historical-scan routine.

Goal: 60-day sweep of relevant Reddit history. Three outputs written to disk in marketing/reddit/intel/.

## Step 0 — Quarterly skip-guard (run this FIRST)

This routine is scheduled Weekly in Claude Desktop because the schedule picker has no Quarterly option, but the actual cadence is quarterly via this guard. A weekly fire-and-skip costs nothing.

Before doing any work:

1. List files in `marketing/reddit/intel/` matching `historical-scan-*.md` (use ls or Glob).
2. Parse the YYYY-MM-DD date from the most recent filename.
3. Compute days since that date.
4. If days < 85: exit IMMEDIATELY with a single-line message: `Skipped: last scan was N days ago (YYYY-MM-DD). Next quarterly run in M days.` Do not run the scan. Do not write any output files. Do not call any Reddit APIs.
5. If days ≥ 85, OR no prior scan file exists: proceed.

## Outputs (when not skipping)

1. marketing/reddit/intel/historical-scan-YYYY-MM-DD.md — pattern intel + competitor mentions + evergreen reply candidates
2. marketing/reddit/intel/_seen-permalinks.txt — flat list of every thread permalink found, for de-dup priming. Append, don't overwrite if file exists.
3. marketing/reddit/intel/recurring-asks.md — categorized list of recurring question patterns

Use the `marketing/reddit/lib/fetch.py` helper for all Reddit calls (Reddit's policy change made the MCP's auth path unusable; we hit reddit.com's `.json` endpoints directly with a UA string + on-disk cache):

- `python3 marketing/reddit/lib/fetch.py search-sub <sub> "<query>" --sort top --time year --limit 100` — keyword-scoped search inside one sub over the past year
- `python3 marketing/reddit/lib/fetch.py browse <sub> --sort top --time year --limit 100` — top-of-year sweep with no keyword
- `python3 marketing/reddit/lib/fetch.py tree <sub> <post_id> --print` — full nested comment tree for verifying threading

Output is JSON on stdout (except `tree --print`). Cache: `~/.cache/humla-reddit/`, 10-min TTL.

## Per-sub scan

Read `marketing/reddit/subreddits.md` first. The historical scan covers the union of Tier 1, Tier 2, and Tier 3 (60-day window). Tier 4 is also scanned, but only for engagement-only candidates and competitor-activity intel.

For each target sub, run `search-sub <sub> "<query>" --sort top --time year --limit 100` for each query pattern from that sub's "Query patterns (lead-finder)" field in subreddits.md. Then post-filter to the last 60 days using `created_utc`. (time=year then post-filter — Reddit's API doesn't have a 60-day option.)

For r/AiNoteTaker specifically, also run `browse AiNoteTaker --sort top --time year --limit 100` (no keyword) to catch posts that don't match any specific keyword — this sub is small enough that a full sweep is feasible and worth it.

If a sub in the registry is marked `Status: unverified`, fetch its rules JSON via curl first and update subreddits.md with the verified data before treating the sub as promo-allowed.

When the registry adds new subs (Michael discovers them), the next historical-scan run picks them up automatically — no need to update this routine prompt.

## Filter and post-filter

- Drop posts older than 60 days (post-filter using created_utc)
- Drop NSFW
- Drop posts with score ≤ 0
- Drop posts where Michael has already commented (check via `tree <sub> <post_id>` and grep for `u/tremendousquotes`)
- Keep both intent posts (asking) and announcement posts (competitor launches) — they go to different sections

## Categorize each surviving thread

For each thread, decide which bucket:

A. **Evergreen reply candidate** — meets ALL of:
   - Asking question (intent marker present in title/body)
   - Has substantive engagement (≥3 comments, ≥3 score)
   - Doesn't have a definitive accepted answer (verify via `tree <sub> <post_id> --print` — same pattern as lead-finder)
   - Posted within last 30 days (older than that, even great replies get buried)
   - Sub allows promo (per marketing/reddit/README.md)

B. **Pattern-intel only** — recurring question pattern but too old / already-answered / wrong sub for promo. Captures the kind of question people keep asking. Quote the title and 1-line body.

C. **Competitor activity** — announcement / launch / "introducing X" post. Note the product name, pitch angle, score, comments. Goes to recurring-asks.md as competitive context.

D. **Skipped** — drop with reason in audit trail.

## Output 1: marketing/reddit/intel/historical-scan-YYYY-MM-DD.md

# Historical Scan — YYYY-MM-DD (last 60 days)

## Scan parameters
- Window: YYYY-MM-DD to YYYY-MM-DD
- Subs covered: [list]
- Total threads inspected: N
- Threads in each bucket: A=X, B=Y, C=Z, skipped=W

## Bucket A — Evergreen reply candidates

For each (sorted by intent strength + recency):

### [Thread title]
- **Sub:** r/...
- **Link:** [permalink]
- **Posted:** [date], [score]↑, [N] comments
- **What they're asking:** [1 sentence]
- **Why still engageable:** [evidence-based, citing comment ID counts from the helper's tree output]
- **Humla fit:** [which differentiator]
- **Reply target:** [OP or u/username + quote]
- **Suggested angle:** [1–2 sentences. Do NOT draft the full reply yet — Michael decides which of these to act on first, then runs each through the humanizer pass manually or via a follow-up routine.]

## Bucket C — Competitor activity

| Date | Sub | Product | Pitch | Score | Comments |
|---|---|---|---|---|---|
| ... | ... | ... | ... | ... | ... |

Notable patterns: [2–3 bullets on what's resonating in competitor launches]

## Audit trail

- Bucket B count: X (full list in recurring-asks.md)
- Skipped: Y, primary reasons: [list]
- API calls made: [rough count]

## Output 2: marketing/reddit/intel/_seen-permalinks.txt

Append every thread permalink encountered (regardless of bucket) to _seen-permalinks.txt, one per line. If file exists, deduplicate before writing. This primes the daily lead-finder's de-dup so it doesn't re-surface anything from this scan.

## Output 3: marketing/reddit/intel/recurring-asks.md

Cluster Bucket B threads by question pattern. Aim for 5–15 clusters.

Format:

# Recurring Asks (last 60 days)

## Cluster: [theme — e.g., "Granola too expensive / looking for free alternative"]
- N threads in last 60 days
- Example phrasings:
  - "[exact title from thread 1]"
  - "[exact title from thread 2]"
  - "[exact title from thread 3]"
- Subs where it appears: r/X, r/Y
- Common pain point in body: [1 sentence]
- Humla differentiator that addresses this: [1 sentence]
- Implied for routines: [e.g., "Add 'too expensive' to lead-finder intent markers"]

## Cluster: [theme]
...

End report. Save all three files. Do not draft full replies — this scan is for intel and shortlist generation, not commenting.
