import { beforeEach, describe, expect, it } from "vitest";
import {
  PANEL_DEFAULT_WIDTH,
  PANEL_FLOOR,
  PANEL_MAX,
  readPanelOpen,
  readPanelWidth,
  resolveStoredPanelOpen,
  resolveStoredPanelWidth,
  writePanelOpen,
  writePanelWidth,
} from "./notePanel";

beforeEach(() => localStorage.clear());

describe("resolveStoredPanelOpen", () => {
  it("is open when nothing was stored", () => {
    expect(resolveStoredPanelOpen(null)).toBe(true);
  });

  it("is closed only when the user last closed it", () => {
    expect(resolveStoredPanelOpen("false")).toBe(false);
    expect(resolveStoredPanelOpen("true")).toBe(true);
    expect(resolveStoredPanelOpen("garbage")).toBe(true);
  });
});

describe("resolveStoredPanelWidth", () => {
  it("falls back to the default for missing or garbage values", () => {
    expect(resolveStoredPanelWidth(null)).toBe(PANEL_DEFAULT_WIDTH);
    expect(resolveStoredPanelWidth("")).toBe(PANEL_DEFAULT_WIDTH);
    expect(resolveStoredPanelWidth("wide")).toBe(PANEL_DEFAULT_WIDTH);
  });

  it("keeps a width saved from a cramped window, down to the floor", () => {
    expect(resolveStoredPanelWidth(String(PANEL_FLOOR))).toBe(PANEL_FLOOR);
    expect(resolveStoredPanelWidth("300")).toBe(300);
    expect(resolveStoredPanelWidth(String(PANEL_MAX))).toBe(PANEL_MAX);
  });

  it("rejects a width outside the bounds", () => {
    expect(resolveStoredPanelWidth(String(PANEL_FLOOR - 1))).toBe(PANEL_DEFAULT_WIDTH);
    expect(resolveStoredPanelWidth(String(PANEL_MAX + 1))).toBe(PANEL_DEFAULT_WIDTH);
  });
});

describe("stored panel state", () => {
  it("round-trips open state and width", () => {
    writePanelOpen(false);
    writePanelWidth(512.4);
    expect(readPanelOpen()).toBe(false);
    expect(readPanelWidth()).toBe(512);

    writePanelOpen(true);
    expect(readPanelOpen()).toBe(true);
  });
});
