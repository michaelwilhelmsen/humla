import { create } from "zustand";
import { getCurrent, onOpenUrl } from "@tauri-apps/plugin-deep-link";
import type { CheckoutSource } from "./cloud";

/**
 * What a `humla://` link asks for. The backend acts on every link itself — it
 * brings the window forward, or starts or stops a recording (`deep_link.rs`) —
 * so anything unrecognised here is a `focus` and needs no further work.
 */
export type DeepLinkAction =
  | { kind: "new-workspace"; source?: CheckoutSource }
  | { kind: "focus" };

/** The only attributions a link may claim; it comes from outside the app. */
const LINK_SOURCES = ["email", "web"] as const satisfies readonly CheckoutSource[];

export function linkSource(value: string | null | undefined): CheckoutSource | undefined {
  return (LINK_SOURCES as readonly string[]).includes(value ?? "")
    ? (value as CheckoutSource)
    : undefined;
}

/** Parse a `humla://` URL; null for anything that isn't one. */
export function parseDeepLink(raw: string): DeepLinkAction | null {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return null;
  }
  if (url.protocol !== "humla:") return null;
  // `humla://new-workspace` puts the route in the host; `humla:///new-workspace`
  // and `humla:new-workspace` put it in the path. `deep_link::parse` (Rust)
  // reads the recording routes the same way; change both.
  const route = `${url.host}${url.pathname}`.replace(/^\/+|\/+$/g, "").toLowerCase();
  if (route === "new-workspace") {
    return { kind: "new-workspace", source: linkSource(url.searchParams.get("source")) };
  }
  return { kind: "focus" };
}

type DeepLinkState = {
  /** A "Create team workspace" request not yet picked up by the switcher. It
   *  waits here through onboarding, because the switcher isn't mounted then. */
  newWorkspace: { source?: CheckoutSource } | null;
  take: () => { source?: CheckoutSource } | null;
};

export const useDeepLinkStore = create<DeepLinkState>((set, get) => ({
  newWorkspace: null,
  take: () => {
    const req = get().newWorkspace;
    if (req) set({ newWorkspace: null });
    return req;
  },
}));

export function handleDeepLinks(urls: readonly string[] | null | undefined) {
  for (const raw of urls ?? []) {
    const action = parseDeepLink(raw);
    if (action?.kind === "new-workspace") {
      useDeepLinkStore.setState({ newWorkspace: { source: action.source } });
    }
  }
}

let bound = false;

/** Listen for links while running, and pick up the one that launched the app. */
export async function bindDeepLinks() {
  if (bound) return;
  bound = true;
  // A launch link can arrive as an event after the listener is up AND be
  // returned by getCurrent, so the stored one is only used if no event came.
  let heard = false;
  try {
    await onOpenUrl((urls) => {
      heard = true;
      handleDeepLinks(urls);
    });
    const initial = await getCurrent();
    if (!heard) handleDeepLinks(initial);
  } catch (e) {
    console.warn("deep links unavailable", e);
  }
}
