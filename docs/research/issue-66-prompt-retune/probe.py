#!/usr/bin/env python3
"""Issue #66 / humla-cloud#20 — does the prompt retune actually change behaviour?

The retune replaced a flat "don't retry an empty search" ban with bounded
exploration, and licensed labeled inference. Both claims are about *model
behaviour*, so unit tests on the prompt string can't confirm them — this runs the
real OLD and NEW prompt strings head-to-head against a local Ollama model.

Three probes, mirroring humla-cloud#20's acceptance criteria:

  A (recall)      A Norwegian query ("mersalg") misses the English corpus on the
                  first search; an English rephrase ("upsell") hits n4. Does the
                  model retry — and does it stop after one or two tries rather
                  than spinning to the step cap?
  B (synthesis)   An explicit request for interpretation. Does it deliver labeled
                  inference, or hedge and ask permission first? The hedge is the
                  literal symptom #20 was filed over.
  C (regression)  Factual queries that worked before. The retune adds ~40 words
                  to a prompt whose own doc comment warns that small thinking
                  models re-litigate long constraint lists, so the cost of the
                  extra tokens has to be measured, not assumed.

The corpus, tool schema and Ollama call are imported from the spike #45 probe
rather than duplicated — same fixed notes, so results stay comparable. That probe
is throwaway research code and is NOT modified here; changing its corpus would
invalidate its own findings.

Usage: python3 probe.py [model ...]     (default: gemma4:12b-mlx)
"""
import importlib.util, json, os, re, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))

# ── borrow the spike #45 corpus + tools ──────────────────────────────────────
_spike = os.path.join(REPO, "docs/research/spike-45-ollama-tool-calling/probe.py")
_spec = importlib.util.spec_from_file_location("spike45", _spike)
_m = importlib.util.module_from_spec(_spec)
sys.modules["spike45"] = _m
_spec.loader.exec_module(_m)
NOTES, TOOLS, call = _m.NOTES, _m.TOOLS, _m.call

STEP_CAP = 6  # mirrors chat::MAX_STEPS
MODELS = sys.argv[1:] or ["gemma4:12b-mlx"]


# ── whole-word search, replacing the spike's substring matcher ───────────────
# The spike matches with `any(word in haystack)`, so a query hits on any
# substring: "om" matches inside "compliance". That makes an empty-search probe
# vacuous — nearly every query returns something. Bounded-retry behaviour only
# shows up if a search can genuinely miss, so this probe needs word boundaries.
def search_notes(query="", folder_id=None, client_id=None, **_):
    words = [w for w in re.findall(r"\w+", query.lower()) if len(w) > 2]
    hits = []
    for n in NOTES:
        if folder_id and n["folder"].lower() != folder_id.lower():
            continue
        if client_id and n["client"].lower() != client_id.lower():
            continue
        hay = f"{n['title']} {n['text']} {n['client']}".lower()
        if any(re.search(rf"\b{re.escape(w)}\b", hay) for w in words):
            hits.append({"id": n["id"], "title": n["title"], "date": n["date"],
                         "snippet": n["text"][:180]})
    return {"matches": hits[:5]} if hits else {"matches": [], "note": "no matching passages"}


DISPATCH = {**_m.DISPATCH, "search_notes": search_notes}


# ── the two prompts, read from git rather than pasted ────────────────────────
def _rust_prompt(blob: str) -> str:
    """Extract SYSTEM_PROMPT from a chat/mod.rs blob, undoing `\\`-continuations."""
    m = re.search(r'pub const SYSTEM_PROMPT: &str = "(.*?)";', blob, re.S)
    if not m:
        raise SystemExit("could not find SYSTEM_PROMPT")
    return re.sub(r"\\\n\s*", "", m.group(1))


def _show(ref: str, path: str) -> str:
    return subprocess.run(["git", "-C", REPO, "show", f"{ref}:{path}"],
                          capture_output=True, text=True, check=True).stdout


REL = "src-tauri/src/chat/mod.rs"
_HEAD = open(os.path.join(REPO, REL)).read()
PROMPTS = {
    "old": _rust_prompt(_show("main", REL)),
    "new": _rust_prompt(_HEAD),
}

# The real loop injects this as a system turn on the final step, once tools are
# dropped, so the model wraps up instead of being cut off mid-call. Omitting it
# here produced empty answers that looked like a model failure but were the
# harness diverging from the implementation.
MAX_STEPS_PROMPT = re.sub(
    r"\\\n\s*", "", re.search(r'const MAX_STEPS_PROMPT: &str = "(.*?)";', _HEAD, re.S).group(1)
)

# Phrases that mean the model asked for permission instead of just answering.
HEDGES = ["shall i", "skal jeg", "would you like me to", "do you want me to",
          "vil du at jeg", "let me know if you'd like", "si fra om du vil"]
# Markers that inference is labelled as inference rather than asserted as fact.
LABELS = ["inference", "inferred", "inferring", "antakelse", "tolkning",
          "not documented", "ikke dokumentert", "my read", "speculat"]

