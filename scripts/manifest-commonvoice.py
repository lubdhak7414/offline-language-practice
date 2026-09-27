#!/usr/bin/env python3
"""Build a corpus_dump_gop_manifest manifest for Common Voice English. Standard library only.

UNVERIFIED: written from Common Voice's documented release layout; nobody
has run it on a real copy yet. Since October 2025 the dataset is only on the
Mozilla Data Collective, which needs an account, so the owner fetches it.
Check these against the copy before trusting any output, and fix this
header once they are confirmed:

  * <root>/validated.tsv with a header naming at least client_id, path,
    sentence, up_votes, down_votes and accents (read by name, so column
    order and extra columns do not matter);
  * <root>/clips/<path>, MP3;
  * `accents` is free text, several accents joined by `|`, empty if unset.

License: CC0-1.0, so it is in calibrate-gop.py's SHIPPABLE. Every clip here
was validated by other contributors, so every word is labelled 10
(label_kind `validated`): it measures false alarms by accent, nothing else.

    python3 scripts/manifest-commonvoice.py ~/corpora/commonvoice/en --convert
    # --convert runs ffmpeg into <out dir>/wav16k/; without it, the ffmpeg
    # commands are written to <out dir>/convert.sh for you to run.

Selection, fixed by seed 762 so reruns pick the same clips: up_votes >= 2,
down_votes == 0, exactly one accent label, at most 20 words, nothing the
app cannot spell (digits), at most 20 clips per client_id, then up to 400
clips per accent. Accents with fewer than --min-clips survivors are dropped.
"""
import argparse
import csv
import os
import random
import shlex
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import olp_manifest  # noqa: E402

SEED = 762


def accent_of(raw):
    """One normalised accent label, or None for none or several."""
    parts = [p.strip() for p in (raw or "").split("|") if p.strip()]
    if len(parts) != 1:
        return None
    return " ".join(parts[0].lower().split())


def select(rows, per_accent=400, per_speaker=20, max_words=20, min_up=2, min_clips=50,
           seed=SEED):
    """Pick clips from validated.tsv rows (dicts). Returns (picked, reasons skipped)."""
    skipped = {"votes": 0, "accent": 0, "length or digits": 0, "speaker cap": 0}
    by_accent = {}
    for r in rows:
        if int(r.get("up_votes") or 0) < min_up or int(r.get("down_votes") or 0) != 0:
            skipped["votes"] += 1
            continue
        acc = accent_of(r.get("accents"))
        if acc is None:
            skipped["accent"] += 1
            continue
        words = olp_manifest.tokenize(r["sentence"])
        if not words or len(words) > max_words or not olp_manifest.spellable(words):
            skipped["length or digits"] += 1
            continue
        by_accent.setdefault(acc, []).append(r)
    rng = random.Random(seed)
    picked = []
    for acc in sorted(by_accent):
        pool = sorted(by_accent[acc], key=lambda r: r["path"])
        rng.shuffle(pool)
        per, chosen = {}, []
        for r in pool:
            if per.get(r["client_id"], 0) >= per_speaker:
                skipped["speaker cap"] += 1
                continue
            per[r["client_id"]] = per.get(r["client_id"], 0) + 1
            chosen.append(r)
            if len(chosen) == per_accent:
                break
        if len(chosen) >= min_clips:
            picked.extend((acc, r) for r in chosen)
    return picked, skipped


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("root", help="Common Voice English folder holding validated.tsv and clips/")
    ap.add_argument("--out", help="manifest path (default ROOT/manifest.tsv)")
    ap.add_argument("--per-accent", type=int, default=400)
    ap.add_argument("--per-speaker", type=int, default=20)
    ap.add_argument("--min-clips", type=int, default=50)
    ap.add_argument("--convert", action="store_true", help="run ffmpeg for missing 16 kHz wavs")
    args = ap.parse_args()

    out = args.out or os.path.join(args.root, "manifest.tsv")
    out_dir = os.path.dirname(os.path.abspath(out))
    with open(os.path.join(args.root, "validated.tsv"), newline="", encoding="utf-8") as f:
        rows = list(csv.DictReader(f, delimiter="\t", quoting=csv.QUOTE_NONE))
    picked, skipped = select(rows, per_accent=args.per_accent, per_speaker=args.per_speaker,
                             min_clips=args.min_clips)

    manifest, commands = [], []
    for acc, r in picked:
        stem = os.path.splitext(r["path"])[0]
        wav = os.path.join(out_dir, "wav16k", f"{stem}.wav")
        mp3 = os.path.join(args.root, "clips", r["path"])
        if not os.path.exists(wav):
            commands.append(["ffmpeg", "-loglevel", "error", "-y", "-i", mp3,
                             "-ar", "16000", "-ac", "1", "-sample_fmt", "s16", wav])
        manifest.append({
            "corpus": "commonvoice", "l1": acc, "speaker": r["client_id"], "utt": stem,
            "split": "eval", "age": "", "wav": os.path.relpath(wav, out_dir),
            "target": r["sentence"], "labels": [10] * len(olp_manifest.tokenize(r["sentence"])),
            "label_kind": "validated",
        })
    olp_manifest.write(out, manifest)

    if commands:
        os.makedirs(os.path.join(out_dir, "wav16k"), exist_ok=True)
        if args.convert:
            for i, c in enumerate(commands, 1):
                subprocess.run(c, check=True)
                if i % 500 == 0:
                    print(f"  converted {i}/{len(commands)}", file=sys.stderr)
        else:
            script = os.path.join(out_dir, "convert.sh")
            with open(script, "w") as f:
                f.write("#!/bin/sh\nset -e\n")
                f.writelines(" ".join(shlex.quote(x) for x in c) + "\n" for c in commands)
            print(f"{len(commands)} clips need converting: sh {script}")
    accents = sorted({a for a, _ in picked})
    print(f"{len(manifest)} clips over {len(accents)} accents -> {out}; skipped "
          + ", ".join(f"{v} {k}" for k, v in skipped.items()))


if __name__ == "__main__":
    main()
