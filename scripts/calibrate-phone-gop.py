#!/usr/bin/env python3
"""Measure phone-level GOP against expert phone scores. Standard library only.

Input is the TSV written by `corpus_dump_phone_gop` (pronounce.rs): one row
per target phone of every speechocean762 utterance, scored by the optional
phoneme model twice, with espeak's phones (`g2p = espeak`, what the app would
do) and with the corpus's own canonical phones (`g2p = oracle`).

    python3 scripts/calibrate-phone-gop.py uint8_train.tsv uint8_test.tsv \\
        --grapheme ~/corpora/speechocean762/gop_dump.tsv \\
        --fp32 fp32_test.tsv --rtf 0.12 --p95-added 1.3 --rss-added 650 --gates

It answers the Phase 7 Stage 6A go/no-go questions:

* Gate 1: is the phone model a better *word and sentence* score than the
  shipped grapheme GOP (`gop_dump.tsv`, same words, paired)?
* Gate 2: can it *name one sound* per attempt and be right at least half
  the time?

Every choice (word formula, percentile tables, the naming cutoff P_NAME,
eligible phone classes) is made on the train split and reported on test.
CIs resample whole speakers (1,000 draws, seed 762), as calibrate-gop.py does.
`--gates` prints each gate as metric, value, threshold, PASS/FAIL and exits 1
if any fails. The cost gate (G1.7) needs the E0 numbers passed in; without
them it FAILs, because an unmeasured cost is not a passed one.

Aligning espeak's phones with the corpus's (ARPAbet, often British
canonicals) is done here, not in the dump: composite units (`ɑːɹ`, `əl`, …)
are split into their parts, both sides are reduced to coarse classes (length
dropped, ɐ=ə, ᵻ=ɪ, ɾ/ʔ=t, ʌ=ə, ɚ=ɜ), and a Levenshtein alignment pairs them.
A phone is labelled only where its parts match the corpus on class; its label
is the lowest expert score among them. Unlabelled phones are counted, and a
phone the policy names without a label is "unjudged".
"""
import argparse
import bisect
import csv
import importlib.util
import json
import math
import os
import sys

_here = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("calibrate_gop", os.path.join(_here, "calibrate-gop.py"))
cg = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cg)

MIS = cg.MISPRONOUNCED_AT_OR_BELOW  # word expert <= 6
FLAG_BELOW = cg.FLAG_BELOW  # pronounce.rs FLAG_BELOW
PHONE_POSITIVE_BELOW = 1.5  # expert mean phones-accuracy (0-2): accented or wrong
CLEAN_SENTENCE_AT_OR_ABOVE = 9
CHILD_BELOW = 13
P_NAME_GRID = [0.5, 1, 2, 3, 5, 7.5, 10, 15, 20]
MIN_REF = 50  # train perfect instances a phone class needs for a percentile

# Gate thresholds, verbatim from the approved plan (Appendix C section 4.4).
G1 = {"auc": 0.83, "precision": 0.30, "sentence": 0.55, "fp32_drop": 0.01,
      "refusals": 0.01, "rtf": 0.30, "p95_added": 2.0, "rss_added": 800}
G2 = {"precision": 0.50, "precision_lo": 0.40, "clean_named": 0.10, "useful": 0.10,
      "class_positives": 40, "class_train_precision": 0.50, "group_precision": 0.45}

# Corpus ARPAbet -> IPA, mirroring `arpa_to_ipa` in pronounce.rs.
ARPA_IPA = {
    "AA": "ɑː", "AE": "æ", "AO": "ɔː", "AW": "aʊ", "AY": "aɪ", "EH": "ɛ", "EY": "eɪ",
    "IH": "ɪ", "OW": "oʊ", "OY": "ɔɪ", "UH": "ʊ", "UW": "uː", "B": "b", "CH": "tʃ", "D": "d",
    "DH": "ð", "F": "f", "G": "ɡ", "HH": "h", "JH": "dʒ", "K": "k", "L": "l", "M": "m",
    "N": "n", "NG": "ŋ", "P": "p", "R": "ɹ", "S": "s", "SH": "ʃ", "T": "t", "TH": "θ",
    "V": "v", "W": "w", "Y": "j", "Z": "z", "ZH": "ʒ",
}
REDUCED = {"AH": ("ə", "ʌ"), "ER": ("ɚ", "ɜː"), "IY": ("i", "iː")}

