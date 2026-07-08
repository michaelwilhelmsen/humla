import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, useLocation } from "react-router-dom";
import App from "../App";
import { mockTauri } from "./tauri";

// Surfaces the real router URL so tests can assert on it — the settings
// dialog is route-backed, so open/close must be visible in the location.
export function LocationProbe() {
  const loc = useLocation();
  return <div data-testid="location">{loc.pathname + loc.search}</div>;
}

export function renderApp(
  initialPath = "/",
  handlers: Parameters<typeof mockTauri>[0] = {},
) {
  mockTauri(handlers);
  return render(
    <MemoryRouter initialEntries={[initialPath]}>
      <App />
      <LocationProbe />
    </MemoryRouter>,
  );
}

export async function openSettingsFromSidebar() {
  await userEvent.click(screen.getByRole("link", { name: /settings/i }));
  return await screen.findByRole("dialog", { name: /settings/i });
}
