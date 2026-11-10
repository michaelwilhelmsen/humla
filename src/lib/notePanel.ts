/** The note view's right context panel: its width bounds, and the open state
 * and width the user last chose, which every note opens with. Device-local, so
 * localStorage rather than the settings table. */

/** `PANEL_FLOOR` is below `PANEL_MIN` on purpose. On a window too narrow to
 * give both columns their nominal minimum, a panel a little under its
 * comfortable width beats a body column that cannot hold its own toolbar. */
export const PANEL_MIN = 320;
export const PANEL_FLOOR = 260;
export const PANEL_MAX = 720;
export const PANEL_DEFAULT_WIDTH = 440;

const OPEN_KEY = "humla.panelOpen";
const WIDTH_KEY = "humla.panelWidth";

/** Open unless the user last closed it. */
export function resolveStoredPanelOpen(raw: string | null): boolean {
  return raw !== "false";
}

/** Bounded by `PANEL_FLOOR`, not `PANEL_MIN`: a width saved from a cramped
 * window is legitimately under the nominal minimum. */
export function resolveStoredPanelWidth(raw: string | null): number {
  const saved = raw == null || raw === "" ? NaN : Number(raw);
  return saved >= PANEL_FLOOR && saved <= PANEL_MAX ? saved : PANEL_DEFAULT_WIDTH;
}

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* storage unavailable: the choice lasts this session only */
  }
}

export const readPanelOpen = (): boolean => resolveStoredPanelOpen(read(OPEN_KEY));
export const writePanelOpen = (open: boolean): void => write(OPEN_KEY, String(open));
export const readPanelWidth = (): number => resolveStoredPanelWidth(read(WIDTH_KEY));
export const writePanelWidth = (width: number): void => write(WIDTH_KEY, String(Math.round(width)));