# espeak units the model holds as one token, split for alignment.
COMPOSITES = {
    "ɑːɹ": ["ɑː", "ɹ"], "ɔːɹ": ["ɔː", "ɹ"], "ɛɹ": ["ɛ", "ɹ"], "ɪɹ": ["ɪ", "ɹ"],
    "ʊɹ": ["ʊ", "ɹ"], "əl": ["ə", "l"], "aɪɚ": ["aɪ", "ɚ"], "aɪə": ["aɪ", "ə"],
    "iə": ["i", "ə"], "n̩": ["ə", "n"],
}
COARSE = {"ɐ": "ə", "ᵻ": "ɪ", "ɾ": "t", "ʔ": "t", "ʌ": "ə", "ɚ": "ɜ", "ɔ": "ɔ"}


def arpa_to_ipa(p):
    base = p.rstrip("0123456789")
    if base in REDUCED:
        return REDUCED[base][0] if p.endswith("0") else REDUCED[base][1]
    return ARPA_IPA.get(base)


def coarse(ipa):
    s = ipa.replace("ː", "")
    return COARSE.get(s, s)


def expand(phone):
    return COMPOSITES.get(phone, [phone])


def align_word(espeak, arpa, acc):
    """Expert score per espeak phone of one word, or None where unlabelled.

    Levenshtein over coarse classes (unit costs, ties toward the diagonal).
    A phone gets a label only if every part of it is paired with a corpus
    phone of the same class; the label is the lowest of their scores.
    """
    parts, owner = [], []
    for i, ph in enumerate(espeak):
        for q in expand(ph):
            parts.append(coarse(q))
            owner.append(i)
    ref = [coarse(arpa_to_ipa(a) or a) for a in arpa]
    n, m = len(parts), len(ref)
    d = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n + 1):
        d[i][0] = i
    for j in range(m + 1):
        d[0][j] = j
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            d[i][j] = min(d[i - 1][j - 1] + (parts[i - 1] != ref[j - 1]), d[i - 1][j] + 1, d[i][j - 1] + 1)
    pair = [None] * n
    i, j = n, m
    while i > 0 and j > 0:
        if d[i][j] == d[i - 1][j - 1] + (parts[i - 1] != ref[j - 1]):
            if parts[i - 1] == ref[j - 1]:
                pair[i - 1] = j - 1
            i, j = i - 1, j - 1
        elif d[i][j] == d[i - 1][j] + 1:
            i -= 1
        else:
            j -= 1
    labels = [[] for _ in espeak]
    ok = [True] * len(espeak)
    for k, o in enumerate(owner):
        if pair[k] is None:
            ok[o] = False
        else:
            labels[o].append(acc[pair[k]])
    return [min(l) if ok[i] and l else None for i, l in enumerate(labels)]


def load(paths):
    """Phone rows and refusal rows from one or more dumps."""
    phones, refusals = [], []
    for p in paths:
        with open(p, newline="") as f:
            for r in csv.DictReader(f, delimiter="\t"):
                age = (r.get("age") or "").strip()
                base = {"split": r["split"], "utt": r["utt"], "speaker": r["speaker"] or r["utt"],
                        "age": int(age) if age.isdigit() else None, "g2p": r["g2p"],
                        "model": r["model"]}
                if r["status"] != "ok":
                    refusals.append(dict(base, why=r["status"]))
                    continue
                base.update(word_index=int(r["word_index"]), word=r["word"],
                            phone_index=int(r["phone_index"]), phone=r["phone"],
                            gop=float(r["gop"]), frames=int(r["frames"]),
                            word_gop=float(r["word_gop"]), human=int(r["human_word"]),
                            arpa=r["canon_arpa"].split(), acc=[float(x) for x in r["canon_acc"].split()])
                phones.append(base)
    return phones, refusals


