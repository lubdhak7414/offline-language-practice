#!/usr/bin/env bash
# The README screenshot, taken headlessly. Linux only.
#
# Runs the app on a private headless Wayland compositor (weston, pixman
# renderer, --debug so weston-screenshooter may capture), with
# XDG_CONFIG_HOME/XDG_DATA_HOME in a scratch directory: no window on your
# desktop, and your real database is never touched. The first launch
# creates the database, onboarding is then marked done, and the second
# launch is captured on the Practice screen.
#
# The webview keeps growing inside the headless output for a while after
# launch, so the capture waits (SHOT_AT, default 40 s) and the crop stops
# where the page had settled. The prompt is random. Check the result before
# committing it.
#
# Needs: weston, weston-screenshooter, python3 with Pillow, the models in
# ./models (for a scoreable prompt), and a debug build with the frontend
# embedded:
#   npx tauri build --debug --no-bundle && scripts/screenshot.sh
set -euo pipefail

cd "$(dirname "$0")/.."
REPO=$PWD
BIN=${BIN:-src-tauri/target/debug/offline-language-practice}
OUT=${OUT:-docs/screenshot.png}
SHOT_AT=${SHOT_AT:-40}
[ -x "$BIN" ] || { echo "no binary at $BIN; npx tauri build --debug --no-bundle first" >&2; exit 2; }
command -v weston-screenshooter >/dev/null || { echo "weston-screenshooter not found" >&2; exit 2; }

# Short: a Wayland socket path over 108 bytes fails to bind.
WORK=$(mktemp -d "${TMPDIR:-/tmp}/olps.XXXX")
RUN="$WORK/r"; CFG="$WORK/c"; DATA="$WORK/d"
mkdir -p "$RUN" "$CFG" "$DATA"; chmod 700 "$RUN"

XDG_RUNTIME_DIR="$RUN" weston --backend=headless --renderer=pixman --debug \
  --socket=w --width=1280 --height=800 >"$WORK/weston.log" 2>&1 &
WESTON=$!
trap 'kill $WESTON 2>/dev/null || true; [ -n "${KEEP:-}" ] || rm -rf "$WORK"' EXIT
for _ in $(seq 1 40); do [ -S "$RUN/w" ] && break; sleep 0.25; done
[ -S "$RUN/w" ] || { echo "weston did not start; see $WORK/weston.log" >&2; exit 1; }

run_app() {  # run_app <seconds>
  env -u DISPLAY WAYLAND_DISPLAY=w XDG_RUNTIME_DIR="$RUN" \
    XDG_CONFIG_HOME="$CFG" XDG_DATA_HOME="$DATA" GDK_BACKEND=wayland \
    WEBKIT_DISABLE_DMABUF_RENDERER=1 OLP_MODELS_DIR="$REPO/models" \
    timeout -s INT "$1" "$BIN" >"$WORK/app-$1.log" 2>&1 || true
}

run_app 15
DB="$CFG/com.offline.practice/app.db"
[ -f "$DB" ] || { echo "no database in $CFG — XDG_CONFIG_HOME not honoured?" >&2; exit 1; }
python3 - "$DB" <<'PY'
import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
c.execute("INSERT OR REPLACE INTO app_settings(key, value) VALUES('onboarded', '1')")
c.commit()
PY

run_app $((SHOT_AT + 5)) &
APP=$!
sleep "$SHOT_AT"
(cd "$WORK" && XDG_RUNTIME_DIR="$RUN" WAYLAND_DISPLAY=w weston-screenshooter) \
  >"$WORK/shooter.log" 2>&1
wait "$APP" || true
SHOT=$(ls "$WORK"/wayland-screenshot-*.png)

# The 900x700 window lands at the same place on the fixed 1280x800 output.
# The crop keeps its frame and title bar and stops where the page had
# settled (see above); override CROP=left,top,right,bottom if that moves.
python3 - "$SHOT" "$OUT" "${CROP:-66,57,968,703}" <<'PY'
import sys
from PIL import Image
box = tuple(int(v) for v in sys.argv[3].split(","))
Image.open(sys.argv[1]).convert("RGB").crop(box).save(sys.argv[2], optimize=True)
print(f"wrote {sys.argv[2]} ({box[2] - box[0]}x{box[3] - box[1]})")
PY
