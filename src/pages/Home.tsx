import { useNavigate } from "react-router-dom";
import { ipc } from "../lib/ipc";
import { useNotesStore } from "../lib/store";

export function Home() {
  const navigate = useNavigate();
  const upsert = useNotesStore((s) => s.upsertLocal);

  async function newNote() {
    const note = await ipc.createNote();
    upsert(note);
    navigate(`/note/${note.id}`);
  }

  return (
    <div className="h-full flex flex-col items-center justify-center gap-6">
      <div className="text-5xl font-light tracking-[-0.02em]">Notes</div>
      <div className="text-[var(--color-text-muted)] flex items-center gap-2">
        <span>Press</span>
        <kbd
          className="px-2 py-0.5 border border-[var(--color-line-visible)] rounded text-xs"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          ⌘N
        </kbd>
        <span>to start a new note</span>
      </div>
      <button onClick={newNote} className="nd-action no-drag mt-2">
        New note
      </button>
    </div>
  );
}
