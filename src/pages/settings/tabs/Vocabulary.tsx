import { useState } from "react";
import { Lock, X } from "lucide-react";
import { Section } from "../components/Section";
import { editableOrgs, orgTerms, storedTerms, useAdminContent, useAdminOrgs, useOrgsState } from "../../../lib/orgContent";
import { useCloudStore } from "../../../lib/cloud";
import { ipc } from "../../../lib/ipc";
import type { OrgSummary } from "../../../lib/orgPolicy";
import { ManagedBadge } from "../ManagedRow";
import { inputClass } from "../types";
import type { SettingsHook } from "../useSettings";

// Deepgram takes at most 100 terms, and every provider drops terms shorter than
// three characters (`transcribe_chunk`).
export const VOCABULARY_LIMIT = 100;
export const MIN_TERM_LENGTH = 3;

export function parseVocabulary(stored: string): string[] {
  const seen = new Set<string>();
  const terms: string[] = [];
  for (const raw of stored.split(/[\n,]/)) {
    const term = raw.trim();
    const key = term.toLowerCase();
    if (!term || seen.has(key)) continue;
    seen.add(key);
    terms.push(term);
  }
  return terms;
}

export function serializeVocabulary(terms: string[]): string {
  return terms.join(", ");
}

function splitDraft(draft: string): string[] {
  return draft
    .split(/[\n,]/)
    .map((t) => t.trim())
    .filter((t) => t.length >= MIN_TERM_LENGTH);
}

