# Humla UI/UX redesign — implementation brief

## Goal
Refactor the Humla frontend to the new design. This is a **visual + structural UX refactor only** — preserve every existing behavior (recording, transcription, diarization, summarization, cloud sync, version history, playback, speaker rename, read-only / recording-lock / pending-sync states). **No backend, IPC, or schema changes.**

## Reference — source of truth for visuals
- **`design/redesign-prototype.html`** — a clickable prototype of the target. It has a top-right **Screen** switcher (Note / Recording / All notes) and a **Theme** toggle (Light / Dark). This is the spec for layout, spacing, and structure.
  - To view: `agent-browser open "file:///Users/michaelwilhelmsen/Documents/Development/Claude Code/humla/design/redesign-prototype.html"` then `agent-browser screenshot /tmp/x.png` and read the image. Use `agent-browser eval "..."` to flip theme/screen (see the segmented-control click handlers in the file).
  - Translate its plain CSS into the app's **Tailwind v4 + `--color-*` tokens + `.nd-*` utilities** — do **not** paste raw prototype CSS into the app.
- Read **`CLAUDE.md`** (architecture) and **`src/styles/globals.css`** (current tokens, fonts, `.nd-*` utilities) before touching anything.

## The design, decided

### 1. Typography — ONE typeface
- Standardize on **Hanken Grotesk** everywhere. Bundle via `@fontsource/hanken-grotesk` (follow the existing font-import pattern — check `main.tsx` / `globals.css`).
- **Remove** Instrument Serif (serif titles), Space Grotesk (body), Space Mono (ALL-CAPS labels). **No serif headlines, no mono.**
- Hierarchy from **size + weight only**. Labels are **sentence case**, not uppercase mono (update the `.nd-label` utility and any section labels).
- Note title ≈ 30px / 600 / -0.022em tracking (was `text-5xl font-serif`).
- Numbers / timers / counts: Hanken + `font-variant-numeric: tabular-nums` (drop `--font-mono`).

### 2. Color / accent — ink, not red
- **The accent becomes ink/black, not red.** ⚠️ Today `--color-accent` *is* red, so its meaning changes — **audit every `--color-accent` usage** and split it:
  - primary action / document avatar / summary bullets / active states → **ink accent** (new value)
  - record dot / "recording" state → a dedicated **red** token (`--record` / keep red)
  - 4th speaker color (currently `--color-accent`) → a dedicated **speaker-red** token
- Light: ink accent `#1c1a14`, white text on filled. Dark: accent **inverts to light ink `#ece5d6`** with dark text, so filled buttons stay visible on the dark canvas. Red stays red in both, reserved **only** for the record dot / recording indicators.
- Keep the warm paper palette in light.

### 3. Dark mode — elevation ladder (this was a real fix)
Dark must read as three distinct layers:
- **canvas** (darkest — center writing area + gutter)
- **cards** (nav + right panel, clearly lighter) with a **visible** warm border
- **pills / buttons** (lighter still) sitting on the cards

The current dark mode has the nav *darker* than the canvas with near-invisible borders, so everything blends — don't reproduce that.

### 4. Layout — inset rounded cards
- Left nav and right context panel are **inset rounded cards** (radius ~14px) floating on the canvas with an equal ~6px gutter on all sides.
- macOS **traffic lights sit inside the nav card** (top-left); nav content is pushed down (~34px top padding) to clear them. Cards are pulled to the top with **equal margin top and bottom**.

### 5. Left nav — grouped (`Sidebar.tsx`)
Currently a flat list. Group with sentence-case section labels + hairline dividers:
workspace switcher → search → **primary** (Home, All notes) → divider → **Folders** section (label + "＋ new") → divider → **pinned footer** (Trash, Settings, account row).

### 6. Note meta bar — replaces the `.nd-prop-table` (`Note.tsx`)
- Replace the 9-row property table with **one compact inline meta bar** under the title. No separator dots — icons + ~7px gap separate items.
- Meta = **note identity only**: author (round avatar + name) · **workspace** (square avatar + name, immediately after author) · date · folder · sync status. Use context-correct icons (calendar, folder, check-circle for synced).
- **Move out of the meta bar** (they are generation settings, not identity):
  - **preset** → Summary panel
  - **language** + **expected speakers** → Transcript panel
- Preserve the underlying fields + handlers (`folder_id`/`moveNote`, `workspace_id`/`setNoteWorkspace`, `summary_preset`, `language`, `expected_speakers`, `summary_provider`) — only the control's *location* changes.

### 7. Right context panel — the major structural change (`Note.tsx`)
- Today Summary + Transcript render as collapsible **cards stacked below the body**, pushing it down. Move them into a **toggleable right sidebar** with tabs: **Summary | Transcript** (drop the Audio tab — playback lives on the transcript scrubber).
- Restructure the Note view into: **toolbar → two-column flex [ scrollable body | right panel ]**. The toolbar gets a panel-toggle button; closing the panel widens the body (focus-writing mode).
- **Summary tab**: leads with two dropdown chips — **preset** + **provider** (Cloud/Local) — then copy / regenerate, then the summary body. Preserve streaming, the reasoning-trace panel, copy, and regenerate.
- **Transcript tab**: leads with two dropdown chips — **language** + **speakers** — then the playback scrubber, speaker pills (inline rename), and turns. Preserve diarization labels/rename, the playback timeline, rediarize, and click-to-edit.
- **Toolbar**: clear **Record** + **Summarize** buttons (Summarize = ink primary) + panel toggle + overflow menu (delete, etc.), replacing the cryptic copy/refresh/more icon row.