def words_of(phones):
    """One record per (utt, word): its phones in order and both word GOPs."""
    by = {}
    for p in phones:
        k = (p["utt"], p["word_index"])
        w = by.get(k)
        if w is None:
            w = by[k] = {"utt": p["utt"], "word_index": p["word_index"], "split": p["split"],
                         "speaker": p["speaker"], "age": p["age"], "human": p["human"],
                         "mean": p["word_gop"], "phones": [], "arpa": p["arpa"], "acc": p["acc"]}
        w["phones"].append(p)
    for w in by.values():
        w["phones"].sort(key=lambda p: p["phone_index"])
        w["min"] = min(p["gop"] for p in w["phones"])
    return by


def select(phones, refusals, g2p, model=None):
    keep = lambda r: r["g2p"] == g2p and (model is None or r["model"] == model)
    return [p for p in phones if keep(p)], [r for r in refusals if keep(r)]


def word_auc(words, key):
    return cg.auc([w[key] for w in words], [w["human"] <= MIS for w in words])


def table_scores(train_words, test_words, key):
    ref = sorted(w[key] for w in train_words if w["human"] == 10)
    knots = cg.build_table(ref)
    for w in test_words:
        w["table"] = cg.interp(knots, w[key])
    return knots


def flag_metrics(words):
    ok = [w for w in words if w["human"] > MIS]
    mis = [w for w in words if w["human"] <= MIS]
    fa = sum(w["table"] < FLAG_BELOW for w in ok)
    hit = sum(w["table"] < FLAG_BELOW for w in mis)
    nan = float("nan")
    return (fa / len(ok) if ok else nan, hit / len(mis) if mis else nan,
            hit / (fa + hit) if fa + hit else nan)


def sentence_pearson(words, scores_json):
    by = {}
    for w in words:
        by.setdefault(w["utt"], []).append(w["table"])
    utts = sorted(by)
    return cg.pearson([sum(by[u]) / len(by[u]) for u in utts],
                      [scores_json[u]["accuracy"] for u in utts]), len(utts)


def load_grapheme(path):
    """(utt, word_index) -> grapheme GOP, and the shipped-style table fitted
    on its train rows (percentile among words rated a perfect 10)."""
    out, ref = {}, []
    with open(path, newline="") as f:
        for r in csv.DictReader(f, delimiter="\t"):
            g = float(r["gop"])
            out[(r["utt"], int(r["word_index"]))] = g
            if r["split"] != "test" and r["human"] == "10":
                ref.append(g)
    return out, cg.build_table(sorted(ref))


def is_child(w):
    return w["age"] is not None and w["age"] < CHILD_BELOW


# ---------------------------------------------------------------- Gate 2

def label_phones(words):
    """Attach `label` (expert score or None) to every phone of every word."""
    for w in words:
        labels = align_word([p["phone"] for p in w["phones"]], w["arpa"], w["acc"])
        for p, l in zip(w["phones"], labels):
            p["label"] = l


def phone_refs(train_phones):
    """Per phone class, sorted train GOPs of instances experts rated perfect (2.0)."""
    refs = {}
    for p in train_phones:
        if p.get("label") == 2.0:
            refs.setdefault(p["phone"], []).append(p["gop"])
    return {k: sorted(v) for k, v in refs.items() if len(v) >= MIN_REF}


def attach_percentiles(phones, refs):
    for p in phones:
        ref = refs.get(p["phone"])
        p["pct"] = 100.0 * bisect.bisect_right(ref, p["gop"]) / len(ref) if ref else None


def class_precision(phones, p_name):
    """Per class: (named-and-positive, named-and-labelled) at pct < p_name."""
    out = {}
    for p in phones:
        if p["pct"] is None or p["pct"] >= p_name or p["label"] is None:
            continue
        c = out.setdefault(p["phone"], [0, 0])
        c[1] += 1
        c[0] += p["label"] < PHONE_POSITIVE_BELOW
    return out


def name_per_attempt(phones, eligible, p_name):
    """utt -> the one phone the policy names (or None), plus every attempt."""
    by = {}
    for p in phones:
        by.setdefault(p["utt"], []).append(p)
    named = {}
    for u, ps in by.items():
        cands = [p for p in ps if p["phone"] in eligible and p["pct"] is not None and p["pct"] < p_name]
        named[u] = min(cands, key=lambda p: (p["pct"], p["gop"])) if cands else None
    return by, named


