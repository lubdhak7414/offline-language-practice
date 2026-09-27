#!/usr/bin/env bash
set -euo pipefail

# Copy the English subset of espeak-ng's compiled data into the repo.
#
#   ./scripts/vendor-espeak-data.sh
#
# Piper phonemizes through espeak-ng, and espeak-ng cannot run without its
# data directory. espeak-rs-sys builds that directory inside its own OUT_DIR
# and compiles the absolute path in as the fallback, so a binary finds it on
# the machine that built it and nowhere else: on a user's machine TTS fails
# with "Failed to initialize eSpeak-ng". The app therefore ships this subset
# as a bundle resource and points espeak at it at startup (lib.rs `setup`).
#
# The files are compiled from the espeak-ng sources vendored in
# espeak-rs-sys, and their format is tied to that version. Re-run this after
# any espeak-rs-sys / piper-rs bump, then run the `real_models` tests, which
# use this copy and nothing else.

root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/src-tauri/resources/espeak-ng-data"

# Any build of espeak-rs-sys will do; the output is identical across debug,
# release and test builds of the same version. Newest first.
src="$(ls -td "$root"/src-tauri/target/*/build/espeak-rs-sys-*/out/share/espeak-ng-data 2>/dev/null | head -n 1 || true)"
if [ -z "$src" ]; then
  echo "no espeak-rs-sys build output found; run a cargo build first" >&2
  exit 1
fi

# en_dict is the en-US dictionary; lang/gmw/en-US is the voice piper selects
# ("en-us" in the voice's .onnx.json). The rest is the language-independent
# phoneme and intonation tables.
files=(phondata phonindex phontab intonations en_dict lang/gmw/en-US)

rm -rf "$dest"
for f in "${files[@]}"; do
  mkdir -p "$dest/$(dirname "$f")"
  cp "$src/$f" "$dest/$f"
done

echo "copied from $src"
(cd "$dest" && find . -type f | sort | xargs sha256sum)
