#!/usr/bin/env python3
"""Score held-sound detection against AMI's hand-transcribed filled pauses.

    python3 scripts/eval-fillers.py ~/corpora/ami-fillers \
        [--control ~/corpora/libri-clean ...] [--learners ~/corpora/so762-fillers]

Each directory holds `segments.tsv` (scripts/prepare-ami-fillers.py) and
`filler_dump.tsv` (the ignored `fluency::corpus::corpus_dump_fillers` test).
A control directory needs neither reference column: it is scored as false
positives per minute only (read speech, where nobody says "um"). A
--learners directory (speechocean762: learners reading aloud, who may
genuinely hesitate) is scored the same way but reported, never gated.

Protocol, fixed before looking at test:
  1. Sweep the thresholds on the dev split only. Pick the setting with the
     highest recall whose precision clears the "go" bar (>= 0.80, Wilson
     lower bound >= 0.70); failing that, the "hedged" bar (>= 0.60);
     failing that, the best F1.
  2. Freeze it, then score the test split once.

Matching is event-level: a detection matches a reference filler when they
overlap or their centres are within 250 ms (AMI word times are forced
alignment), and each reference matches at most one detection. A detection
that only matches a filler said by *another* speaker (headset crosstalk,
`other_fillers`) is ignored rather than counted either way.

The gate (plan Appendix D section 1.4), on test:
  GO      precision >= 0.80 with Wilson lower bound >= 0.70, recall >= 0.40
          and >= 2x the text-only recall, and <= 0.5 false positives per
          minute on the LibriSpeech test-clean control.
  HEDGED  precision 0.60-0.80 with the other conditions met.
  NO-GO   anything else.

Standard library only.
"""

import argparse
import itertools
import math
import sys
from pathlib import Path

TOL_MS = 250
FILLER_LETTERS = set("AUMHER")

# The sweep. The detector's current constants are one point in it
# (spread 2.5, energy 6.0, 200-1500 ms, blank 0.7, no short-letter bypass).
GRID = {
    "spread_st": [1.0, 1.5, 2.0, 2.5, 3.0, 4.0],
    "energy_sd_db": [2.0, 3.0, 4.0, 5.0, 6.0],
    "min_ms": [100, 150, 200, 250, 300],
    "max_ms": [1000, 1500, 2500],
    "blank_frac": [0.0, 0.5, 0.7, 0.9],
    "short_ok": [True, False],
}
CURRENT = dict(spread_st=2.5, energy_sd_db=6.0, min_ms=200, max_ms=1500, blank_frac=0.7, short_ok=False)


def spans(field):
    out = []
    for part in field.split(";") if field else []:
        a, b = part.split("-")
        out.append((int(a), int(b)))
    return out


def load(d):
    d = Path(d).expanduser()
    segs = {}
    with open(d / "segments.tsv") as f:
        header = f.readline().rstrip("\n").split("\t")
        for line in f:
            r = dict(zip(header, line.rstrip("\n").split("\t")))
            segs[r["id"]] = {
                "split": r.get("split", "control"),
                "refs": spans(r.get("ref_fillers", "")),
                "others": spans(r.get("other_fillers", "")),
                "dur_ms": int(r["dur_ms"]) if r.get("dur_ms") else 0,
                "cands": [],
                "text": [],
            }
    with open(d / "filler_dump.tsv") as f:
        header = f.readline().rstrip("\n").split("\t")
        for line in f:
            r = dict(zip(header, line.rstrip("\n").split("\t")))
            s = segs[r["id"]]
            a, b = int(r["start_ms"]), int(r["end_ms"])
            if r["row"] == "seg":
                s["dur_ms"] = b
            elif r["row"] == "text":
                s["text"].append((a, b))
            elif r["row"] == "cand":
                s["cands"].append(
                    {
                        "span": (a, b),
                        "spread": float(r["spread_st"]),
                        "esd": float(r["energy_sd_db"]),
                        "blank": float(r["blank_frac"]),
                        "letters": r["letters"],
                        "under": r["under"],
                        "kind": r["kind"],
                    }
                )
    return segs


def is_filled_pause(c, p):
    """The detector's filled-pause rule, with the thresholds as parameters."""
    a, b = c["span"]
    if not p["min_ms"] <= b - a <= p["max_ms"]:
        return False
    if c["spread"] > p["spread_st"] or c["esd"] > p["energy_sd_db"]:
        return False
    if c["under"] not in ("none", "filler_shaped"):
        return False
    letters = c["letters"]
    if not set(letters) <= FILLER_LETTERS:
        return False
    return c["blank"] >= p["blank_frac"] or (p["short_ok"] and len(letters) <= 2)


