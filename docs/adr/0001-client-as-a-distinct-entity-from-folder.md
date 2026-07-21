# Client is a distinct entity from Folder, not reused

While scoping the AI-chat rollup feature (which entity should a per-account sales-opportunity summary key on?), we needed a real-world "who is this meeting with" concept. Humla already has Folder — a general-purpose, user-defined organizational bucket — and reusing it was the obvious, zero-new-schema path. We introduced a separate **Client** entity instead, because Folder is deliberately meaning-free (a user might file notes by project, by time period, or by meeting type, not necessarily by client), and the product explicitly wants Folder and Client to be independently filterable dimensions in search/chat (e.g. "sales opportunities from my client meetings within this folder"), not one collapsing into the other.

## Considered Options

- **Reuse Folder directly.** Rejected: folders aren't guaranteed to represent one client, and collapsing the two dimensions would prevent independent Folder + Client filtering.
- **Fuzzy-match Clients from meeting titles/participants.** Rejected: no structured participant-tracking exists today, and a wrong silent auto-tag would corrupt that client's rollup — the same reliability concern that already ruled out fuzzy matching elsewhere in this effort.
- **New Client entity, manually tagged (chosen).**

## Consequences

- A Note has zero-or-one Client (mirroring Folder's existing cardinality) and zero-or-one Folder, independently.
- Client tagging is manual only, v1 — no inference, no auto-suggestion.
- Client must exist as a syncable, workspace-scoped concept server-side (for cloud-workspace rollups) in addition to locally, following the existing `client_id`-keyed sync pattern already used for Notes/Folders.
