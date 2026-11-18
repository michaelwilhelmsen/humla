import { useCallback, useEffect, useRef, useState } from "react";
import { cloudApi, cloudError, useCloudStore, type SecondFactor } from "./cloud";
import {
  lookupEmail,
  microsoftOffered,
  needsLookup,
  NO_LOOKUP,
  settleLookup,
  startLookup,
  type MicrosoftLookup,
} from "./signInMethods";

const LOOKUP_DELAY_MS = 400;

/**
 * Signing in to the cloud, for every surface that does it (Settings → Account,
 * onboarding, the new-workspace sheet). An account with two-factor
 * authentication answers the password, or a Microsoft sign-in, with a `code`
 * step, which the surface renders as `TwoFactorCodeForm`. Each action resolves
 * true once the user is signed in and the cloud store has caught up.
 *
 * `email` is the form's email field: "Sign in with Microsoft" shows when the
 * server offers it to everyone or to that address's domain, and the address
 * prefills Microsoft's sign-in page. Call `emailBlurred` from the field.
 */
export function useCloudSignIn(email = "") {
  const refresh = useCloudStore((s) => s.refresh);
  const signedOutServer = useCloudStore((s) =>
    s.status.configured && !s.status.logged_in ? s.status.base_url : "",
  );
  const [step, setStep] = useState<"password" | "code">("password");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lookup, setLookup] = useState<MicrosoftLookup>(NO_LOOKUP);
  const lookupRef = useRef(lookup);
  const [waitingForMicrosoft, setWaitingForMicrosoft] = useState(false);
  const waiting = useRef(false);

  const updateLookup = useCallback((next: MicrosoftLookup) => {
    lookupRef.current = next;
    setLookup(next);
  }, []);

  // A new server starts over; `latest` keeps counting so an answer from the
  // last one can't settle a lookup made against this one.
  useEffect(() => {
    const reset = () => updateLookup({ ...NO_LOOKUP, latest: lookupRef.current.latest });
    reset();
    if (!signedOutServer) return;
    let stale = false;
    cloudApi
      .signInMethods()
      .then((m) => !stale && updateLookup({ ...lookupRef.current, everyone: m.microsoft }))
      .catch(() => {});
    return () => {
      stale = true;
      reset();
    };
  }, [signedOutServer, updateLookup]);

  const lookUp = useCallback(
    (address: string) => {
      if (!signedOutServer || !needsLookup(lookupRef.current, address)) return;
      const [started, seq] = startLookup(lookupRef.current, address);
      updateLookup(started);
      cloudApi
        .signInMethods(started.asking ?? undefined)
        .catch(() => ({ microsoft: false }))
        .then((m) => updateLookup(settleLookup(lookupRef.current, seq, m.microsoft)));
    },
    [signedOutServer, updateLookup],
  );

  const emailKey = lookupEmail(email);
  useEffect(() => {
    if (!emailKey) return;
    const timer = setTimeout(() => lookUp(emailKey), LOOKUP_DELAY_MS);
    return () => clearTimeout(timer);
  }, [emailKey, lookUp]);

  const emailBlurred = () => lookUp(email);

  // Leaving the form ends a sign-in still waiting on the browser.
  useEffect(
    () => () => {
      if (waiting.current) void cloudApi.cancelMicrosoft();
    },
    [],
  );

  async function run(action: () => Promise<boolean>): Promise<boolean> {
    setBusy(true);
    setError(null);
    try {
      return await action();
    } finally {
      setBusy(false);
    }
  }

  const signIn = (email: string, password: string) =>
    run(async () => {
      try {
        const result = await cloudApi.login(email, password);
        if (result.step === "two_factor") {
          setStep("code");
          return false;
        }
      } catch (e) {
        setError(cloudError(e).message);
        return false;
      }
      await refresh();
      return true;
    });

  const signInWithMicrosoft = () =>
    run(async () => {
      waiting.current = true;
      setWaitingForMicrosoft(true);
      try {
        const result = await cloudApi.loginMicrosoft(emailKey ? email.trim() : undefined);
        if (result.step === "two_factor") {
          setStep("code");
          return false;
        }
      } catch (e) {
        const failure = cloudError(e);
        if (failure.reason !== "cancelled") setError(failure.message);
        return false;
      } finally {
        waiting.current = false;
        setWaitingForMicrosoft(false);
      }
      await refresh();
      return true;
    });

  const cancelMicrosoft = () => void cloudApi.cancelMicrosoft();

  const signUp = (email: string, password: string, name: string) =>
    run(async () => {
      try {
        await cloudApi.signup(email, password, name);
      } catch (e) {
        setError(cloudError(e).message);
        return false;
      }
      await refresh();
      return true;
    });

  const verify = (factor: SecondFactor) =>
    run(async () => {
      try {
        await cloudApi.verifyTwoFactor(factor);
      } catch (e) {
        const failure = cloudError(e);
        // The sign-in it answered is gone; only a new sign-in starts another.
        if (failure.reason === "challenge_expired") setStep("password");
        setError(failure.message);
        return false;
      }
      setStep("password");
      await refresh();
      return true;
    });

  function back() {
    setStep("password");
    setError(null);
  }

  return {
    step,
    busy,
    error,
    setError,
    signIn,
    signUp,
    verify,
    back,
    microsoftOffered: microsoftOffered(lookup, email),
    emailBlurred,
    waitingForMicrosoft,
    signInWithMicrosoft,
    cancelMicrosoft,
  };
}
