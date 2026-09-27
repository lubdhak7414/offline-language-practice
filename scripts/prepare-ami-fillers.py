#!/usr/bin/env python3
"""Cut AMI headset audio into per-speaker segments with their filled pauses.

    python3 scripts/prepare-ami-fillers.py ~/corpora/ami ~/corpora/ami-fillers

Input: the AMI Meeting Corpus (CC BY 4.0) manual annotations unpacked under
<ami>/annotations (words/, segments/, corpusResources/meetings.xml), the
individual headset WAVs under <ami>/audio/<meeting>/<meeting>.Headset-N.wav,
and <ami>/dev.txt and test.txt listing meeting ids (the dev and test
meetings of the standard full-corpus ASR partition).

Output, under <out>:
  wav/<meeting>.<agent>.<n>.wav   one transcriber segment of 4-30 s
  segments.tsv                    id, wav, split, dur_ms, overlap_ms,
                                  ref_fillers, other_fillers, ref_text

ref_fillers is `start-end;start-end` in ms from the segment start, for every
word spelled um/uh/uhm/er/erm (in AMI practice almost all are "um" and "uh").
"mm", "hmm", "mm-hmm" and "uh-huh" are left out: they are backchannels or
agreement, not hesitations, and AMI does not tell the two uses of "mm"
apart. Word times are AMI's forced alignment, so they are approximate; the
evaluation matches with a tolerance for that reason.

Other speakers leak into every headset (overlap_ms is how much of the
segment their segments cover; the median is about a fifth). A filler one of
them says there is audible but not this speaker's, so other_fillers lists
those spans, same format, for the evaluation to treat as "don't care".

Standard library only. AMI headset audio is 16 kHz 16-bit mono; anything
else stops the script rather than being resampled.
"""

import sys
import wave
import xml.etree.ElementTree as ET
from pathlib import Path

NITE = "{http://nite.sourceforge.net/}"
FILLERS = {"um", "uh", "uhm", "er", "erm"}
MIN_S, MAX_S = 4.0, 30.0
RATE = 16_000


def channels(meetings_xml):
    """meeting -> {agent letter: headset channel}."""
    out = {}
    for m in ET.parse(meetings_xml).getroot():
        obs = m.get("observation")
        out[obs] = {s.get("nxt_agent"): int(s.get("channel")) for s in m if s.get("channel")}
    return out


def words(path):
    """(start_s, end_s, lower-case text) for every timed, non-punctuation word."""
    out = []
    for e in ET.parse(path).getroot():
        if e.tag != "w" or e.get("punc") or e.get("starttime") is None:
            continue
        try:
            out.append((float(e.get("starttime")), float(e.get("endtime")), (e.text or "").strip()))
        except ValueError:
            continue
    return out


def segments(path):
    out = []
    for e in ET.parse(path).getroot():
        if e.tag != "segment":
            continue
        try:
            out.append((float(e.get("transcriber_start")), float(e.get("transcriber_end"))))
        except (TypeError, ValueError):
            continue
    return out


def overlap(a0, a1, spans):
    return sum(max(0.0, min(a1, b1) - max(a0, b0)) for b0, b1 in spans)


def filler_spans(ws, s0, s1):
    """`a-b;c-d` in ms from s0 for filler words that start inside [s0, s1)."""
    return ";".join(
        f"{round((max(a, s0) - s0) * 1000)}-{round((min(b, s1) - s0) * 1000)}"
        for a, b, t in ws
        if s0 <= a < s1 and t.lower() in FILLERS
    )


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    ami, out = Path(sys.argv[1]).expanduser(), Path(sys.argv[2]).expanduser()
    ann = ami / "annotations"
    chan = channels(ann / "corpusResources" / "meetings.xml")
    (out / "wav").mkdir(parents=True, exist_ok=True)

    rows, n_ref, n_skipped = [], 0, 0
    for split in ("dev", "test"):
        for meeting in (ami / f"{split}.txt").read_text().split():
            agents = sorted(chan[meeting])
            segs = {a: segments(ann / "segments" / f"{meeting}.{a}.segments.xml") for a in agents}
            said = {a: words(ann / "words" / f"{meeting}.{a}.words.xml") for a in agents}
            for agent in agents:
                wav_in = ami / "audio" / meeting / f"{meeting}.Headset-{chan[meeting][agent]}.wav"
                if not wav_in.is_file():
                    print(f"missing {wav_in}", file=sys.stderr)
                    continue
                ws = said[agent]
                others = [s for a in agents if a != agent for s in segs[a]]
                other_words = sorted(w for a in agents if a != agent for w in said[a])
                with wave.open(str(wav_in), "rb") as src:
                    fmt = (src.getframerate(), src.getnchannels(), src.getsampwidth())
                    if fmt != (RATE, 1, 2):
                        sys.exit(f"{wav_in}: {fmt} is not 16 kHz 16-bit mono; resample first")
                    total = src.getnframes()
                    for n, (s0, s1) in enumerate(segs[agent]):
                        if not MIN_S <= s1 - s0 <= MAX_S:
                            n_skipped += 1
                            continue
                        f0, f1 = int(round(s0 * RATE)), min(int(round(s1 * RATE)), total)
                        if f1 - f0 < MIN_S * RATE:
                            n_skipped += 1
                            continue
                        inside = [w for w in ws if s0 <= w[0] < s1]
                        refs = filler_spans(ws, s0, s1)
                        n_ref += len(refs.split(";")) if refs else 0
                        seg_id = f"{meeting}.{agent}.{n}"
                        rel = Path("wav") / f"{seg_id}.wav"
                        src.setpos(f0)
                        frames = src.readframes(f1 - f0)
                        with wave.open(str(out / rel), "wb") as dst:
                            dst.setnchannels(1)
                            dst.setsampwidth(2)
                            dst.setframerate(RATE)
                            dst.writeframes(frames)
                        text = " ".join(w[2] for w in inside).replace("\t", " ")
                        rows.append(
                            (
                                seg_id,
                                str(rel),
                                split,
                                str(round((f1 - f0) * 1000 / RATE)),
                                str(round(overlap(s0, s1, others) * 1000)),
                                refs,
                                filler_spans(other_words, s0, s1),
                                text,
                            )
                        )

    with open(out / "segments.tsv", "w") as f:
        f.write("id\twav\tsplit\tdur_ms\toverlap_ms\tref_fillers\tother_fillers\tref_text\n")
        for r in rows:
            f.write("\t".join(r) + "\n")
    minutes = sum(int(r[3]) for r in rows) / 60_000
    print(
        f"{len(rows)} segments ({minutes:.0f} min), {n_ref} reference fillers, "
        f"{n_skipped} segments outside {MIN_S:.0f}-{MAX_S:.0f} s -> {out / 'segments.tsv'}"
    )


if __name__ == "__main__":
    main()
