import { describe, expect, it, vi } from "vitest";
import { mockTauri } from "../test/tauri";
import { recordsExports, recordsOpens, reportCopied } from "./audit";

const scope = { w1: true, w2: false };

describe("the audit scope", () => {
  it("records exports for every organisation workspace, opens only under audit_reads", () => {
    expect([recordsExports(scope, "w1"), recordsOpens(scope, "w1")]).toEqual([true, true]);
    expect([recordsExports(scope, "w2"), recordsOpens(scope, "w2")]).toEqual([true, false]);
  });

  it("records nothing for Personal, a workspace in no organisation, or no organisation at all", () => {
    for (const ws of ["", " ", null, undefined, "w3", "constructor"]) {
      expect(recordsExports(scope, ws)).toBe(false);
      expect(recordsOpens(scope, ws)).toBe(false);
    }
    expect(recordsExports({}, "w1")).toBe(false);
    expect(recordsOpens({}, "w1")).toBe(false);
  });
});

describe("reportCopied", () => {
  it("calls the backend only for an organisation note", async () => {
    const copied = vi.fn(() => null);
    mockTauri({ audit_note_copied: copied });
    reportCopied(scope, { id: "n1", workspace_id: "" });
    reportCopied(scope, { id: "n2", workspace_id: "w3" });
    reportCopied({}, { id: "n3", workspace_id: "w1" });
    reportCopied(scope, { id: "n4", workspace_id: "w2" });
    await vi.waitFor(() => expect(copied).toHaveBeenCalledTimes(1));
    expect(copied).toHaveBeenCalledWith({ noteId: "n4" });
  });
});
