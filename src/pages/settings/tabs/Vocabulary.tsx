import { useState } from "react";
import { X } from "lucide-react";
import { Section } from "../components/Section";
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

export function VocabularySection({ s, update }: Pick<SettingsHook, "s" | "update">) {
  const terms = parseVocabulary(s.custom_vocabulary);
  const [draft, setDraft] = useState("");
  const full = terms.length >= VOCABULARY_LIMIT;

  const candidates = draft
    .split(/[\n,]/)
    .map((t) => t.trim())
    .filter((t) => t.length >= MIN_TERM_LENGTH);

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
        <p className="text-xs text-[var(--color-text-muted)]">
          <code>gpt-4o-transcribe-diarize</code> ignores them.
        </p>
      </div>
    </Section>
  );
}
