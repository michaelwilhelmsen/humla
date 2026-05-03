import { useEffect, useState } from "react";
import { ipc, type SummaryPrompt } from "../../../lib/ipc";
import { SUMMARY_PRESETS, presetLabel, presetPromptForLang } from "../../../lib/presets";
import { inputClass } from "../types";
import { Btn } from "./Btn";
import { Modal } from "./Modal";

// Two-section UI: built-in presets are read-only previews; user-defined
// prompts live in their own list with create / edit / delete via a
// modal editor. The modal keeps room for future per-prompt settings
// (language pin, default-for-which-note-types, etc.) without crowding
// the main Summary tab.
export function SummaryPromptsManager({ language }: { language: string }) {
  const [prompts, setPrompts] = useState<SummaryPrompt[]>([]);
  const [editing, setEditing] = useState<EditingState | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Inline two-step delete confirmation. Tauri's webview blocks
  // window.confirm (it deadlocks the main thread, so the runtime no-ops
  // it silently), so we keep the confirmed-id in component state and
  // swap the row's buttons to Cancel + Confirm delete on first click.
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    ipc.summaryPromptsList()
      .then((next) => {
        if (!cancelled) setPrompts(next);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function refresh() {
    const next = await ipc.summaryPromptsList();
    setPrompts(next);
  }

  function startNew() {
    setEditing({ kind: "new", name: "", content: "" });
  }

  function startEdit(p: SummaryPrompt) {
    setEditing({ kind: "edit", id: p.id, name: p.name, content: p.content });
  }

  async function save() {
    if (!editing) return;
    const trimmedName = editing.name.trim();
    if (!trimmedName) {
      setError("Name is required.");
      return;
    }
    try {
      if (editing.kind === "new") {
        await ipc.summaryPromptsCreate(trimmedName, editing.content);
      } else {
        await ipc.summaryPromptsUpdate(editing.id, trimmedName, editing.content);
      }
      await refresh();
      setEditing(null);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmRemove(p: SummaryPrompt) {
    try {
      await ipc.summaryPromptsDelete(p.id);
      await refresh();
      setConfirmDeleteId(null);
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-col gap-2">
        <p className="text-xs uppercase tracking-wide text-[var(--color-text-muted)]">
          Built-in
        </p>
        <p className="text-xs text-[var(--color-text-muted)]">
          Maintained presets — adapt automatically to the recording's
          language. Read-only.
        </p>
        <div className="flex flex-wrap gap-2">
          {SUMMARY_PRESETS.map((p) => (
            <span
              key={p.value}
              className="text-xs px-2 py-1 rounded bg-[var(--color-pill-hover)] text-[var(--color-text)]"
              title={presetPromptForLang(p, language)}
            >
              {presetLabel(p)}
            </span>
          ))}
        </div>
      </div>

      <div className="flex flex-col gap-2">
        <div className="flex items-center justify-between">
          <p className="text-xs uppercase tracking-wide text-[var(--color-text-muted)]">
            Your prompts
          </p>
          <Btn onClick={startNew}>+ New prompt</Btn>
        </div>
        {prompts.length === 0 ? (
          <p className="text-xs text-[var(--color-text-muted)]">
            No custom prompts yet. Create one to use specialised
            instructions for a recurring meeting type.
          </p>
        ) : (
          <div className="flex flex-col gap-2">
            {prompts.map((p) => {
              const isConfirming = confirmDeleteId === p.id;
              return (
                <div
                  key={p.id}
                  className="flex flex-col gap-2 px-3 py-2 rounded-md border border-[var(--color-line)]"
                >
                  <div className="flex items-center gap-2">
                    <div className="flex-1 min-w-0">
                      <div className="text-sm font-medium truncate">{p.name}</div>
                      <p className="text-xs text-[var(--color-text-muted)] truncate font-mono">
                        {p.content.split("\n")[0].slice(0, 80) || "—"}
                      </p>
                    </div>
                    <div className="flex gap-2">
                      {isConfirming ? (
                        <>
                          <Btn onClick={() => setConfirmDeleteId(null)}>
                            Cancel
                          </Btn>
                          <Btn onClick={() => confirmRemove(p)}>
                            Confirm delete
                          </Btn>
                        </>
                      ) : (
                        <>
                          <Btn onClick={() => startEdit(p)}>Edit</Btn>
                          <Btn onClick={() => setConfirmDeleteId(p.id)}>
                            Delete
                          </Btn>
                        </>
                      )}
                    </div>
                  </div>
                  {isConfirming && (
                    <p className="text-xs text-[var(--color-text-muted)]">
                      Notes using this prompt will fall back to the legacy
                      custom prompt.
                    </p>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>

      <p className="text-xs text-[var(--color-text-muted)]">
        Tip: custom prompts run literally — write them in the language you
        want the summary in, or include a line like "Reply in Norwegian".
        Built-in presets handle the language switch for you automatically.
      </p>

      {error && (
        <p className="text-sm text-red-600 dark:text-red-400 break-all">
          {error}
        </p>
      )}

      <Modal
        open={editing !== null}
        onClose={() => setEditing(null)}
        title={editing?.kind === "new" ? "New prompt" : "Edit prompt"}
      >
        {editing && (
          <PromptEditor
            value={editing}
            onChange={setEditing}
            onCancel={() => setEditing(null)}
            onSave={save}
            onSeed={(presetValue) => {
              const preset = SUMMARY_PRESETS.find((p) => p.value === presetValue);
              if (!preset) return;
              setEditing({
                ...editing,
                content: presetPromptForLang(preset, language),
              });
            }}
          />
        )}
      </Modal>
    </div>
  );
}

type EditingState =
  | { kind: "new"; name: string; content: string }
  | { kind: "edit"; id: string; name: string; content: string };

function PromptEditor({
  value,
  onChange,
  onCancel,
  onSave,
  onSeed,
}: {
  value: EditingState;
  onChange: (next: EditingState) => void;
  onCancel: () => void;
  onSave: () => void;
  onSeed: (presetValue: string) => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-1.5">
        <label className="text-xs uppercase tracking-wide text-[var(--color-text-muted)]">
          Name
        </label>
        <input
          type="text"
          value={value.name}
          onChange={(e) => onChange({ ...value, name: e.target.value })}
          placeholder="Standup notes"
          className={inputClass}
          autoFocus
        />
      </div>

      <div className="flex flex-col gap-1.5">
        <label className="text-xs uppercase tracking-wide text-[var(--color-text-muted)]">
          Content
        </label>
        <select
          onChange={(e) => {
            if (e.target.value) onSeed(e.target.value);
            e.currentTarget.value = "";
          }}
          className={inputClass + " w-auto py-1 text-xs"}
          defaultValue=""
          aria-label="Seed content from preset"
        >
          <option value="" disabled>
            Seed from preset…
          </option>
          {SUMMARY_PRESETS.map((p) => (
            <option key={p.value} value={p.value}>
              {presetLabel(p)}
            </option>
          ))}
        </select>
        <textarea
          value={value.content}
          onChange={(e) => onChange({ ...value, content: e.target.value })}
          rows={12}
          className={inputClass + " leading-relaxed font-mono text-xs"}
          placeholder="Write your system prompt here. The model will see this as its instructions plus the note's notes + transcript."
        />
      </div>

      <div className="flex justify-end gap-2 pt-2 border-t border-[var(--color-line)]">
        <Btn onClick={onCancel}>Cancel</Btn>
        <Btn onClick={onSave}>Save</Btn>
      </div>
    </div>
  );
}
