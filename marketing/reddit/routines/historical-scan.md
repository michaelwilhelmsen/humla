# Historical Scan — One-shot bootstrap (and quarterly refresh)

**Purpose:** Sweep the last 60 days of relevant Reddit history to (a) capture pattern intel, (b) find evergreen threads still worth a thoughtful reply, (c) prime the de-dup list, (d) calibrate the voice/intent post-filter.

This is **not** a daily routine. Run it:

- Once now to bootstrap
- Quarterly thereafter for a refresh
- After any major Humla launch (the conversation may shift)

**Execution:** Claude Desktop **Local** Routine on **Weekly** schedule with a built-in skip-guard. The guard makes the routine exit immediately if the last scan was less than 85 days ago, so the effective cadence is quarterly. Local Routines don't expose a Monthly/Quarterly option in the schedule picker, so this is the cleanest workaround.

(Alternative: keep schedule as **Manual** and trigger by hand every ~90 days via a calendar reminder. Both work; pick what fits your workflow.)

---

## Setup in Claude Desktop

1. Routines → New routine → **Local**
2. **Name:** `humla-historical-scan`
3. **Description:** `One-shot 60-day sweep to capture pattern intel, surface evergreen reply candidates, and prime the lead-finder de-dup list.`
4. **Instructions:** the one-liner from the Instructions section below (points at `historical-scan.prompt.md`)
5. **Select folder:** `~/Documents/Development/Claude Code/humla`
6. **Worktree:** off
7. **Ask permissions:** Default
8. **Schedule:** Weekly (the prompt's skip-guard makes the effective cadence quarterly — see Step 0 in the Instructions block below)

---

## Instructions

The actual prompt the routine executes lives in [`historical-scan.prompt.md`](historical-scan.prompt.md).

In Claude Desktop, the routine's Instructions field is:

```
Read marketing/reddit/routines/historical-scan.prompt.md and EXECUTE every step in order. Do NOT paste the file content back as a response — actually run the steps. Start with Step 0 (Quarterly skip-guard); if it says to exit, exit immediately. Today's date is your reference.
```

When tuning the prompt, edit `historical-scan.prompt.md` directly and commit. The routine picks up the new version on the next run; no Claude Desktop re-paste required.


---

## What to do with the output

After the scan completes, Michael's review process:

1. **Read `historical-scan-YYYY-MM-DD.md`.** Bucket A is the actionable shortlist — typically 5–15 evergreen threads worth a thoughtful reply *over the coming 1–2 weeks*, not all at once.
2. **Skim `recurring-asks.md`.** This is the strategic value — it tells you which messages will resonate based on what people actually ask. Use it to:
   - Tune the lead-finder's intent post-filter (add new keywords that match real phrasings)
   - Inform the drafts routine (titles that worked, pain points to lead with)
   - Prioritize the Open Recorder asset library (clip the differentiators that address the most-recurring asks)
3. **Don't act on Bucket A all at once.** Reddit pattern-detection flags accounts that suddenly drop 10 maker comments in a week. Spread across 1–2 weeks, mixed with karma-builder threads.
4. **`_seen-permalinks.txt` is automatic** — the daily lead-finder reads it as part of de-dup so you don't see those threads again unless something changes (e.g., score jumps).

## Cadence

- **Once now** (bootstrap)
- **Quarterly** (every ~90 days, to refresh pattern intel and pick up emerging competitors)
- **After major Humla milestones** (launch, big feature, pricing change) — the audience's questions often shift right after these
