#!/usr/bin/env bash
# Farwork reference stack — stop the upstream Agents-Anywhere server.
# Default: stop the process only, keep PostgreSQL data, Redis keys and file storage.
# --reset-data: additionally DROP DATABASE, clear the Redis key prefix and delete
#   the local file store, so the next reference-up.sh does a full bootstrap again.
set -euo pipefail

UPSTREAM_SRC="${UPSTREAM_SRC:-/tmp/anywhere-survey}"
REF_ROOT="${REF_ROOT:-/tmp/farwork-ref}"
RUN_DIR="$REF_ROOT/run"
PID_FILE="$RUN_DIR/server.pid"
LOG_FILE="$RUN_DIR/server.log"

PORT="${PORT:-8000}"
HOST="${HOST:-127.0.0.1}"
DB_HOST="${DB_HOST:-127.0.0.1}"
DB_PORT="${DB_PORT:-5432}"
DB_NAME="${DB_NAME:-agents_anywhere}"
DB_USER="${DB_USER:-agents_anywhere}"
PGADMIN_USER="${PGADMIN_USER:-$(id -un)}"
REDIS_URL="${REDIS_URL:-redis://127.0.0.1:6379/0}"
REDIS_PREFIX="${REDIS_PREFIX:-farwork-ref}"
FILES_ROOT="$REF_ROOT/data/files"

RESET_DATA=0
for arg in "$@"; do
  case "$arg" in
    --reset-data) RESET_DATA=1 ;;
    -h|--help) sed -n '2,7p' "$0"; exit 0 ;;
    *) printf 'unknown argument: %s (use --reset-data)\n' "$arg" >&2; exit 2 ;;
  esac
done

ok()   { printf '  ok   %s\n' "$*"; }
note() { printf '  ..   %s\n' "$*"; }
die()  { printf 'FATAL: %s\n' "$*" >&2; exit 1; }

psql_admin() {
  PGHOST="$DB_HOST" PGPORT="$DB_PORT" PGUSER="$PGADMIN_USER" \
    psql -d postgres -v ON_ERROR_STOP=1 "$@"
}

# ------------------------------------------------------------ stop the server
printf '=== stop server ===\n'
STOPPED=0
if [ -f "$PID_FILE" ]; then
  PID="$(cat "$PID_FILE")"
  if kill -0 "$PID" 2>/dev/null; then
    kill "$PID" 2>/dev/null || true
    for _ in $(seq 1 40); do
      kill -0 "$PID" 2>/dev/null || break
      sleep 0.25
    done
    if kill -0 "$PID" 2>/dev/null; then
      note "pid $PID ignored SIGTERM — sending SIGKILL"
      kill -9 "$PID" 2>/dev/null || true
    fi
    ok "stopped server pid $PID"
    STOPPED=1
  else
    note "pid file pointed at $PID, which is not running (stale)"
  fi
  rm -f "$PID_FILE"
else
  note "no pid file at $PID_FILE"
fi

# Fallback: a listener on PORT that is *our* uvicorn (matched by command line,
# never a blanket port kill) means the pid file was lost.
if [ "$STOPPED" = "0" ] && command -v lsof >/dev/null; then
  LEFTOVER="$(lsof -nP -iTCP:"$PORT" -sTCP:LISTEN -t 2>/dev/null || true)"
  for pid in $LEFTOVER; do
    cmdline="$(ps -o command= -p "$pid" 2>/dev/null || true)"
    case "$cmdline" in
      *uvicorn*agent_server.app:create_app*)
        kill "$pid" 2>/dev/null || true
        # Wait for the listener to release the port, otherwise the next
        # reference-up.sh races a still-bound socket.
        for _ in $(seq 1 40); do
          lsof -nP -iTCP:"$PORT" -sTCP:LISTEN -t >/dev/null 2>&1 || break
          sleep 0.25
        done
        if lsof -nP -iTCP:"$PORT" -sTCP:LISTEN -t >/dev/null 2>&1; then
          kill -9 "$pid" 2>/dev/null || true
        fi
        ok "stopped stray reference uvicorn pid $pid on port $PORT"
        STOPPED=1
        ;;
      *)
        note "port $PORT held by unrelated process $pid, leaving it alone"
        ;;
    esac
  done
fi

if [ "$STOPPED" = "0" ]; then
  note "server was not running"
fi

# --------------------------------------------------------------- reset data
if [ "$RESET_DATA" = "1" ]; then
  printf '\n=== reset data ===\n'
  psql_admin -c "DROP DATABASE IF EXISTS \"$DB_NAME\" WITH (FORCE)" >/dev/null
  ok "dropped database $DB_NAME (role $DB_USER kept)"
  if [ "$(redis-cli -u "$REDIS_URL" ping 2>/dev/null)" = "PONG" ]; then
    DELETED=0
    while IFS= read -r key; do
      [ -n "$key" ] || continue
      redis-cli -u "$REDIS_URL" del "$key" >/dev/null
      DELETED=$((DELETED + 1))
    done < <(redis-cli -u "$REDIS_URL" --scan --pattern "$REDIS_PREFIX:*" 2>/dev/null || true)
    ok "cleared $DELETED Redis key(s) with prefix $REDIS_PREFIX:*"
  else
    note "Redis not reachable — skipped key cleanup"
  fi
  # Guard the rm: absolute path under REF_ROOT, never a bare/computed root.
  case "$FILES_ROOT" in
    "$REF_ROOT"/data/files) rm -rf "$FILES_ROOT"; ok "deleted local file store $FILES_ROOT" ;;
    *) die "refusing to delete unexpected files root: $FILES_ROOT" ;;
  esac
  rm -f "$REF_ROOT/.install-stamp"
  ok "next reference-up.sh will bootstrap from an empty database"
else
  printf '\n  data kept: database %s, Redis %s (prefix %s), files %s\n' \
    "$DB_NAME" "$REDIS_URL" "$REDIS_PREFIX" "$FILES_ROOT"
fi