def policy_stats(by, named, attempts=None):
    """(precision judged, precision with unjudged as wrong, named, judged)."""
    right = judged = n = 0
    for u in (attempts if attempts is not None else by):
        p = named.get(u)
        if p is None:
            continue
        n += 1
        if p["label"] is not None:
            judged += 1
            right += p["label"] < PHONE_POSITIVE_BELOW
    nan = float("nan")
    return (right / judged if judged else nan, right / n if n else nan, n, judged)


def choose_policy(train_phones, test_phones, scores_json):
    """Pick P_NAME and eligible classes on train (test only supplies the
    G2.4 positives-count floor the plan sets). Returns (p_name, eligible,
    per-class table at that cutoff, test positives per class).

    A cutoff is feasible on train when the named phones are right at least
    half the time and at most 10% of clean attempts get one (G2.1/G2.2's
    bars, applied to train). Among feasible cutoffs the one that names in
    the most attempts wins; with none feasible, the most precise one does,
    so the test figures show how far off the best train choice was.
    """
    test_pos = {}
    for p in test_phones:
        if p["label"] is not None and p["label"] < PHONE_POSITIVE_BELOW:
            test_pos[p["phone"]] = test_pos.get(p["phone"], 0) + 1
    best = None
    for p_name in P_NAME_GRID:
        cp = class_precision(train_phones, p_name)
        eligible = {c for c, (pos, lab) in cp.items()
                    if lab and pos / lab >= G2["class_train_precision"]
                    and test_pos.get(c, 0) >= G2["class_positives"]}
        if not eligible:
            continue
        by, named = name_per_attempt(train_phones, eligible, p_name)
        prec, _, n, _ = policy_stats(by, named)
        clean = [u for u in by if scores_json[u]["accuracy"] >= CLEAN_SENTENCE_AT_OR_ABOVE]
        clean_rate = sum(named[u] is not None for u in clean) / len(clean) if clean else 1.0
        feasible = prec >= G2["precision"] and clean_rate <= G2["clean_named"]
        key = (feasible, n if feasible else prec)
        if best is None or key > best[0]:
            best = (key, p_name, eligible, cp)
    if best is None:
        return None, set(), class_precision(train_phones, 10), test_pos
    return best[1], best[2], best[3], test_pos


# ---------------------------------------------------------------- report

def row(gid, metric, value, threshold, ok):
    return (gid, metric, value, threshold, "PASS" if ok else "FAIL")


def fmt(x, nd=3):
    return "-" if x is None or x != x else f"{x:.{nd}f}"


