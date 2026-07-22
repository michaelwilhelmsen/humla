#!/usr/bin/env python3
"""embeddinggemma multilingual sanity check (issue #48, non-blocking).

Embeds a small set of query/passage pairs in Norwegian, English, and a spot of
other languages via Ollama's OpenAI-compatible /v1/embeddings, then checks that
each query's most-similar passage (cosine) is its intended match — i.e. that
semantic retrieval works acceptably per language. This is a sanity check, not a
benchmark: embeddinggemma is the sole embedder, so the lever for a weak language
is retrieval tuning (chunk boundaries, RRF params, query formatting), never a
model swap.

Run:
    ollama pull embeddinggemma
    python3 probe.py            # writes findings-run.json next to this file
"""

import json, math, os, sys, urllib.request

BASE = os.environ.get("OLLAMA_BASE", "http://localhost:11434/v1")
MODEL = "embeddinggemma"

# (language, query, correct_passage, [distractor_passages]) — the correct
# passage is a semantic (not lexical) match so keyword search alone would miss.
CASES = [
    ("en", "How much did we cut the marketing budget?",
     "We agreed to reduce advertising spend by twenty percent this quarter.",
     ["The new hire starts on Monday.", "Lunch was catered by the usual place."]),
    ("nb", "Hvor mye kuttet vi i markedsføringsbudsjettet?",
     "Vi ble enige om å redusere annonsekostnadene med tjue prosent dette kvartalet.",
     ["Den nyansatte begynner på mandag.", "Lunsjen ble levert av det vanlige stedet."]),
    ("nb", "Hvem er ansvarlig for lanseringen?",
     "Kari tar eierskap til produktlanseringen neste måned.",
     ["Møtet ble flyttet til torsdag.", "Vi diskuterte reisebudsjettet kort."]),
    ("de", "Wann ist die Frist für das Angebot?",
     "Das Angebot muss bis Freitagabend eingereicht werden.",
     ["Der Kaffee war heute besonders gut.", "Das Team wächst nächstes Jahr."]),
    ("es", "¿Quién aprobó el presupuesto?",
     "La directora financiera dio el visto bueno al presupuesto del tercer trimestre.",
     ["El almuerzo fue a la una.", "La sala de reuniones estaba ocupada."]),
    # Cross-lingual: English query, Norwegian passage (embeddinggemma is
    # multilingual — this should still retrieve).
    ("en->nb", "What did we decide about hiring?",
     "Vi bestemte oss for å ansette to nye utviklere i første kvartal.",
     ["Vi snakket om kontorlokalene.", "Budsjettet ble godkjent uten endringer."]),
]


def embed(texts):
    body = json.dumps({"model": MODEL, "input": texts}).encode()
    req = urllib.request.Request(BASE + "/embeddings", data=body,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as r:
        data = json.loads(r.read())
    return [d["embedding"] for d in sorted(data["data"], key=lambda d: d.get("index", 0))]


def cosine(a, b):
    dot = sum(x * y for x, y in zip(a, b))
    na = math.sqrt(sum(x * x for x in a))
    nb = math.sqrt(sum(y * y for y in b))
    return dot / (na * nb) if na and nb else 0.0


def main():
    results, passed = [], 0
    for lang, query, correct, distractors in CASES:
        passages = [correct] + distractors
        vecs = embed([query] + passages)
        qv, pvs = vecs[0], vecs[1:]
        sims = [cosine(qv, pv) for pv in pvs]
        best = max(range(len(sims)), key=lambda i: sims[i])
        ok = best == 0  # index 0 is the correct passage
        passed += ok
        results.append({"lang": lang, "query": query, "correct_rank_top": ok,
                        "correct_sim": round(sims[0], 4),
                        "best_distractor_sim": round(max(sims[1:]), 4)})
        print(f"[{'PASS' if ok else 'FAIL'}] {lang:6} sim={sims[0]:.3f} "
              f"vs distractor {max(sims[1:]):.3f}  {query[:48]}")

    report = {"model": MODEL, "passed": passed, "total": len(CASES), "cases": results}
    out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "findings-run.json")
    with open(out, "w") as f:
        json.dump(report, f, indent=2, ensure_ascii=False)
    print(f"\n{passed}/{len(CASES)} passed → {out}")
    sys.exit(0 if passed == len(CASES) else 1)


if __name__ == "__main__":
    main()
