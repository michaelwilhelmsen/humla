#!/usr/bin/env python3
"""Spike #45 — local Ollama tool-calling reliability probe (throwaway).

Drives Ollama's native /api/chat with the retrieval tool schema the agentic
chat loop (#47) will use, over a small fixed note corpus, and scores each
candidate model on the four axes from the issue:
  (1) well-formed tool calls, (2) right tool + sane args,
  (3) consumes tool results into a grounded answer, (4) terminates (no loop).

Usage: python3 probe45.py [model ...]   (defaults to the two installed models)
Writes a transcript JSON next to itself and prints a scorecard.
"""
import json, sys, urllib.request, time, os

OLLAMA = "http://localhost:11434/api/chat"
STEP_CAP = 6
MODELS = sys.argv[1:] or ["qwen3.5:9b", "gemma4:12b-mlx"]

# ---- fixed note corpus the tools read ---------------------------------------
NOTES = [
    {"id": "n1", "title": "Acme kickoff", "date": "2026-05-02", "client": "Acme", "folder": "Sales",
     "text": "Acme wants the pricing model switched to per-seat. Jane raised concerns about the migration timeline. Action: send revised quote by Friday."},
    {"id": "n2", "title": "Acme follow-up", "date": "2026-05-14", "client": "Acme", "folder": "Sales",
     "text": "Confirmed per-seat pricing at $12 per seat. Jane is now happy with the plan. Risk: Acme procurement is slow and may slip to Q3."},
    {"id": "n3", "title": "Internal standup", "date": "2026-05-15", "client": "", "folder": "Team",
     "text": "Discussed hiring a backend engineer. No client topics today."},
    {"id": "n4", "title": "Globex discovery", "date": "2026-06-01", "client": "Globex", "folder": "Sales",
     "text": "Globex interested but budget unclear. They want a security review before committing. Opportunity: upsell the compliance add-on."},
    {"id": "n5", "title": "Globex security review", "date": "2026-06-20", "client": "Globex", "folder": "Sales",
     "text": "Security review passed. Budget now approved for 40 seats. Risk: their legal wants a custom DPA which could delay signing."},
    {"id": "n6", "title": "Roadmap planning", "date": "2026-06-22", "client": "", "folder": "Team",
     "text": "Prioritized the export feature and bulk actions for Q3. No client-specific commitments."},
    {"id": "n7", "title": "Initech renewal", "date": "2026-07-01", "client": "Initech", "folder": "Sales",
     "text": "Initech unhappy about recent downtime. At churn risk. They asked for a discount to renew. Action: escalate to the account owner."},
]

def _match(n, query, folder_id, client_id):
    if folder_id and n["folder"].lower() != folder_id.lower():
        return False
    if client_id and n["client"].lower() != client_id.lower():
        return False
    hay = (n["title"] + " " + n["text"] + " " + n["client"]).lower()
    return any(w in hay for w in query.lower().split())

def search_notes(query="", folder_id=None, client_id=None, **_):
    hits = [{"id": n["id"], "title": n["title"], "date": n["date"], "snippet": n["text"][:180]}
            for n in NOTES if _match(n, query, folder_id, client_id)]
    return {"matches": hits[:5]} if hits else {"matches": [], "note": "no matching passages"}

def get_note(id=None, note_id=None, **_):
    nid = id or note_id
    for n in NOTES:
        if n["id"] == nid:
            return {"id": n["id"], "title": n["title"], "date": n["date"], "text": n["text"]}
    return {"error": f"no note with id {nid!r}"}

def list_notes(folder_id=None, client_id=None, **_):
    out = [{"id": n["id"], "title": n["title"], "date": n["date"], "client": n["client"], "folder": n["folder"]}
           for n in NOTES if (not folder_id or n["folder"].lower() == folder_id.lower())
           and (not client_id or n["client"].lower() == client_id.lower())]
    return {"notes": out}

DISPATCH = {"search_notes": search_notes, "get_note": get_note, "list_notes": list_notes}

def _filters(extra):
    props = {"folder_id": {"type": "string", "description": "optional: restrict to this folder name"},
             "client_id": {"type": "string", "description": "optional: restrict to this client name"}}
    props.update(extra)
    return props

TOOLS = [
    {"type": "function", "function": {"name": "search_notes",
        "description": "Full-text search across the user's meeting notes; returns matching passages with note ids.",
        "parameters": {"type": "object", "properties": _filters({"query": {"type": "string", "description": "search terms"}}), "required": ["query"]}}},
    {"type": "function", "function": {"name": "get_note",
        "description": "Fetch the full text of one note by its id.",
        "parameters": {"type": "object", "properties": {"id": {"type": "string", "description": "the note id, e.g. n2"}}, "required": ["id"]}}},
    {"type": "function", "function": {"name": "list_notes",
        "description": "List notes (id, title, date, client, folder), optionally filtered by folder or client.",
        "parameters": {"type": "object", "properties": _filters({}), "required": []}}},
]

SYSTEM = ("You are Humla's chat assistant. Answer questions grounded in the user's meeting notes. "
          "Use the tools to search and read notes before answering — do not answer from memory. "
          "Cite the note ids you used. If the notes don't contain the answer, say so plainly rather than guessing.")

