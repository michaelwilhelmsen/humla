You are running the Humla weekly draft routine (Friday edition).

Goal: Take Monday's research and produce 1 publishable Reddit post draft, ready for Michael to review, edit, and ship next week.

Use the `marketing/reddit/lib/fetch.py` helper for any verification searches needed (`browse`, `search`, `search-sub`, `tree`). See the Monday section for the command surface.

Steps:

1. Read this week's research file in marketing/reddit/research/YYYY-Www.md (find latest by listing the directory).
2. Pick the recommended topic (or pivot if something more timely emerged this week — quick search to verify).
3. Decide target sub. Verify rules from marketing/reddit/README.md:
   - r/macapps: needs 10 local karma. Use [App] flair. Disclose maker. Open source? Prefix [OS].
   - r/SideProject: any time, lenient.
   - r/LocalLLaMA: must be hand-written, 1/10 rule. Likely skip if drafting AI-assisted.
   - r/MacOS: Saturday UTC only.
   - r/AiNoteTaker: niche but high intent. Disclose.
   - r/ClaudeCode: natural fit (Humla built with Claude Code).
4. Draft the post:
   - Title: ≤80 chars, hooks with the differentiator (parallel mic+sys, offline diarization, no bots, BYO key)
   - Body: 200–500 words, plain prose, no bullet-point soup, no emoji, no marketing copy
   - Lead with the problem
   - Specific technical detail in the first 2 sentences
   - Honest about what Humla isn't (macOS-only, requires API key for cloud option, etc.)
   - End with a soft CTA (link to humla.no, open to feedback)

Critical:
- This is a DRAFT for Michael to review. He will rewrite in his voice. Don't ship-ready.
- Mark all assumptions with [TODO: verify] inline.
- No claims about benchmarks, accuracy, or comparisons unless Michael has measured them.
- Specify the Open Recorder clip(s) to include with the post.

Output: Write to marketing/reddit/drafts/YYYY-Www-[slug].md:

---
target_sub: r/...
flair: ...
publish_window: [date or "any"]
status: draft
asset_clips:
  - [clip filename]: [what it shows]
---

# Draft: [working title]

## Title
[≤80 chars]

## Body

[200–500 words plain prose]

## Open Recorder shot list

For this post, record the following clips with Open Recorder (~/Applications/Open Recorder.app or equivalent):

1. **[clip name]** — Setup: [what to show]. Duration: ~15s. Export as: GIF if <8MB else MP4. Save to `marketing/reddit/intel/assets/[slug].gif`.
2. ...

Tips for these clips:
- Use ScreenCaptureKit native capture (default on macOS, hides cursor cleanly)
- Keep smart zoom ON for cursor tracking
- Cursor smoothing ON
- Solid neutral background (#0F0F0F or wallpaper) for upload-friendliness
- Trim aggressively in the editor — Reddit attention span is 5s

## Notes for Michael
- [voice/tone note]
- [things to verify]
- [things to remove if too marketing-y]
- [alternative angles to consider]

## Self-promo disclosure
"I'm the dev — happy to answer questions"-style line to include in body or first comment.

End. Save the file.
