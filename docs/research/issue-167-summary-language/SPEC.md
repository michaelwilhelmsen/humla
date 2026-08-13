# Issue #167 — summary comes back in Norwegian for an English meeting on `Language = Auto-detect`

Implementation spec. Nothing here has been coded; this is the plan to start from
locally. All line numbers are against `cde2e70`.

## The bug, confirmed

The reporter's diagnosis holds on every point I could check by reading:

| Claim | Where | Status |
|---|---|---|
| `auto` → `"the same language as the input"` | `src-tauri/src/presets.rs:14-19` | confirmed |
| `auto` directive → `"Respond in the same language as the user's notes."` | `src-tauri/src/commands/summary.rs:262` | confirmed |
| Norwegian labels + Norwegian `(ingen)` for an empty body | `summary.rs:176-186` | confirmed |
| Whisper's detected language never reaches the summary | repo-wide | confirmed — `detected_language` / `detect_language` / `full_lang_id` have **zero hits** across Rust, TS and Swift |

So on `auto` with no typed notes, every language cue in the prompt is Norwegian:
the directive points at a span whose entire content is the Norwegian word
`(ingen)`, and the block labels around it are `[Notater]` / `[Transkripsjon]`.
The model obliges.

Two things the issue doesn't mention, both worth folding in:

1. **The Norwegian labels leak into the English preset branch too**
   (`presets.rs:44`: `"You produce meeting notes from [Notater] (user-written)
   and [Transkripsjon] (auto)"`). With an explicit `language = "en"` the hard
   `Reply in English` directive overrides them, which is why this only surfaces
   as a bug on `auto`, where the directive is deferential by design.
2. **`DEFAULT_SUMMARY_PROMPT` (`commands.rs:110`) is hardcoded Norwegian and
   contains `Skriv på norsk`.** It is used for the `custom` preset and as the
   fallback when a `custom:<id>` row is missing (`summary.rs:128-139`). That
   prompt summarises in Norwegian for *every* language setting, not just `auto`
   — on `en` it directly contradicts the appended directive. Out of scope for
   #167 but it is the same class of bug and will be reported as its own issue
   unless you want it folded in here.

Blast radius is narrower than it looks: `DEFAULT_LANGUAGE = "no"`
(`commands.rs:64`), so only users who deliberately switched the global setting
to Auto-detect are affected.

## Scope

Two fixes, in this order. Fix 2 is a two-line change that resolves the reported
symptom on its own; Fix 1 is the root cause and takes the `auto` branch out of
the guessing business entirely. Ship 2 first so it can go out without waiting
on 1.

---

## Fix 2 — point the `auto` directive at the transcript

**Files:** `src-tauri/src/commands/summary.rs`, `src-tauri/src/presets.rs`,
`src/lib/presets.ts`

The two directives disagree about what to match — the preset says "the input",
the directive says "the user's notes" — and when notes are empty those are
different spans. Both should name the transcript, which is the one block
guaranteed to hold the meeting's actual language.

1. `summary.rs:262` — replace
   `"Respond in the same language as the user's notes."` with a directive
   anchored to the transcript and explicitly *not* to the surrounding
   scaffolding, e.g.
   `"Respond in the same language as the transcript block, not the language of these instructions or labels."`
   The second clause matters: the frame is Norwegian and the model is otherwise
   free to read that as the target.
2. `presets.rs:15` — `"the same language as the input"` →
   `"the same language as the transcript"`.
3. `src/lib/presets.ts:77` — same change to the `auto` entry of
   `LANGUAGE_LABELS`. This mirror is load-bearing (`CLAUDE.md`: keep
   `presets.rs` and `presets.ts` in sync); the frontend copy is what the
   Settings prompt preview renders.

Deliberately **not** doing the issue's third suggestion (neutralising the
`[Notater]` / `[Transkripsjon]` labels). The labels are referenced by every
preset prompt in both files *and* by user-authored rows in the
`summary_prompts` table — those keep saying `[Notater]` and would stop matching
the user message. Once Fix 1 lands there is no deferential `auto` branch left
for the labels to mislead, so the migration cost buys nothing.

---

## Fix 1 — persist the detected language and resolve `auto` against it

Capture what the STT provider actually detected, store it on the note, and
resolve `auto` → that code before the prompt is built.

### 1a. Carry the detection out of the adapters

`src-tauri/src/stt/adapter.rs:49-53` — add to `TranscribeResult`:

