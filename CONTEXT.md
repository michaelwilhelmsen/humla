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
The real-world business relationship (a company, or a person where the person *is* the relationship — a sole trader, an independent consultant) a Note is optionally about — zero-or-one per Note, always manually tagged by the user, never inferred or fuzzy-matched. Distinct from Folder: a Folder is about how the user organizes their own Notes for browsing; a Client is about who the other side of the meeting actually is, independent of how it's filed. The two are orthogonal and independently filterable — a search or chat query can scope by Folder, by Client, or both together. **Never a per-attendee record**: because a Note has zero-or-one Client, a Client per human you met would make "which meetings were with K2" unanswerable. Who spoke is a Speaker; who the meeting was with is the Client.
_Avoid_: Account, contact, customer

**Speaker**:
The label naming who is talking in a transcript turn — the `Hege Tronshaugen:` in `Hege Tronshaugen: …`. A string the user asserted (by renaming the diarizer's `Speaker N:`), not a record of a person: Humla has no Person entity, and a Speaker's identity lives in the transcript text and nowhere else. The set of Speakers in one Note is derived from that text, never stored independently of it. Only ever someone who *spoke* — a silent attendee produces no label, so "participant" and "attendee" claim attendance Humla never observed. See `docs/adr/0002-no-person-entity-speakers-are-derived.md`.
_Avoid_: Contact, person, attendee, participant

**Rollup**:
A precomputed, standing summary of everything known about one Client — risk signals, sentiment trend, open items — built from every Note tagged to that Client, refreshed as new Notes arrive rather than recomputed from scratch each time it's read.
_Avoid_: Digest, report, brief
