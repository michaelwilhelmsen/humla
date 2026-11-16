# Frontend (`src/`)

## Stack

React 19, TypeScript, Vite 6, Tauri 2 (`@tauri-apps/api`), React Router 7 (`/note/:id`, settings, home), Zustand, Tiptap 3 (StarterKit + Placeholder + Suggestion + BubbleMenu), react-markdown + remark-gfm, Tailwind v4 (`@tailwindcss/vite`), lucide-react, Radix primitives. Auto-update polls `latest.json` from GitHub releases on launch.

## Stores and events

`useNotesStore` (notes, folders) and `useRecordingStore` (status, errors, diagnostics); backend events are bound once via `bindBackendListeners`: `transcript_replaced`, `summary_ready`, `summary_thinking_delta`, `summary_content_delta`, `summary_status`, `title_status`, `transcribe_status`, `diarize_status`, `recording_status`, `recording_error`, `recording_diagnostic`, `local_whisper_progress`, `local_whisper_download_error`, `diarize_download_progress`.

On `transcribe_status` (`step` / `index` / `count` / `doneMs` / `totalMs` / `take` / `takes`), a measure naming a step **replaces** the run's measure while a bracket merges into it, so a counter can't survive into a step that counts nothing.

## Anchored panels: `src/components/ui/`

Every anchored panel goes through one of four wrappers that dress Radix in Humla's tokens and share one surface look via `surface.ts`. Don't hand-roll another popover.

- `Menu` — a list of choices. Defaults to `modal={false}`.
- `Popover` — arbitrary content.
- `Select` — a value picker. Radix `Select` is modal: the page behind is inert and aria-hidden, so tests must press outside rather than click through.
- `Combobox` (#116, built on `Popover`) — when the user must type a value not in the list. Focus stays in the input; the highlight is `aria-activedescendant`.

Not shadcn: its token layer (`--background`, `--primary`, …) is deliberately absent. jsdom needs the `ResizeObserver` / `scrollIntoView` / pointer-capture shims in `src/test/setup.ts`.

## Themes

Two independent axes, both attributes on `<html>`: `data-palette` (which design) × `data-theme` (`light` / `dark`, absent = follow the OS).

- Two designs: **warm** (Hanken Grotesk, cream, gold) and **graphite** (SF Pro, white, DM Mono commands, compact ink chrome). Both carry both modes; a derived mode is marked as derived in its file.
- The stored setting is `palette`; an unknown value falls back to `warm`, which is why `warm.css` alone also matches `:root:not([data-palette])` (withdrawn ids `nothing` and `ember` resolve through it; pinned in `palette.test.ts`).
- **A theme owns typography and metrics, not just colour.** The contract is `src/lib/themeContract.ts` (SHAPE declared once, COLOUR in all three blocks); `src/styles/themes/themes.contract.test.ts` names every missing token, requires the two dark blocks to be identical, and rejects component rules in theme files.
- Adding a theme: one CSS file + one `PALETTE_REGISTRY` row + one `@import`. See `src/styles/themes/README.md`.
- Structural CSS is in `src/styles/globals.css`, tokens in `src/styles/themes/*.css`. Base resets are in `@layer base`. `globals.css`'s `@theme` block only feeds Tailwind's utilities (`font-sans`, `font-code`, `rounded-card`); an unlayered theme file overrides it.

### Warm

- One typeface, Hanken Grotesk; hierarchy from size and weight only. `--font-serif` / `--font-mono` alias `--font-sans`; real code uses `--font-code`. No serif headlines, no uppercase labels (sentence case).
- Accent is Humla gold `--color-accent: #ffdc6c` in both modes, `--color-on-accent` for glyphs on it. Text, icon and link uses take `--color-accent-text` (`#7a5a12` light / `#f3d178` dark). Filled accent is reserved for `.nd-btn-primary`; selected states use `--color-accent-soft` + `--color-accent-text`.
- Red is never the accent: `--color-record` is the recording dot, `--color-danger` is destructive. A warm-accent theme must push danger away from it by hue.

### Tokens and utilities

- Page background is `--color-canvas`, not `--color-bg`. `--color-pill` is transparent by design; use `--color-pill-hover` for a fill.
- Speaker colours: `--color-interactive` / `--color-success` / `--color-warning` / `--color-speaker-4` (a dedicated red, not `--color-accent`), cycling for 5+.
- Layout: nav and right panel are inset rounded cards on the canvas (~6px gutter, no borders); traffic lights sit inside the nav card. A grid view's `.nd-well` makes `<main>` a third card. Dark mode elevation: canvas → cards → pills.
- Unlayered utilities: `.nd-btn` / `.nd-btn-primary` / `.nd-btn-icon` / `.nd-btn-icon-sm` (radius `--control-radius`), `.nd-chip`, `.nd-meta` (+ `.is-interactive`), `.nd-label`, `.nd-bare`, `.nd-recpill`, `.nd-speaker-pill`, `.nd-speaker-dot`, `.nd-word` / `.nd-word-active`.
- In `@layer components` (so a Tailwind utility can nudge them): `.nd-title`, `.nd-heading`, `.nd-navrow` (+ `.is-active`), `.nd-bar`, `.nd-badge`, `.nd-code`, `.nd-list` / `.nd-row` / `.nd-row-lg`, `.nd-well` / `.nd-well-bar` / `.nd-notegrid` / `.nd-notecard`.
- **Deleted, don't reference**: `.nd-action`, `.nd-prop-*`, `.nd-select`, `.nd-ctl`, `.rec-bar`.
- `.nd-meta` is the one picker-trigger chip: borderless, `.is-interactive` for hover, `.is-filled` for a resting `surface-2` fill where it stands alone on a card.

## Note view

- Two columns: scrollable body + a toggleable, resizable right panel with tabs **Summary | Transcript | Chat**. Width and open state are kept in `localStorage` (`humla.panelWidth` / `humla.panelOpen`, `src/lib/notePanel.ts`); a recording's auto-open isn't saved.
- The panel clamp respects `BODY_MIN` so the body column keeps room for its toolbar; `PANEL_FLOOR` sits below `PANEL_MIN` for windows too narrow for both.
- A compact meta bar under the title carries identity only (author · workspace · date · folder · sync). Generation settings live in the panels (preset + provider in Summary, language + speakers in Transcript).
- **`NoteToolbar` degrades by container query** (`@container`, three steps: back label tightens → action labels drop → back label goes), because `.nd-btn` is `flex-shrink: 0; white-space: nowrap`. Thresholds are against the row's content box (padding excluded), so one set covers both sidebar states. They come from `scripts/measure-toolbar.js`; re-derive after changing the row or a theme's control metrics.
- **The recording bar degrades the same way** (#177), by `ROW_STEPS`, in order: the diagnostics pill's seconds and chunk count (kept in its `title`), the word PAUSED, the diagnostics pill, "Summarizing…" down to its spinner, the frozen elapsed reading after stop (#182), and last the progress pill's unit counter (`counter`). Two arrangements (with and without a summary pill); each step fires earlier in the tighter one, pinned by an ordering test. Measured with `scripts/measure-recording-bar.js` (tightest slack 14px).
- The stop of a capture with **Transcribe manually** on draws nothing, timer included; `recording_status.deferred` is the discriminator, not an empty drain.
