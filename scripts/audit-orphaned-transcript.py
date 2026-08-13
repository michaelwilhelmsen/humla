#!/usr/bin/env python3
"""Report notes whose transcript holds text no session timeline accounts for (#169).

Strictly read-only: opens the SQLite DB with `mode=ro` and never writes to
either the DB or the recordings tree. Safe to run while Humla is open.

The check mirrors the backend's `rebuild_note_transcript`: group each session's
`timeline.jsonl` into transcript lines (consecutive same-label entries join with
a space, a label change starts a new line), join the sessions in manifest order,
and compare the result to `notes.transcript`. Anything in the transcript that the
projection cannot account for is text the styled reader hides and the next
rebuild -- cycling a speaker label, deleting a chunk, re-diarizing, unifying
sessions -- deletes outright.

Comparison is on normalised word sequences, never on lines or characters:

  * Speaker labels are stripped from both sides. A cross-note rename rewrites
    `notes.transcript` but not the timeline, so comparing labels would flag every
    renamed note as an orphan.
  * The two grouping rules differ by design -- the backend groups on label alone,
    the frontend also on session -- so line counts legitimately disagree.

Usage:
    python3 scripts/audit-orphaned-transcript.py            # summary + affected notes
    python3 scripts/audit-orphaned-transcript.py --verbose  # list every note
    python3 scripts/audit-orphaned-transcript.py --quiet    # counts only, no text
    python3 scripts/audit-orphaned-transcript.py --json     # machine-readable
    python3 scripts/audit-orphaned-transcript.py --data-dir /path/to/no.humla.app
"""

from __future__ import annotations

import argparse
import datetime
import difflib
import json
import os
import re
import sqlite3
import sys
from pathlib import Path

DEFAULT_DATA_DIR = Path.home() / "Library/Application Support/no.humla.app"
LEGACY_SESSION_ID = "legacy"
# Any of these in the recordings dir root means a pre-sessions note whose assets
# sit flat, which resolves to one synthesized legacy session.
FLAT_ASSET_FILES = ("timeline.jsonl", "playback.wav", "mic.wav", "sys.wav", "chunks.json")

# A leading `Some Name: ` on a line. Bounded and colon-anchored so it cannot eat
# a sentence that merely contains a colon.
LABEL_PREFIX = re.compile(r"^[^:\n]{1,60}:[ \t]+")
WORD = re.compile(r"[0-9a-zæøåüöäéèêàôçñ]+")

# A run this short is punctuation drift or a one-word disfluency, not lost text.
MIN_INTERESTING_RUN = 3


def normalise(text: str) -> list[str]:
    """Transcript text -> comparable word sequence, labels and punctuation gone."""
    words: list[str] = []
    for line in text.splitlines():
        line = LABEL_PREFIX.sub("", line.strip())
        words.extend(WORD.findall(line.lower()))
    return words


def as_date(value) -> str:
    """created_at is epoch milliseconds; render it as a plain date."""
    try:
        return datetime.datetime.fromtimestamp(int(value) / 1000).strftime("%Y-%m-%d")
    except (TypeError, ValueError, OSError):
        return str(value or "")[:10]


def resolve_sessions(recordings_dir: Path) -> list[tuple[str, Path]]:
    """Ordered (session_id, asset_dir) pairs. Mirrors sessions::resolve_sessions."""
    manifest_path = recordings_dir / "sessions.json"
    if manifest_path.is_file():
        try:
            manifest = json.loads(manifest_path.read_text())
        except (OSError, json.JSONDecodeError):
            manifest = None
        if manifest:
            entries = manifest.get("sessions") or []
            pairs = [
                (str(e.get("id", "")), recordings_dir / str(e.get("id", "")))
                for e in sorted(entries, key=lambda e: e.get("index", 0))
                # Same trust boundary as the Rust side: an id is joined onto a
                # path, so anything that isn't a plain segment is dropped.
                if e.get("id") and "/" not in str(e["id"]) and str(e["id"]) not in (".", "..")
            ]
            if pairs:
                return pairs
    if any((recordings_dir / f).exists() for f in FLAT_ASSET_FILES):
        return [(LEGACY_SESSION_ID, recordings_dir)]
    return []


def group_timeline(path: Path) -> tuple[str, int, int]:
    """One session's timeline -> (transcript contribution, entries, malformed lines).

    Mirrors `group_values_to_transcript`: consecutive same-label entries join
    with a space, a label change starts a new line, empty text is skipped.
    """
    try:
        content = path.read_text()
    except OSError:
        return "", 0, 0
    out: list[str] = []
    last_label: str | None = None
    entries = malformed = 0
    for line in content.splitlines():
        if not line.strip():
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            # parse_session_timeline skips these silently, which is one way a
            # timeline ends up present but short.
            malformed += 1
            continue
        entries += 1
        label = str(entry.get("label") or "")
        text = str(entry.get("text") or "").strip()
        if not text:
            continue
        if last_label != label:
            if out:
                out.append("\n")
            if label:
                out.append(f"{label}: ")
            last_label = label
        elif out:
            out.append(" ")
        out.append(text)
    return "".join(out), entries, malformed


def project(recordings_dir: Path) -> tuple[str, dict]:
    """The transcript the timelines can account for, plus per-note stats."""
    sessions = resolve_sessions(recordings_dir)
    parts: list[str] = []
    stats = {"sessions": len(sessions), "timelines": 0, "entries": 0, "malformed": 0}
    for _, session_dir in sessions:
        timeline = session_dir / "timeline.jsonl"
        if not timeline.is_file():
            continue
        stats["timelines"] += 1
        part, entries, malformed = group_timeline(timeline)
        stats["entries"] += entries
        stats["malformed"] += malformed
        if part.strip():
            parts.append(part)
    return "\n".join(parts), stats


