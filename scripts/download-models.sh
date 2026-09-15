#!/usr/bin/env bash
set -euo pipefail

# Download offline model assets (STT + TTS) from Hugging Face.
# Usage: ./scripts/download-models.sh [OUT_DIR]  (default: models)
#
# Hash policy: pass the expected sha256 as 3rd arg to dl() to enforce it.
# Until hashes are pinned, dl() downloads to a .part file, prints the
# sha256 to pin, then moves into place.

OUT="${1:-models}"
HF="https://huggingface.co"
W2V_DIR="$HF/darjusul/wav2vec2-ONNX-collection/resolve/main/en_wav2vec2-base-960h"
PIPER_DIR="$HF/rhasspy/piper-voices/resolve/main/en/en_US/libritts_r/medium"

mkdir -p "$OUT"
trap 'rm -f "$OUT"/*.part' ERR INT TERM

dl() {
  local url="$1"
  local dest="$2"
  local expected="${3:-TO-VERIFY}"
  local tmp="${dest}.part"

  echo "==> $url -> $dest"
  curl -fL --retry 3 -o "$tmp" "$url"

  local actual
  actual="$(sha256sum "$tmp" | awk '{print $1}')"
  if [[ "$expected" == "TO-VERIFY" ]]; then
    echo "    sha256: $actual  (TO-VERIFY: pin this hash in the script)"
  elif [[ "$actual" != "$expected" ]]; then
    echo "    ERROR: sha256 mismatch for $dest" >&2
    echo "    expected: $expected" >&2
    echo "    actual:   $actual" >&2
    rm -f "$tmp"
    exit 1
  else
    echo "    sha256 OK: $actual"
  fi
  mv -f "$tmp" "$dest"
}

dl "$W2V_DIR/model.onnx" "$OUT/wav2vec2.onnx"
dl "$W2V_DIR/vocab.json" "$OUT/vocab.json"
dl "$PIPER_DIR/en_US-libritts_r-medium.onnx" "$OUT/en_US-libritts_r-medium.onnx"
dl "$PIPER_DIR/en_US-libritts_r-medium.onnx.json" "$OUT/en_US-libritts_r-medium.onnx.json"

echo "Done. Models are in $OUT/"
echo "Note: backend find_model checks models/ and src-tauri/models/."
