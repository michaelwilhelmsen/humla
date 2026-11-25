# Notes, titles and the transcript reader

## The timeline is canonical (#169, [ADR-0004](../adr/0004-the-timeline-is-canonical-for-a-notes-content.md))

The styled reader renders from the merged timelines, so transcript text no timeline accounts for is invisible there, and the first `rebuild_note_transcript` (cycling a label, deleting a chunk, re-diarizing, unifying) deletes it from the transcript, summary input, chat retrieval and embeddings. Three defences:

- **Prevention.** `diarize_and_apply` doesn't return early when the diarize model is missing; it serializes the chunks with an empty label (word timings kept), so the take still writes a session that a later re-diarize can attach to.
- **Repair on open.** `note_timeline_repair` compares `comparable_words` of the transcript with the timelines' projection; `orphaned_prefix` recovers leading lines the projection misses and `synthesize_orphan_timeline` writes them as a session at **manifest index 0**, named "Earlier transcript" (never "Recording 0"). Idempotent; it runs after the workspace session pull. A note with no timeline at all is left alone (it keeps the free-text editor).
- **Render-time guard.** When `coversTranscript` is false, the note falls back to `TranscriptView` over the whole string. The comparison is the backend's; the client never re-derives the grouping rule (Rust groups on label alone, `groupTimeline` on label *and* session). Compare word sequences, never line counts.

`split_label` (Rust) and `parseTranscriptLines` (`Note.tsx`) are the same "`Label: ` prefix" rule; change both. `commit_rebuilt_transcript` refuses to commit an empty projection over a non-empty transcript.

## Editing goes through the timeline (#170)

- On a note with a timeline, editing is per turn: a hover pencil opens the turn, and commit calls `note_timeline_set_chunk_text` with the turn's chunk indices (`TimelineGroup.indices`). It writes the text into the lowest index, empties the rest and re-derives the transcript, like `note_timeline_set_chunk_label` and `note_timeline_delete_chunk`.
- An edited turn drops its word timings and keeps its bounds (it still highlights during playback).
- A note with no timeline keeps the whole-transcript textarea (`TranscriptEditor`).
- Both are locked while a recording is in flight.

## Transcript view

