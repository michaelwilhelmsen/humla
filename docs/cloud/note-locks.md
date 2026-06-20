# Recording locks (`note_locks`)

Server schema for the **shared-note recording lock**: when one teammate records a
workspace note, others can't record the same note at the same time. Without it,
two recorders each push a whole-note last-write-wins record carrying only *their
own* transcript — one stream wins, the other is scattered into "(conflict copy)"
notes, and the audio collides on upload. (It's also acoustically pointless: one
recorder already captures the whole meeting — their mic is them, their system
audio is everyone else.)

This is **server-authoritative**: the claim is decided by PocketBase at write
time via a `UNIQUE` index, not optimistically per-client and reconciled later.
The client code is already shipped (`commands/cloud.rs`, `recording.rs`,
`Note.tsx`); it is inert until this collection exists on the server.

> Status: **spec only — not yet applied.** Drop the migration below into
> `server/pb_migrations/` when the sync server is created. Until then the client
> simply records unlocked (a missing collection looks like "cloud unreachable",
> which degrades to today's behavior).

## Collection: `note_locks` (type `base`)

| Field | Type | Required | Notes |
|---|---|---|---|
| `note` | text (max 60) | yes | the note's `client_id` (UUID). **`UNIQUE` index** — the mutex |
| `workspace` | relation → `workspaces` (single, cascade) | yes | scopes the API rules to workspace members |
| `holder` | relation → `users` (single, cascade) | yes | who holds the lock |
| `holder_name` | text (max 200) | no | denormalized display name for the "X is recording…" banner |
| `expires` | date | yes | heartbeat horizon (claim time + 45 s) |
| `created` | autodate (onCreate) | — | standard |
| `updated` | autodate (onCreate+onUpdate) | — | standard |

**Index (the atomic guarantee):**

```sql
CREATE UNIQUE INDEX idx_note_locks_note ON note_locks (note)
```

At most one live lock per note. The first claimant's `INSERT` wins; a concurrent
second `INSERT` is rejected by the index (PocketBase returns `400` with
`validation_not_unique`), which the client reads as "someone else is recording."

## API rules

| Rule | Expression | Why |
|---|---|---|
| **list** | `@request.auth.id != "" && workspace.members.id ?= @request.auth.id` | members can see locks in their workspaces (drives the banner) |
| **view** | `@request.auth.id != "" && workspace.members.id ?= @request.auth.id` | same |
| **create** | `@request.auth.id != "" && holder = @request.auth.id && workspace.members.id ?= @request.auth.id` | only a member, only claiming for themselves |
| **update** | `@request.auth.id != "" && holder = @request.auth.id` | only the holder may heartbeat (extend `expires`) |
| **delete** | `@request.auth.id != "" && (holder = @request.auth.id || expires < @now)` | the holder releases on stop; **anyone may reap a stale lock** (`expires < @now`) so a crashed recorder never pins a note |

The `expires < @now` delete clause is what makes stale takeover safe **without a
custom hook**: a still-valid lock won't delete, so a takeover attempt re-conflicts
on the unique index and the client reports the real holder. Staleness is decided
server-side, so clients never trust their own clock.

## Migration (drop-in)

Save as `server/pb_migrations/<timestamp>_created_note_locks.js` — pick a
timestamp **after** the migration that creates `workspaces`. PocketBase applies
it on next `serve`.

```js
/// <reference path="../pb_data/types.d.ts" />

// Recording lock for shared notes — see docs/cloud/note-locks.md.
migrate((app) => {
  const users = app.findCollectionByNameOrId("users");
  const workspaces = app.findCollectionByNameOrId("workspaces");

  const c = new Collection({
    type: "base",
    name: "note_locks",
    listRule: "@request.auth.id != '' && workspace.members.id ?= @request.auth.id",
    viewRule: "@request.auth.id != '' && workspace.members.id ?= @request.auth.id",
    createRule:
      "@request.auth.id != '' && holder = @request.auth.id && workspace.members.id ?= @request.auth.id",
    updateRule: "@request.auth.id != '' && holder = @request.auth.id",
    deleteRule: "@request.auth.id != '' && (holder = @request.auth.id || expires < @now)",
    fields: [
      { name: "note", type: "text", required: true, max: 60 },
      {
        name: "workspace",
        type: "relation",
        required: true,
        maxSelect: 1,
        collectionId: workspaces.id,
        cascadeDelete: true,
      },
      {
        name: "holder",
        type: "relation",
        required: true,
        maxSelect: 1,
        collectionId: users.id,
        cascadeDelete: true,
      },
      { name: "holder_name", type: "text", max: 200 },
      { name: "expires", type: "date", required: true },
      { name: "created", type: "autodate", onCreate: true },
      { name: "updated", type: "autodate", onCreate: true, onUpdate: true },
    ],
    indexes: ["CREATE UNIQUE INDEX idx_note_locks_note ON note_locks (note)"],
  });

  app.save(c);
}, (app) => {
  // rollback
  const c = app.findCollectionByNameOrId("note_locks");
  app.delete(c);
});
```

### Or via the Admin UI

1. **New collection** → base → name `note_locks`.
2. Add fields per the table above (relations point at `users` / `workspaces`,
   single-select, cascade delete; `expires` is a Date; keep the auto `created` /
   `updated`).
3. **Indexes** → add `CREATE UNIQUE INDEX idx_note_locks_note ON note_locks (note)`.
4. **API rules** → paste the five expressions above.

## Client contract

The client (`src-tauri/src/commands/cloud.rs`) talks to this collection over the
normal PocketBase REST API:

- **Claim** (`recording_start`, workspace notes only): `POST note_locks`
  `{note, workspace, holder, holder_name, expires:+45s}`. A `validation_not_unique`
  `400` → it fetches the row and tries to `DELETE` it (succeeds only if stale per
  the delete rule); if the retry still conflicts, recording is refused with
  *"{name} is recording this note."*
- **Heartbeat** (every **15 s** while recording): `PATCH note_locks/{id}`
  `{expires:+45s}`.
- **Release** (`recording_stop` / crash cleanup): `DELETE note_locks/{id}`.
- **Status** (Note view polls every ~10 s): `GET note_locks?filter=note='{id}' && expires > @now`
  → drives the banner + disabled Record button (`cloud_note_recording_status`).

Constants live in `cloud.rs` (`LOCK_TTL_SECS = 45`, `LOCK_HEARTBEAT_SECS = 15`).
TTL must stay comfortably above the heartbeat so two missed pings don't expire a
live lock.

**Deliberate soft edge:** if the server is *unreachable*, the claim is skipped
and recording proceeds **unlocked** — a flaky network must never block capture,
and when the server is down, sync is degraded anyway. The lock is hard-enforced
whenever the server is reachable.

## Quick test (once the server exists)

1. Two clients signed in as different users in the same workspace, same shared note.
2. A hits Record → B sees "A is recording…" within ~10 s, B's Record is disabled.
3. B hits Record anyway (or ⌘R) → refused with A's name (server-enforced).
4. A stops → B's banner clears within ~10 s, B can record.
5. Kill A's app mid-recording → B can record after ≤45 s (stale lock reaped).
