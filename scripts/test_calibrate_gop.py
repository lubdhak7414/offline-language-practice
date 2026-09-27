#!/usr/bin/env python3
"""Tests for calibrate-gop.py. Standard library only, no corpus needed.

    python3 -m unittest scripts/test_calibrate_gop.py
"""
import contextlib
import importlib.util
import io
import os
import tempfile
import unittest

_spec = importlib.util.spec_from_file_location(
    "calibrate_gop", os.path.join(os.path.dirname(os.path.abspath(__file__)), "calibrate-gop.py"))
cg = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cg)

OLD_HEADER = "split\tutt\tage\tword_index\tword\thuman\tgop\tscore_now\tstart_ms\tend_ms"
NEW_HEADER = OLD_HEADER + "\tcorpus\tl1\tspeaker\tlabel_kind"


def old_row(split, utt, human, gop, age="30"):
    return f"{split}\t{utt}\t{age}\t0\tWORD\t{human}\t{gop}\t50\t0\t100"


def new_row(split, utt, human, gop, corpus, l1, speaker, kind):
    return old_row(split, utt, human, gop, age="") + f"\t{corpus}\t{l1}\t{speaker}\t{kind}"


class Load(unittest.TestCase):
    def test_old_format_rows_parse_with_defaults(self):
        rows = cg.parse([OLD_HEADER, old_row("train", "000030012", 10, "-0.5"),
                         old_row("test", "010300001", 3, "-4.25", age="")])
        self.assertEqual(len(rows), 2)
        for r in rows:
            self.assertEqual(r["corpus"], "speechocean762")
            self.assertEqual(r["l1"], "mandarin")
            self.assertEqual(r["label_kind"], "score0_10")
        # speechocean762 utterance ids carry the speaker (0 + 4 digits + 4 digits).
        self.assertEqual([r["speaker"] for r in rows], ["0003", "1030"])
        self.assertEqual(rows[0]["age"], 30)
        self.assertIsNone(rows[1]["age"])
        self.assertEqual(rows[1]["gop"], -4.25)

    def test_new_columns_override_defaults(self):
        rows = cg.parse([NEW_HEADER, new_row("eval", "ksp_a0001", 10, "0", "cmuarctic",
                                             "indian-english", "ksp", "validated")])
        r = rows[0]
        self.assertEqual((r["corpus"], r["l1"], r["speaker"], r["label_kind"]),
                         ("cmuarctic", "indian-english", "ksp", "validated"))

    def test_unlabelled_words_are_skipped(self):
        self.assertEqual(cg.parse([OLD_HEADER, old_row("train", "000030012", -1, "0")]), [])


class Auc(unittest.TestCase):
    def test_perfect_separator_is_one(self):
        scores = [-5.0, -4.0, -3.0, -0.1, 0.0, 0.0]
        mispronounced = [True, True, True, False, False, False]
        self.assertEqual(cg.auc(scores, mispronounced), 1.0)
        self.assertEqual(cg.auc(scores, [not m for m in mispronounced]), 0.0)

    def test_ties_count_half(self):
        self.assertEqual(cg.auc([0.0, 0.0], [True, False]), 0.5)

    def test_one_class_is_undefined(self):
        a = cg.auc([0.0, -1.0], [False, False])
        self.assertNotEqual(a, a)


class ShippableGuard(unittest.TestCase):
    def test_rust_refuses_a_non_commercial_fit(self):
        with tempfile.NamedTemporaryFile("w", suffix=".tsv", delete=False) as f:
            f.write("\n".join([NEW_HEADER,
                               new_row("train", "u1", 10, "0", "l2arctic", "hindi", "s1", "binary"),
                               new_row("test", "u2", 0, "-3", "l2arctic", "hindi", "s1", "binary")]))
        self.addCleanup(os.unlink, f.name)
        out = io.StringIO()
        with contextlib.redirect_stdout(out), self.assertRaises(SystemExit) as cm:
            cg.main([f.name, "--fit-on", "l2arctic", "--rust"])
        self.assertIsInstance(cm.exception.code, str)  # a message means exit status 1
        self.assertIn("CC BY-NC 4.0", cm.exception.code)
        self.assertNotIn("GOP_PERCENTILE", out.getvalue())

    def test_one_bad_corpus_among_good_ones_is_enough(self):
        self.assertIn("license unknown", cg.unshippable(["speechocean762", "mystery"]))

    def test_shippable_corpora_pass(self):
        self.assertIsNone(cg.unshippable(["speechocean762", "commonvoice", "cmuarctic"]))


class Bootstrap(unittest.TestCase):
    def rows(self):
        rows = []
        for s in range(12):
            for w in range(8):
                human = 10 if (s * 8 + w) % 5 else 2
                rows.append({"speaker": f"s{s}", "human": human, "gop": -0.3 * ((s + w) % 7),
                             "table": 5 if (s + w) % 4 == 0 else 50})
        return rows

    def test_fixed_seed_is_deterministic(self):
        a = cg.bootstrap(self.rows(), cg.metrics, n=200)
        b = cg.bootstrap(self.rows(), cg.metrics, n=200)
        self.assertEqual(a, b)
        self.assertEqual(len(a), 4)

    def test_seed_changes_the_draws(self):
        a = cg.bootstrap(self.rows(), cg.metrics, n=200, seed=1)
        b = cg.bootstrap(self.rows(), cg.metrics, n=200, seed=2)
        self.assertNotEqual(a, b)

    def test_interval_brackets_the_point_estimate(self):
        rows = self.rows()
        fa = cg.metrics(rows)[0]
        lo, hi = cg.bootstrap(rows, cg.metrics, n=500)[0]
        self.assertLessEqual(lo, fa)
        self.assertLessEqual(fa, hi)


if __name__ == "__main__":
    unittest.main()
