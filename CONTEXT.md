# Humla

Personal macOS meeting-notes app: freeform user notes plus recorded audio, transcribed and AI-summarized. This file is a glossary of project-specific domain vocabulary — not architecture, not implementation.

## Language

**Note**:
A single meeting record: the user's typed notes, the audio transcript, and an AI-generated summary, together as one unit.
_Avoid_: Meeting, recording, entry

**Folder**:
A general-purpose, user-defined bucket a Note can optionally belong to (zero-or-one). Used purely for browsing/organizing however the user likes — by project, by time period, by meeting type — with no fixed meaning beyond "the user filed it here."
_Avoid_: Category, tag, group

**Client**:
The real-world business relationship (a company or person) a Note is optionally about — zero-or-one per Note, always manually tagged by the user, never inferred or fuzzy-matched. Distinct from Folder: a Folder is about how the user organizes their own Notes for browsing; a Client is about who the other side of the meeting actually is, independent of how it's filed. The two are orthogonal and independently filterable — a search or chat query can scope by Folder, by Client, or both together.
_Avoid_: Account, contact, customer

**Rollup**:
A precomputed, standing summary of everything known about one Client — risk signals, sentiment trend, open items — built from every Note tagged to that Client, refreshed as new Notes arrive rather than recomputed from scratch each time it's read.
_Avoid_: Digest, report, brief
