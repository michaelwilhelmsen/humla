import { describe, it, expect } from "vitest";
import { offeredIn, orgTerms, pickerTemplates, templateOptions } from "./orgContent";
import { SUMMARY_PRESETS, presetLabel } from "./presets";
import type { SummaryPrompt } from "./ipc";

function prompt(id: string, extra: Partial<SummaryPrompt> = {}): SummaryPrompt {
  return { id, name: id, content: "", created_at: 0, updated_at: 0, workspace_id: "", ...extra };
}

const everyone = prompt("org-all", { org_id: "o1", org_workspaces: [] });
const intake = prompt("org-w1", { org_id: "o1", org_workspaces: ["w1"] });
const mine = prompt("mine");
const shared = prompt("shared", { workspace_id: "w1" });

describe("offeredIn", () => {
  it("offers a template with no departments everywhere, Personal included", () => {
    expect(offeredIn(everyone, "w1")).toBe(true);
    expect(offeredIn(everyone, "")).toBe(true);
  });

  it("offers a department template only in its departments", () => {
    expect(offeredIn(intake, "w1")).toBe(true);
    expect(offeredIn(intake, "w2")).toBe(false);
    expect(offeredIn(intake, "")).toBe(false);
  });
});

describe("pickerTemplates", () => {
  it("splits organisation templates offered here from the user's own", () => {
    expect(pickerTemplates([mine, intake, everyone, shared], "w2")).toEqual({
      org: [everyone],
      own: [mine, shared],
    });
  });
});

describe("templateOptions", () => {
  const builtIns = SUMMARY_PRESETS.map((p) => presetLabel(p));

  it("lists organisation templates, then built-ins, then the user's own", () => {
    const options = templateOptions([mine, intake, everyone], "w1");
    expect(options.map((o) => o.label)).toEqual(["org-w1", "org-all", ...builtIns, "mine"]);
    expect(options.filter((o) => o.separatorBefore).map((o) => o.label)).toEqual([builtIns[0], "mine"]);
  });

  it("is unchanged without an organisation: built-ins, then a divider and the user's own", () => {
    const options = templateOptions([mine, shared], "w1");
    expect(options).toEqual([
      ...SUMMARY_PRESETS.map((p) => ({ value: p.value, label: presetLabel(p) })),
      { value: "custom:mine", label: "mine", separatorBefore: true },
      { value: "custom:shared", label: "shared" },
    ]);
  });
});

describe("orgTerms", () => {
  it("joins every organisation's terms without duplicates regardless of case", () => {
    expect(
      orgTerms([
        { id: "o1", name: "A", reason: "", vocabulary: ["Northwind", " Fjord ", ""] },
        { id: "o2", name: "B", reason: "", vocabulary: ["fjord", "Harbour"] },
      ]),
    ).toEqual(["Northwind", "Fjord", "Harbour"]);
  });
});
