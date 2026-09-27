#!/usr/bin/env python3
"""Tests for calibrate-phone-gop.py. Standard library only, no corpus needed.

    python3 -m unittest scripts/test_calibrate_phone_gop.py
"""
import contextlib
import importlib.util
import io
import json
import os
import random
import tempfile
import unittest

_spec = importlib.util.spec_from_file_location(
    "calibrate_phone_gop",
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "calibrate-phone-gop.py"))
pg = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(pg)

HEADER = ("split\tutt\tage\tspeaker\tword_index\tword\tphone_index\tphone\tg2p\tgop\tframes"
          "\tstart_ms\tend_ms\tword_gop\thuman_word\tcanon_arpa\tcanon_acc\tmodel\tstatus")


class Align(unittest.TestCase):
    def test_exact_match_labels_every_phone(self):
        self.assertEqual(pg.align_word(["θ", "ɪ", "ŋ", "k"], ["TH", "IH1", "NG", "K"],
                                       [2.0, 1.0, 2.0, 0.5]), [2.0, 1.0, 2.0, 0.5])

    def test_composites_take_the_lowest_part(self):
        # car: espeak k ɑːɹ, corpus K AA1 R.
        self.assertEqual(pg.align_word(["k", "ɑːɹ"], ["K", "AA1", "R"], [2.0, 1.8, 0.4]), [2.0, 0.4])
        # apple: æ p əl vs AE P AH0 L.
        self.assertEqual(pg.align_word(["æ", "p", "əl"], ["AE1", "P", "AH0", "L"],
                                       [2.0, 2.0, 1.0, 1.9]), [2.0, 2.0, 1.0])

    def test_coarse_classes_absorb_espeak_variants(self):
        # Flap and reduced vowels match their corpus counterparts.
        self.assertEqual(pg.align_word(["ɪ", "ɾ"], ["IH0", "T"], [2.0, 1.2]), [2.0, 1.2])
        self.assertEqual(pg.align_word(["ɐ"], ["AH0"], [1.0]), [1.0])

    def test_a_mismatched_phone_gets_no_label(self):
        # British canonical AA vs espeak æ: substituted, so not judged.
        self.assertEqual(pg.align_word(["æ", "f", "t", "ɚ"], ["AA1", "F", "T", "ER0"],
                                       [2.0, 2.0, 2.0, 2.0]), [None, 2.0, 2.0, 2.0])

    def test_extra_espeak_phone_is_unlabelled(self):
        self.assertEqual(pg.align_word(["h", "ə", "l", "oʊ"], ["HH", "L", "OW1"], [2.0, 1.0, 2.0]),
                         [2.0, None, 1.0, 2.0])


class Mapping(unittest.TestCase):
    def test_stress_picks_the_reduced_vowel(self):
        self.assertEqual(pg.arpa_to_ipa("AH0"), "ə")
        self.assertEqual(pg.arpa_to_ipa("AH1"), "ʌ")
        self.assertEqual(pg.arpa_to_ipa("IY0"), "i")
        self.assertEqual(pg.arpa_to_ipa("ER2"), "ɜː")
        self.assertEqual(pg.arpa_to_ipa("G"), "ɡ")
        self.assertIsNone(pg.arpa_to_ipa("XX"))


def synthetic_dump(path, seed=1, good=True):
    """A small two-split dump where low GOP tracks low expert scores when
    `good`, and is unrelated to them otherwise."""
    rng = random.Random(seed)
    lines, scores, graph = [HEADER], {}, ["split\tutt\tage\tword_index\tword\thuman\tgop\tscore_now\tstart_ms\tend_ms"]
    for split in ("train", "test"):
        for s in range(30):
            spk = f"{split}{s:03d}"
            age = 8 if s % 2 else 30
            for u in range(6):
                utt = f"{spk}u{u}"
                words, accs = [], []
                for wi in range(4):
                    bad = rng.random() < 0.3
                    human = rng.choice([3, 5]) if bad else 10
                    pacc = [0.5 if bad and k == 1 else 2.0 for k in range(3)]
                    gops = [(-6.0 * rng.random() if bad and k == 1 else -0.3 * rng.random())
                            if good else -3.0 * rng.random() for k in range(3)]
                    wg = sum(gops) / 3
                    for k, (ph, arpa) in enumerate(zip(["θ", "ɪ", "k"], ["TH", "IH1", "K"])):
                        lines.append("\t".join(map(str, [
                            split, utt, age, spk, wi, "THICK", k, ph, "espeak", f"{gops[k]:.4f}", 1,
                            0, 20, f"{wg:.4f}", human, "TH IH1 K", " ".join(map(str, pacc)), "q", "ok"])))
                    # The grapheme model is a noisier view of the same word.
                    graph.append("\t".join(map(str, [split, utt, age, wi, "THICK", human,
                                                     f"{wg + rng.gauss(0, 1.5):.4f}", 0, 0, 0])))
                    accs.append(human)
                scores[utt] = {"accuracy": round(sum(accs) / len(accs))}
    with open(path, "w") as f:
        f.write("\n".join(lines) + "\n")
    gpath = os.path.join(os.path.dirname(path), "gop_dump.tsv")
    with open(gpath, "w") as f:
        f.write("\n".join(graph) + "\n")
    os.makedirs(os.path.join(os.path.dirname(path), "resource"), exist_ok=True)
    with open(os.path.join(os.path.dirname(path), "resource", "scores.json"), "w") as f:
        json.dump(scores, f)
    return gpath


class Load(unittest.TestCase):
    def test_refusal_rows_are_counted_not_scored(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "dump.tsv")
            with open(p, "w") as f:
                f.write(HEADER + "\n" + "\t".join(["test", "u1", "30", "s1", "-1", "", "-1", "", "espeak",
                                                   "", "", "", "", "", "", "", "", "q", "word_count"]) + "\n")
            phones, refusals = pg.load([p])
            self.assertEqual(phones, [])
            self.assertEqual(refusals[0]["why"], "word_count")


class Gates(unittest.TestCase):
    def run_main(self, argv):
        out = io.StringIO()
        code = 0
        with contextlib.redirect_stdout(out):
            try:
                pg.main(argv)
            except SystemExit as e:
                code = e.code
        return code, out.getvalue()

    def test_gates_print_every_row_and_fail_without_cost(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "dump.tsv")
            synthetic_dump(p)
            code, out = self.run_main([p, "--boot", "50", "--gates"])
            for gid in ("G1.1", "G1.2", "G1.3", "G1.4", "G1.5", "G1.6", "G1.7",
                        "G2.1", "G2.2", "G2.3", "G2.4", "G2.5", "G2.6"):
                self.assertIn(gid, out)
            # No fp32 dump and no E0 numbers: those gates cannot pass.
            self.assertEqual(code, 1)
            self.assertRegex(out, r"G1\.7 .*not given.*FAIL")
            self.assertRegex(out, r"G1\.4 .*not measured.*FAIL")

    def test_an_informative_phone_score_beats_a_noisy_grapheme_one(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "dump.tsv")
            synthetic_dump(p, good=True)
            _, out = self.run_main([p, "--boot", "50"])
            self.assertRegex(out, r"G1\.1 .* PASS")

    def test_an_uninformative_phone_score_fails(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "dump.tsv")
            synthetic_dump(p, good=False)
            code, out = self.run_main([p, "--boot", "50", "--gates"])
            self.assertEqual(code, 1)
            self.assertRegex(out, r"G1\.1 .* FAIL")


if __name__ == "__main__":
    unittest.main()