```rust
/// ISO 639-1 code the provider reported detecting, when it reports one at
/// all. `None` on every provider/model combination that doesn't (see the
/// per-provider notes below) — callers must treat absence as normal, not
/// as an error.
pub detected_language: Option<String>,
```

`TranscribeResult` derives `Default`, and the two construction sites
(`stt/openai.rs:73`, `stt/local.rs:68`) plus Deepgram's are explicit struct
literals, so this is a compile-error-guided change with no silent defaults.

Per provider — **coverage is genuinely partial, and the spec should not pretend
otherwise**:

- **`stt/openai_compat.rs`** (serves OpenAI *and* Groq) — add
  `language: Option<String>` to `VerboseResponse` (`:18-23`) and return it.
  Two caveats:
  - `verbose_json` returns the **English language name** (`"english"`,
    `"norwegian"`), not a code. Needs a reverse lookup — add
    `code_for_english_name(&str) -> Option<&'static str>` to
    `src-tauri/src/languages.rs`, matching case-insensitively against the
    existing `LANGUAGES` table. Do **not** reuse `english_name`'s
    `unwrap_or("English")` fallback pattern here; an unknown name must return
    `None`, not silently mean English.
  - `verbose` is only true when `supports_word_timestamps(model)` is
    (`stt/openai.rs:51`), i.e. **`whisper-1` only**. `gpt-4o-transcribe*`
    rejects `verbose_json` and goes down the `PlainResponse` path, which has no
    language field → `None`. Don't flip those models to verbose to get the
    field; ADR 0003 already parked that family.
- **`stt/local.rs`** — the real fix for the reporter's setup (they run local
  Large v3 Turbo). whisper-rs 0.16 exposes
  `WhisperState::full_lang_id_from_state() -> c_int`
  (`whisper-rs-0.16.0/src/whisper_state/mod.rs:333`) and
  `whisper_rs::get_lang_str(id) -> Option<&'static str>`
  (`standalone.rs:46`), which already returns the code (`"en"`). Read it inside
  the `spawn_blocking` closure in
  `local_whisper::transcribe_file_segments` (`local_whisper.rs:427-508`) right
  after `state.full(...)` succeeds, and thread it out through
  `transcribe_file_segments` → `transcribe_file_with_words` → the adapter.
  That means a new return shape for two `pub` fns; a small
  `struct SegmentedTranscript { segments, detected_language }` beats growing
  the tuple, which is already `(String, Vec<Word>)`.
  Note `full_lang_id_from_state` is only meaningful when we passed no
  language (`local_whisper.rs:421` sets `lang = None` exactly on `"auto"`) —
  otherwise it echoes what we forced. Return `None` unless `language == "auto"`
  so we never store an echo as if it were a detection.
- **`stt/deepgram.rs`** — leave at `None` for now. Deepgram only reports a
  detected language when asked with `detect_language=true`, which is a request
  shape change (`:93-100`) and a nested response field, and the reporter isn't
  on Deepgram. Note it as a follow-up rather than half-wiring it.

### 1b. Vote across chunks, don't trust the first

A recording is many chunks and a 2-second "mm-hm" chunk detects as anything.
`ChunkRecord` (`recording.rs:138-144`) already survives to post-stop inside
`chunk_log`, so:

1. Add `detected_language: Option<String>` to `ChunkRecord`, populated in
   `transcribe_chunk` where the result is destructured
   (`commands.rs:5113`) and pushed (`commands.rs:5221-5226`).
2. Add a pure `fn majority_language(chunks: &[ChunkRecord]) -> Option<String>`
   in `commands.rs` beside the other post-stop helpers. Weight each vote by
   chunk text length rather than counting chunks one-for-one — a 40-word chunk
   is far better evidence than a 3-word one, and unweighted counting is exactly
   how a handful of filler chunks would outvote the meeting. Require the winner
   to clear a minimum share (start at 60%) or return `None`; a genuinely
   bilingual recording should decline to answer rather than pick a side.

### 1c. Persist it

New note column, following `expected_speakers` as the template:

- `db.rs` — idempotent `ALTER TABLE notes ADD COLUMN detected_language TEXT`
  beside `:253`; add to `NOTE_COLS` (`:392`), the `Note` struct (`:7-33`), the
  row mapping (`~:2920` — appending keeps the existing positional indices
  stable), and `NotePatch` (`:645-660`). A plain `Option<String>` patch field
  is enough — there's no "clear it back to auto" gesture, so this doesn't need
  `expected_speakers`' double `Option`.
- `src/lib/ipc.ts:13-48` — add `detected_language: string | null` to the `Note`
  type. Additive; no component needs it yet.