def missing_runs(transcript_words: list[str], projection_words: list[str]) -> list[tuple[int, list[str]]]:
    """Runs of transcript words the projection has no counterpart for."""
    matcher = difflib.SequenceMatcher(None, transcript_words, projection_words, autojunk=False)
    runs: list[tuple[int, list[str]]] = []
    for tag, i1, i2, _j1, _j2 in matcher.get_opcodes():
        if tag in ("delete", "replace"):
            run = transcript_words[i1:i2]
            if len(run) >= MIN_INTERESTING_RUN:
                runs.append((i1, run))
    return runs


def audit(data_dir: Path) -> list[dict]:
    db_path = data_dir / "notes.sqlite"
    if not db_path.is_file():
        sys.exit(f"no database at {db_path}")
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        "SELECT id, title, transcript, created_at FROM notes "
        "WHERE deleted_at IS NULL AND transcript IS NOT NULL AND transcript <> '' "
        "ORDER BY created_at"
    ).fetchall()
    conn.close()

    results = []
    for row in rows:
        recordings_dir = data_dir / "recordings" / row["id"]
        projection, stats = project(recordings_dir)
        transcript_words = normalise(row["transcript"])
        projection_words = normalise(projection)
        runs = missing_runs(transcript_words, projection_words) if projection_words else []
        results.append(
            {
                "id": row["id"],
                "title": (row["title"] or "").strip() or "(untitled)",
                "created_at": as_date(row["created_at"]),
                "transcript_words": len(transcript_words),
                "projection_words": len(projection_words),
                "missing_words": sum(len(run) for _, run in runs),
                "runs": runs,
                # Leading orphan = the #169 shape: a prior-take snapshot the
                # timelines never covered, sitting at the front of the transcript.
                "leading_orphan": bool(runs) and runs[0][0] == 0,
                **stats,
            }
        )
    return results


def classify(note: dict) -> str:
    if note["timelines"] == 0:
        # No timeline at all -> the reader falls back to plain text, so nothing
        # is hidden. Still worth counting: a rebuild would empty the transcript.
        return "no-timeline"
    if note["missing_words"]:
        return "orphaned"
    return "ok"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--data-dir", type=Path, default=DEFAULT_DATA_DIR)
    parser.add_argument("--verbose", action="store_true", help="list every note, not just affected ones")
    parser.add_argument("--quiet", action="store_true", help="counts only; never print transcript text")
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    args = parser.parse_args()

    notes = audit(args.data_dir)
    for note in notes:
        note["status"] = classify(note)

    if args.json:
        print(
            json.dumps(
                [
                    {k: v for k, v in note.items() if k != "runs"}
                    | ({} if args.quiet else {"first_missing": " ".join(note["runs"][0][1][:20]) if note["runs"] else None})
                    for note in notes
                ],
                indent=2,
            )
        )
        return

    orphaned = [n for n in notes if n["status"] == "orphaned"]
    no_timeline = [n for n in notes if n["status"] == "no-timeline"]
    malformed = [n for n in notes if n["malformed"]]

    print(f"{len(notes)} notes with a transcript, under {args.data_dir}\n")

    if orphaned:
        print(f"ORPHANED TEXT -- {len(orphaned)} note(s). Hidden in the reader; the next")
        print("rebuild (cycle a speaker label, delete a chunk, re-diarize, unify) deletes it.\n")
        for note in sorted(orphaned, key=lambda n: -n["missing_words"]):
            pct = 100 * note["missing_words"] / max(note["transcript_words"], 1)
            where = "at the start" if note["leading_orphan"] else f"in {len(note['runs'])} run(s)"
            print(f"  {note['title'][:56]}")
            print(f"    {note['id']}  {note['created_at']}")
            print(
                f"    {note['missing_words']} of {note['transcript_words']} words unaccounted for"
                f" ({pct:.0f}%), {where}; {note['sessions']} session(s), {note['entries']} timeline entries"
            )
            if not args.quiet:
                start, run = note["runs"][0]
                print(f'    first gap @word {start}: "{" ".join(run[:14])}{"..." if len(run) > 14 else ""}"')
            print()

    if no_timeline:
        print(f"NO TIMELINE -- {len(no_timeline)} note(s). Nothing is hidden (the reader falls")
        print("back to plain text), but a rebuild would empty the transcript entirely.")
        for note in no_timeline:
            print(f"  {note['id']}  {note['created_at']}  {note['transcript_words']:>6} words  {note['title'][:44]}")
        print()

    if malformed:
        print(f"MALFORMED TIMELINE LINES -- {len(malformed)} note(s); these are skipped on read,")
        print("so the timeline is present but short.")
        for note in malformed:
            print(f"  {note['id']}  {note['malformed']} bad line(s) of {note['entries'] + note['malformed']}")
        print()

    if args.verbose:
        print("ALL NOTES")
        for note in notes:
            print(
                f"  [{note['status']:>11}] {note['transcript_words']:>6}w transcript /"
                f" {note['projection_words']:>6}w projected  {note['sessions']}s"
                f"  {note['title'][:44]}"
            )
        print()

    ok = len(notes) - len(orphaned) - len(no_timeline)
    print(f"summary: {ok} clean, {len(orphaned)} with orphaned text, {len(no_timeline)} with no timeline")
    if orphaned:
        total = sum(n["missing_words"] for n in orphaned)
        print(f"         {total} words are currently invisible and one click from deletion")


if __name__ == "__main__":
    main()