def is_lengthening(c, p):
    """The detector's lengthening rule: steady, held >= 300 ms, over a real word."""
    a, b = c["span"]
    return (
        300 <= b - a <= p["max_ms"]
        and c["spread"] <= p["spread_st"]
        and c["esd"] <= p["energy_sd_db"]
        and c["under"] == "real"
    )


def near(d, r):
    return min(d[1], r[1]) > max(d[0], r[0]) or abs((d[0] + d[1]) - (r[0] + r[1])) / 2 <= TOL_MS


def match(dets, refs, others):
    """(true positives, false positives) for one segment."""
    pairs = sorted(
        (abs((d[0] + d[1]) - (r[0] + r[1])), i, j)
        for i, d in enumerate(dets)
        for j, r in enumerate(refs)
        if near(d, r)
    )
    used_d, used_r = set(), set()
    for _, i, j in pairs:
        if i not in used_d and j not in used_r:
            used_d.add(i)
            used_r.add(j)
    fp = sum(
        1 for i, d in enumerate(dets) if i not in used_d and not any(near(d, o) for o in others)
    )
    return len(used_d), fp


def wilson(k, n, z=1.96):
    if n == 0:
        return (0.0, 1.0)
    p = k / n
    den = 1 + z * z / n
    mid = (p + z * z / (2 * n)) / den
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / den
    return (max(0.0, mid - half), min(1.0, mid + half))


def ranks(v):
    order = sorted(range(len(v)), key=lambda i: v[i])
    r = [0.0] * len(v)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and v[order[j + 1]] == v[order[i]]:
            j += 1
        for k in range(i, j + 1):
            r[order[k]] = (i + j) / 2 + 1
        i = j + 1
    return r


def spearman(x, y):
    rx, ry = ranks(x), ranks(y)
    n = len(x)
    mx, my = sum(rx) / n, sum(ry) / n
    cov = sum((a - mx) * (b - my) for a, b in zip(rx, ry))
    vx = math.sqrt(sum((a - mx) ** 2 for a in rx))
    vy = math.sqrt(sum((b - my) ** 2 for b in ry))
    return cov / (vx * vy) if vx and vy else float("nan")


def score(segs, detect):
    """Pooled counts for detections `detect(seg) -> [span]`."""
    tp = fp = n_ref = 0
    minutes = 0.0
    per_seg = []
    for s in segs:
        dets = detect(s)
        t, f = match(dets, s["refs"], s["others"])
        tp, fp, n_ref = tp + t, fp + f, n_ref + len(s["refs"])
        minutes += s["dur_ms"] / 60_000
        per_seg.append((len(dets), len(s["refs"])))
    prec = tp / (tp + fp) if tp + fp else 0.0
    rec = tp / n_ref if n_ref else 0.0
    return {
        "tp": tp,
        "fp": fp,
        "n_ref": n_ref,
        "precision": prec,
        "precision_ci": wilson(tp, tp + fp),
        "recall": rec,
        "recall_ci": wilson(tp, n_ref),
        "f1": 2 * prec * rec / (prec + rec) if prec + rec else 0.0,
        "fp_per_min": fp / minutes if minutes else 0.0,
        "minutes": minutes,
        "per_seg": per_seg,
    }


def acoustic(p):
    return lambda s: [c["span"] for c in s["cands"] if is_filled_pause(c, p)]


def text_only(s):
    return s["text"]


def combined(p):
    # What the Meter would count: spelled fillers plus the filled pauses the
    # detector found (it already drops candidates under a spelled filler).
    return lambda s: s["text"] + acoustic(p)(s)


def fmt(m):
    lo, hi = m["precision_ci"]
    rlo, rhi = m["recall_ci"]
    return (
        f"P {m['precision']:.3f} [{lo:.3f}, {hi:.3f}]  R {m['recall']:.3f} [{rlo:.3f}, {rhi:.3f}]  "
        f"F1 {m['f1']:.3f}  tp {m['tp']} fp {m['fp']} refs {m['n_ref']}  "
        f"FP/min {m['fp_per_min']:.2f}"
    )


