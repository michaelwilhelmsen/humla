import { Routes, Route, Navigate } from "react-router-dom";
import { useEffect } from "react";
import { Layout } from "./components/Layout";
import { Home } from "./pages/Home";
import { Note } from "./pages/Note";
import { Folder } from "./pages/Folder";
import { Settings } from "./pages/Settings";
import { useGlobalShortcuts } from "./lib/shortcuts";
import { useNotesStore } from "./lib/store";
import { useThemeBoot } from "./lib/theme";
import { usePaletteBoot } from "./lib/palette";

export default function App() {
  useGlobalShortcuts();
  useThemeBoot();
  usePaletteBoot();
  const refresh = useNotesStore((s) => s.refresh);

  useEffect(() => {
    refresh();
  }, [refresh]);

  return (
    <Routes>
      <Route element={<Layout />}>
        <Route path="/" element={<Home />} />
        <Route path="/note/:id" element={<Note />} />
        <Route path="/folder/:id" element={<Folder />} />
        <Route path="/settings" element={<Settings />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  );
}