export function VocabularySection({ s, update }: Pick<SettingsHook, "s" | "update">) {
  const terms = parseVocabulary(s.custom_vocabulary);
  const orgsState = useOrgsState();
  const orgs = orgsState ?? [];
  const adminOrgs = useAdminOrgs();
  const twoFactorOn = useCloudStore((st) => st.status.two_factor?.enabled ?? false);
  const adminContent = useAdminContent(orgsState, adminOrgs, twoFactorOn);
  // An administered organisation's terms are edited as the server holds them,
  // once read; until then they show locked from the cache like anyone else's.
  const outsideConfig: OrgSummary[] =
    orgsState === null
      ? []
      : adminOrgs
          .filter((a) => !orgs.some((o) => o.id === a.id))
          .map((a) => ({ id: a.id, name: a.name, reason: "", vocabulary: [], workspaces: [] }));
  const { editable: administered, locked: twoFactorLocked } = editableOrgs(
    [...orgs, ...outsideConfig],
    adminOrgs,
    twoFactorOn,
  );
  const read = (id: string) => adminContent.list.find((a) => a.summary.id === id && a.content);
  const editable = administered.flatMap((o) => read(o.id)?.summary ?? []);
  const locked = orgTerms(orgs.filter((o) => !editable.some((e) => e.id === o.id)));
  const [draft, setDraft] = useState("");
  const full = terms.length >= VOCABULARY_LIMIT;
  const candidates = splitDraft(draft);
  const badged = orgs.filter(
    (o) => o.vocabulary.some((t) => t.trim()) || editable.some((e) => e.id === o.id) || twoFactorLocked.includes(o),
  );

  function add() {
    if (candidates.length === 0) return;
    const next = parseVocabulary([...terms, ...candidates].join("\n")).slice(0, VOCABULARY_LIMIT);
    void update("custom_vocabulary", serializeVocabulary(next));
    setDraft("");
  }

  function remove(term: string) {
    void update("custom_vocabulary", serializeVocabulary(terms.filter((t) => t !== term)));
  }

  return (
    <>
      {(locked.length > 0 || editable.length > 0 || twoFactorLocked.length > 0) && (
        <section className="mb-8">
          <div className="flex items-center justify-between gap-3 mb-2.5 min-h-7">
            <h2 className="text-sm font-medium">From your organisation</h2>
            <div className="flex flex-wrap justify-end gap-1">
              {badged.map((o) => (
                <ManagedBadge key={o.id} source={o.name} reason={o.reason} />
              ))}
            </div>
          </div>
          <div className="flex flex-col gap-3">
            {editable.map((o) => (
              <OrgTermsEditor
                key={o.id}
                org={o}
                named={orgs.length + outsideConfig.length > 1}
                onEdited={adminContent.reload}
              />
            ))}
            {locked.length > 0 && (
              <div className="rounded-[var(--radius-card)] border border-[var(--color-line-visible)] bg-[var(--color-surface)] px-4 py-3.5">
                <ul className="flex flex-wrap gap-1.5" aria-label="Organisation terms">
                  {locked.map((term) => (
                    <li
                      key={term}
                      className="inline-flex items-center gap-1 px-2 py-1 rounded-md text-xs bg-[var(--color-pill-hover)] text-[var(--color-text-muted)]"
                    >
                      <Lock size={10} strokeWidth={2} aria-hidden />
                      {term}
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {twoFactorLocked.map((o) => (
              <p key={o.id} className="text-xs text-[var(--color-text-muted)]">
                {o.name} requires two-factor authentication to edit its terms. Set it up in Settings → Account.
              </p>
            ))}
          </div>
        </section>
      )}
      <Section title="Your terms">
        <div className="flex flex-col gap-3 py-3.5">
          {terms.length > 0 && (
            <ul className="flex flex-wrap gap-1.5" aria-label="Your terms">
              {terms.map((term) => (
                <li
                  key={term}
                  className="inline-flex items-center gap-1 pl-2 pr-1 py-1 rounded-md text-xs bg-[var(--color-pill-hover)] text-[var(--color-text)]"
                >
                  {term}
                  <button
                    type="button"
                    aria-label={`Remove ${term}`}
                    onClick={() => remove(term)}
                    className="grid place-items-center w-4 h-4 rounded text-[var(--color-text-muted)] hover:text-[var(--color-text)]"
                  >
                    <X size={11} strokeWidth={2} aria-hidden />
                  </button>
                </li>
              ))}
            </ul>
          )}
          <div className="flex gap-2">
            <input
              type="text"
              aria-label="Add a term"
              placeholder="Add a term"
              value={draft}
              disabled={full}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  add();
                }
              }}
              className={inputClass}
            />
            <button
              type="button"
              className="nd-btn"
              disabled={full || candidates.length === 0}
              onClick={add}
            >
              Add
            </button>
          </div>
          <p className="text-xs text-[var(--color-text-muted)]">Limit: {VOCABULARY_LIMIT}</p>
        </div>
      </Section>
    </>
  );
}

/** An administered organisation's terms as the server holds them, edited
 *  there and read again through `onEdited`. */
function OrgTermsEditor({
  org,
  named,
  onEdited,
}: {
  org: OrgSummary;
  named: boolean;
  onEdited: () => void;
}) {
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const terms = storedTerms(org.vocabulary);
  // A stored term may hold a comma ("Smith, John"), so only lines split.
  const candidates = draft
    .split("\n")
    .map((t) => t.trim())
    .filter((t) => t.length >= MIN_TERM_LENGTH);
  const full = terms.length >= VOCABULARY_LIMIT;

  async function edit(add: string[], remove: string[]) {
    setBusy(true);
    try {
      await ipc.orgVocabularyEdit(org.id, add, remove);
      setError(null);
      onEdited();
      if (add.length > 0) setDraft("");
    } catch (e) {
      setError(String(e));
      onEdited();
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-3 rounded-[var(--radius-card)] border border-[var(--color-line-visible)] bg-[var(--color-surface)] px-4 py-3.5">
      {named && <h3 className="text-xs font-medium text-[var(--color-text-muted)]">{org.name}</h3>}
      {terms.length > 0 && (
        <ul className="flex flex-wrap gap-1.5" aria-label={`${org.name} terms`}>
          {terms.map((term) => (
            <li
              key={term}
              className="inline-flex items-center gap-1 pl-2 pr-1 py-1 rounded-md text-xs bg-[var(--color-pill-hover)] text-[var(--color-text)]"
            >
              {term}
              <button
                type="button"
                aria-label={`Remove ${term} from ${org.name}`}
                disabled={busy}
                onClick={() => void edit([], [term])}
                className="grid place-items-center w-4 h-4 rounded text-[var(--color-text-muted)] hover:text-[var(--color-text)]"
              >
                <X size={11} strokeWidth={2} aria-hidden />
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="flex gap-2">
        <input
          type="text"
          aria-label={`Add a term for ${org.name}`}
          placeholder={`Add a term for ${org.name}`}
          value={draft}
          disabled={full || busy}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && candidates.length > 0) {
              e.preventDefault();
              void edit(candidates, []);
            }
          }}
          className={inputClass}
        />
        <button
          type="button"
          className="nd-btn"
          disabled={full || busy || candidates.length === 0}
          onClick={() => void edit(candidates, [])}
        >
          Add
        </button>
      </div>
      {error && (
        <p role="alert" className="text-xs" style={{ color: "var(--color-danger)" }}>
          {error}
        </p>
      )}
    </div>
  );
}
