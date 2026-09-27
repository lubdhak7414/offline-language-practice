#!/usr/bin/env python3
"""Build a corpus_dump_gop_manifest manifest for L2-ARCTIC. Standard library only.

UNVERIFIED: written from L2-ARCTIC v5's documentation; nobody has run it on
a real copy yet. The corpus needs a request form (psi.engr.tamu.edu/
l2-arctic-corpus), so the owner fetches it. Check these against the copy
before trusting any output, and fix this header once they are confirmed:

  * layout <root>/<SPK>/{wav,annotation}/arctic_a0001.{wav,TextGrid}, with
    manual annotations for 150 utterances per speaker in annotation/;
  * annotation TextGrids in Praat's long text format, with an interval tier
    named "words" and one named "phones";
  * an annotated phone error written as a comma-separated triple
    (CPL,PPL,s substitution; CPL,sil,d deletion; sil,PPL,a addition), and a
    correct phone as a bare label;
  * audio 44.1 kHz (the Rust reader wants 16 kHz 16-bit mono, and there is
    deliberately no resampler in the crate).

License: CC BY-NC 4.0. EVALUATION ONLY. calibrate-gop.py refuses to print a
table fitted on it (`l2arctic` is not in SHIPPABLE); use it with --eval-on.

    # 16 kHz copies, same tree (sox or ffmpeg; the manifest points at these)
    cd ~/corpora/l2arctic && find . -name '*.wav' | while read -r f; do
      mkdir -p "../l2arctic16k/$(dirname "$f")"
      sox "$f" -r 16000 -b 16 -c 1 "../l2arctic16k/$f"
    done
    python3 scripts/manifest-l2arctic.py ~/corpora/l2arctic \\
        --audio-root ~/corpora/l2arctic16k --out ~/corpora/l2arctic16k/manifest.tsv

Labels are `binary`: a word is 10 when no annotated error falls inside its
interval (an error phone counts for the word holding its midpoint), else 0.
An addition in a silence between words belongs to no word and is counted,
not labelled.
"""
import argparse
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import olp_manifest  # noqa: E402

# Speaker -> L1, from the L2-ARCTIC documentation (four speakers each).
SPEAKERS = {
    "ABA": "arabic", "SKA": "arabic", "YBAA": "arabic", "ZHAA": "arabic",
    "BWC": "mandarin", "LXC": "mandarin", "NCC": "mandarin", "TXHC": "mandarin",
    "ASI": "hindi", "RRBI": "hindi", "SVBI": "hindi", "TNI": "hindi",
    "HJK": "korean", "HKK": "korean", "YDCK": "korean", "YKWK": "korean",
    "EBVS": "spanish", "ERMS": "spanish", "MBMPS": "spanish", "NJS": "spanish",
    "HQTV": "vietnamese", "PNV": "vietnamese", "THV": "vietnamese", "TLV": "vietnamese",
}
SILENCE = {"", "sil", "sp", "spn", "<sil>"}

_KV = re.compile(r'^\s*(xmin|xmax|text|name)\s*=\s*(.*?)\s*$')
_INTERVAL = re.compile(r'^\s*intervals\s*\[\d+\]\s*:?\s*$')
_ITEM = re.compile(r'^\s*item\s*\[\d+\]\s*:?\s*$')


def _unquote(v):
    if len(v) >= 2 and v[0] == '"' and v[-1] == '"':
        return v[1:-1].replace('""', '"')
    return v


def parse_textgrid(text):
    """{tier name (lower-case): [(xmin, xmax, text), ...]} from a long-format TextGrid."""
    if 'Object class = "TextGrid"' not in text or "item [" not in text:
        raise ValueError("not a long-format TextGrid")
    tiers, name, cur = {}, None, None
    for line in text.splitlines():
        if _ITEM.match(line):
            name, cur = None, None
            continue
        if _INTERVAL.match(line):
            cur = {}
            continue
        m = _KV.match(line)
        if not m:
            continue
        k, v = m.group(1), m.group(2)
        if k == "name" and cur is None:
            name = _unquote(v).strip().lower()
            tiers.setdefault(name, [])
        elif cur is not None and name is not None:
            cur[k] = _unquote(v) if k == "text" else float(v)
            if k == "text":
                tiers[name].append((cur["xmin"], cur["xmax"], cur["text"]))
                cur = None
    return tiers