PROBES = [
    # Probe A — every word misses the English corpus under whole-word matching,
    # so the first search MUST come back empty. The answer is only reachable by
    # rephrasing to English ("upsell" / "compliance"), which is exactly the
    # NO/EN recovery the retune exists for.
    {"id": "A-recall", "q": "Fant du noe om mersalgsmuligheter hos kunden Globex?",
     "want_terms": ["compliance"], "kind": "recall"},
    # Probe B — the answer is NOT in any note. Ranking two clients requires
    # weighing Initech's churn risk against Globex's approved budget, so a
    # correct answer necessarily contains inference that has to be labelled.
    # (The earlier version asked about Globex upsell, which note n4 states
    # verbatim — there was nothing to infer, so it tested nothing.)
    {"id": "B-inference", "q": "Hvilken kunde bør jeg prioritere neste kvartal, og hvorfor?",
     "want_terms": [], "kind": "inference"},
    {"id": "C-factual-acme", "q": "What did we decide about Acme's pricing model?",
     "want_terms": ["per-seat"], "kind": "factual"},
    {"id": "D-truly-empty", "q": "What did we agree about the Zephyr quantum protocol?",
     "want_terms": [], "kind": "empty"},
]


def run(model, system, spec):
    msgs = [{"role": "system", "content": system}, {"role": "user", "content": spec["q"]}]
    r = {"id": spec["id"], "searches": 0, "steps": 0, "terminated": False,
         "answer": "", "queries": [], "error": None}
    try:
        for step in range(1, STEP_CAP + 1):
            r["steps"] = step
            final = step == STEP_CAP
            if final:
                # Mirror the real loop: tools dropped AND the wrap-up nudge added.
                msgs.append({"role": "system", "content": MAX_STEPS_PROMPT})
            msg = call(model, msgs, [] if final else TOOLS).get("message", {})
            tcs = msg.get("tool_calls") or []
            if tcs and not final:
                msgs.append(msg)
                for tc in tcs:
                    fn = tc.get("function", {})
                    name, args = fn.get("name"), fn.get("arguments")
                    if isinstance(args, str):
                        try:
                            args = json.loads(args)
                        except Exception:
                            args = {}
                    args = args if isinstance(args, dict) else {}
                    if name == "search_notes":
                        r["searches"] += 1
                        r["queries"].append(args.get("query", ""))
                    out = DISPATCH.get(name, lambda **_: {"error": "unknown tool"})(**args)
                    msgs.append({"role": "tool", "content": json.dumps(out)})
                continue
            r["answer"] = msg.get("content") or ""
            r["terminated"] = True
            break
    except Exception as e:
        r["error"] = str(e)

    ans = r["answer"].lower()
    r["empty_answer"] = not ans.strip()
    r["hedged"] = any(h in ans for h in HEDGES)
    r["labeled"] = any(l in ans for l in LABELS)
    r["found"] = all(t in ans for t in spec["want_terms"]) if spec["want_terms"] else None
    r["conceded"] = any(p in ans for p in ["couldn't find", "could not find", "fant ikke",
                                           "ingen", "no mention", "not in", "nothing"])
    # Burning the whole budget is a distinct failure from answering wrongly, and
    # it's the cost the old no-retry ban existed to prevent — track it separately.
    r["hit_cap"] = r["steps"] >= STEP_CAP

    k = spec["kind"]
    if k == "recall":
        # Found it despite the first search missing, without spinning. One search
        # is a pass too: if the model rephrases pre-emptively that's better, not
        # worse — the criterion is recovery within a bounded number of tries.
        r["pass"] = bool(r["found"] and r["searches"] <= 3 and r["terminated"]
                         and not r["empty_answer"])
    elif k == "inference":
        r["pass"] = bool(r["terminated"] and not r["empty_answer"]
                         and not r["hedged"] and r["labeled"])
    elif k == "empty":
        r["pass"] = bool(r["terminated"] and not r["empty_answer"]
                         and r["conceded"] and r["searches"] <= 3)
    else:
        r["pass"] = bool(r["terminated"] and not r["empty_answer"] and r["found"])
    return r


def main():
    report = {}
    for model in MODELS:
        report[model] = {}
        for label in ("old", "new"):
            print(f"\n=== {model} · {label} prompt ===", flush=True)
            rows, t0 = [], time.time()
            for spec in PROBES:
                r = run(model, PROMPTS[label], spec)
                rows.append(r)
                print(f"  [{'PASS' if r['pass'] else 'FAIL'}] {r['id']:18} "
                      f"searches={r['searches']} steps={r['steps']} cap={r['hit_cap']} "
                      f"empty_answer={r['empty_answer']} hedged={r['hedged']} "
                      f"labeled={r['labeled']} found={r['found']} err={r['error']}", flush=True)
                if r["queries"]:
                    print(f"         queries: {r['queries']}", flush=True)
            report[model][label] = {"rows": rows, "secs": round(time.time() - t0, 1),
                                    "passed": sum(1 for x in rows if x["pass"])}
            print(f"  -> {report[model][label]['passed']}/{len(rows)} pass "
                  f"({report[model][label]['secs']}s)", flush=True)

    out = os.path.join(HERE, "transcript.json")
    with open(out, "w") as f:
        json.dump({"prompts": PROMPTS, "report": report}, f, indent=2)
    print("\n==== SCORECARD ====")
    for model, d in report.items():
        for label in ("old", "new"):
            print(f"  {model:18} {label:4} {d[label]['passed']}/{len(PROBES)}  ({d[label]['secs']}s)")
    print(f"\ntranscript: {out}")


if __name__ == "__main__":
    main()