def gate1(train_words, test_words, graph, graph_knots, fp32_words, refusals, n_test_utts, scores_json, args):
    rows = []
    formula = max(("mean", "min"), key=lambda k: word_auc(train_words, k))
    print(f"word GOP formula chosen on train: {formula} "
          f"(train AUC mean {fmt(word_auc(train_words, 'mean'))}, min {fmt(word_auc(train_words, 'min'))})")
    paired = [w for w in test_words if (w["utt"], w["word_index"]) in graph]
    for w in paired:
        w["graph"] = graph[(w["utt"], w["word_index"])]
    a_phone, a_graph = word_auc(paired, formula), word_auc(paired, "graph")
    diff = lambda ws: word_auc(ws, formula) - word_auc(ws, "graph")
    (lo, hi), = cg.bootstrap(paired, diff, key="speaker", n=args.boot)
    print(f"test words scored by both models: {len(paired)} of {len(test_words)} phone-scored; "
          f"AUC phone {fmt(a_phone)} vs grapheme {fmt(a_graph)}, "
          f"diff 95% CI [{fmt(lo)}, {fmt(hi)}]")
    rows.append(row("G1.1", "word AUC (expert <= 6)", f"{fmt(a_phone)} (diff CI [{fmt(lo)}, {fmt(hi)}])",
                    f">= {G1['auc']} and CI > 0", a_phone >= G1["auc"] and lo > 0))

    for w in paired:
        w["table"] = cg.interp(graph_knots, w["graph"])
    g_fa, g_catch, g_prec = flag_metrics(paired)
    g_sent, _ = sentence_pearson(paired, scores_json)
    print(f"grapheme baseline on the same words: flags {fmt(100 * g_fa, 1)}% of correct, catches "
          f"{fmt(100 * g_catch, 1)}%, precision {fmt(g_prec)}, sentence Pearson {fmt(g_sent)}")
    table_scores(train_words, paired, formula)
    fa, catch, prec = flag_metrics(paired)
    print(f"phone table (train-fitted): flags {fmt(100 * fa, 1)}% of correct test words, "
          f"catches {fmt(100 * catch, 1)}% of mispronounced, precision {fmt(prec)}")
    rows.append(row("G1.2", f"word precision at the train cutoff (test FA {fmt(100 * fa, 1)}%)",
                    f"{fmt(prec)} (grapheme {fmt(g_prec)})", f">= {G1['precision']}",
                    prec >= G1["precision"]))

    r_sent, n_sent = sentence_pearson(paired, scores_json)
    rows.append(row("G1.3", f"sentence Pearson vs expert accuracy (n={n_sent})",
                    f"{fmt(r_sent)} (grapheme {fmt(g_sent)})",
                    f">= {G1['sentence']}", r_sent >= G1["sentence"]))

    if fp32_words:
        common = [w for w in test_words if (w["utt"], w["word_index"]) in fp32_words]
        a_q = word_auc(common, formula)
        a_f = word_auc([fp32_words[(w["utt"], w["word_index"])] for w in common], formula)
        rows.append(row("G1.4", f"fp32 - uint8 word AUC (n={len(common)})",
                        f"{fmt(a_f - a_q)} (fp32 {fmt(a_f)}, uint8 {fmt(a_q)})",
                        f"<= {G1['fp32_drop']}", a_f - a_q <= G1["fp32_drop"]))
    else:
        rows.append(row("G1.4", "fp32 - uint8 word AUC", "not measured", f"<= {G1['fp32_drop']}", False))

    ok5, parts = True, []
    for name, sel in (("children", is_child), ("adults", lambda w: not is_child(w) and w["age"] is not None)):
        sub = [w for w in paired if sel(w)]
        ap, ag = word_auc(sub, formula), word_auc(sub, "graph")
        parts.append(f"{name} {fmt(ap)} vs {fmt(ag)}")
        ok5 = ok5 and ap >= ag
    rows.append(row("G1.5", "AUC by age, phone vs grapheme", "; ".join(parts), ">= grapheme each", ok5))

    refused = {r["utt"] for r in refusals if r["split"] == "test"}
    rate = len(refused) / n_test_utts if n_test_utts else float("nan")
    whys = {}
    for r in refusals:
        if r["split"] == "test":
            whys[r["why"]] = whys.get(r["why"], 0) + 1
    rows.append(row("G1.6", f"refused test utterances {len(refused)}/{n_test_utts} {whys or ''}",
                    fmt(100 * rate, 2) + "%", f"<= {100 * G1['refusals']:.0f}%", rate <= G1["refusals"]))

    cost = (args.rtf, args.p95_added, args.rss_added)
    if None in cost:
        rows.append(row("G1.7", "cost: RTF / added p95 s / added RSS MiB", "not given",
                        f"<= {G1['rtf']} / {G1['p95_added']} / {G1['rss_added']}", False))
    else:
        rows.append(row("G1.7", "cost: RTF / added p95 s / added RSS MiB",
                        f"{args.rtf:.3f} / {args.p95_added:.2f} / {args.rss_added:.0f}",
                        f"<= {G1['rtf']} / {G1['p95_added']} / {G1['rss_added']}",
                        args.rtf <= G1["rtf"] and args.p95_added <= G1["p95_added"]
                        and args.rss_added <= G1["rss_added"]))
    return rows, formula


