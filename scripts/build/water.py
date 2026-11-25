#!/usr/bin/env python3
"""Summarize ZuTalk JSONL logs (or a diagnose bundle logs/) for E9 review."""
import glob, json, os, re, sys

CAPTION_LAG_P99_LIMIT_MS = 1000  # PLAN.md §7 I1; P1.md §1 E1
WATER_MARK_EVERY_MS = 1000       # P1.md §3.5 1 Hz watch sampler
BEAT_STALLED_MS = 2000           # app/src/beat.rs BEAT_STALLED_MS
UPSTREAM = {"Tape": "Capture", "Sent": "Capture", "Heard": "Sent", "Final": "Heard",
            "Draft": "Final(0)", "Shell": "Frame", "Screen": "Shell"}

arg = sys.argv[1] if len(sys.argv) > 1 else "logs"
files = glob.glob(os.path.join(arg, "zutalk*.jsonl")) if os.path.isdir(arg) else [arg]
files.sort(key=lambda p: -int(m.group(1)) if (m := re.search(r"\.(\d+)\.jsonl$", p)) else 0)
if not files:
    sys.exit(f"no zutalk*.jsonl files found at {arg}")

prev_seq, seq_gaps, log_lost, tick_at = None, 0, 0, {}
marks, verdicts, snaps, snap_lines, alerts, whos, starts, net_jobs = {}, [], [], {}, [], [], [], []
last_lag = None

for path in files:
    for raw in open(path, encoding="utf-8"):
        if not raw.strip(): continue
        r = json.loads(raw)
        ev, seq, t = r.get("event", ""), r.get("seq", 0), r.get("t_ms", 0)
        if seq:
            if prev_seq is not None and seq > prev_seq + 1: seq_gaps += seq - prev_seq - 1
            prev_seq = seq
        if ev == "log.lost": log_lost += int(r.get("n", 0))
        elif ev == "water.mark" and r.get("snap", 0) == 0:
            st, ln = r["stage"], r.get("lane", -1)
            key = f"{st}({ln})" if ln >= 0 else st
            tick_at[key] = int(r.get("at", 0))
            up = UPSTREAM.get(st)
            up_key = f"{up}({ln})" if up in ("Sent", "Heard") and ln >= 0 else up
            lag = max(0, tick_at[up_key] - tick_at[key]) if up_key in tick_at else 0
            m = marks.setdefault(key, [0, 0, None, 0])
            m[0] += 1; m[1] = max(m[1], lag)
            if m[2] is not None and t - m[2] > WATER_MARK_EVERY_MS * 2:
                m[3] += (t - m[2]) // WATER_MARK_EVERY_MS - 1
            m[2] = t
        elif ev == "judge.verdict": verdicts.append(r)
        elif ev == "stall.snapshot": snaps.append(r)
        elif ev == "stall.snapshot.end": snap_lines[r.get("snap")] = r.get("lines")
        elif ev in ("part.gave_up", "shell.main_thread.stalled", "ui.frame.gap", "hub.turn.slow"): alerts.append(r)
        elif ev == "identity.who": whos.append(r)
        elif ev == "recording.start": starts.append(r)
        elif ev == "job.start" and str(r.get("kind", "")).startswith("net."): net_jobs.append(r)
        elif ev == "stat.spread" and r.get("snap", 0) == 0 and r.get("name") == "caption.lag_ms":
            last_lag = (r.get("p50"), r.get("p99"), r.get("most"), r.get("n"))

print(f"files={len(files)} seq_gaps={seq_gaps} log.lost={log_lost}")
for k, (cnt, lag, _, miss) in sorted(marks.items()): print(f"water {k}: samples={cnt} max_lag={lag} missing_s={miss}")
for v in verdicts: print(f"verdict t={v['t_ms']} {v['stage']}({v['lane']}) {v['from']}->{v['to']} why={v['why']} action={v['action']}")
for s in snaps: print(f"snapshot snap={s['snap']} t={s['t_ms']} {s['stage']}({s['lane']}) {s['health']} why={s['why']} idle_ms={s['idle_ms']} lines={snap_lines.get(s['snap'])}")
for a in alerts: print(f"alert {a['event']} t={a['t_ms']} " + " ".join(f"{k}={v}" for k, v in a.items() if k not in ("t_ms","seq","level","component","event")))
for w in whos: print(f"who t={w['t_ms']} {w['from']}->{w['to']} why={w['why']} made={w['made']}")
for st in starts: print(f"recording t={st['t_ms']} rec={st['rec']} captions={st['captions']} lanes={st['lanes']} source={st['source']}")
print(f"net_jobs={len(net_jobs)}")
print(f"caption.lag_ms: n={last_lag[3]} p50={last_lag[0]}ms p99={last_lag[1]}ms most={last_lag[2]}ms ({'OK' if last_lag[1]<=CAPTION_LAG_P99_LIMIT_MS else 'EXCEEDS'} <= {CAPTION_LAG_P99_LIMIT_MS}ms)" if last_lag else "caption.lag_ms: (none)")