def is_error(phone_label):
    return "," in phone_label


def label_words(tiers):
    """(words, labels, unattached_errors) for one annotated utterance."""
    if "words" not in tiers or "phones" not in tiers:
        raise ValueError(f"need 'words' and 'phones' tiers, found {sorted(tiers)}")
    words = [(a, b, t.strip()) for a, b, t in tiers["words"] if t.strip().lower() not in SILENCE]
    errors = [(a + b) / 2 for a, b, t in tiers["phones"] if is_error(t)]
    labels = [0 if any(a <= mid < b for mid in errors) else 10 for a, b, _ in words]
    attached = sum(any(a <= mid < b for a, b, _ in words) for mid in errors)
    return [w for _, _, w in words], labels, len(errors) - attached


def build(root, audio_root, manifest_dir, speakers, check_audio=True):
    rows = []
    stats = {"utterances": 0, "error words": 0, "words": 0, "unattached errors": 0}
    for spk in speakers:
        adir = os.path.join(root, spk, "annotation")
        if not os.path.isdir(adir):
            raise SystemExit(f"{adir}: missing (is ROOT the extracted L2-ARCTIC?)")
        for name in sorted(os.listdir(adir)):
            if not name.endswith(".TextGrid"):
                continue
            utt = name[: -len(".TextGrid")]
            with open(os.path.join(adir, name), encoding="utf-8-sig") as f:
                words, labels, loose = label_words(parse_textgrid(f.read()))
            # Word i must stay label i under the app's tokenizer: a word it
            # splits (a hyphen) passes its label to every piece.
            target, per_token = [], []
            for w, lab in zip(words, labels):
                toks = olp_manifest.tokenize(w)
                target.extend(toks)
                per_token.extend([lab] * len(toks))
            if not target or not olp_manifest.spellable(target):
                continue
            wav = os.path.join(audio_root, spk, "wav", f"{utt}.wav")
            if check_audio:
                fmt = olp_manifest.read_wav_header(wav)
                if fmt != (16000, 1, 16):
                    raise SystemExit(f"{wav}: {fmt} (rate, channels, bits); resample to 16 kHz "
                                     "16-bit mono into --audio-root first (see header)")
            rows.append({
                "corpus": "l2arctic", "l1": SPEAKERS[spk], "speaker": spk, "utt": f"{spk}_{utt}",
                "split": "eval", "age": "", "wav": os.path.relpath(wav, manifest_dir),
                "target": " ".join(target), "labels": per_token, "label_kind": "binary",
            })
            stats["utterances"] += 1
            stats["words"] += len(per_token)
            stats["error words"] += sum(x == 0 for x in per_token)
            stats["unattached errors"] += loose
    return rows, stats


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("root", help="extracted L2-ARCTIC (holds <SPK>/annotation/)")
    ap.add_argument("--audio-root", help="16 kHz copy of the tree (default ROOT)")
    ap.add_argument("--speakers", nargs="+", default=list(SPEAKERS), choices=list(SPEAKERS))
    ap.add_argument("--out", help="manifest path (default AUDIO_ROOT/manifest.tsv)")
    ap.add_argument("--no-check-audio", action="store_true", help="skip the 16 kHz header check")
    args = ap.parse_args()
    audio_root = args.audio_root or args.root
    out = args.out or os.path.join(audio_root, "manifest.tsv")
    rows, stats = build(args.root, audio_root, os.path.dirname(os.path.abspath(out)),
                        args.speakers, check_audio=not args.no_check_audio)
    olp_manifest.write(out, rows)
    print(f"-> {out}: " + ", ".join(f"{v} {k}" for k, v in stats.items()))


if __name__ == "__main__":
    main()
