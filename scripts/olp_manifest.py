"""Shared by the scripts/manifest-*.py builders. Standard library only.

A manifest is a TSV the `corpus_dump_gop_manifest` test in pronounce.rs reads:
one row per utterance, columns MANIFEST_COLUMNS. `wav` is relative to the
manifest's own directory (or absolute) and must be 16 kHz 16-bit mono: the
Rust reader refuses anything else rather than resample. `labels` is one
integer per word of `target`, as the app tokenizes it, so word i of the score
lines up with label i. Manifests live next to the corpus in ~/corpora, never
in the repo.
"""
import csv
import os

MANIFEST_COLUMNS = ["corpus", "l1", "speaker", "utt", "split", "age", "wav", "target",
                    "labels", "label_kind"]
LABEL_KINDS = {"score0_10", "binary", "validated"}


def tokenize(text):
    """Python twin of pronounce.rs `tokenize`: runs of alphanumerics, upper-cased,
    with apostrophes kept inside a word and trimmed from its end."""
    out, word = [], []

    def push():
        w = "".join(word).rstrip("'")
        if w:
            out.append(w)
        word.clear()

    for ch in text:
        if ch.isalnum():
            word.append(ch.upper())
        elif ch in ("'", "’") and word:
            word.append("'")
        elif word:
            push()
    push()
    return out


def spellable(words):
    """The ASR vocab is A-Z plus apostrophe; a digit makes the scorer refuse."""
    return all(c == "'" or ("A" <= c <= "Z") for w in words for c in w)


def write(path, rows):
    """Write manifest rows (dicts keyed by MANIFEST_COLUMNS). Returns the count."""
    for r in rows:
        if r["label_kind"] not in LABEL_KINDS:
            raise ValueError(f"{r['utt']}: unknown label_kind {r['label_kind']!r}")
        if len(r["labels"]) != len(tokenize(r["target"])):
            raise ValueError(f"{r['utt']}: {len(r['labels'])} labels for "
                             f"{len(tokenize(r['target']))} words")
        for k in MANIFEST_COLUMNS:
            if "\t" in str(r[k]) or "\n" in str(r[k]):
                raise ValueError(f"{r['utt']}: tab or newline in {k}")
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w", newline="") as f:
        w = csv.writer(f, delimiter="\t", lineterminator="\n", quoting=csv.QUOTE_NONE,
                       escapechar=None, quotechar=None)
        w.writerow(MANIFEST_COLUMNS)
        for r in rows:
            w.writerow([" ".join(str(x) for x in r["labels"]) if k == "labels" else r[k]
                        for k in MANIFEST_COLUMNS])
    return len(rows)


def read_wav_header(path):
    """(sample_rate, channels, bits) from a RIFF WAVE file's fmt chunk."""
    with open(path, "rb") as f:
        head = f.read(12)
        if len(head) < 12 or head[:4] != b"RIFF" or head[8:12] != b"WAVE":
            raise ValueError(f"{path}: not a RIFF WAVE file")
        while True:
            ch = f.read(8)
            if len(ch) < 8:
                raise ValueError(f"{path}: no fmt chunk")
            size = int.from_bytes(ch[4:8], "little")
            if ch[:4] == b"fmt ":
                body = f.read(size)
                return (int.from_bytes(body[4:8], "little"), int.from_bytes(body[2:4], "little"),
                        int.from_bytes(body[14:16], "little"))
            f.seek(size + (size & 1), 1)
