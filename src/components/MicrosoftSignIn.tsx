import type { useCloudSignIn } from "../lib/useCloudSignIn";

function MicrosoftLogo() {
  return (
    <svg width="14" height="14" viewBox="0 0 21 21" aria-hidden="true">
      <rect x="1" y="1" width="9" height="9" fill="#f25022" />
      <rect x="11" y="1" width="9" height="9" fill="#7fba00" />
      <rect x="1" y="11" width="9" height="9" fill="#00a4ef" />
      <rect x="11" y="11" width="9" height="9" fill="#ffb900" />
    </svg>
  );
}

/** "Sign in with Microsoft", shown only when the server offers it; while the
 *  browser is out, a line saying so with a way to give up. */
export function MicrosoftSignIn({ auth }: { auth: ReturnType<typeof useCloudSignIn> }) {
  if (!auth.microsoftOffered) return null;
  if (auth.waitingForMicrosoft) {
    return (
      <div className="flex items-center gap-3 text-xs text-[var(--color-text-muted)]">
        <span>Continue in your browser…</span>
        <button
          type="button"
          onClick={auth.cancelMicrosoft}
          className="text-xs text-[var(--color-interactive)] hover:underline"
        >
          Cancel
        </button>
      </div>
    );
  }
  return (
    <div>
      <button
        type="button"
        onClick={() => void auth.signInWithMicrosoft()}
        disabled={auth.busy}
        className="nd-btn"
      >
        <MicrosoftLogo />
        Sign in with Microsoft
      </button>
    </div>
  );
}
