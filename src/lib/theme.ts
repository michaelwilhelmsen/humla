import { useEffect } from "react";
import { create } from "zustand";
import { ipc } from "./ipc";

export type Theme = "system" | "light" | "dark";

type ThemeState = {
  theme: Theme;
  setTheme: (t: Theme) => Promise<void>;
  hydrate: () => Promise<void>;
};

function apply(theme: Theme) {
  const root = document.documentElement;
  if (theme === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", theme);
}

export const useThemeStore = create<ThemeState>((set) => ({
  theme: "system",
  setTheme: async (theme) => {
    apply(theme);
    set({ theme });
    await ipc.setSetting("theme", theme);
  },
  hydrate: async () => {
    const stored = (await ipc.getSetting("theme")) as Theme | null;
    const theme: Theme = stored === "light" || stored === "dark" || stored === "system" ? stored : "system";
    apply(theme);
    set({ theme });
  },
}));

export function useThemeBoot() {
  const hydrate = useThemeStore((s) => s.hydrate);
  useEffect(() => { hydrate(); }, [hydrate]);
}