- **Write it in `run_post_stop_chain` (`commands.rs:1966`), *not* in
  `diarize_and_apply`.** `diarize_and_apply` returns early when the diarize
  model isn't downloaded (`:2551-2558`) and when there are no chunks
  (`:2544-2547`), so a user without the diarize model would never get a
  detected language. Put it before the `diarize_and_apply` call, off
  `post_stop.chunks`. File import inherits this for free — `finish_import`
  drives the same chain.
- Set it only when it isn't already set, and only from a capture whose resolved
  language was `auto`. A resumed recording appends to an existing note; the
  first real detection should stick.

**Sync:** nothing to do, and that's deliberate. `crates/cloud-sync` builds its
push payload from an explicit field list (`src/lib.rs:488-498`) and reads local
notes with an explicit `SELECT` (`:1153`), so a new column is invisible to the
wire and to pulls without being touched — consistent with ADR 0002's
derived/local-only convention (this is a cache of what the model heard, not a
record). Consequence to accept: a teammate's pulled note has no
`detected_language`, so its `auto` summary falls back to Fix 2's
transcript-anchored directive. That's the correct degradation.

### 1d. Use it

`summary.rs:147-161` — after resolving `language`, if it is `"auto"` and the
note has a `detected_language`, use that code instead. Everything downstream
(`resolve_prompt` → `presets::prompt`, `language_directive`) then takes the
normal explicit-language path and emits a hard `IMPORTANT: Write the entire
response in English.` — no deference to the Norwegian frame at all. The `auto`
branches survive only as the fallback for notes with no detection (pre-change
notes, Deepgram, `gpt-4o-transcribe`, bilingual recordings), which is why Fix 2
is not made redundant by Fix 1.

---

## Tests

**Constraint you should know about before planning the TDD loop: none of the
Rust tests can run in the cloud container this spec was written in.** `cargo
check` fails at `gdk-3.0` (Tauri's Linux backend needs GTK system libs), and
past that `keyring`'s `apple-native` and `whisper-rs`'s `metal` are macOS-only.
`cargo fetch` works, so the dependency graph resolves — but red/green has to
happen on your Mac. Nothing below has been executed.

Pure unit tests, in-crate:

- `languages::code_for_english_name` — `"english"` → `en`, `"Norwegian"` → `no`
  (case-insensitive), `"Klingon"` → `None`. The `None` case is the one that
  matters; it guards against `english_name`'s silent-English fallback being
  copied over.
- `majority_language` — empty input → `None`; unanimous → that code; one short
  stray chunk against a long body → the body's language (this is the
  regression test for the whole feature); an even bilingual split → `None`;
  chunks with `None` detections ignored rather than counted as a language.
- `language_directive("auto")` — asserts the string names the transcript and
  does not name the notes. Cheap, but it's the assertion that pins Fix 2
  against a future well-meaning reword.
- `presets::prompt(preset, "auto")` — contains `"the same language as the
  transcript"`.

Integration-ish, worth the setup:

- A `db` round-trip test for the new column (write via `NotePatch`, read back),
  in the existing `db.rs` test module.
- `openai_compat::transcribe` verbose parsing already has a JSON-fixture
  pattern to copy in `stt/deepgram.rs:193-216` — add a fixture with
  `"language": "english"` asserting `detected_language == Some("en")`, and one
  without the field asserting `None`.

Frontend: `pnpm lint` + `pnpm build` for the `ipc.ts` and `presets.ts` edits.
There's no component behaviour change, so no new vitest.

## Manual verification

The unit tests can't prove the model complies — that needs the reporter's exact
setup, since the whole bug is about how a local Ollama model reads a
Norwegian-framed prompt:

1. Settings → Language = Auto-detect, no per-note override. Summary provider
   Local / Ollama `gemma4:12b-mlx`, thinking off, Meeting preset.
2. Record ~30 s of English, type **no** notes, stop, summarise.
3. Expect an English summary. Check the note row's `detected_language` is `en`.
4. Repeat in Norwegian — must still come back Norwegian (the failure mode of an
   over-corrected fix is forcing everything to English).
5. Repeat step 2 on a note whose `detected_language` is `NULL` (set it back by
   hand) to exercise Fix 2's path alone.

## Suggested commits

1. `fix(summary): anchor the auto language directive to the transcript` — Fix 2
   plus its tests. Independently shippable; closes the reported symptom.
2. `feat(stt): surface each provider's detected language` — 1a + its tests.
   No behaviour change yet.
3. `feat(summary): resolve auto to the recording's detected language` — 1b–1d.
