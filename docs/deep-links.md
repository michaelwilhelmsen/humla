# Start and stop a recording from a link

Humla answers `humla://` links, so anything on your Mac that can open a URL
can start and stop a recording: Shortcuts, Raycast, Alfred, a Stream Deck, a
shell script.

| Link | What it does |
|---|---|
| `humla://record/start` | Starts recording into a new note, titled with the start time. |
| `humla://record/start?name=Daily%20Standup` | The same, with the note titled "Daily Standup". |
| `humla://record/stop` | Stops the recording. |

From a terminal:

```bash
open "humla://record/start?name=Daily%20Standup"
```

In Shortcuts, use the **Open URLs** action. Encode the name, so a space
becomes `%20`; the **URL Encode** action does that for you.

## What happens

- A start link always records into a **new note**, with your default language
  and summary preset, in the workspace you are in. Whatever note is open in
  the window is left alone.
- Humla doesn't open its window for a link. If the window is already on
  screen, macOS may still bring Humla to the front, as it does for any app
  that opens a link.
- A notification names the note when the recording starts. When a stop link
  ends a recording, however it was started, another notification says when it
  has been saved.
- If Humla isn't running, the link opens it, as any launch would, and starts
  recording as soon as Humla has loaded.
- It is an ordinary recording: stop it from the menu bar, with the hotkey,
  from the note, or with the stop link.
- Humla's automatic titles don't replace the name you pass, unless the name
  looks like one Humla makes up itself, such as `Recording 10 Nov 09:30`.
  Without a name, the note is titled with the start time until Humla names it
  from what was said.

## When a link does nothing

A link never starts a second recording and never cuts into one that is
starting. When it can't do what it asks, it does nothing and a notification
says why:

- **Start while recording.** The recording you have keeps going.
- **Start while the last recording is still being saved.** After you stop,
  Humla finishes the transcript and works out who spoke before it can record
  again, which can take a few minutes after a long meeting. For back-to-back
  meetings this means the second start link can arrive too early; open it
  again once the "saved" notification appears.
- **Start while a file is being imported.** Try again once the import is done.
- **Stop when nothing is recording**, or while a recording is still starting.

These notifications only appear once you have allowed notifications for
Humla. Humla asks during setup, or with its first notification if it hasn't
asked before. If you said no, allow them in **Settings → Recording →
Permissions** in Humla, or in **System Settings → Notifications**.

## Links and privacy

- A web page can ask your browser to open a `humla://` link, and the browser
  asks you first whether to open Humla. Shortcuts and scripts are things you
  set up yourself, so Humla adds no confirmation of its own.
- Nothing a link does is silent. Every recording a link starts posts a
  notification, and the menu-bar icon turns into a red disc while the
  microphone is in use.
- The recording is handled with your own settings, like any other: the same
  transcription provider, the same audio retention.
