import {
  Routes,
  Route,
  Navigate,
  useNavigate,
  useLocation,
  type Location,
} from "react-router-dom";
import { useEffect, useState } from "react";
import { Layout } from "./components/Layout";
import { Home } from "./pages/Home";
import { AllNotes } from "./pages/AllNotes";
import Chat from "./pages/Chat";
import { Note } from "./pages/Note";
import { Folder } from "./pages/Folder";
import { Trash } from "./pages/Trash";
import { SettingsDialog } from "./pages/settings/SettingsDialog";
import { Onboarding } from "./pages/onboarding";
import { ipc } from "./lib/ipc";
import { useGlobalShortcuts } from "./lib/shortcuts";
import { bindBackendListeners, useNotesStore } from "./lib/store";
import { useCloudStore } from "./lib/cloud";
import { useThemeBoot } from "./lib/theme";
import { usePaletteBoot } from "./lib/palette";

export default function App() {
  useGlobalShortcuts();
  useThemeBoot();
  usePaletteBoot();
  const refresh = useNotesStore((s) => s.refresh);
  const refreshCloud = useCloudStore((s) => s.refresh);

  // Takeover guard. `null` = still reading the flag (render nothing — no flash
  // of either the app or the wizard); `false` = show the first-run wizard
  // instead of the normal app; `true` = normal app. The grandfathering
  // migration (src-tauri/src/db.rs) has already set the flag for existing
  // installs before the frontend ever reads it, so only genuinely fresh
  // installs land in the `false` branch.
  const [onboardingDone, setOnboardingDone] = useState<boolean | null>(null);

  useEffect(() => {
    let cancelled = false;
    ipc
      .getSetting("onboarding_completed")
      .then((v) => {
        if (!cancelled) setOnboardingDone(v === "true");
      })
      .catch(() => {
        // If we can't read the flag, fail open to the app rather than trapping
        // the user in a wizard we can't dismiss.
        if (!cancelled) setOnboardingDone(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    // Bind global backend listeners here — NOT only in Layout — so they're
    // live during the first-run onboarding takeover too (Layout isn't in the
    // tree then, and e.g. the wizard's model-download progress feeds
    // useDownloadStore through these). Idempotent via a module-level flag, so
    // Layout's own call stays a harmless no-op.
    bindBackendListeners();
    refresh();
    refreshCloud();
  }, [refresh, refreshCloud]);

  // Still resolving the flag — render nothing to avoid a flash of the wrong UI.
  if (onboardingDone === null) {
    return <div className="h-full w-full bg-[var(--color-canvas)]" />;
  }

  // Fresh install: full-window takeover. Completing swaps to the normal app in
  // place (no reload) via the state flip.
  if (!onboardingDone) {
    return <OnboardingTakeover onDone={() => setOnboardingDone(true)} />;
  }

  return <AppRoutes />;
}

// Home stand-in when `/settings` is opened with no prior in-app view
// (fresh launch on the settings URL): there's nothing to restore behind
// the dialog, so it dims Home.
const HOME_FALLBACK = {
  pathname: "/",
  search: "",
  hash: "",
  state: null,
  key: "settings-home-fallback",
} as Location;

function AppRoutes() {
  const location = useLocation();
  const navigate = useNavigate();
  // `/settings` renders as a dialog OVER the app, not as a page swap: the
  // routed view underneath keeps showing the last non-settings location.
  const isSettings = location.pathname.startsWith("/settings");
  const [background, setBackground] = useState<Location | null>(null);
  useEffect(() => {
    if (!isSettings) setBackground(location);
  }, [location, isSettings]);

  const displayLocation = isSettings ? (background ?? HOME_FALLBACK) : location;

  // Closing pops the history entry the entry point pushed, landing back on
  // the view behind the dialog. Opened cold (no background) there's nothing
  // to pop to — replace with Home.
  const closeSettings = () => {
    if (background) navigate(-1);
    else navigate("/", { replace: true });
  };

  return (
    <>
      <Routes location={displayLocation}>
        {/* Manual re-run entry (nag chip / "Run setup again", built by a later
            package). Same component; navigates home on completion. Lives outside
            <Layout> so it's also a full-window takeover. */}
        <Route path="/onboarding" element={<OnboardingRoute />} />
        <Route element={<Layout />}>
          <Route path="/" element={<Home />} />
          <Route path="/all-notes" element={<AllNotes />} />
          {/* Library-level destination, same tier as "All notes" (#95). */}
          <Route path="/chat" element={<Chat />} />
          <Route path="/note/:id" element={<Note />} />
          <Route path="/folder/:id" element={<Folder />} />
          <Route path="/trash" element={<Trash />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Route>
      </Routes>
      {isSettings && <SettingsDialog onClose={closeSettings} />}
    </>
  );
}

// Fresh-install takeover wrapper. Completing flips the guard state (so the
// normal app renders in place, no reload). If the final step passed a
// destination ("/note/<id>" from "Create your first note"), navigate there
// BEFORE flipping the guard so <Routes> mounts already pointing at the note —
// no flash of Home in between.
function OnboardingTakeover({ onDone }: { onDone: () => void }) {
  const navigate = useNavigate();
  return (
    <Onboarding
      onDone={(destination) => {
        if (destination) navigate(destination, { replace: true });
        onDone();
      }}
    />
  );
}

// Wrapper for the manual `/onboarding` route: resumes from live state and
// returns to the home screen when the wizard finishes or is skipped — unless
// the final step handed us a destination (the "Create your first note" CTA),
// which overrides the default so it can't be clobbered.
function OnboardingRoute() {
  const navigate = useNavigate();
  return (
    <Onboarding
      onDone={(destination) => navigate(destination ?? "/", { replace: true })}
    />
  );
}
