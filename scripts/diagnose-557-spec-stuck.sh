#!/bin/sh
set -u

container=neige-calm-569-server-1
window_secs=1800
tmp_db=/tmp/diagnose-557-$$.db

usage() {
  echo "usage: $0 [--container NAME] [--window-secs N]" >&2
}

cleanup() {
  rm -f "$tmp_db" "$tmp_db-wal" "$tmp_db-shm"
}
trap cleanup EXIT HUP INT TERM

while [ "$#" -gt 0 ]; do
  case "$1" in
    --container)
      [ "$#" -ge 2 ] || { usage; exit 64; }
      container=$2
      shift 2
      ;;
    --window-secs)
      [ "$#" -ge 2 ] || { usage; exit 64; }
      case "$2" in
        ''|*[!0-9]*)
          usage
          exit 64
          ;;
      esac
      window_secs=$2
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 64
      ;;
  esac
done

[ "$window_secs" -gt 0 ] || { usage; exit 64; }

if ! command -v docker >/dev/null 2>&1; then
  echo "error: docker is not available" >&2
  exit 2
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "error: python3 is not available on the host" >&2
  exit 2
fi

db_path=$(
  docker exec "$container" sh -c '
    url=${CALM_DB_URL:-sqlite:///var/lib/neige-calm/calm.db?mode=rwc}
    case "$url" in
      sqlite://*)
        path=${url#sqlite://}
        path=${path%%\?*}
        printf "%s\n" "$path"
        ;;
      *)
        printf "%s\n" /var/lib/neige-calm/calm.db
        ;;
    esac
  ' 2>/dev/null
)

if [ -z "$db_path" ]; then
  echo "error: cannot inspect container '$container'" >&2
  exit 2
fi

if ! docker exec "$container" sh -c 'test -r "$1"' sh "$db_path" >/dev/null 2>&1; then
  echo "error: sqlite db is not readable in container '$container': $db_path" >&2
  exit 2
fi

if ! docker exec "$container" sh -c 'cat "$1"' sh "$db_path" >"$tmp_db"; then
  echo "error: failed to copy sqlite db from container '$container': $db_path" >&2
  exit 2
fi

for suffix in -wal -shm; do
  if docker exec "$container" sh -c 'test -r "$1"' sh "$db_path$suffix" >/dev/null 2>&1; then
    docker exec "$container" sh -c 'cat "$1"' sh "$db_path$suffix" >"$tmp_db$suffix" || {
      echo "error: failed to copy sqlite sidecar from container '$container': $db_path$suffix" >&2
      exit 2
    }
  fi
done

python3 - "$tmp_db" "$window_secs" "$container" "$db_path" <<'PY'
import sqlite3
import sys
import time

db_path, window_secs_raw, container, container_db_path = sys.argv[1:5]
try:
    window_secs = int(window_secs_raw)
except ValueError:
    print("error: --window-secs must be an integer", file=sys.stderr)
    sys.exit(64)

try:
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    con.row_factory = sqlite3.Row
    con.execute("PRAGMA query_only = ON")
    cutoff_ms = int(time.time() * 1000) - window_secs * 1000

    hooks = con.execute(
        """
        SELECT COALESCE(json_extract(payload, '$.kind'), '<missing>') AS hook_kind,
               COUNT(*) AS count
          FROM events
         WHERE kind = 'codex.hook'
           AND at >= ?
         GROUP BY hook_kind
         ORDER BY hook_kind
        """,
        (cutoff_ms,),
    ).fetchall()

    runtimes = con.execute(
        """
        SELECT id,
               card_id,
               kind,
               status,
               COALESCE(json_extract(handle_state_json, '$.phase'), '<null>') AS phase,
               updated_at_ms
          FROM runtimes
         WHERE kind LIKE 'shared-spec%'
         ORDER BY updated_at_ms DESC, created_at_ms DESC, id DESC
        """
    ).fetchall()
except sqlite3.Error as exc:
    print(f"error: failed to query sqlite db: {exc}", file=sys.stderr)
    sys.exit(2)

counts = {row["hook_kind"]: row["count"] for row in hooks}
non_stop_count = sum(
    count for hook_kind, count in counts.items() if hook_kind != "hook.codex.stop"
)
stop_count = counts.get("hook.codex.stop", 0)
bug_present = non_stop_count > 0 and stop_count == 0

print(f"Container: {container}")
print(f"Database: {container_db_path}")
print(f"Window seconds: {window_secs}")
print()
print("Codex hook counts:")
print("hook_kind                         count")
print("--------------------------------  -----")
if hooks:
    for row in hooks:
        print(f"{row['hook_kind']:<32}  {row['count']:>5}")
else:
    print("<none>                                0")
print()
print("Shared spec runtimes:")
print("id                                    card_id                               status        phase          updated_at_ms")
print("------------------------------------  ------------------------------------  ------------  -------------  -------------")
if runtimes:
    for row in runtimes:
        print(
            f"{row['id']:<36}  {row['card_id']:<36}  "
            f"{row['status']:<12}  {row['phase']:<13}  {row['updated_at_ms']}"
        )
else:
    print("<none>")
print()
if bug_present:
    print("BUG #557 PRESENT")
    sys.exit(1)

print("NOT REPRODUCED IN THIS WINDOW")
sys.exit(0)
PY
