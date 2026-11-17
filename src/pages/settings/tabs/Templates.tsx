import { useEffect, useState, type ReactNode } from "react";
import { Copy, Eye, Lock, Pencil, Plus, Trash2 } from "lucide-react";
import { ipc, type SummaryPrompt } from "../../../lib/ipc";
import { useCloudStore } from "../../../lib/cloud";
import {
  pickerTemplates,
  useActiveWorkspaceId,
  useOrgContentVersion,
  useOrgs,
} from "../../../lib/orgContent";
import { ManagedBadge } from "../ManagedRow";
import {
  SUMMARY_PRESETS,
  presetLabel,
  presetPromptForLang,
  presetTemplateText,
} from "../../../lib/presets";
import { inputClass } from "../types";
import { Modal } from "../components/Modal";

const quietBtnClass =
  "inline-flex items-center gap-1 px-2 py-1 rounded-md text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-pill-hover)] transition-colors";

type Draft = { id: string | null; name: string; content: string };

// Templates are `summary_prompts` rows. One created while a team workspace is
// active belongs to that workspace and syncs to every member, who can all edit
// it; personal ones stay on this Mac. The server refuses every template write
// from a viewer, and a new template always lands in the active workspace, so a
// viewer there can neither edit shared templates nor create one. An
// organisation's templates are read-only and listed only in the departments
// they are offered to.
export function TemplatesSection({ language }: { language: string }) {
  const [templates, setTemplates] = useState<SummaryPrompt[]>([]);
  const [viewing, setViewing] = useState<{ name: string; content: string; template: string } | null>(null);
  const [editing, setEditing] = useState<Draft | null>(null);
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const workspaceName = useCloudStore(
    (s) => s.status.current_workspace?.name ?? s.status.locked_current_workspace?.name ?? null,
  );
  const currentRole = useCloudStore((s) => s.status.current_workspace?.role);
  const workspaces = useCloudStore((s) => s.status.workspaces);
  const activeWorkspaceId = useActiveWorkspaceId();
  const orgs = useOrgs();
  const orgVersion = useOrgContentVersion();

  useEffect(() => {
    let cancelled = false;
    ipc
      .summaryPromptsList()
      .then((next) => {
        if (!cancelled) setTemplates(next);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [orgVersion]);

  async function refresh() {
    setTemplates(await ipc.summaryPromptsList());
  }

  async function save() {
    if (!editing) return;
    const name = editing.name.trim();
    if (!name) return;
    try {
      if (editing.id === null) {
        await ipc.summaryPromptsCreate(name, editing.content);
      } else {
        await ipc.summaryPromptsUpdate(editing.id, name, editing.content);
      }
      await refresh();
      setEditing(null);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }

  async function remove(id: string) {
    try {
      await ipc.summaryPromptsDelete(id);
      await refresh();
      setConfirmDeleteId(null);
    } catch (e) {
      setError(String(e));
    }
  }

  function duplicate(name: string, content: string) {
    setEditing({ id: null, name: `${name} (copy)`, content });
  }

  const { org: fromOrg, own } = pickerTemplates(templates, activeWorkspaceId, orgs);
  const shared = own.filter((t) => t.workspace_id);
  const personal = own.filter((t) => !t.workspace_id);
  const orgsShown = orgs.filter((o) => fromOrg.some((t) => t.org_id === o.id));
  const departmentNames = (t: SummaryPrompt) =>
    (t.org_workspaces ?? []).flatMap((id) => workspaces.find((w) => w.id === id)?.name ?? []);
  const inWorkspace = workspaceName !== null || shared.length > 0;
  const canCreate = !(inWorkspace && currentRole === "viewer");
  const canEdit = (t: SummaryPrompt) =>
    !t.workspace_id || workspaces.find((w) => w.id === t.workspace_id)?.role !== "viewer";
  const newButton = canCreate && (
    <button
      type="button"
      className="nd-btn"
      onClick={() => setEditing({ id: null, name: "", content: "" })}
    >
      <Plus size={14} aria-hidden /> New template
    </button>
  );

  const ownRow = (t: SummaryPrompt) =>
    confirmDeleteId === t.id ? (
      <div key={t.id} className="flex items-center justify-between gap-4 py-2.5">
        <div className="min-w-0">
          <div className="text-sm truncate">Delete “{t.name}”?</div>
          <p className="text-xs text-[var(--color-text-muted)] mt-0.5">
            Notes using it fall back to Meeting.
          </p>
        </div>
        <div className="flex gap-1.5 shrink-0">
          <button type="button" className="nd-btn" onClick={() => setConfirmDeleteId(null)}>
            Cancel
          </button>
          <button
            type="button"
            className="nd-btn"
            style={{ color: "var(--color-danger)" }}
            onClick={() => remove(t.id)}
          >
            Delete
          </button>
        </div>
      </div>
    ) : (
      <TemplateRow
        key={t.id}
        name={t.name}
        actions={
          !canEdit(t) ? (
            <button
              type="button"
              className={quietBtnClass}
              aria-label={`View ${t.name}`}
              onClick={() => setViewing({ name: t.name, content: t.content, template: t.content })}
            >
              <Eye size={13} aria-hidden /> View
            </button>
          ) : (
            <>
              <button
                type="button"
                className={quietBtnClass}
                aria-label={`Edit ${t.name}`}
                onClick={() => setEditing({ id: t.id, name: t.name, content: t.content })}
              >
                <Pencil size={13} aria-hidden /> Edit
              </button>
              <button
                type="button"
                className={quietBtnClass}
                aria-label={`Delete ${t.name}`}
                onClick={() => setConfirmDeleteId(t.id)}
              >
                <Trash2 size={13} aria-hidden />
              </button>
            </>
          )
        }
      />
    );

  return (
    <>
      {fromOrg.length > 0 && (
        <GroupSection
          title="From your organisation"
          aside={
            <div className="flex flex-wrap justify-end gap-1">
              {orgsShown.map((o) => (
                <ManagedBadge key={o.id} source={o.name} reason={o.reason} />
              ))}
            </div>
          }
        >
          {fromOrg.map((t) => (
            <TemplateRow
              key={t.id}
              name={t.name}
              locked
              departments={departmentNames(t)}
              actions={
                <>
                  <button
                    type="button"
                    className={quietBtnClass}
                    aria-label={`View ${t.name}`}
                    onClick={() => setViewing({ name: t.name, content: t.content, template: t.content })}
                  >
                    <Eye size={13} aria-hidden /> View
                  </button>
                  {canCreate && (
                    <button
                      type="button"
                      className={quietBtnClass}
                      aria-label={`Duplicate ${t.name}`}
                      onClick={() => duplicate(t.name, t.content)}
                    >
                      <Copy size={13} aria-hidden /> Duplicate
                    </button>
                  )}
                </>
              }
            />
          ))}
        </GroupSection>
      )}

      <GroupSection title="Built-in">
        {SUMMARY_PRESETS.map((p) => {
          const name = presetLabel(p);
          return (
            <TemplateRow
              key={p.value}
              name={name}
              actions={
                <>
                  <button
                    type="button"
                    className={quietBtnClass}
                    aria-label={`View ${name}`}
                    onClick={() =>
                      setViewing({
                        name,
                        content: presetPromptForLang(p, language),
                        template: presetTemplateText(p),
                      })
                    }
                  >
                    <Eye size={13} aria-hidden /> View
                  </button>
                  {canCreate && (
                    <button
                      type="button"
                      className={quietBtnClass}
                      aria-label={`Duplicate ${name}`}
                      onClick={() => duplicate(name, presetTemplateText(p))}
                    >
                      <Copy size={13} aria-hidden /> Duplicate
                    </button>
                  )}
                </>
              }
            />
          );
        })}
      </GroupSection>

      {inWorkspace && (
        <GroupSection
          title={workspaceName ? `Shared in ${workspaceName}` : "Shared"}
          aside={newButton}
        >
          {shared.length === 0 ? <EmptyRow /> : shared.map(ownRow)}
        </GroupSection>
      )}

      {(!inWorkspace || personal.length > 0) && (
        <GroupSection title="Your templates" aside={inWorkspace ? undefined : newButton}>
          {personal.length === 0 ? <EmptyRow /> : personal.map(ownRow)}
        </GroupSection>
      )}

      {error && (
        <p className="text-sm break-all" style={{ color: "var(--color-danger)" }}>
          {error}
        </p>
      )}

      <Modal open={viewing !== null} onClose={() => setViewing(null)} title={viewing?.name ?? ""}>
        {viewing && (
          <div className="flex flex-col gap-3">
            <pre className="whitespace-pre-wrap text-sm leading-relaxed rounded-md border border-[var(--color-line)] bg-[var(--color-canvas)] p-3 font-[var(--font-sans)]">
              {viewing.content}
            </pre>
            {canCreate && (
              <div className="flex justify-end">
                <button
                  type="button"
                  className="nd-btn"
                  onClick={() => {
                    duplicate(viewing.name, viewing.template);
                    setViewing(null);
                  }}
                >
                  <Copy size={14} aria-hidden /> Duplicate
                </button>
              </div>
            )}
          </div>
        )}
      </Modal>

      <Modal
        open={editing !== null}
        onClose={() => setEditing(null)}
        title={editing?.id ? "Edit template" : "New template"}
      >
        {editing && (
          <TemplateEditor
            value={editing}
            onChange={setEditing}
            onCancel={() => setEditing(null)}
            onSave={save}
          />
        )}
      </Modal>
    </>
  );
}

function EmptyRow() {
  return <p className="text-xs text-[var(--color-text-muted)] py-3.5">No templates yet.</p>;
}

function GroupSection({
  title,
  aside,
  children,
}: {
  title: string;
  aside?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="mb-8">
      <div className="flex items-center justify-between gap-3 mb-2.5 min-h-7">
        <h2 className="text-sm font-medium">{title}</h2>
        {aside}
      </div>
      <div className="flex flex-col divide-y divide-[var(--color-line)] rounded-[var(--radius-card)] border border-[var(--color-line-visible)] bg-[var(--color-surface)] px-4 py-1">
        {children}
      </div>
    </section>
  );
}

function TemplateRow({
  name,
  actions,
  locked = false,
  departments = [],
}: {
  name: string;
  actions: ReactNode;
  locked?: boolean;
  departments?: string[];
}) {
  return (
    <div className="flex items-center justify-between gap-4 py-2.5">
      <div className="min-w-0 flex flex-wrap items-center gap-x-2 gap-y-1">
        {locked && (
          <Lock size={12} strokeWidth={2} aria-hidden className="shrink-0 text-[var(--color-text-muted)]" />
        )}
        <span className="text-sm truncate min-w-0">{name}</span>
        {departments.map((d) => (
          <span
            key={d}
            className="px-1.5 py-0.5 rounded-md text-xs text-[var(--color-text-muted)] bg-[var(--color-pill-hover)]"
          >
            {d}
          </span>
        ))}
      </div>
      <div className="flex gap-0.5 shrink-0">{actions}</div>
    </div>
  );
}

function TemplateEditor({
  value,
  onChange,
  onCancel,
  onSave,
}: {
  value: Draft;
  onChange: (next: Draft) => void;
  onCancel: () => void;
  onSave: () => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      <label className="flex flex-col gap-1.5">
        <span className="text-xs font-medium text-[var(--color-text-muted)]">Name</span>
        <input
          type="text"
          value={value.name}
          onChange={(e) => onChange({ ...value, name: e.target.value })}
          placeholder="Team standup"
          className={inputClass}
          autoFocus
        />
      </label>
      <label className="flex flex-col gap-1.5">
        <span className="text-xs font-medium text-[var(--color-text-muted)]">Instructions</span>
        <textarea
          value={value.content}
          onChange={(e) => onChange({ ...value, content: e.target.value })}
          rows={12}
          className={inputClass + " leading-relaxed"}
          placeholder="What the summary should contain."
        />
      </label>
      <div className="flex justify-end gap-2">
        <button type="button" className="nd-btn" onClick={onCancel}>
          Cancel
        </button>
        <button
          type="button"
          className="nd-btn nd-btn-primary"
          disabled={!value.name.trim()}
          onClick={onSave}
        >
          Save template
        </button>
      </div>
    </div>
  );
}
