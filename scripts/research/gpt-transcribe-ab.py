#!/usr/bin/env python3
"""T2 (#129): whisper-1 vs gpt-transcribe on real Humla chunks.

Production-faithful A/B. Cuts chunks at the SAME VAD boundaries the sidecar
produced (from a note's chunks.json), feeds each the SAME prior_context the
production path would have (the preceding chunk's committed text, capped at
150 words, per recording.rs's TranscriptTrail) and the SAME custom vocabulary.

The four arms isolate the two live questions:
  A  whisper-1     language=no,  prompt=vocab+prior_context      <- production today
  B  gpt-transcribe languages=no, prompt=prior_context, keywords=vocab
  C  gpt-transcribe NO hint,      prompt=prior_context, keywords=vocab
  D  gpt-transcribe languages=no+en, prompt=prior_context, keywords=vocab

B vs C answers "does the hint help or suppress code-switching" (T3).
B vs D answers "does adding en help the English loanwords" (T3).
A vs B is the quality verdict (T2).

keywords carries the vocabulary and prompt carries prior_context per T6 (#133).
"""
import json, os, subprocess, sys, textwrap, urllib.request, mimetypes, uuid

REC = os.path.expanduser("~/Library/Application Support/no.humla.app/recordings")
KEY = subprocess.run(["security","find-generic-password","-s","no.humla.app",
                      "-a","openai_api_key","-w"],capture_output=True,text=True).stdout.strip()
VOCAB = "Michael, Stian, Petter, Kurt, Yoann, AI, Screenpartner, Guro, Vaaje, Haugen, Wilhelmsen, Claude, Claude Code, Wordpress, Gemini"

def post(path, fields, files):
    b = uuid.uuid4().hex; out = b""
    for k, vs in fields:
        for v in (vs if isinstance(vs, list) else [vs]):
            out += f'--{b}\r\nContent-Disposition: form-data; name="{k}"\r\n\r\n{v}\r\n'.encode()
    for k, fn, data in files:
        out += (f'--{b}\r\nContent-Disposition: form-data; name="{k}"; filename="{fn}"\r\n'
                f'Content-Type: audio/wav\r\n\r\n').encode() + data + b"\r\n"
    out += f"--{b}--\r\n".encode()
    r = urllib.request.Request("https://api.openai.com/v1/audio/transcriptions", data=out,
        headers={"Authorization": f"Bearer {KEY}", "Content-Type": f"multipart/form-data; boundary={b}"})
    try:
        with urllib.request.urlopen(r, timeout=180) as resp: return resp.read().decode()
    except urllib.error.HTTPError as e: return f"HTTP {e.code}: {e.read().decode()[:200]}"

def cut(wav, start_ms, dur_ms, dest):
    subprocess.run(["ffmpeg","-loglevel","error","-y","-ss",f"{start_ms/1000:.3f}",
                    "-t",f"{dur_ms/1000:.3f}","-i",wav,"-ac","1","-ar","16000",
                    "-c:a","pcm_s16le",dest],check=True)

def trail(mic, i, words=150):
    """The prior_context production would have had: preceding committed text."""
    acc=[]
    for c in reversed(mic[:i]):
        acc = c["text"].split() + acc
        if len(acc) >= words: break
    return " ".join(acc[-words:])

def main(note_id, idxs):
    mic = [c for c in json.load(open(f"{REC}/{note_id}/chunks.json"))["chunks"]
           if c["source"]=="mic" and c.get("words")]
    wav = f"{REC}/{note_id}/mic.wav"
    rows=[]
    for i in idxs:
        c = mic[i]; dur = max(w["end_ms"] for w in c["words"]) + 300
        tmp=f"/tmp/ab_{i}.wav"; cut(wav, c["start_ms"], dur, tmp)
        data=open(tmp,"rb").read(); pc=trail(mic,i)
        arms={
          "A whisper-1 (production)": ("whisper-1",[("language","no"),("prompt",f"{VOCAB}. {pc}")]),
          "B gpt languages=no":      ("gpt-transcribe",[("languages","no"),("prompt",pc),("keywords",VOCAB)]),
          "C gpt no hint":           ("gpt-transcribe",[("prompt",pc),("keywords",VOCAB)]),
          "D gpt languages=no+en":   ("gpt-transcribe",[("languages",["no","en"]),("prompt",pc),("keywords",VOCAB)]),
        }
        res={}
        for name,(model,extra) in arms.items():
            f=[("model",model),("response_format","text")]+extra
            res[name]=post("", f, [("file",f"chunk{i}.wav",data)]).strip()
        rows.append({"idx":i,"start_ms":c["start_ms"],"dur_ms":dur,
                     "stored_whisper1":c["text"],"prior_context_tail":pc[-160:],"arms":res})
        print(f"  chunk {i} done", file=sys.stderr)
    return rows

if __name__=="__main__":
    note=sys.argv[1]; idxs=[int(x) for x in sys.argv[2].split(",")]
    print(json.dumps(main(note,idxs),ensure_ascii=False,indent=2))
