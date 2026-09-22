#!/usr/bin/env python3
"""Fit the GOP -> 0..100 score curve to human ratings. Standard library only.

Input is the TSV written by the `corpus_dump_gop` test in pronounce.rs: one
row per word of speechocean762 (OpenSLR-101, CC BY 4.0), with the raw GOP the
app computes and the experts' 0-10 accuracy for that word.

    python3 scripts/calibrate-gop.py gop_dump.tsv
    python3 scripts/calibrate-gop.py gop_dump.tsv --cutoffs 10 20   # GOP cutoffs

Everything is fitted on the corpus's train split and reported on its test split.

What the first full run (2026-09-22) found, and why the report looks like this:

* Fitting E[expert score | GOP] (isotonic regression) is *calibrated* — mean
  error falls from ~29 to ~9 points — and useless: 92% of the corpus's words
  are rated a perfect 10, so the expected score for any GOP is >= 79 and every
  word, including every badly mispronounced one, lands in "good". A tutor
  needs to decide when to flag a word, not predict an average.
* So the useful reading is an operating point: score a word by where its GOP
  falls among words the experts rated perfect (a percentile), and flag below a
  cutoff whose false-alarm rate is stated. That table is printed below.
* About 60% of correctly-said words sit at GOP ~ 0 (the model's best guess is
  the target), so above the tail there is no gradation at all.

Two different questions, kept apart in the report:

* Discrimination (AUC for "was this word mispronounced?") is rank-based, so it
  is the same for every monotone mapping. It measures what GOP itself can tell
  apart, and no curve can improve it.
* Calibration (does a score of 70 mean what the rubric says 70 means?) is what
  a fitted curve changes.
"""
import argparse
import bisect
import csv
import math
import sys

KNOTS = 32
# Rubric bands (README): 10 perfect, 7-9 correct with an accent, 4-6 under 30%
# of phones wrong, 0-3 more than 30% wrong or a different word.
MISPRONOUNCED_AT_OR_BELOW = 6
VERDICT_GOOD, VERDICT_UNCLEAR = 70, 40  # pronounce.rs VERDICT_* constants


def load(path):
    rows = []
    with open(path, newline="") as f:
        for r in csv.DictReader(f, delimiter="\t"):
            human = int(r["human"])
            if human < 0:
                continue
            age = r["age"].strip()
            rows.append({
                "split": r["split"], "gop": float(r["gop"]), "now": int(r["score_now"]),
                "human": human, "y": human * 10.0, "age": int(age) if age.isdigit() else None,
            })
    return rows


def pav(xs, ys):
    """Isotonic (non-decreasing) regression. Returns (block_x_max, block_value)."""
    order = sorted(range(len(xs)), key=lambda i: xs[i])
    blocks = []  # [sum_y, count, x_max]
    for i in order:
        blocks.append([ys[i], 1, xs[i]])
        while len(blocks) > 1 and blocks[-2][0] / blocks[-2][1] > blocks[-1][0] / blocks[-1][1]:
            s, n, x = blocks.pop()
            blocks[-1][0] += s
            blocks[-1][1] += n
            blocks[-1][2] = x
    return [b[2] for b in blocks], [b[0] / b[1] for b in blocks]


def step(model, x):
    xmax, vals = model
    i = bisect.bisect_left(xmax, x)
    return vals[min(i, len(vals) - 1)]


def quantile(sorted_xs, q):
    pos = q * (len(sorted_xs) - 1)
    lo = int(math.floor(pos))
    hi = min(lo + 1, len(sorted_xs) - 1)
    return sorted_xs[lo] + (sorted_xs[hi] - sorted_xs[lo]) * (pos - lo)


def make_knots(model, train_x):
    xs = sorted(train_x)
    gops = sorted({round(quantile(xs, k / (KNOTS - 1)), 4) for k in range(KNOTS)})
    knots, last = [], -1
    for g in gops:
        v = max(last, int(round(step(model, g))))  # rounding must not break monotonicity
        knots.append((g, v))
        last = v
    return knots


def interp(knots, gop):
    if gop <= knots[0][0]:
        return knots[0][1]
    if gop >= knots[-1][0]:
        return knots[-1][1]
    i = bisect.bisect_right([k[0] for k in knots], gop)
    (x0, y0), (x1, y1) = knots[i - 1], knots[i]
    return int(round(y0 + (y1 - y0) * (gop - x0) / (x1 - x0)))


def pearson(a, b):
    n = len(a)
    ma, mb = sum(a) / n, sum(b) / n
    cov = sum((x - ma) * (y - mb) for x, y in zip(a, b))
    va = sum((x - ma) ** 2 for x in a)
    vb = sum((y - mb) ** 2 for y in b)
    return cov / math.sqrt(va * vb) if va > 0 and vb > 0 else float("nan")


def ranks(v):
    order = sorted(range(len(v)), key=lambda i: v[i])
    r = [0.0] * len(v)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and v[order[j + 1]] == v[order[i]]:
            j += 1
        for k in range(i, j + 1):
            r[order[k]] = (i + j) / 2.0
        i = j + 1
    return r


def spearman(a, b):
    return pearson(ranks(a), ranks(b))


def auc(scores, positive):
    """P(a random mispronounced word scores lower than a random good one)."""
    r = ranks(scores)
    npos = sum(positive)
    nneg = len(positive) - npos
    if npos == 0 or nneg == 0:
        return float("nan")
    sum_pos = sum(ri + 1 for ri, p in zip(r, positive) if p)
    # Mann-Whitney U for the positive class, then flip: low score = mispronounced.
    u = sum_pos - npos * (npos + 1) / 2
    return 1 - u / (npos * nneg)


