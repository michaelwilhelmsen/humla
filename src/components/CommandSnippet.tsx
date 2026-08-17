import { useState } from "react";
import { Check, Copy } from "lucide-react";

// A shell command in a monospace pill with a copy-to-clipboard button.
// Shared by the settings provider tabs (Chat, Summaries) so an
// `ollama pull …` command is one click to copy. The onboarding wizard has
// its own inline variant (staged card layout); this is the settings-row one.
export function CommandSnippet({
  command,
  ariaLabel = "Copy command",
  block = false,
}: {
  command: string;
  ariaLabel?: string;
  /** Render as a wrapped multi-line block instead of a single truncated line.
   *  A config file stanza (#172's Codex snippet) is several lines and every one
   *  of them matters, so truncating it to one row would hide the part the user
   *  has to paste. */
  block?: boolean;
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
    <div className={block ? "flex items-start gap-2" : "flex items-center gap-2"}>
      <code
        // .nd-code carries the theme's mono face AND its command size — this is
        // the surface graphite's "DM Mono 14px" is about.
        className={
          "nd-code flex-1 min-w-0 px-3 py-2 rounded-md bg-[var(--color-pill-hover)] " +
          (block ? "whitespace-pre-wrap break-words" : "truncate")
        }
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
