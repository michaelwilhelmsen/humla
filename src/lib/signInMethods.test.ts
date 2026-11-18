import { describe, it, expect } from "vitest";
import {
  lookupEmail,
  microsoftOffered,
  needsLookup,
  NO_LOOKUP,
  settleLookup,
  startLookup,
  type MicrosoftLookup,
} from "./signInMethods";

describe("lookupEmail", () => {
  it("asks only about something that looks like an address", () => {
    expect(lookupEmail(" Kari@Rana.Kommune.no ")).toBe("kari@rana.kommune.no");
    expect(lookupEmail("kari+humla@rana.kommune.no")).toBe("kari+humla@rana.kommune.no");
    for (const partial of ["", "kari", "kari@", "kari@rana", "kari@rana.", "kari@rana.k", "a b@c.no"]) {
      expect(lookupEmail(partial)).toBeNull();
    }
  });
});

function answer(state: MicrosoftLookup, email: string, microsoft: boolean): MicrosoftLookup {
  const [started, seq] = startLookup(state, email);
  return settleLookup(started, seq, microsoft);
}

describe("Microsoft lookups", () => {
  it("offers Microsoft for a matching domain and hides it for another", () => {
    let s = answer(NO_LOOKUP, "kari@rana.kommune.no", true);
    expect(microsoftOffered(s, "kari@rana.kommune.no")).toBe(true);
    expect(microsoftOffered(s, "KARI@rana.kommune.no ")).toBe(true);
    expect(microsoftOffered(s, "ola@gmail.com")).toBe(false);
    expect(microsoftOffered(s, "kari@rana.kommune")).toBe(false);
    s = answer(s, "ola@gmail.com", false);
    expect(microsoftOffered(s, "ola@gmail.com")).toBe(false);
  });

  it("lets only the latest lookup win", () => {
    const [a, seqA] = startLookup(NO_LOOKUP, "kari@rana.kommune.no");
    const [b, seqB] = startLookup(a, "ola@gmail.com");
    // The older lookup answers last.
    const s = settleLookup(settleLookup(b, seqB, false), seqA, true);
    expect(s.answer).toEqual({ email: "ola@gmail.com", microsoft: false });
    expect(microsoftOffered(s, "kari@rana.kommune.no")).toBe(false);
    expect(microsoftOffered(s, "ola@gmail.com")).toBe(false);

    // A stale yes for gmail never shows while the user is back on the matching domain.
    const [c, seqC] = startLookup(NO_LOOKUP, "ola@gmail.com");
    const [d, seqD] = startLookup(c, "kari@rana.kommune.no");
    const t = settleLookup(settleLookup(d, seqC, true), seqD, true);
    expect(microsoftOffered(t, "kari@rana.kommune.no")).toBe(true);
    expect(microsoftOffered(t, "ola@gmail.com")).toBe(false);
  });

  it("asks again when the email changes back before the other answer arrives", () => {
    const s = answer(NO_LOOKUP, "kari@rana.kommune.no", true);
    expect(needsLookup(s, "kari@rana.kommune.no")).toBe(false);
    const [pending] = startLookup(s, "ola@gmail.com");
    expect(needsLookup(pending, "ola@gmail.com")).toBe(false);
    expect(needsLookup(pending, "kari@rana.kommune.no")).toBe(true);
    expect(needsLookup(pending, "kari@rana")).toBe(false);
  });

  it("offers Microsoft to every email on a server that offers it to everyone", () => {
    const s: MicrosoftLookup = { ...NO_LOOKUP, everyone: true };
    expect(microsoftOffered(s, "")).toBe(true);
    expect(microsoftOffered(s, "ola@gmail.com")).toBe(true);
    expect(needsLookup(s, "ola@gmail.com")).toBe(false);
  });
});