### 8. Body compaction (prose)
Note body ≈ 15.5px / line-height **1.5** (was 1.68) / paragraph margin ~0.68em (was 1em) / headings ~19px / 600.

### 9. Recording mode (`RecordingBar.tsx`)
- Floating bar = **two pills**: (1) status `● mic Xs · ● sys Xs · N chunks` (green mic dot, gray sys dot); (2) **red-outlined** timer/controls pill `● 0:48 │ ⏸ │ ⏹` (stop = red square), segmented with thin dividers. **No meter/equalizer visualizer.**
- Meta bar swaps the sync chip for **`● Recording`** (red, pulsing).
- Live transcript (in the panel) header: **`● Recording  mic Xs · sys Xs · N chunks  0:48`** (no visualizer). Live lines are **plain text with no speaker labels** (labels are applied post-stop — keep that). Hint line: "Speakers are identified when you stop."

### 10. All notes (`AllNotes.tsx`)
- Header: title + count + sort/view icon buttons + **New note** button; filter chips (All / Recorded / Shared / No folder).
- **Grouped by time** (Today / Yesterday / Earlier this week / …).
- Rows: leading icon (**mic** = recorded, **doc** = typed-only), title, one-line preview, trailing **date + folder chip + duration chip**.

### 11. Speaker pill colors (unchanged semantics — 4, cycling)
blue / green / gold / red — light `#2f6fb0 / #5b8a45 / #bd962f / #c0463f`, dark `#6aa3e0 / #8cc06f / #ddb255 / #e07a72`.

## Target tokens (from the prototype)

**Light**
| role | value |
|---|---|
| canvas (page bg → `--color-canvas`) | `#faf8f2` |
| sidebar / card bg (nav + panel) | `#f1ece1` |
| surface (pills, content panel) | `#fffef9` |
| surface-2 | `#f4efe4` |
| raised (hover, avatars) | `#ebe5d6` |
| text | `#211e17` |
| text-muted | `#6d695e` |
| text-faint | `#a7a08f` |
| line | `#e9e3d4` |
| line-2 | `#ddd5c2` |
| accent (ink) / accent-text | `#1c1a14` |
| accent-soft | `#e8e3d6` |
| record (red) | `#cf352c` |

**Dark**
| role | value |
|---|---|
| canvas | `#0e0c08` |
| sidebar / card bg | `#1d1912` |
| surface (pills) | `#2a2418` |
| surface-2 | `#241f16` |
| raised | `#342d20` |
| text | `#ece5d6` |
| text-muted | `#9c968a` |
| text-faint | `#6a655b` |
| line | `#322c22` |
| card border | `#423a27` |
| accent (light ink) / accent-text | `#ece5d6` |
| accent-soft | `#352f23` |
| record (red) | `#e5544b` |

(Map these onto the app's existing `--color-*` names — read `globals.css` for the exact names. Filled-accent elements need dark text in dark mode, e.g. `#1a1407`.)

## File map
| File | Work |
|---|---|
| `src/styles/globals.css` | tokens, fonts, `.nd-*` utilities (Phase 1) |
| `src/components/Layout.tsx` | shell: inset cards + right-panel region (Phase 2/4) |
| `src/components/Sidebar.tsx` | grouped nav + inset card + traffic lights inside (Phase 2) |
| `src/pages/Note.tsx` | meta bar, right panel, toolbar, prose — **largest change** (Phase 3/4/5) |
| `src/components/RecordingBar.tsx` | two-pill bar (Phase 6) |
| `src/pages/AllNotes.tsx` | grouped list rows (Phase 7) |
| `src/pages/{Home,Folder,Trash}.tsx`, `src/pages/Settings.tsx` | apply the design language (Phase 8, follow-up) |

## Phased plan (build + verify after each phase, in BOTH themes)
1. **Tokens + typography** (`globals.css`) — Hanken everywhere, ink accent + dark inversion, dark elevation ladder, card radii/border tokens. App should still render; spot-check a note light + dark.
2. **Left nav** — inset card, traffic lights inside, grouped sections, pinned footer.
3. **Note meta bar** — compact inline bar; add workspace; relocate preset/language/speakers out (wire them into the panel in Phase 4).
4. **Right context panel** — restructure Note into two columns; Summary/Transcript tabs; move preset→Summary, language+speakers→Transcript; remove the below-body cards and the Audio tab.
5. **Body compaction** — prose sizing.
6. **Recording bar** — two-pill layout; live-transcript header in the panel.
7. **All notes** — grouped list.
8. **Follow-up** — Home / Settings / Folder / Trash to match.

## Constraints / gotchas
- **Front-end only.** No backend/IPC/db edits. Preserve all states: `readOnly`, recording lock (`lockedBy`), pending sync, summarizing, diarizing, view-only banners.
- **Both themes must work** — verify light and dark every phase.
- Tauri webview **blocks `window.confirm/prompt/alert`** — use inline UI / the `Modal` component.
- The page bg token is **`--color-canvas`** (not `--color-bg`).
- pnpm: use **`~/.volta/bin/pnpm`** (10.x); the PATH `pnpm` is 9.2.0 and errors on a store mismatch.
- Verify with **`pnpm tauri dev`** (sidecars already built) or `pnpm dev` (frontend only). Exercise: note view, start/stop a recording, all-notes — in light and dark.
- Don't regress the memory'd fixes (e.g. transcript view height matching the textarea to avoid page-jump on click-to-edit; StrictMode-safe `listen()` cleanup).
