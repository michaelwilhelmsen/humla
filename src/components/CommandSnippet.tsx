import { useState } from "react";
import { Check, Copy } from "lucide-react";

// A shell command in a monospace pill with a copy-to-clipboard button.
// Shared by the settings provider tabs (Chat, Summaries) so an
// `ollama pull …` command is one click to copy. The onboarding wizard has
// its own inline variant (staged card layout); this is the settings-row one.
export function CommandSnippet({
  command,
  ariaLabel = "Copy command",
}: {
  command: string;
  ariaLabel?: string;
}) {
  const [copied, setCopied] = useState(false);

  function copy() {
    navigator.clipboard?.writeText(command).then(
      () => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      },
      () => {
        /* clipboard blocked — non-fatal */
      },
    );
  }

  return (
    <div className="flex items-center gap-2">
      <code
        className="flex-1 min-w-0 truncate px-3 py-2 rounded-md text-xs bg-[var(--color-pill-hover)]"
        style={{ fontFamily: "var(--font-code)" }}
      >
        {command}
      </code>
      <button type="button" onClick={copy} className="nd-btn shrink-0" aria-label={ariaLabel}>
        {copied ? (
          <>
            <Check size={13} strokeWidth={2.5} />
            Copied
          </>
        ) : (
          <>
            <Copy size={13} strokeWidth={2} />
            Copy
          </>
        )}
      </button>
    </div>
  );
}
