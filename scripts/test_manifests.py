#!/usr/bin/env python3
"""Tests for the scripts/manifest-*.py builders. Standard library only, no corpus.

    python3 -m unittest discover -s scripts -p 'test_*.py'
"""
import importlib.util
import os
import struct
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"),
                                                  os.path.join(HERE, f"{name}.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


om = load("olp_manifest")
cmu = load("manifest-cmuarctic")
l2 = load("manifest-l2arctic")
cv = load("manifest-commonvoice")


def wav_bytes(rate=16000, channels=1, bits=16, samples=4):
    data = b"\0" * (samples * channels * bits // 8)
    fmt = struct.pack("<HHIIHH", 1, channels, rate, rate * channels * bits // 8,
                      channels * bits // 8, bits)
    body = b"WAVE" + b"LIST" + struct.pack("<I", 3) + b"abc\0" \
        + b"fmt " + struct.pack("<I", len(fmt)) + fmt + b"data" + struct.pack("<I", len(data)) + data
    return b"RIFF" + struct.pack("<I", len(body)) + body


class Tokenize(unittest.TestCase):
    # The same cases pronounce.rs pins for `tokenize`: word i of a manifest's
    # labels must be word i of the score.
    def test_matches_the_app(self):
        self.assertEqual(om.tokenize("Hello, world!"), ["HELLO", "WORLD"])
        self.assertEqual(om.tokenize("  spaced   out  "), ["SPACED", "OUT"])
        self.assertEqual(om.tokenize("!!! ???"), [])
        self.assertEqual(om.tokenize("Don't stop."), ["DON'T", "STOP"])
        self.assertEqual(om.tokenize("'Quoted' words'"), ["QUOTED", "WORDS"])
        self.assertEqual(om.tokenize("twenty-five"), ["TWENTY", "FIVE"])
        self.assertEqual(om.tokenize("it’s"), ["IT'S"])

    def test_digits_are_not_spellable(self):
        self.assertFalse(om.spellable(om.tokenize("March 16, 1908")))
        self.assertTrue(om.spellable(om.tokenize("Don't stop")))


class Write(unittest.TestCase):
    def row(self, **kw):
        r = {"corpus": "cmuarctic", "l1": "us-english", "speaker": "bdl", "utt": "u1",
             "split": "eval", "age": "", "wav": "a.wav", "target": "Hello, world.",
             "labels": [10, 10], "label_kind": "validated"}
        r.update(kw)
        return r

    def test_round_trip(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "m.tsv")
            om.write(p, [self.row()])
            with open(p) as f:
                lines = f.read().splitlines()
        self.assertEqual(lines[0].split("\t"), om.MANIFEST_COLUMNS)
        self.assertEqual(lines[1].split("\t")[om.MANIFEST_COLUMNS.index("labels")], "10 10")

    def test_refuses_labels_that_do_not_line_up(self):
        with tempfile.TemporaryDirectory() as d, self.assertRaises(ValueError):
            om.write(os.path.join(d, "m.tsv"), [self.row(labels=[10])])

    def test_refuses_unknown_label_kind(self):
        with tempfile.TemporaryDirectory() as d, self.assertRaises(ValueError):
            om.write(os.path.join(d, "m.tsv"), [self.row(label_kind="stars")])

    def test_reads_the_fmt_chunk_past_other_chunks(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "a.wav")
            with open(p, "wb") as f:
                f.write(wav_bytes(rate=44100))
            self.assertEqual(om.read_wav_header(p), (44100, 1, 16))


class CmuArctic(unittest.TestCase):
    def test_builds_from_the_released_layout(self):
        with tempfile.TemporaryDirectory() as root:
            d = os.path.join(root, "cmu_us_ksp_arctic")
            os.makedirs(os.path.join(d, "wav"))
            os.makedirs(os.path.join(d, "etc"))
            with open(os.path.join(d, "etc", "txt.done.data"), "w") as f:
                f.write('( arctic_a0001 "Author of the danger trail, Philip Steels, etc." )\n'
                        '( arctic_a0002 "At sea, Monday, March 16, 1908." )\n')
            for u in ("arctic_a0001", "arctic_a0002", "arctic_a0003"):
                with open(os.path.join(d, "wav", f"{u}.wav"), "wb") as f:
                    f.write(wav_bytes())
            rows, skipped = cmu.build(root, root, ["ksp"])
        self.assertEqual([r["utt"] for r in rows], ["ksp_arctic_a0001"])
        self.assertEqual(rows[0]["labels"], [10] * 8)
        self.assertEqual(rows[0]["l1"], "indian-english")
        self.assertEqual(rows[0]["wav"], os.path.join("cmu_us_ksp_arctic", "wav", "arctic_a0001.wav"))
        self.assertEqual(skipped, {"no transcript": 1, "unspellable": 1})

    def test_rejects_audio_that_is_not_16k(self):
        with tempfile.TemporaryDirectory() as root:
            d = os.path.join(root, "cmu_us_bdl_arctic")
            os.makedirs(os.path.join(d, "wav"))
            os.makedirs(os.path.join(d, "etc"))
            with open(os.path.join(d, "etc", "txt.done.data"), "w") as f:
                f.write('( arctic_a0001 "Hello there." )\n')
            with open(os.path.join(d, "wav", "arctic_a0001.wav"), "wb") as f:
                f.write(wav_bytes(rate=32000))
            with self.assertRaises(ValueError):
                cmu.build(root, root, ["bdl"])


TEXTGRID = '''File type = "ooTextFile"
Object class = "TextGrid"

xmin = 0
xmax = 2.0
tiers? <exists>
size = 2
item []:
    item [1]:
        class = "IntervalTier"
        name = "words"
        xmin = 0
        xmax = 2.0
        intervals: size = 5
        intervals [1]:
            xmin = 0
            xmax = 0.3
            text = ""
        intervals [2]:
            xmin = 0.3
            xmax = 0.8
            text = "the"
        intervals [3]:
            xmin = 0.8
            xmax = 1.2
            text = "well-known"
        intervals [4]:
            xmin = 1.2
            xmax = 1.5
            text = "sil"
        intervals [5]:
            xmin = 1.5
            xmax = 2.0
            text = "cat"
    item [2]:
        class = "IntervalTier"
        name = "phones"
        xmin = 0
        xmax = 2.0
        intervals: size = 5
        intervals [1]:
            xmin = 0.3
            xmax = 0.5
            text = "DH,D,s"
        intervals [2]:
            xmin = 0.5
            xmax = 0.8
            text = "AH0"
        intervals [3]:
            xmin = 0.8
            xmax = 1.2
            text = "W"
        intervals [4]:
            xmin = 1.2
            xmax = 1.5
            text = "sil,AH,a"
        intervals [5]:
            xmin = 1.5
            xmax = 2.0
            text = "K"
'''


class L2Arctic(unittest.TestCase):
    def test_parses_both_tiers(self):
        tiers = l2.parse_textgrid(TEXTGRID)
        self.assertEqual(sorted(tiers), ["phones", "words"])
        self.assertEqual(tiers["words"][1], (0.3, 0.8, "the"))
        self.assertEqual(len(tiers["phones"]), 5)

    def test_an_error_marks_the_word_holding_it(self):
        words, labels, loose = l2.label_words(l2.parse_textgrid(TEXTGRID))
        self.assertEqual(words, ["the", "well-known", "cat"])
        self.assertEqual(labels, [0, 10, 10])
        self.assertEqual(loose, 1)  # the addition sits in a silence

    def test_refuses_short_format(self):
        with self.assertRaises(ValueError):
            l2.parse_textgrid('File type = "ooTextFile short"\n"TextGrid"\n0\n2\n')

    def test_split_word_passes_its_label_to_every_piece(self):
        with tempfile.TemporaryDirectory() as root:
            os.makedirs(os.path.join(root, "HKK", "annotation"))
            with open(os.path.join(root, "HKK", "annotation", "arctic_a0001.TextGrid"), "w") as f:
                f.write(TEXTGRID.replace('text = "W"', 'text = "W,V,s"'))
            rows, stats = l2.build(root, root, root, ["HKK"], check_audio=False)
        self.assertEqual(rows[0]["target"], "THE WELL KNOWN CAT")
        self.assertEqual(rows[0]["labels"], [0, 0, 0, 10])
        self.assertEqual((rows[0]["l1"], rows[0]["label_kind"]), ("korean", "binary"))
        self.assertEqual(stats["error words"], 3)


class CommonVoice(unittest.TestCase):
    def rows(self):
        out = []
        for i in range(60):
            out.append({"client_id": f"c{i % 2}", "path": f"a{i}.mp3", "sentence": "Hello there.",
                        "up_votes": "2", "down_votes": "0", "accents": "India and South Asia"})
        out += [
            {"client_id": "x", "path": "down.mp3", "sentence": "Hi.", "up_votes": "5",
             "down_votes": "1", "accents": "England English"},
            {"client_id": "x", "path": "two.mp3", "sentence": "Hi.", "up_votes": "5",
             "down_votes": "0", "accents": "England English|Irish English"},
            {"client_id": "x", "path": "num.mp3", "sentence": "Room 101.", "up_votes": "5",
             "down_votes": "0", "accents": "England English"},
            {"client_id": "x", "path": "none.mp3", "sentence": "Hi.", "up_votes": "5",
             "down_votes": "0", "accents": ""},
        ]
        return out

    def test_filters_and_caps(self):
        picked, skipped = cv.select(self.rows(), per_accent=400, per_speaker=20, min_clips=10)
        self.assertEqual(len(picked), 40)  # two speakers, 20 each
        self.assertEqual({a for a, _ in picked}, {"india and south asia"})
        self.assertEqual(skipped, {"votes": 1, "accent": 2, "length or digits": 1, "speaker cap": 20})

    def test_selection_is_seeded(self):
        a, _ = cv.select(self.rows(), per_accent=15, min_clips=1)
        b, _ = cv.select(list(reversed(self.rows())), per_accent=15, min_clips=1)
        self.assertEqual([r["path"] for _, r in a], [r["path"] for _, r in b])

    def test_small_accents_are_dropped(self):
        picked, _ = cv.select(self.rows(), min_clips=41)
        self.assertEqual(picked, [])


if __name__ == "__main__":
    unittest.main()