def gate2(train_words, test_words, scores_json, args):
    rows = []
    label_phones(train_words + test_words)
    train_phones = [p for w in train_words for p in w["phones"]]
    test_phones = [p for w in test_words for p in w["phones"]]
    for name, ps in (("train", train_phones), ("test", test_phones)):
        lab = [p for p in ps if p["label"] is not None]
        pos = sum(p["label"] < PHONE_POSITIVE_BELOW for p in lab)
        print(f"{name} phones: {len(ps)}, labelled {len(lab)} ({fmt(100 * len(lab) / len(ps), 1)}%), "
              f"positive (expert < {PHONE_POSITIVE_BELOW}) {pos} ({fmt(100 * pos / max(len(lab), 1), 1)}%)")
    refs = phone_refs(train_phones)
    attach_percentiles(train_phones + test_phones, refs)
    p_name, eligible, cp, test_pos = choose_policy(train_phones, test_phones, scores_json)
    print(f"policy chosen on train: P_NAME {p_name}, eligible classes {sorted(eligible) or 'none'}")
    top = sorted(cp.items(), key=lambda kv: -kv[1][1])[:12]
    print("  train class precision at that cutoff (named positive / named labelled, test positives): "
          + ", ".join(f"{c} {pos}/{lab} ({test_pos.get(c, 0)})" for c, (pos, lab) in top))
    rows.append(row("G2.4", "eligible classes (>= 40 test positives, train precision >= 0.50)",
                    ", ".join(sorted(eligible)) or "none", ">= 1 class", bool(eligible)))
    if not eligible:
        for gid, metric, thr in (("G2.1", "precision of named phones", ">= 0.50, lower CI >= 0.40"),
                                 ("G2.2", "clean attempts with a named phone", "<= 10%"),
                                 ("G2.3", "attempts with a positive that get a name", ">= 10%"),
                                 ("G2.5", "precision children / adults", ">= 0.45 each")):
            rows.append(row(gid, metric, "nothing is ever named", thr, False))
        rows.append(("G2.6", "L2-ARCTIC, same policy", "not run (no corpus)", ">= 0.40 in 4/6 L1s", "N/A"))
        return sorted(rows)

    by, named = name_per_attempt(test_phones, eligible, p_name)
    prec, prec_strict, n, judged = policy_stats(by, named)
    records = [{"speaker": ps[0]["speaker"], "utt": u} for u, ps in by.items()]
    stat = lambda sample: policy_stats(by, named, [r["utt"] for r in sample])[0]
    (lo, hi), = cg.bootstrap(records, stat, key="speaker", n=args.boot)
    rows.append(row("G2.1", f"precision of named phones (named {n}, judged {judged}; unjudged as wrong "
                    f"{fmt(prec_strict)})", f"{fmt(prec)} [{fmt(lo)}, {fmt(hi)}]",
                    ">= 0.50, lower CI >= 0.40", prec >= G2["precision"] and lo >= G2["precision_lo"]))

    clean = [u for u in by if scores_json[u]["accuracy"] >= CLEAN_SENTENCE_AT_OR_ABOVE]
    clean_rate = sum(named[u] is not None for u in clean) / len(clean) if clean else float("nan")
    rows.append(row("G2.2", f"clean attempts (sentence >= 9, n={len(clean)}) with a named phone",
                    fmt(100 * clean_rate, 1) + "%", "<= 10%", clean_rate <= G2["clean_named"]))

    with_pos = [u for u, ps in by.items()
                if any(p["label"] is not None and p["label"] < PHONE_POSITIVE_BELOW for p in ps)]
    useful = sum(named[u] is not None for u in with_pos) / len(with_pos) if with_pos else float("nan")
    rows.append(row("G2.3", f"attempts with a positive phone (n={len(with_pos)}) that get a name",
                    fmt(100 * useful, 1) + "%", ">= 10%", useful >= G2["useful"]))

    ok5, parts = True, []
    for name, sel in (("children", lambda a: a is not None and a < CHILD_BELOW),
                      ("adults", lambda a: a is not None and a >= CHILD_BELOW)):
        us = [u for u, ps in by.items() if sel(ps[0]["age"])]
        pr = policy_stats(by, named, us)[0]
        parts.append(f"{name} {fmt(pr)}")
        ok5 = ok5 and pr >= G2["group_precision"]
    rows.append(row("G2.5", "precision children / adults", "; ".join(parts), ">= 0.45 each", ok5))
    rows.append(("G2.6", "L2-ARCTIC, same policy", "not run (no corpus)", ">= 0.40 in 4/6 L1s", "N/A"))
    return sorted(rows)