def pick(dev):
    results = []
    keys = list(GRID)
    for values in itertools.product(*(GRID[k] for k in keys)):
        p = dict(zip(keys, values))
        if p["min_ms"] >= p["max_ms"]:
            continue
        results.append((p, score(dev, acoustic(p))))
    go = [r for r in results if r[1]["precision"] >= 0.80 and r[1]["precision_ci"][0] >= 0.70]
    hedged = [r for r in results if r[1]["precision"] >= 0.60]
    if go:
        tier, pool = "go", go
        best = max(pool, key=lambda r: (r[1]["recall"], r[1]["precision"]))
    elif hedged:
        tier, pool = "hedged", hedged
        best = max(pool, key=lambda r: (r[1]["recall"], r[1]["precision"]))
    else:
        tier, pool = "f1", results
        best = max(pool, key=lambda r: (r[1]["f1"], r[1]["precision"]))
    top_p = max(results, key=lambda r: (r[1]["precision"], r[1]["recall"]))
    return tier, best, top_p, len(results)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("corpus")
    ap.add_argument("--control", action="append", default=[], help="read-speech dump dir (FP/min only)")
    ap.add_argument("--learners", action="append", default=[], help="learner read-speech dump dir (reported only)")
    args = ap.parse_args()

    segs = load(args.corpus)
    dev = [s for s in segs.values() if s["split"] == "dev"]
    test = [s for s in segs.values() if s["split"] == "test"]
    if not dev or not test:
        sys.exit("need both dev and test segments")

    print("== dev ==")
    print(f"text only        {fmt(score(dev, text_only))}")
    print(f"current consts   {fmt(score(dev, acoustic(CURRENT)))}")
    tier, (frozen, dm), (tp_p, tp_m), n = pick(dev)
    print(f"swept {n} settings; best tier on dev: {tier}")
    print(f"highest-precision setting on dev: {tp_p}\n                 {fmt(tp_m)}")
    print(f"FROZEN: {frozen}")
    print(f"frozen on dev    {fmt(dm)}")

    print("\n== test (scored once, frozen thresholds) ==")
    t_text = score(test, text_only)
    t_ac = score(test, acoustic(frozen))
    t_all = score(test, combined(frozen))
    print(f"text only        {fmt(t_text)}")
    print(f"acoustic         {fmt(t_ac)}")
    print(f"text + acoustic  {fmt(t_all)}")
    for name, m in (("acoustic", t_ac), ("text + acoustic", t_all), ("text only", t_text)):
        dets, refs = zip(*m["per_seg"])
        print(f"per-segment count Spearman ({name}): {spearman(list(dets), list(refs)):.3f}")
    leng = sum(1 for s in test for c in s["cands"] if is_lengthening(c, frozen))
    print(
        f"lengthening at the frozen thresholds: {leng} on test, "
        f"{leng / t_ac['minutes']:.2f}/min (no reference labels; not gated)"
    )

    control_fp = None
    for d in args.control:
        c = list(load(d).values())
        m = score(c, acoustic(frozen))
        control_fp = m["fp_per_min"] if control_fp is None else max(control_fp, m["fp_per_min"])
        print(f"control {d}: {m['fp']} detections in {m['minutes']:.1f} min = {m['fp_per_min']:.2f}/min")

    for d in args.learners:
        m = score(list(load(d).values()), acoustic(frozen))
        print(f"learners {d}: {m['fp']} detections in {m['minutes']:.1f} min = {m['fp_per_min']:.2f}/min (not gated)")

    p, (plo, _), r = t_ac["precision"], t_ac["precision_ci"], t_ac["recall"]
    recall_ok = r >= 0.40 and r >= 2 * t_text["recall"]
    control_ok = control_fp is not None and control_fp <= 0.5
    if p >= 0.80 and plo >= 0.70 and recall_ok and control_ok:
        verdict = "GO"
    elif p >= 0.60 and recall_ok and control_ok:
        verdict = "HEDGED"
    else:
        verdict = "NO-GO"
    print(
        f"\nGATE: {verdict}  (precision {p:.3f}, lower {plo:.3f}; recall {r:.3f} vs "
        f"2x text {2 * t_text['recall']:.3f}; control FP/min "
        f"{'not run' if control_fp is None else f'{control_fp:.2f}'})"
    )
    if control_fp is None and verdict == "NO-GO" and p >= 0.60 and recall_ok:
        print("(the LibriSpeech control was not run; without it the gate cannot pass)")


if __name__ == "__main__":
    main()
