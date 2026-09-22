#!/usr/bin/env bash
# Backup -> restore -> restart, against the real binary. Linux only.
#
# The unit tests in backup.rs cover the file rotation; this covers what they
# cannot: that the built app applies a staged restore on its next launch,
# before tauri-plugin-sql opens the database, and that the result survives
# a second launch. It runs the app on a private headless Wayland compositor
# with XDG_CONFIG_HOME/XDG_DATA_HOME pointed at a scratch directory, so it
# never opens a window on your desktop and never touches your real database.
#
# The live database is left the way a crash leaves it: its newest row exists
# only in the write-ahead log. The restore must install the backup without
# that WAL bleeding into it, and the rotated app.db.bak must keep the row.
#
# Needs: weston, python3 (its sqlite3 module), a debug build of the app.
#   cargo build --manifest-path src-tauri/Cargo.toml && scripts/restore-e2e.sh
set -euo pipefail

cd "$(dirname "$0")/.."
BIN=${BIN:-src-tauri/target/debug/offline-language-practice}
[ -x "$BIN" ] || { echo "no binary at $BIN; cargo build first" >&2; exit 2; }
command -v weston >/dev/null || { echo "weston not found" >&2; exit 2; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/olp-restore-e2e.XXXXXX")
RUN="$WORK/run"; CFG="$WORK/config"; DATA="$WORK/data"
DB_DIR="$CFG/com.offline.practice"
mkdir -p "$RUN" "$CFG" "$DATA"; chmod 700 "$RUN"

XDG_RUNTIME_DIR="$RUN" weston --backend=headless --socket=olp-e2e \
  --width=1024 --height=768 >"$WORK/weston.log" 2>&1 &
WESTON=$!
trap 'kill $WESTON 2>/dev/null || true; [ -n "${KEEP:-}" ] || rm -rf "$WORK"' EXIT
for _ in $(seq 1 40); do [ -S "$RUN/olp-e2e" ] && break; sleep 0.25; done
[ -S "$RUN/olp-e2e" ] || { echo "weston did not start; see $WORK/weston.log" >&2; exit 1; }

# One launch: up long enough for every plugin's setup hook, then SIGINT.
launch() {
  local log="$WORK/$1.log" rc=0
  env -u DISPLAY WAYLAND_DISPLAY=olp-e2e XDG_RUNTIME_DIR="$RUN" \
    XDG_CONFIG_HOME="$CFG" XDG_DATA_HOME="$DATA" GDK_BACKEND=wayland \
    WEBKIT_DISABLE_DMABUF_RENDERER=1 \
    timeout -s INT "${LAUNCH_SECS:-12}" "$BIN" >"$log" 2>&1 || rc=$?
  # 124 is timeout's own exit: the app was still running when stopped.
  if [ "$rc" -ne 124 ]; then
    echo "launch $1 exited $rc before it was stopped:" >&2; tail -5 "$log" >&2; exit 1
  fi
}

fronts() {
  python3 - "$1" <<'PY'
import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
ok = c.execute("PRAGMA integrity_check").fetchone()[0]
print(ok, " ".join(sorted(r[0] for r in c.execute("SELECT content_front FROM cards"))))
PY
}

check() {  # check <label> <actual> <expected>
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1: got '$2', want '$3'" >&2; exit 1; fi
}

echo "1. first launch creates and migrates the database"
launch first
[ -f "$DB_DIR/app.db" ] || { echo "no database in $DB_DIR — XDG_CONFIG_HOME not honoured?" >&2; exit 1; }

echo "2. back up, let live data diverge, crash with a WAL-only row, stage"
python3 - "$DB_DIR" "$WORK/backup.db" <<'PY'
import os, shutil, sqlite3, sys
d, backup = sys.argv[1], sys.argv[2]
ins = ("INSERT INTO cards (id, deck_id, content_front, content_back, created_at) "
       "VALUES (?, 'default', ?, 'B', 0)")
db = sqlite3.connect(os.path.join(d, "app.db"))
db.execute(ins, ("b1", "BACKUP-ROW")); db.commit()
db.execute("VACUUM INTO ?", (backup,))               # what backup_to does
db.execute("DELETE FROM cards WHERE id = 'b1'")
db.execute(ins, ("l1", "LIVE-ROW")); db.commit()
db.close()
shutil.copy(backup, os.path.join(d, "app.db.restore"))  # what stage_restore does
if os.fork() == 0:                                     # killed mid-session:
    db = sqlite3.connect(os.path.join(d, "app.db"))   # committed, never
    db.execute("PRAGMA wal_autocheckpoint=0")          # checkpointed
    db.execute(ins, ("w1", "LIVE-WAL-ROW")); db.commit()
    os._exit(0)
os.wait()
PY
[ -s "$DB_DIR/app.db-wal" ] || { echo "fixture left no WAL frames" >&2; exit 1; }

echo "3. restart applies the staged restore"
launch restart
grep -q "restored database from staged backup" "$WORK/restart.log" \
  || { echo "restart did not apply the restore:" >&2; tail -5 "$WORK/restart.log" >&2; exit 1; }
check "live db is exactly the backup"      "$(fronts "$DB_DIR/app.db")"     "ok BACKUP-ROW"
check ".bak kept the WAL-only row"         "$(fronts "$DB_DIR/app.db.bak")" "ok LIVE-ROW LIVE-WAL-ROW"
check "staged file consumed"               "$([ -e "$DB_DIR/app.db.restore" ] && echo left || echo gone)" "gone"

echo "4. a second restart does not re-apply it"
launch again
check "no second restore" "$(grep -c 'restored database' "$WORK/again.log" || true)" "0"
check "still the backup"  "$(fronts "$DB_DIR/app.db")" "ok BACKUP-ROW"

echo "restore survives a restart"
