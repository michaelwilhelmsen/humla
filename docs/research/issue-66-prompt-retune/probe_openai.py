#!/usr/bin/env python3
"""humla-cloud#20 probes A and B against a GPT-5-class model, without a deploy.

`probe.py` tests the *local* prompt on Ollama. But cloud#20's rationale is
explicitly about GPT-5-class behaviour — "on a GPT-5-class model it forbids the
one cheap alternative-phrasing retry that recovers recall" — so the local run
can't confirm the claim the retune actually rests on.

This runs the *cloud* SYSTEM_PROMPT (old from `main`, new from the retune branch)
against the real default model over the same fixed corpus. It is NOT a substitute
for the live probes in cloud#20's acceptance criteria: those exercise the deployed
service end-to-end, including retrieval over real workspace notes. This isolates
the prompt variable alone.

Requires OPENAI_API_KEY in the environment. Costs a few cents. The key is never
printed or written to the transcript file.

Usage:
    export OPENAI_API_KEY=...        # not stored; your shell only
    python3 probe_openai.py [model]  # default: the service's DEFAULT_CHAT_MODEL
"""
import importlib.util, json, os, re, subprocess, sys, time, urllib.error, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
CLOUD = os.environ.get("CLOUD_REPO", os.path.abspath(os.path.join(REPO, "..", "humla-cloud")))

API = "https://api.openai.com/v1/chat/completions"
KEY = os.environ.get("OPENAI_API_KEY")
if not KEY:
    raise SystemExit("OPENAI_API_KEY is not set — see the module docstring.")

# ── reuse the Ollama probe's corpus, tools, probes and scoring ───────────────
_spec = importlib.util.spec_from_file_location("probe_local", os.path.join(HERE, "probe.py"))
_p = importlib.util.module_from_spec(_spec)
sys.modules["probe_local"] = _p
_spec.loader.exec_module(_p)
DISPATCH, TOOLS, PROBES = _p.DISPATCH, _p.TOOLS, _p.PROBES
HEDGES, LABELS, STEP_CAP = _p.HEDGES, _p.LABELS, _p.STEP_CAP


def _git_show(ref, path):
    return subprocess.run(["git", "-C", CLOUD, "show", f"{ref}:{path}"],
                          capture_output=True, text=True, check=True).stdout


def _ts_const(blob, name):
    m = re.search(rf'(?:export )?const {name} =\s*"(.*?)";', blob, re.S)
    if not m:
        raise SystemExit(f"could not find {name} in the cloud chat.ts")
    return m.group(1)


REL = "chat-service/src/chat.ts"
_HEAD = open(os.path.join(CLOUD, REL)).read()
PROMPTS = {"old": _ts_const(_git_show("main", REL), "SYSTEM_PROMPT"),
           "new": _ts_const(_HEAD, "SYSTEM_PROMPT")}
MAX_STEPS_PROMPT = _ts_const(_HEAD, "MAX_STEPS_PROMPT")

# The service's own default. Read from config.ts so this can't drift from prod.
_cfg = open(os.path.join(CLOUD, "chat-service/src/config.ts")).read()
DEFAULT_MODEL = re.search(r'DEFAULT_CHAT_MODEL = "([^"]+)"', _cfg).group(1)
MODEL = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_MODEL

# OpenAI tool schema is the Ollama one minus Ollama's wrapper differences — the
# shape `{"type":"function","function":{...}}` is already what OpenAI expects.
OA_TOOLS = TOOLS


def call(messages, tools):
    body = {"model": MODEL, "messages": messages}
    if tools:
        body["tools"] = tools
    # No `temperature`: gpt-5.x / o-series reject it (mirrors is_reasoning_model()
    # in openai.rs). No max_tokens either — reasoning models want
    # max_completion_tokens, and the default is fine for a probe.
    req = urllib.request.Request(
        API, data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {KEY}"})
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        detail = e.read().decode()[:400]
        raise RuntimeError(f"HTTP {e.code}: {detail}") from None


def run(system, spec):
    msgs = [{"role": "system", "content": system}, {"role": "user", "content": spec["q"]}]
    r = {"id": spec["id"], "searches": 0, "steps": 0, "terminated": False,
         "answer": "", "queries": [], "error": None}
    try:
        for step in range(1, STEP_CAP + 1):
            r["steps"] = step
            final = step == STEP_CAP
            if final:
                msgs.append({"role": "system", "content": MAX_STEPS_PROMPT})
            msg = call(msgs, [] if final else OA_TOOLS)["choices"][0]["message"]
            tcs = msg.get("tool_calls") or []
            if tcs and not final:
                msgs.append(msg)
                for tc in tcs:
                    fn = tc.get("function", {})
                    name = fn.get("name")
                    try:
                        args = json.loads(fn.get("arguments") or "{}")
                    except Exception:
                        args = {}
                    args = args if isinstance(args, dict) else {}
                    if name == "search_notes":
                        r["searches"] += 1
                        r["queries"].append(args.get("query", ""))
                    out = DISPATCH.get(name, lambda **_: {"error": "unknown tool"})(**args)
                    msgs.append({"role": "tool", "tool_call_id": tc["id"],
                                 "content": json.dumps(out)})
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
    r["hit_cap"] = r["steps"] >= STEP_CAP

    k = spec["kind"]
    if k == "recall":
        r["pass"] = bool(r["found"] and r["searches"] <= 3 and r["terminated"] and not r["empty_answer"])
    elif k == "inference":
        r["pass"] = bool(r["terminated"] and not r["empty_answer"] and not r["hedged"] and r["labeled"])
    elif k == "empty":
        r["pass"] = bool(r["terminated"] and not r["empty_answer"] and r["conceded"] and r["searches"] <= 3)
    else:
        r["pass"] = bool(r["terminated"] and not r["empty_answer"] and r["found"])
    return r


def main():
    print(f"model: {MODEL}  (service default: {DEFAULT_MODEL})")
    report = {}
    for label in ("old", "new"):
        print(f"\n=== {MODEL} · cloud {label} prompt ===", flush=True)
        rows, t0 = [], time.time()
        for spec in PROBES:
            r = run(PROMPTS[label], spec)
            rows.append(r)
            print(f"  [{'PASS' if r['pass'] else 'FAIL'}] {r['id']:18} "
                  f"searches={r['searches']} steps={r['steps']} cap={r['hit_cap']} "
                  f"empty_answer={r['empty_answer']} hedged={r['hedged']} "
                  f"labeled={r['labeled']} found={r['found']} err={r['error']}", flush=True)
            if r["queries"]:
                print(f"         queries: {r['queries']}", flush=True)
        report[label] = {"rows": rows, "secs": round(time.time() - t0, 1),
                         "passed": sum(1 for x in rows if x["pass"])}
        print(f"  -> {report[label]['passed']}/{len(rows)} pass ({report[label]['secs']}s)", flush=True)

    out = os.path.join(HERE, "transcript-openai.json")
    with open(out, "w") as f:
        json.dump({"model": MODEL, "prompts": PROMPTS, "report": report}, f, indent=2)
    print("\n==== SCORECARD ====")
    for label in ("old", "new"):
        print(f"  cloud {label:4} {report[label]['passed']}/{len(PROBES)}  ({report[label]['secs']}s)")
    print(f"\ntranscript: {out}")


if __name__ == "__main__":
    main()