# ---- probe queries ----------------------------------------------------------
QUERIES = [
    {"q": "What did we decide about Acme's pricing model?", "kind": "single",
     "tool": "search_notes", "terms": ["per-seat", "12"]},
    {"q": "List all my notes in the Sales folder.", "kind": "single",
     "tool": "list_notes", "terms": ["Acme", "Globex"]},
    {"q": "Read note n2 and tell me the risk it mentions.", "kind": "single",
     "tool": "get_note", "terms": ["procurement", "Q3"]},
    {"q": "Which of my clients are at risk right now, and why?", "kind": "multi",
     "tool": "search_notes", "terms": ["Initech", "churn"]},
    {"q": "Connect the dots: what are the open risks and opportunities across my client meetings?", "kind": "multi",
     "tool": "search_notes", "terms": ["Globex", "Initech"]},
    {"q": "What did we agree about the Zephyr quantum protocol?", "kind": "empty",
     "tool": "search_notes", "empty": True},
    {"q": "Summarize where things stand with Globex.", "kind": "multi",
     "tool": "search_notes", "terms": ["security", "seats"]},
    {"q": "What action items do I have for Acme?", "kind": "single",
     "tool": "search_notes", "terms": ["quote", "Friday"]},
]

def call(model, messages, tools):
    body = json.dumps({"model": model, "messages": messages, "tools": tools,
                       "stream": False, "options": {"temperature": 0}}).encode()
    req = urllib.request.Request(OLLAMA, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=180) as r:
        return json.loads(r.read())

def run_query(model, spec):
    messages = [{"role": "system", "content": SYSTEM}, {"role": "user", "content": spec["q"]}]
    m = {"q": spec["q"], "kind": spec["kind"], "well_formed": True, "used_tool": False,
         "first_tool": None, "right_tool": False, "sane_args": False, "consumed": False,
         "terminated": False, "steps": 0, "answer": "", "error": None, "calls": []}
    try:
        for step in range(1, STEP_CAP + 1):
            m["steps"] = step
            final = step == STEP_CAP
            resp = call(model, messages, [] if final else TOOLS)
            msg = resp.get("message", {})
            tcs = msg.get("tool_calls") or []
            if tcs and not final:
                m["used_tool"] = True
                messages.append(msg)
                for tc in tcs:
                    fn = tc.get("function", {})
                    name = fn.get("name")
                    args = fn.get("arguments")
                    if isinstance(args, str):
                        try:
                            args = json.loads(args)
                        except Exception:
                            args = {"_raw": args}
                    m["calls"].append({"name": name, "args": args})
                    if m["first_tool"] is None:
                        m["first_tool"] = name
                        m["right_tool"] = (name == spec["tool"])
                        m["sane_args"] = isinstance(args, dict) and name in DISPATCH and bool(args or name == "list_notes")
                    result = DISPATCH.get(name, lambda **_: {"error": f"unknown tool {name}"})(**(args if isinstance(args, dict) else {}))
                    messages.append({"role": "tool", "content": json.dumps(result)})
                continue
            # text answer
            m["answer"] = msg.get("content", "") or ""
            m["terminated"] = True
            break
        else:
            m["terminated"] = False
        # consumed / grounded heuristic
        ans = m["answer"].lower()
        if spec.get("empty"):
            m["consumed"] = any(p in ans for p in ["couldn't find", "could not find", "no ", "don't have", "do not have", "nothing", "no mention", "not in"])
        else:
            m["consumed"] = m["used_tool"] and any(t.lower() in ans for t in spec.get("terms", []))
    except Exception as e:
        m["error"] = str(e)
        m["well_formed"] = False
    m["clean"] = bool(m["well_formed"] and m["terminated"] and not m["error"]
                      and (m["right_tool"] or spec["kind"] == "multi") and m["sane_args"] and m["consumed"])
    return m

def main():
    report = {}
    for model in MODELS:
        print(f"\n=== {model} ===", flush=True)
        rows = []
        t0 = time.time()
        for spec in QUERIES:
            r = run_query(model, spec)
            rows.append(r)
            flag = "OK " if r["clean"] else "xxx"
            print(f"  [{flag}] {spec['kind']:6} tool={r['first_tool']} steps={r['steps']} "
                  f"consumed={r['consumed']} term={r['terminated']} err={r['error']}  :: {spec['q'][:52]}", flush=True)
        clean = sum(1 for r in rows if r["clean"])
        report[model] = {"clean": clean, "total": len(rows), "secs": round(time.time() - t0, 1), "rows": rows}
        print(f"  -> {clean}/{len(rows)} clean  ({report[model]['secs']}s)", flush=True)
    out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "probe45-transcript.json")
    with open(out, "w") as f:
        json.dump(report, f, indent=2)
    print("\n==== SCORECARD ====")
    for model, d in report.items():
        print(f"  {model:20} {d['clean']}/{d['total']} clean  ({d['secs']}s)")
    print(f"\ntranscript: {out}")

if __name__ == "__main__":
    main()
