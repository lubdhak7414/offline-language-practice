#!/usr/bin/env python3
"""Fit the GOP -> 0..100 score curve to human ratings. Standard library only.

Input is the TSV written by the `corpus_dump_gop` / `corpus_dump_gop_manifest`
tests in pronounce.rs: one row per word, with the raw GOP the app computes and
a human label for that word. speechocean762 (OpenSLR-101, CC BY 4.0) carries
the experts' 0-10 accuracy; other corpora carry what they have (see LABEL
KINDS below).

    python3 scripts/calibrate-gop.py gop_dump.tsv
    python3 scripts/calibrate-gop.py gop_dump.tsv --cutoffs 10 20   # GOP cutoffs
    python3 scripts/calibrate-gop.py gop_dump.tsv --rust            # the table
    python3 scripts/calibrate-gop.py gop_dump.tsv \\
        --eval-on cmuarctic.tsv l2arctic.tsv --by l1                # by first language

Positional TSVs are what the table may be fitted on: rows of a `--fit-on`
corpus (default speechocean762) whose split is not `test` build it, rows
whose split is `test` are held out and every headline figure is on them.
`--eval-on` TSVs are only ever measured, never fitted. `--rust` refuses to
print a table fitted on any corpus not in SHIPPABLE, so a constant derived
from non-commercial data can never be pasted into the app by accident.

Columns: split utt age word_index word human gop score_now start_ms end_ms,
plus optional corpus l1 speaker label_kind. Old speechocean dumps have none
of the optional four; they default to speechocean762 / mandarin / the
speaker encoded in the utterance id / score0_10.

LABEL KINDS (never pool a Pearson across them):
  score0_10  expert accuracy 0-10 (speechocean762)
  binary     10 = no annotated error in the word, 0 = one or more (L2-ARCTIC)
  validated  10 for every word: a read judged correct, so only false alarms
             can be measured (CMU ARCTIC, Common Voice)

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
import json
import math
import os
import random
import sys

KNOTS = 32
# Rubric bands (README): 10 perfect, 7-9 correct with an accent, 4-6 under 30%
# of phones wrong, 0-3 more than 30% wrong or a different word.
MISPRONOUNCED_AT_OR_BELOW = 6
CORRECT_AT_OR_ABOVE = 7
FLAG_BELOW = 10  # pronounce.rs FLAG_BELOW: a word scoring below it is flagged

# Corpora a shipped constant may be fitted on, with the license that allows it.
SHIPPABLE = {
    "speechocean762": "CC BY 4.0",
    "commonvoice": "CC0-1.0",
    "cmuarctic": "CMU ARCTIC permissive",
}
# Corpora known to be evaluation-only, so the refusal can name the license.
EVAL_ONLY = {
    "l2arctic": "CC BY-NC 4.0",
    "epadb": "CC BY-NC 4.0",
    "speechaccent": "CC BY-NC-SA 4.0",
}

DEFAULTS = {"corpus": "speechocean762", "l1": "mandarin", "label_kind": "score0_10"}
BOOT_SEED = 762


def default_speaker(corpus, utt):
    # speechocean762 ids are 0 + 4-digit speaker + 4-digit utterance (checked
    # against utt2spk: all 5,000 agree). Anything else is its own speaker,
    # which makes a bootstrap over it no narrower than one over utterances.
    if corpus == "speechocean762" and len(utt) == 9 and utt.isdigit():
        return utt[1:5]
    return utt


def parse(lines, src="<memory>"):
    """Rows from TSV text lines (header first). Unlabelled words are skipped."""
    rows = []
    for r in csv.DictReader(lines, delimiter="\t"):
        human = int(r["human"])
        if human < 0:
            continue
        age = (r.get("age") or "").strip()
        corpus = (r.get("corpus") or "").strip() or DEFAULTS["corpus"]
        rows.append({
            "split": r["split"], "utt": r["utt"], "gop": float(r["gop"]), "now": int(r["score_now"]),
            "human": human, "y": human * 10.0, "age": int(age) if age.isdigit() else None,
            "corpus": corpus,
            "l1": (r.get("l1") or "").strip() or DEFAULTS["l1"],
            "speaker": (r.get("speaker") or "").strip() or default_speaker(corpus, r["utt"]),
            "label_kind": (r.get("label_kind") or "").strip() or DEFAULTS["label_kind"],
            "src": src,
        })
    return rows


def load(path):
    with open(path, newline="") as f:
        return parse(f, src=path)


def unshippable(fit_on):
    """Why a table fitted on these corpora may not ship, or None if it may."""
    bad = [c for c in fit_on if c not in SHIPPABLE]
    if not bad:
        return None
    named = ", ".join(f"{c} ({EVAL_ONLY.get(c, 'license unknown')})" for c in bad)
    return (f"refusing --rust: fitted on {named}. Only {', '.join(sorted(SHIPPABLE))} may "
            "feed a shipped constant; use the others with --eval-on")


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
    """P(a random mispronounced word scores lower than a random good one), ties half.

    One sort and one pass (Mann-Whitney U), so the bootstrap can afford it.
    """
    pairs = sorted(zip(scores, positive), reverse=True)
    npos = sum(1 for _, p in pairs if p)
    nneg = len(pairs) - npos
    if npos == 0 or nneg == 0:
        return float("nan")
    wins, neg_above, i = 0.0, 0, 0
    while i < len(pairs):
        j, pos_here, neg_here = i, 0, 0
        while j < len(pairs) and pairs[j][0] == pairs[i][0]:
            if pairs[j][1]:
                pos_here += 1
            else:
                neg_here += 1
            j += 1
        wins += pos_here * (neg_above + 0.5 * neg_here)
        neg_above += neg_here
        i = j
    return wins / (npos * nneg)


def human_band(h):
    return "good" if h >= CORRECT_AT_OR_ABOVE else "unclear" if h >= 4 else "poor"


def report(name, rows, scores):
    humans = [r["y"] for r in rows]
    mis = [r["human"] <= MISPRONOUNCED_AT_OR_BELOW for r in rows]
    mae = sum(abs(s - h) for s, h in zip(scores, humans)) / len(rows)
    print(f"  {name:19} pearson {pearson(scores, humans):.3f}  spearman "
          f"{spearman(scores, humans):.3f}  MAE {mae:5.1f}  AUC(mispronounced) {auc(scores, mis):.3f}")


def confusion(name, rows, scores):
    labels = ["good", "unclear", "poor"]
    m = {(h, f): 0 for h in labels for f in (True, False)}
    for r, s in zip(rows, scores):
        m[(human_band(r["human"]), s < FLAG_BELOW)] += 1
    print(f"  {name}: rows = expert band, columns = flagged (score < {FLAG_BELOW}) or not")
    print("             " + "".join(f"{c:>12}" for c in ("flagged", "not flagged")))
    for h in labels:
        total = m[(h, True)] + m[(h, False)] or 1
        cells = "".join(f"{100 * m[(h, f)] / total:11.1f}%" for f in (True, False))
        print(f"    {h:8} {cells}   (n={total})")


def reliability(name, rows, scores):
    print(f"  {name}: mean expert score by app-score band")
    for lo in range(0, 100, 20):
        hi = lo + 20 if lo < 80 else 101
        sel = [r["y"] for r, s in zip(rows, scores) if lo <= s < hi]
        if sel:
            print(f"    app {lo:3}-{min(hi, 100):3}: expert mean {sum(sel) / len(sel):5.1f}  (n={len(sel)})")


# Percentile levels the shipped table is pinned at. Dense below the flag
# cutoff, where the decision is made; sparse near the top, where ~60% of
# correctly-said words sit at GOP ~ 0 and the curve is a near-vertical step.
LEVELS = [0, 0.5, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 14, 16, 18, 20, 22, 24,
          26, 28, 30, 32, 34, 36, 38, 40, 60, 80, 100]


def build_table(ref):
    # Each knot's value is the true percentile *at* its GOP — the share of
    # correctly-said words at or below it — not the level it was requested at.
    # Most correct words tie at GOP ~ 0, so several levels land on one GOP;
    # taking the requested level there would score a perfect word at 36.
    knots = []
    for g in sorted({round(quantile(ref, lv / 100.0), 4) for lv in LEVELS}):
        v = int(round(100.0 * bisect.bisect_right(ref, g) / len(ref)))
        if knots and v <= knots[-1][1] and g > knots[-1][0]:
            v = knots[-1][1]  # non-decreasing, never inventing a rise
        knots.append((g, v))
    return knots


def measure_table(knots, test):
    score = lambda g: interp(knots, g)
    ok = [r for r in test if r["human"] >= CORRECT_AT_OR_ABOVE]
    mis = [r for r in test if r["human"] <= MISPRONOUNCED_AT_OR_BELOW]
    print(f"\npercentile table ({len(knots)} knots), measured on held-out test as interpolated:")
    fa = sum(score(r["gop"]) < FLAG_BELOW for r in ok) / len(ok) if ok else float("nan")
    line = f"  flags {100 * fa:.1f}% of correctly-said words"
    if mis:
        hit = sum(score(r["gop"]) < FLAG_BELOW for r in mis) / len(mis)
        line += f", catches {100 * hit:.1f}% of mispronounced"
    print(line)

    # Only speechocean762 has expert sentence scores, in resource/scores.json.
    so = [r for r in test if r["corpus"] == "speechocean762"]
    if not so:
        return
    by = {}
    for r in so:
        by.setdefault(r["utt"], []).append(r)
    sent = {u: sum(score(w["gop"]) for w in ws) / len(ws) for u, ws in by.items()}
    print("  sentence score (mean of word scores) against expert sentence accuracy:")
    print("  (needs resource/scores.json next to the speechocean762 TSV; skipped if absent)")
    sj = os.path.join(os.path.dirname(os.path.abspath(so[0]["src"])), "resource", "scores.json")
    if os.path.exists(sj):
        with open(sj) as f:
            S = json.load(f)
        acc = [S[u]["accuracy"] for u in sent]
        print(f"    pearson {pearson(list(sent.values()), acc):.3f}")
        for lo, hi, name in ((90, 101, "Clear"), (75, 90, "Mostly clear"), (55, 75, "Understandable"),
                             (30, 55, "Hard to follow"), (0, 30, "Needs work")):
            a = [S[u]["accuracy"] for u, v in sent.items() if lo <= v < hi]
            if a:
                print(f"    {name:15} ({lo:3}-{min(hi, 100):3}): n={len(a):4}  expert sentence accuracy "
                      f"mean {sum(a) / len(a):.2f}/10")


def print_rust(knots):
    print("\n// ---- paste into pronounce.rs ----")
    print(f"pub const GOP_PERCENTILE: [(f32, u8); {len(knots)}] = [")
    for g, v in knots:
        print(f"    ({g:.4f}, {v}),")
    print("];")


def metrics(rows):
    """(false-alarm rate, catch rate, precision, AUC on raw GOP) at FLAG_BELOW.

    Needs each row's `table` score. NaN where the group has no words of the
    kind a figure needs (a validated corpus has no mispronounced words).
    """
    nan = float("nan")
    ok = [r for r in rows if r["human"] >= CORRECT_AT_OR_ABOVE]
    mis = [r for r in rows if r["human"] <= MISPRONOUNCED_AT_OR_BELOW]
    fa = sum(r["table"] < FLAG_BELOW for r in ok)
    hit = sum(r["table"] < FLAG_BELOW for r in mis)
    return (
        fa / len(ok) if ok else nan,
        hit / len(mis) if mis else nan,
        hit / (fa + hit) if mis and fa + hit else nan,
        auc([r["gop"] for r in ok + mis], [False] * len(ok) + [True] * len(mis)),
    )


def bootstrap(rows, stat, key="speaker", n=1000, seed=BOOT_SEED, alpha=0.05):
    """Percentile CI for each component of `stat`, resampling whole `key`s.

    Words from one speaker are not independent, so the unit resampled is the
    speaker, not the word. Returns [(lo, hi), ...], one pair per component.
    """
    groups = {}
    for r in rows:
        groups.setdefault(r[key], []).append(r)
    ids = sorted(groups)
    rng = random.Random(seed)
    draws = []
    for _ in range(n):
        sample = [r for k in rng.choices(ids, k=len(ids)) for r in groups[k]]
        v = stat(sample)
        draws.append(v if isinstance(v, tuple) else (v,))
    out = []
    for c in range(len(draws[0]) if draws else 0):
        vals = sorted(d[c] for d in draws if d[c] == d[c])
        out.append((quantile(vals, alpha / 2), quantile(vals, 1 - alpha / 2)) if vals
                   else (float("nan"), float("nan")))
    return out


def group_key(by):
    if by == "age":
        return lambda r: ("unknown" if r["age"] is None
                          else "children (<13)" if r["age"] < 13 else "adults (>=13)")
    return lambda r: r[by]


def reading(fa_ci, a):
    # Interpretation thresholds from the calibration plan, applied verbatim.
    if fa_ci[0] > 0.15:
        return "over-flags"
    if a == a and a < 0.70:
        return "close to chance"
    if 0.05 <= fa_ci[0] and fa_ci[1] <= 0.15:
        return "holds" if a == a and a >= 0.75 else "FA holds" if a != a else "FA holds, AUC weak"
    if fa_ci[1] < 0.05:
        return "flags less"
    return "inconclusive"


def by_report(rows, by, n_boot):
    pct = lambda x: "   -  " if x != x else f"{100 * x:5.1f}%"
    ci = lambda p: "     -       " if p[0] != p[0] else f"[{100 * p[0]:4.1f}-{100 * p[1]:4.1f}]"
    num = lambda x: "  -  " if x != x else f"{x:.3f}"
    ci_num = lambda p: "      -      " if p[0] != p[0] else f"[{p[0]:.3f}-{p[1]:.3f}]"
    key = group_key(by)
    groups = {}
    for r in rows:
        groups.setdefault(key(r), []).append(r)
    print(f"\nby {by}: flag = table score < {FLAG_BELOW}; 95% CIs from {n_boot} bootstrap "
          f"resamples (seed {BOOT_SEED}) of whole speakers, or of utterances (unit utt) where a "
          "group has one speaker, so its CI speaks for that speaker only; AUC on raw GOP")
    print(f"  {'group':18} {'kind':10} {'spk':>4} {'unit':>7} {'words':>6}  {'correct flagged':>17}  "
          f"{'mispron. caught':>17}  {'flags right':>17}  {'AUC':>19}  reading")
    for g in sorted(groups):
        sub = groups[g]
        kinds = sorted({r["label_kind"] for r in sub})
        fa, hit, prec, a = metrics(sub)
        speakers = len({r["speaker"] for r in sub})
        unit = "speaker" if speakers > 1 else "utt"
        cis = bootstrap(sub, metrics, key=unit, n=n_boot)
        print(f"  {g:18} {'+'.join(kinds):10} {speakers:4} {unit:>7} {len(sub):6}  "
              f"{pct(fa)} {ci(cis[0])}  {pct(hit)} {ci(cis[1])}  {pct(prec)} {ci(cis[2])}  "
              f"{num(a)} {ci_num(cis[3])}  {reading(cis[0], a)}")
        # Binary labels have no sentence score; compare against the share of
        # words with no annotated error, rank-only because the scales differ.
        binary = [r for r in sub if r["label_kind"] == "binary"]
        if binary:
            utts = {}
            for r in binary:
                utts.setdefault((r["corpus"], r["utt"]), []).append(r)
            mean = [sum(w["table"] for w in ws) / len(ws) for ws in utts.values()]
            clean = [sum(w["human"] == 10 for w in ws) / len(ws) for ws in utts.values()]
            print(f"  {'':18} sentence mean vs share of error-free words, spearman "
                  f"{spearman(mean, clean):.3f} (n={len(utts)})")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("tsv", nargs="+", help="dumps the table may be fitted on (see --fit-on)")
    ap.add_argument("--fit-on", nargs="+", default=["speechocean762"], metavar="CORPUS",
                    help="corpora whose non-test rows build the table (default speechocean762)")
    ap.add_argument("--eval-on", nargs="+", default=[], metavar="TSV",
                    help="extra dumps that are only measured, never fitted")
    ap.add_argument("--by", choices=["l1", "corpus", "age", "speaker"],
                    help="false alarms, catch rate, precision and AUC per group, with CIs")
    ap.add_argument("--boot", type=int, default=1000, help="bootstrap resamples for --by")
    ap.add_argument("--rust", action="store_true",
                    help="print the percentile table for pronounce.rs (shippable corpora only)")
    ap.add_argument("--cutoffs", type=float, nargs="*", default=[],
                    help="print the GOP value at these percentiles of correctly-rated words")
    args = ap.parse_args(argv)

    if args.rust:
        why = unshippable(args.fit_on)
        if why:
            sys.exit(why)

    rows = [r for p in args.tsv for r in load(p)]
    skipped = sorted({r["corpus"] for r in rows} - set(args.fit_on))
    if skipped:
        print(f"note: ignoring rows of {', '.join(skipped)} (not in --fit-on; use --eval-on)",
              file=sys.stderr)
    rows = [r for r in rows if r["corpus"] in args.fit_on]
    train = [r for r in rows if r["split"] != "test"]
    test = [r for r in rows if r["split"] == "test"]
    if not train or not test:
        sys.exit("need both train and test rows")
    evals = [r for p in args.eval_on for r in load(p)]
    leaked = sorted({r["corpus"] for r in evals if r["corpus"] in args.fit_on and r["split"] != "test"})
    if leaked:
        print(f"warning: --eval-on rows of {', '.join(leaked)} are also fitted on; "
              "their figures are in-sample", file=sys.stderr)

    # Only graded labels have an expert mean to regress on or correlate with.
    graded_train = [r for r in train if r["label_kind"] == "score0_10"]
    graded = [r for r in test if r["label_kind"] == "score0_10"]

    # The table: percentile of a word's GOP among words labelled a perfect 10.
    ref = sorted(r["gop"] for r in train if r["human"] == 10)
    knots = build_table(ref)
    tabled = lambda sel: [interp(knots, r["gop"]) for r in sel]

    print(f"words: {len(train)} fit, {len(test)} held-out test; "
          f"mispronounced (expert <= {MISPRONOUNCED_AT_OR_BELOW}) in test: "
          f"{sum(r['human'] <= MISPRONOUNCED_AT_OR_BELOW for r in test)}")
    if graded_train and graded:
        # "score at dump time" is score_now: whatever mapping the app had when
        # the dump ran (the retired TAU curve, for the 2026-09-22 dump).
        model = pav([r["gop"] for r in graded_train], [r["y"] for r in graded_train])
        iso_knots = make_knots(model, [r["gop"] for r in graded_train])
        now = [r["now"] for r in graded]
        pct_scores = tabled(graded)
        fitted = [interp(iso_knots, r["gop"]) for r in graded]
        exact = [step(model, r["gop"]) for r in graded]
        print("\ntest split, expert-graded words")
        report("score at dump time", graded, now)
        report("percentile table", graded, pct_scores)
        report("isotonic, 32 knots", graded, fitted)
        report("isotonic, exact", graded, exact)
        print("  (the 32-knot table vs the full isotonic step it approximates: "
              f"max |diff| {max(abs(a - b) for a, b in zip(fitted, exact)):.1f})")
        for group, sel in (("children (<13)", lambda r: r["age"] is not None and r["age"] < 13),
                           ("adults (>=13)", lambda r: r["age"] is not None and r["age"] >= 13)):
            sub = [r for r in graded if sel(r)]
            if sub:
                print(f"\n{group}: {len(sub)} words")
                report("score at dump time", sub, [r["now"] for r in sub])
                report("percentile table", sub, tabled(sub))
        print()
        confusion("score at dump time", graded, now)
        confusion("percentile table", graded, pct_scores)
        confusion("isotonic, 32 knots", graded, fitted)
        print()
        reliability("score at dump time", graded, now)
        reliability("percentile table", graded, pct_scores)
        reliability("isotonic, 32 knots", graded, fitted)

    # Operating points: percentile of this word's GOP among words the experts
    # rated a perfect 10 (fit rows), and what flagging below each cutoff does.
    pct = lambda g: 100.0 * bisect.bisect_right(ref, g) / len(ref)
    ok = [r for r in test if r["human"] >= CORRECT_AT_OR_ABOVE]
    mis = [r for r in test if r["human"] <= MISPRONOUNCED_AT_OR_BELOW]
    bad = [r for r in test if r["human"] <= 3]
    if ok and mis and bad:
        print("\noperating points (flag a word below this percentile of correctly-said words)")
        print("  cutoff  correct flagged  mispronounced caught  badly caught  flags that are right")
        for t in (2, 5, 10, 15, 20, 25, 30):
            fa = sum(pct(r["gop"]) < t for r in ok)
            hit = sum(pct(r["gop"]) < t for r in mis)
            hitb = sum(pct(r["gop"]) < t for r in bad)
            prec = hit / (fa + hit) if fa + hit else float("nan")
            print(f"  {t:5}%  {100 * fa / len(ok):14.1f}%  {100 * hit / len(mis):19.1f}%  "
                  f"{100 * hitb / len(bad):11.1f}%  {100 * prec:18.1f}%")
        fa = sum(r["now"] < FLAG_BELOW for r in ok)
        hit = sum(r["now"] < FLAG_BELOW for r in mis)
        prec = 100 * hit / (fa + hit) if fa + hit else float("nan")
        print(f"  score at dump time < {FLAG_BELOW}: flags {100 * fa / len(ok):.1f}% of correct words, "
              f"catches {100 * hit / len(mis):.1f}% of mispronounced, {prec:.1f}% of flags right")
    for c in args.cutoffs:
        g = quantile(ref, c / 100.0)
        print(f"  GOP cutoff at the {c:g}th percentile of correctly-said words: {g:.4f}")

    measure_table(knots, test)

    if args.by:
        pool = test + evals
        for r in pool:
            r["table"] = interp(knots, r["gop"])
        by_report(pool, args.by, args.boot)

    if args.rust:
        print_rust(knots)


if __name__ == "__main__":
    main()
