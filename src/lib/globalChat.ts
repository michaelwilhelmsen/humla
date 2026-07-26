// Where the `/chat` pane publishes its session projection so the SIDEBAR can
// render the conversation list (issue #95).
//
// The list belongs in the left nav — it is navigation, not context about the
// thing on screen — but the nav is a sibling of the routed page under `Layout`,
// so the two cannot be wired together with props.
//
// Deliberately just a mailbox: no fetching, no derived state, no second copy of
// anything. `ChatPanel` stays the sole owner of the conversations, the active id
// and the paging; this holds the last projection it published, and the page
// clears it on unmount so a stale list can't outlive the pane that owned it.

import { create } from "zustand";
import type { ChatSessionControls } from "../components/ChatPanel";

type GlobalChatState = {
  controls: ChatSessionControls | null;
  setControls: (controls: ChatSessionControls | null) => void;
};

export const useGlobalChatStore = create<GlobalChatState>((set) => ({
  controls: null,
  setControls: (controls) => set({ controls }),
}));
