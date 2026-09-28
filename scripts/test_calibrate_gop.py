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


class RepeatFlags(unittest.TestCase):
    def rows(self):
        def w(utt, word, human, now):
            return f"test\t{utt}\t30\t0\t{word}\t{human}\t-1.0\t{now}\t0\t100"
        lines = [OLD_HEADER,
                 # speaker 0001: CAT flagged in 3 utterances (twice in the first), DOG in 1 of 2,
                 # SUN said once and flagged, HAT said twice and never flagged
                 w("000010001", "CAT", 2, 3), w("000010001", "Cat", 10, 5),
                 w("000010002", "CAT", 4, 9), w("000010003", "CAT", 10, 0),
                 w("000010001", "DOG", 6, 1), w("000010002", "DOG", 10, 100),
                 w("000010002", "SUN", 9, 2),
                 w("000010001", "HAT", 3, 50), w("000010003", "HAT", 3, 60),
                 # speaker 0002 flags CAT in 2 utterances: same word, different speaker
                 w("000020001", "CAT", 5, 4), w("000020002", "CAT", 10, 7),
                 # not flagged at exactly FLAG_BELOW
                 w("000020002", "SUN", 1, cg.FLAG_BELOW)]
        return cg.parse(lines)

    def test_flag_counts_are_per_speaker_and_per_utterance(self):
        flagged = cg.flag_repeats(self.rows())
        got = sorted((r["speaker"], r["word"], r["utt"][-1], r["nflag"], r["nutt"], r["human"])
                     for r in flagged)
        self.assertEqual(got, [
            # CAT flagged twice in utterance 1 still counts that utterance once
            ("0001", "cat", "1", 3, 3, 2), ("0001", "cat", "1", 3, 3, 10),
            ("0001", "cat", "2", 3, 3, 4), ("0001", "cat", "3", 3, 3, 10),
            ("0001", "dog", "1", 1, 2, 6),
            ("0001", "sun", "2", 1, 1, 9),
            ("0002", "cat", "1", 2, 2, 5), ("0002", "cat", "2", 2, 2, 10),
        ])

    def test_unflagged_and_at_cutoff_words_are_left_out(self):
        flagged = cg.flag_repeats(self.rows())
        self.assertFalse([r for r in flagged if r["word"] == "hat"])
        self.assertFalse([r for r in flagged if r["speaker"] == "0002" and r["word"] == "sun"])

    def test_precision_per_group_counts_each_flagged_instance(self):
        flagged = cg.flag_repeats(self.rows())
        once, twice, thrice, diff = cg.repeat_precision(flagged)
        self.assertEqual(once, 1 / 2)      # dog (6: yes), sun (9: no)
        self.assertEqual(twice, 3 / 6)     # 0001 cat (2, 10, 4, 10), 0002 cat (5, 10)
        self.assertEqual(thrice, 2 / 4)    # 0001 cat only
        self.assertEqual(diff, twice - once)

    def test_empty_group_is_nan(self):
        rows = [r for r in self.rows() if r["word"] != "cat"]
        _, twice, thrice, diff = cg.repeat_precision(cg.flag_repeats(rows))
        for x in (twice, thrice, diff):
            self.assertNotEqual(x, x)

    def test_report_counts_pairs_and_restricts_to_repeated_words(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            cg.repeat_report(self.rows(), 50)
        lines = out.getvalue().splitlines()
        row = lambda block, name: next(
            l for l in lines[lines.index(block):] if l.strip().startswith(name)).split()
        all_words = "  all words: 8 flagged instances, 2 speakers"
        repeated = "  words the speaker says in >= 2 utterances: 7 flagged instances, 2 speakers"
        self.assertIn(all_words, lines)
        self.assertIn(repeated, lines)  # sun (said once) drops out
        # pairs, flags: exactly 1 -> dog, sun; >= 2 -> both cats; >= 3 -> speaker 0001's cat
        self.assertEqual(row(all_words, "flagged in exactly 1")[-4:-2], ["2", "2"])
        self.assertEqual(row(all_words, "flagged in >= 2")[-4:-2], ["2", "6"])
        self.assertEqual(row(all_words, "flagged in >= 3")[-4:-2], ["1", "4"])
        self.assertEqual(row(repeated, "flagged in exactly 1")[-4:-2], ["1", "1"])


if __name__ == "__main__":
    unittest.main()
