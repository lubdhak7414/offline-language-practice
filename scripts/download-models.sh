#!/usr/bin/env bash
set -euo pipefail

# Download the offline model assets (speech recognition + voice).
#
#   ./scripts/download-models.sh [OUT_DIR]     # default: models
#
# The app can fetch these itself during onboarding; this script exists for
# headless setups, CI, distro packagers and anyone pre-seeding a machine.
# It fetches the same files, from the same pinned URLs, and checks the same
# hashes as `src-tauri/src/download.rs` — when you re-pin one, re-pin both.
#
# Two rules, both learned the hard way:
#
#   * URLs pin an immutable commit, never a branch. The previous version of
#     this script fetched `resolve/main/en_wav2vec2-base-960h/model.onnx`;
#     upstream later reorganized its directories and that URL became a 404.
#     A branch ref is a promise upstream never made.
#   * A missing or mismatched hash is a hard error. There is no
#     "print it and continue" path: an unverified model produces wrong
#     transcripts silently, which is worse than no model at all.

OUT="${1:-models}"

W2V_REV="10fe51e66cf604261f729d7f66499ee56e43b62d"
PIPER_REV="c10ece1aade47bb51c153c893d14e5bf8e5b7117"
W2V="https://huggingface.co/darjusul/wav2vec2-ONNX-collection/resolve/$W2V_REV/wav2vec2_onnx_models/en_wav2vec2-asr-base-960h"
PIPER="https://huggingface.co/rhasspy/piper-voices/resolve/$PIPER_REV/en/en_US/libritts_r/medium"

# sha256 of the exact bytes served at the URLs above. The two large ones
# also match the LFS oids Hugging Face reports, i.e. a digest computed
# independently by the host.
SHA_MODEL="200bb76524fca2316ee3713338e2f517ca0a032bbc5423b5441d50ddb8761a88"
SHA_VOCAB="b6c1048b94eb345c58a7c857ed4c198f2b26f43c22b06661409d6720eee35069"
SHA_VOICE="10bb85e071d616fcf4071f369f1799d0491492ab3c5d552ec19fb548fac13195"
SHA_VOICE_CFG="b471dc60d2d8335e819c393d196d6fbf792817f40051257b269878505bc9afb3"

# `sha256sum` is GNU; macOS ships `shasum` instead. Resolve once, loudly.
if command -v sha256sum >/dev/null 2>&1; then
  sha256_of() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
  sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }
else
  echo "ERROR: need sha256sum or shasum on PATH to verify downloads." >&2
  exit 1
fi

command -v curl >/dev/null 2>&1 || { echo "ERROR: curl is required." >&2; exit 1; }

mkdir -p "$OUT"
trap 'rm -f "$OUT"/*.part' INT TERM

dl() {
  local url="$1" dest="$2" expected="$3"
  local tmp="${dest}.part"

  if [[ -z "$expected" || ${#expected} -ne 64 ]]; then
    echo "ERROR: no valid sha256 pinned for $dest" >&2
    exit 1
  fi

  # Already installed and correct? Don't re-download 380 MB.
  if [[ -f "$dest" ]] && [[ "$(sha256_of "$dest")" == "$expected" ]]; then
    echo "==> $(basename "$dest") already present and verified"
    return 0
  fi

  echo "==> $url"
  echo "    -> $dest"
  # --proto/--tlsv1.2: never be downgraded off https.
  # --continue-at -  : resume a previous partial fetch.
  # --retry-all-errors: plain --retry ignores connection resets.
  curl --proto '=https' --tlsv1.2 -fL \
       --retry 3 --retry-all-errors \
       --max-time 1800 \
       --continue-at - \
       -o "$tmp" "$url"

  local actual
  actual="$(sha256_of "$tmp")"
  if [[ "$actual" != "$expected" ]]; then
    echo "    ERROR: sha256 mismatch for $dest" >&2
    echo "    expected: $expected" >&2
    echo "    actual:   $actual" >&2
    echo "    The partial file has been deleted. If this repeats, the pinned" >&2
    echo "    revision may have been rewritten upstream — do not bypass this." >&2
    rm -f "$tmp"
    exit 1
  fi
  echo "    sha256 OK"
  mv -f "$tmp" "$dest"
}

dl "$W2V/model.onnx"                          "$OUT/wav2vec2.onnx"                     "$SHA_MODEL"
dl "$W2V/vocab.json"                          "$OUT/vocab.json"                        "$SHA_VOCAB"
dl "$PIPER/en_US-libritts_r-medium.onnx"      "$OUT/en_US-libritts_r-medium.onnx"      "$SHA_VOICE"
dl "$PIPER/en_US-libritts_r-medium.onnx.json" "$OUT/en_US-libritts_r-medium.onnx.json" "$SHA_VOICE_CFG"

echo
echo "Done. Models are in $OUT/"
echo "The app also searches src-tauri/models/, \$OLP_MODELS_DIR, and its own"
echo "app-data models/ directory (where in-app downloads land)."
