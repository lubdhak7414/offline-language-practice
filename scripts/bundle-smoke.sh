#!/usr/bin/env bash
# Does the built Linux bundle install and boot away from the build machine?
# Linux only.
#
#   scripts/bundle-smoke.sh path/to/app_amd64.deb [path/to/app_amd64.AppImage]
#
# The bug this exists for: TTS once worked only on the builder's machine,
# because espeak-ng found its data through a path inside the build tree. Every
# test and every local run passed; only an install somewhere else could show
# it. So this unpacks the bundle into a scratch root (dpkg-deb -x: nothing is
# installed on the host, no sudo), runs the *unpacked* binary on a private
# Xvfb with scratch HOME/XDG_* directories, and requires:
#
#   1. the espeak-ng data is inside the bundle, next to the binary;
#   2. the binary's shared libraries all resolve (deb only);
#   3. the process is still alive after SMOKE_MIN_SECS (default 10);
#   4. it created app.db in the scratch config/data dir, within
#      SMOKE_MAX_SECS (default 30) — so the setup hooks and the SQL plugin ran;
#   5. its output never says "panicked", "PluginInitialization" or
#      "Failed to initialize eSpeak".
#
# With an AppImage too, the same runs on its extracted tree (through AppRun,
# so the bundled GTK hook applies). It does not touch the host's desktop,
# display, D-Bus session or real config, and it is stopped by process group,
# never by name.
#
# Needs: xvfb-run (apt: xvfb xauth), setsid, and dpkg-deb or ar+tar.
# Exit: 0 pass, 1 the bundle failed a check, 2 this machine cannot run it.
#   KEEP=1  keep the scratch directory (its path is printed).
set -euo pipefail

BAD='panicked|PluginInitialization|Failed to initialize eSpeak'
MIN=${SMOKE_MIN_SECS:-10}
MAX=${SMOKE_MAX_SECS:-30}