def band(score):
    return "good" if score >= VERDICT_GOOD else "unclear" if score >= VERDICT_UNCLEAR else "poor"


def human_band(h):
    return "good" if h >= 7 else "unclear" if h >= 4 else "poor"


def report(name, rows, scores):
    humans = [r["y"] for r in rows]
    mis = [r["human"] <= MISPRONOUNCED_AT_OR_BELOW for r in rows]
    mae = sum(abs(s - h) for s, h in zip(scores, humans)) / len(rows)
    print(f"  {name:8} pearson {pearson(scores, humans):.3f}  spearman "
          f"{spearman(scores, humans):.3f}  MAE {mae:5.1f}  AUC(mispronounced) {auc(scores, mis):.3f}")


def confusion(name, rows, scores):
    labels = ["good", "unclear", "poor"]
    m = {(h, s): 0 for h in labels for s in labels}
    for r, s in zip(rows, scores):
        m[(human_band(r["human"]), band(s))] += 1
    print(f"  {name}: rows = human band, columns = app verdict")
    print("             " + "".join(f"{s:>9}" for s in labels))
    for h in labels:
        total = sum(m[(h, s)] for s in labels) or 1
        cells = "".join(f"{100 * m[(h, s)] / total:8.1f}%" for s in labels)
        print(f"    {h:8} {cells}   (n={total})")


def reliability(name, rows, scores):
    print(f"  {name}: mean expert score by app-score band")
    for lo in range(0, 100, 20):
        hi = lo + 20 if lo < 80 else 101
        sel = [r["y"] for r, s in zip(rows, scores) if lo <= s < hi]
        if sel:
            print(f"    app {lo:3}-{min(hi, 100):3}: expert mean {sum(sel) / len(sel):5.1f}  (n={len(sel)})")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("tsv")
    ap.add_argument("--cutoffs", type=float, nargs="*", default=[],
                    help="print the GOP value at these percentiles of correctly-rated words")
    args = ap.parse_args()

    rows = load(args.tsv)
    train = [r for r in rows if r["split"] == "train"]
    test = [r for r in rows if r["split"] == "test"]
    if not train or not test:
        sys.exit("need both train and test rows")

    model = pav([r["gop"] for r in train], [r["y"] for r in train])
    knots = make_knots(model, [r["gop"] for r in train])

    print(f"words: {len(train)} train, {len(test)} test; "
          f"mispronounced (expert <= {MISPRONOUNCED_AT_OR_BELOW}) in test: "
          f"{sum(r['human'] <= MISPRONOUNCED_AT_OR_BELOW for r in test)}")
    now = [r["now"] for r in test]
    fitted = [interp(knots, r["gop"]) for r in test]
    exact = [step(model, r["gop"]) for r in test]
    print("\ntest split")
    report("current", test, now)
    report("fitted", test, fitted)
    report("isotonic", test, exact)
    print("  (the 32-knot table vs the full isotonic step it approximates: "
          f"max |diff| {max(abs(a - b) for a, b in zip(fitted, exact)):.1f})")
    for group, sel in (("children (<13)", lambda r: r["age"] is not None and r["age"] < 13),
                       ("adults (>=13)", lambda r: r["age"] is not None and r["age"] >= 13)):
        sub = [r for r in test if sel(r)]
        if sub:
            print(f"\n{group}: {len(sub)} words")
            report("current", sub, [r["now"] for r in sub])
            report("fitted", sub, [interp(knots, r["gop"]) for r in sub])
    print()
    confusion("current", test, now)
    confusion("fitted", test, fitted)
    print()
    reliability("current", test, now)
    reliability("fitted", test, fitted)

    # Operating points: percentile of this word's GOP among words the experts
    # rated a perfect 10 (train split), and what flagging below each cutoff does.
    ref = sorted(r["gop"] for r in train if r["human"] == 10)
    pct = lambda g: 100.0 * bisect.bisect_right(ref, g) / len(ref)
    ok = [r for r in test if r["human"] >= 7]
    mis = [r for r in test if r["human"] <= MISPRONOUNCED_AT_OR_BELOW]
    bad = [r for r in test if r["human"] <= 3]
    print("\noperating points (flag a word below this percentile of correctly-said words)")
    print("  cutoff  correct flagged  mispronounced caught  badly caught  flags that are right")
    for t in (2, 5, 10, 15, 20, 25, 30):
        fa = sum(pct(r["gop"]) < t for r in ok)
        hit = sum(pct(r["gop"]) < t for r in mis)
        hitb = sum(pct(r["gop"]) < t for r in bad)
        prec = hit / (fa + hit) if fa + hit else float("nan")
        print(f"  {t:5}%  {100 * fa / len(ok):14.1f}%  {100 * hit / len(mis):19.1f}%  "
              f"{100 * hitb / len(bad):11.1f}%  {100 * prec:18.1f}%")
    for name, cut in (("current verdict poor (<40)", 40), ("current not-good (<70)", 70)):
        fa = sum(r["now"] < cut for r in ok)
        hit = sum(r["now"] < cut for r in mis)
        print(f"  {name}: flags {100 * fa / len(ok):.1f}% of correct words, catches "
              f"{100 * hit / len(mis):.1f}% of mispronounced, {100 * hit / (fa + hit):.1f}% of flags right")
    for c in args.cutoffs:
        g = quantile(ref, c / 100.0)
        print(f"  GOP cutoff at the {c:g}th percentile of correctly-said words: {g:.4f}")


if __name__ == "__main__":
    main()
