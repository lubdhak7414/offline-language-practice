#!/usr/bin/env python3
"""Build a corpus_dump_gop_manifest manifest for CMU ARCTIC. Standard library only.

CMU ARCTIC (festvox.org/cmu_arctic): the same ~1,132 phonetically balanced
prompts read by US, Canadian, Scottish and Indian English speakers. "Free
for use for any purpose (commercial or otherwise)" with the CMU copyright
notice kept (its COPYING), so it is in calibrate-gop.py's SHIPPABLE. There
are no word labels: every word is a read judged correct (label_kind
`validated`, 10 each), so it measures false alarms only. Its prompts are
the ones L2-ARCTIC's speakers read, which makes it the native baseline on
identical text.

    mkdir -p ~/corpora/cmu_arctic && cd ~/corpora/cmu_arctic
    for s in bdl slt clb rms jmk awb ksp; do
      curl -LO http://festvox.org/cmu_arctic/packed/cmu_us_${s}_arctic.tar.bz2
      tar xjf cmu_us_${s}_arctic.tar.bz2
    done
    python3 scripts/manifest-cmuarctic.py ~/corpora/cmu_arctic

Verified 2026-09-27 against those seven packed archives: each extracts to
cmu_us_<spk>_arctic/{wav/arctic_[ab]NNNN.wav, etc/txt.done.data}; every wav
is 16 kHz 16-bit mono already, so nothing needs resampling;
txt.done.data lines read `( arctic_a0001 "Author of the danger trail, ..." )`.
Not every wav has a transcript line (bdl has 1,132 wavs and 1,131 lines,
jmk 1,132 and 1,114); only utterances with both are listed.
"""
import argparse
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import olp_manifest  # noqa: E402

# Speaker -> accent, from the festvox page ("US English jmk by Canadian
# English male", ...). The column is named l1 for all corpora; here it holds
# the speaker's English accent.
SPEAKERS = {
    "bdl": "us-english", "slt": "us-english", "clb": "us-english", "rms": "us-english",
    "jmk": "canadian-english", "awb": "scottish-english", "ksp": "indian-english",
}
LINE = re.compile(r'^\(\s*(\S+)\s+"(.*)"\s*\)\s*$')


def read_prompts(path):
    prompts = {}
    with open(path, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            if not line.strip():
                continue
            m = LINE.match(line)
            if not m:
                raise ValueError(f"{path}:{n}: unexpected line {line.strip()!r}")
            prompts[m.group(1)] = m.group(2)
    return prompts


def build(root, manifest_dir, speakers, check_audio=True):
    rows, skipped = [], {"no transcript": 0, "unspellable": 0}
    for spk in speakers:
        d = os.path.join(root, f"cmu_us_{spk}_arctic")
        prompts = read_prompts(os.path.join(d, "etc", "txt.done.data"))
        wavs = sorted(f[:-4] for f in os.listdir(os.path.join(d, "wav")) if f.endswith(".wav"))
        for utt in wavs:
            if utt not in prompts:
                skipped["no transcript"] += 1
                continue
            words = olp_manifest.tokenize(prompts[utt])
            # "March 16, 1908": the app cannot spell a digit, and guessing how
            # it was read would put words in the speaker's mouth.
            if not words or not olp_manifest.spellable(words):
                skipped["unspellable"] += 1
                continue
            wav = os.path.join(d, "wav", f"{utt}.wav")
            if check_audio:
                fmt = olp_manifest.read_wav_header(wav)
                if fmt != (16000, 1, 16):
                    raise ValueError(f"{wav}: {fmt} (rate, channels, bits), need 16 kHz 16-bit mono")
            rows.append({
                "corpus": "cmuarctic", "l1": SPEAKERS[spk], "speaker": spk, "utt": f"{spk}_{utt}",
                "split": "eval", "age": "", "wav": os.path.relpath(wav, manifest_dir), "target": prompts[utt],
                "labels": [10] * len(words), "label_kind": "validated",
            })
    return rows, skipped


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("root", help="directory holding the extracted cmu_us_<spk>_arctic folders")
    ap.add_argument("--speakers", nargs="+", default=list(SPEAKERS), choices=list(SPEAKERS))
    ap.add_argument("--out", help="manifest path (default ROOT/manifest.tsv)")
    args = ap.parse_args()
    out = args.out or os.path.join(args.root, "manifest.tsv")
    rows, skipped = build(args.root, os.path.dirname(os.path.abspath(out)), args.speakers)
    olp_manifest.write(out, rows)
    print(f"{len(rows)} utterances from {len(args.speakers)} speakers -> {out}; skipped "
          + ", ".join(f"{v} {k}" for k, v in skipped.items()))


if __name__ == "__main__":
    main()