DEB=${1:-}
APPIMAGE=${2:-}
if [ -z "$DEB" ] || [ $# -gt 2 ]; then
  echo "usage: $0 <app.deb> [<app.AppImage>]" >&2
  exit 2
fi

need() { command -v "$1" >/dev/null 2>&1 || { echo "bundle-smoke: '$1' not found" >&2; exit 2; }; }
[ "$(uname -s)" = Linux ] || { echo "bundle-smoke: Linux only" >&2; exit 2; }
need xvfb-run
need setsid
[ -f "$DEB" ] || { echo "bundle-smoke: no such file: $DEB" >&2; exit 2; }
if [ -n "$APPIMAGE" ]; then
  [ -f "$APPIMAGE" ] || { echo "bundle-smoke: no such file: $APPIMAGE" >&2; exit 2; }
  [ -x "$APPIMAGE" ] || { echo "bundle-smoke: $APPIMAGE is not executable" >&2; exit 2; }
fi
DEB=$(readlink -f "$DEB")
[ -z "$APPIMAGE" ] || APPIMAGE=$(readlink -f "$APPIMAGE")

WORK=$(mktemp -d "${TMPDIR:-/tmp}/olp-smoke.XXXXXX")
GROUP=""   # process group of the running app (and its Xvfb); empty when none
LOG=""     # the running app's combined stdout/stderr

fail() {
  echo "FAIL: $*" >&2
  if [ -n "$LOG" ] && [ -s "$LOG" ]; then
    echo "--- last lines of the app's output ($LOG) ---" >&2
    tail -n 25 "$LOG" >&2
  fi
  exit 1
}

# SIGTERM, a moment to exit, then SIGKILL — to the group we started, by
# number. Nothing here matches on a name.
stop_app() {
  [ -n "$GROUP" ] || return 0
  local sig=$GROUP
  [ "$(ps -o pgid= -p "$GROUP" 2>/dev/null | tr -d ' ')" = "$GROUP" ] && sig=-$GROUP
  kill -TERM -- "$sig" 2>/dev/null || true
  for _ in $(seq 1 50); do
    kill -0 -- "$sig" 2>/dev/null || break
    sleep 0.1
  done
  kill -KILL -- "$sig" 2>/dev/null || true
  wait "$GROUP" 2>/dev/null || true
  GROUP=""
}
cleanup() {
  stop_app
  if [ -n "${KEEP:-}" ]; then echo "kept $WORK"; else rm -rf "$WORK"; fi
}
trap cleanup EXIT

extract_deb() {  # extract_deb <deb> <dest>
  mkdir -p "$2"
  if command -v dpkg-deb >/dev/null 2>&1; then
    dpkg-deb -x "$1" "$2"
    return
  fi
  # No dpkg (a non-Debian dev machine): a .deb is an ar archive around a tarball.
  need ar
  mkdir -p "$WORK/ar"
  (cd "$WORK/ar" && ar x "$1")
  local data
  for data in "$WORK"/ar/data.tar.*; do
    tar -xf "$data" -C "$2"
    return
  done
  fail "$1 has no data.tar.* member; it is not a .deb"
}

# The vendored espeak data must ship in the bundle. Same file list as
# scripts/vendor-espeak-data.sh; change them together.
check_espeak() {  # check_espeak <root> <label>
  local phontab dir f
  phontab=$(find "$1" -type f -path '*/espeak-ng-data/phontab' | head -n 1)
  [ -n "$phontab" ] ||
    fail "$2: no espeak-ng-data/phontab anywhere in the bundle. TTS would look for its data in the build machine's path."
  dir=$(dirname "$phontab")
  for f in phondata phonindex phontab intonations en_dict lang/gmw/en-US; do
    [ -s "$dir/$f" ] || fail "$2: espeak-ng-data/$f is missing or empty in the bundle."
  done
  echo "ok   $2: espeak-ng data ships (${dir#"$1"/})"
}

# Launch <cmd...> under xvfb-run with a scratch home; sets GROUP, LOG, APP_PID
# and CFG/DATA. xvfb-run supplies DISPLAY; the host's display, Wayland socket
# and D-Bus session are removed so nothing can reach the desktop.
launch() {  # launch <label> <cmd> [args...]
  local base="$WORK/$1"; shift
  mkdir -p "$base"/home "$base"/config "$base"/data "$base"/cache "$base"/tmp "$base"/run
  chmod 700 "$base/run"
  CFG="$base/config"; DATA="$base/data"; LOG="$base/app.log"
  : >"$LOG"
  # The wrapper records its PID and then execs, so the pid file names the app
  # itself (AppRun execs onward too), not a shell.
  # shellcheck disable=SC2016  # $$, $1 and $@ are for the inner bash
  setsid env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u APPDIR -u APPIMAGE \
    HOME="$base/home" XDG_CONFIG_HOME="$CFG" XDG_DATA_HOME="$DATA" \
    XDG_CACHE_HOME="$base/cache" XDG_RUNTIME_DIR="$base/run" TMPDIR="$base/tmp" \
    WEBKIT_DISABLE_DMABUF_RENDERER=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1 \
    xvfb-run -a -e "$base/xvfb.log" -s "-screen 0 1280x800x24" \
    bash -c 'echo $$ >"$1"; shift; exec "$@"' _ "$base/pid" "$@" >"$LOG" 2>&1 &
  GROUP=$!
  APP_PID=""
}

# Poll until the app has run MIN seconds and written app.db, failing at once
# if it dies, panics, or MAX seconds pass without a database.
watch() {  # watch <label> <root>: exe must live under <root>
  local label=$1 root=$2 t=0 db exe rc
  while :; do
    sleep 1
    t=$((t + 1))
    if [ -z "$APP_PID" ] && [ -s "$WORK/$label/pid" ]; then APP_PID=$(cat "$WORK/$label/pid"); fi
    if ! kill -0 "$GROUP" 2>/dev/null; then
      rc=0; wait "$GROUP" 2>/dev/null || rc=$?
      GROUP=""
      fail "$label: the app (or xvfb-run) exited with status $rc after ${t}s."
    fi
    if [ -n "$APP_PID" ] && ! kill -0 "$APP_PID" 2>/dev/null; then
      fail "$label: the app exited after ${t}s."
    fi
    if grep -Eq "$BAD" "$LOG"; then
      fail "$label: the output matches /$BAD/."
    fi
    db=$(find "$CFG" "$DATA" -name app.db 2>/dev/null | head -n 1)
    if [ "$t" -ge "$MIN" ] && [ -n "$db" ]; then break; fi
    if [ "$t" -ge "$MAX" ]; then
      if [ -n "$db" ]; then break; fi
      fail "$label: no app.db under the scratch config/data dirs after ${MAX}s."
    fi
  done
  [ -n "$APP_PID" ] || fail "$label: xvfb-run never started the app."
  exe=$(readlink -f "/proc/$APP_PID/exe" 2>/dev/null || true)
  case "$exe" in
    "$root"/*) ;;
    *) fail "$label: the running binary is '$exe', not one from the unpacked bundle ($root)." ;;
  esac
  echo "ok   $label: alive after ${t}s, ${db#"$WORK"/} created"
  stop_app
}

# --- .deb ----------------------------------------------------------------

DEB_ROOT="$WORK/deb-root"
echo "== $(basename "$DEB")"
extract_deb "$DEB" "$DEB_ROOT"

mapfile -t bins < <(find "$DEB_ROOT/usr/bin" -maxdepth 1 -type f -perm -u+x 2>/dev/null)
[ "${#bins[@]}" -eq 1 ] || fail "deb: expected one executable in usr/bin, found ${#bins[@]}."
BIN=${bins[0]}

check_espeak "$DEB_ROOT" deb

# A clean machine has only what the package's Depends pulls in. The build
# machine has more, so this can only catch a library the binary needs and no
# runtime package provides on the runner either; the rest is the job of
# `Depends`, which a real install would resolve.
if command -v ldd >/dev/null 2>&1; then
  missing=$(ldd "$BIN" 2>&1 | grep 'not found' || true)
  [ -z "$missing" ] || fail "deb: shared libraries the binary cannot resolve:
$missing"
  echo "ok   deb: shared libraries resolve"
fi

launch deb "$BIN"
watch deb "$DEB_ROOT"

# --- AppImage ------------------------------------------------------------

if [ -n "$APPIMAGE" ]; then
  echo "== $(basename "$APPIMAGE")"
  mkdir -p "$WORK/appimage"
  # Extraction needs neither FUSE nor a display.
  (cd "$WORK/appimage" && "$APPIMAGE" --appimage-extract >/dev/null) ||
    fail "appimage: --appimage-extract failed."
  AI_ROOT="$WORK/appimage/squashfs-root"
  [ -x "$AI_ROOT/AppRun" ] || fail "appimage: no AppRun in the extracted image."
  check_espeak "$AI_ROOT" appimage
  launch appimage "$AI_ROOT/AppRun"
  watch appimage "$AI_ROOT"
fi

echo "bundle smoke passed"
