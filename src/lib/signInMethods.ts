/** The address to ask the server about, or null while the field doesn't hold
 *  one yet. */
export function lookupEmail(email: string): string | null {
  const trimmed = email.trim().toLowerCase();
  return /^[^\s@]+@[^\s@]+\.[^\s@.]{2,}$/.test(trimmed) ? trimmed : null;
}

/** What the server has said about Microsoft sign-in so far: `everyone` is its
 *  answer with no email, `answer` the latest email lookup's, and `asking` the
 *  address that lookup was for. */
export type MicrosoftLookup = {
  everyone: boolean;
  latest: number;
  asking: string | null;
  answer: { email: string; microsoft: boolean } | null;
};

export const NO_LOOKUP: MicrosoftLookup = {
  everyone: false,
  latest: 0,
  asking: null,
  answer: null,
};

/** Whether `email` still needs asking about. */
export function needsLookup(state: MicrosoftLookup, email: string): boolean {
  const key = lookupEmail(email);
  return !state.everyone && key !== null && key !== state.asking;
}

/** Start a lookup for `email`; settle it with the returned sequence number. */
export function startLookup(state: MicrosoftLookup, email: string): [MicrosoftLookup, number] {
  const seq = state.latest + 1;
  return [{ ...state, latest: seq, asking: lookupEmail(email) }, seq];
}

/** An answer counts only when no lookup started after it. */
export function settleLookup(
  state: MicrosoftLookup,
  seq: number,
  microsoft: boolean,
): MicrosoftLookup {
  if (seq !== state.latest || state.asking === null) return state;
  return { ...state, answer: { email: state.asking, microsoft } };
}

export function microsoftOffered(state: MicrosoftLookup, email: string): boolean {
  if (state.everyone) return true;
  const key = lookupEmail(email);
  return key !== null && state.answer?.email === key && state.answer.microsoft;
}