def print_rows(title, rows):
    print(f"\n{title}")
    print(f"  {'id':5} {'metric':62} {'value':38} {'threshold':26} result")
    for gid, metric, value, thr, res in rows:
        print(f"  {gid:5} {metric:62} {value:38} {thr:26} {res}")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("tsv", nargs="+", help="phone dumps (train and test; one model)")
    ap.add_argument("--grapheme", help="the grapheme gop_dump.tsv for the paired comparison "
                    "(default: gop_dump.tsv in the corpus dir)")
    ap.add_argument("--scores", help="speechocean762 resource/scores.json (default: found beside "
                    "the grapheme dump)")
    ap.add_argument("--fp32", nargs="*", default=[], help="fp32 phone dump(s), test split, for G1.4")
    ap.add_argument("--model", help="model tag to use from the dumps (default: the only one)")
    ap.add_argument("--g2p", default="espeak", choices=["espeak", "oracle"])
    ap.add_argument("--rtf", type=float, help="E0: real-time factor of the phone model")
    ap.add_argument("--p95-added", type=float, help="E0: added p95 latency, s, for 10 s of audio")
    ap.add_argument("--rss-added", type=float, help="E0: added peak RSS, MiB")
    ap.add_argument("--boot", type=int, default=1000)
    ap.add_argument("--gates", action="store_true", help="exit 1 unless every gate passes")
    args = ap.parse_args(argv)

    phones, refusals = load(args.tsv)
    models = sorted({p["model"] for p in phones})
    model = args.model or (models[0] if len(models) == 1 else None)
    if model is None:
        sys.exit(f"several models in the dumps ({', '.join(models)}); pick one with --model")
    grapheme = args.grapheme or os.path.join(os.path.dirname(os.path.abspath(args.tsv[0])), "gop_dump.tsv")
    if not os.path.exists(grapheme):
        grapheme = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(args.tsv[0]))), "gop_dump.tsv")
    scores_path = args.scores or os.path.join(os.path.dirname(os.path.abspath(grapheme)), "resource", "scores.json")
    with open(scores_path) as f:
        scores_json = json.load(f)
    graph, graph_knots = load_grapheme(grapheme)

    ph, rf = select(phones, refusals, args.g2p, model)
    words = words_of(ph)
    train = [w for w in words.values() if w["split"] != "test"]
    test = [w for w in words.values() if w["split"] == "test"]
    if not train or not test:
        sys.exit("need both train and test rows")
    n_test_utts = len({r["utt"] for r in ph + rf if r["split"] == "test"})
    print(f"model {model}, g2p {args.g2p}: {len(train)} train / {len(test)} test words, "
          f"{n_test_utts} test utterances")

    fp32_words = {}
    if args.fp32:
        fph, _ = load(args.fp32)
        fph = [p for p in fph if p["g2p"] == args.g2p]
        fp32_words = {k: w for k, w in words_of(fph).items() if w["split"] == "test"}

    g1, formula = gate1(train, test, graph, graph_knots, fp32_words, rf, n_test_utts, scores_json, args)
    g2 = gate2(train, test, scores_json, args)
    print_rows(f"Gate 1: ship the optional model as a better word/sentence score ({args.g2p} G2P)", g1)
    print_rows("Gate 2: name one sound per attempt", g2)
    failed = [r[0] for r in g1 + g2 if r[4] == "FAIL"]
    g1_pass = all(r[4] == "PASS" for r in g1)
    print(f"\nGate 1 {'PASS' if g1_pass else 'FAIL'}; Gate 2 "
          f"{'PASS' if all(r[4] != 'FAIL' for r in g2) else 'FAIL'}"
          + (f" (failed: {', '.join(failed)})" if failed else ""))
    if args.gates and failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