- In the right panel's Transcript tab, styled by default with `white-space: pre-wrap`.
- **A turn is titled with its speaker** (#176): a coloured dot, the name and (in the timeline reader) its time in its own take, with the text full width beneath. Titled at the start of a run of consecutive turns by one speaker. The name is the identity; colour (four, cycling) is the scanning aid.
- Both readers do this — `TranscriptPlayer` (timeline) and `TranscriptView` (fallback, mostly a teammate's note) — through one component, `TurnTitle`. Run rules differ by one thing: `groupTimeline` splits on label and session, the fallback on label alone.
- An unlabelled turn gets no title; in the player it keeps the inline row (the pencil and delete live on the title line).
- `SpeakerLabels` chip strip: derives labels from the transcript on every render; rename is a line-anchored regex rewrite (`/^Speaker N: /gm` → `/^Michael: /gm`), no metadata table.

## Clean view (#217)

- **A view, never a second transcript.** The Transcript tab's Clean | Verbatim switch (`transcript_view`, default Clean; shown while `clean_transcripts` is on) leaves out the words a clean pass marked hidden, and a turn with nothing left. The timeline and `notes.transcript` stay verbatim; summary, chat, export, MCP and webhook read verbatim.
- **The model only answers with a mask.** `commands/clean.rs` sends windows of about 160 numbered words (turns kept whole where they fit) to the note's summary provider through `resolve_provider` (so a governed note is refused like its summary), thinking off, an OpenAI reasoning model swapped for `TITLE_FALLBACK_MODEL`. The answer is `{"hide": [numbers]}`; it can't add or change a word, so every word shown keeps its timing and highlighting works in Clean.
- **Guards after the answer:** numbers and a short list of negations are never hidden; an entry is hidden whole only at three words or fewer; a turn of eight words or more that would lose over 40% keeps them all. An unusable answer leaves its window verbatim.
- **Stored per take in `clean.json`**, local to this Mac (derived, never synced): entry fingerprint (`clean::fingerprint`, FNV-1a over the entry's words) → hidden word indices. `note_timeline` attaches the mask matching each entry's current words as `hidden`; a renamed speaker keeps it, an edited or re-split entry loses it and shows verbatim until cleaned again.
- **When it runs:** spawned at the end of the post-stop chain and of a deferred transcription (it waits for the note to stop being busy), and from the reader when Clean is shown with entries lacking a mask (once per note and coverage per app session). "Clean again" re-reads every entry. Failure on the automatic path is silent; the switch's caption names the reason. Progress on `clean_status`. The pass ends with `purge_note_assets_if_gone`.
- **Display:** `showWords` (`src/lib/cleanView.ts`) capitalises a word that a hidden run left opening a sentence and moves a sentence end a hidden word carried onto the word before it. Per-turn editing is Verbatim's.

## Automatic titles (#90)

- A note whose title Humla owns gets a short phrase from its content. `menubar::is_replaceable_title` is the single answer to "may this title be replaced?": empty, a `Recording 19 Aug 14:32` timestamp, or `Imported audio`. A real filename stem is never touched. Mirrored in TS as `isReplaceableTitle` (`src/lib/noteSync.ts`); change both.
- Every trigger reads the note through `title_input`: head of the typed notes, the transcript via `sample_transcript` (whole up to 3,000 chars, otherwise six windows from the middle of six equal stretches), and the summary when one exists.
- Two triggers: the post-stop chain, spawned **concurrently with the diarize pass** (the transcript is final once `recording_stop` drains), and a debounced body-settled checkpoint for a typed note that was never recorded. It runs after `record_detected_language`. Not on summary completion and not during a recording.
- Reuses `resolve_provider`, forces `think: false`, substitutes a small non-reasoning model when the summary model reasons, and doesn't stream.
- Failure is silent on the automatic path and loud on ⋯ → **Regenerate title**, which bypasses eligibility. That item is disabled while the note has nothing to read, while a capture or deferred transcription is changing the transcript, and while a title is being written.
- One-shot: once written, a title is no longer replaceable.
- The call is bracketed by a per-note `title_status` event; the open note shows a shimmer in place of the title.

## Note grid and folders

- All notes and Folder render `NoteCard` in `.nd-notegrid`, with no date headings; the card carries its date (`formatNoteDate`). Card: date, title, four-line excerpt, then state · client · folder.
- **Excerpt** (`noteExcerpt`) prefers the summary's opening line, then typed body, then the transcript with speaker labels stripped. Horizontal rules, model preamble ("Her er …", lines ending in `:`) and fragments under 20 chars are skipped (the first kept only when nothing stronger exists); a fragment body yields to the transcript.
- **Filters on four axes**: status, Client, who recorded it, and a **No folder** toggle. A Folder view renders the same bar with that toggle off (`showUnfiled`). A folder's filter is cleared when the folder changes.
- **The status filter asks what a note has, so its rungs overlap** (`hasStatus`); the card's dot is an exclusive ladder (`noteState`). Separate label maps: `NOTE_STATE_LABEL` / `NOTE_FILTER_LABEL`. Don't read the filter off the ladder: Recorded would then match nothing on default settings.
- `recorded` comes from `notes_with_recordings` (a manifest entry via `resolve_sessions`, not the audio), kept in `useNotesStore.recordedNoteIds` and refreshed when a capture reaches idle and when a replay finishes.
- The owner filter renders only in a workspace with more than one member; "Me" includes notes whose `owner` is empty.
- Multi-select (#19): Cmd/Shift-click, Space, Esc and a checkbox in the card's top-right corner.
- `.nd-well` darkens the scroll region, because the card's edge is only tone (graphite's canvas and surface are both `#ffffff`). Selection is the accent ring, not a fill (`--color-accent-soft` can land darker than the well). Radius from `--radius-card`, shadow from `--shadow-card`. Under `prefers-contrast: more` the card takes a border and drops the shadow.
- `.nd-notegrid`'s track is `minmax(min(var(--card-min, 300px), 100%), 1fr)`; the `min()` stops it overflowing when the column is narrower than one card.
- Folders: a flat list with per-note assignment; sidebar search matches titles, bodies and transcripts and shows each hit's folder.
