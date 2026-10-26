#!/usr/bin/env bash
# Farwork reference stack — run the *upstream* Agents-Anywhere server locally
# against native PostgreSQL + Redis (no Docker), then bootstrap + verify login.
#
# Purpose: this is the "control implementation" that Farwork's Rust gateway
# proxies /api/v2 against until each domain is replaced. See docs/reference-stack.md.
#
# Usage:  bash scripts/reference-up.sh
# Env overrides (all optional): PORT, HOST, DB_NAME, DB_USER, DB_PASSWORD,
#   DB_HOST, DB_PORT, PGADMIN_USER, REDIS_URL, REDIS_PREFIX, SERVER_SECRET,
#   UPSTREAM_SRC, REF_ROOT, REF_VENV, ADMIN_EMAIL, ADMIN_PASSWORD,
#   SETUP_TOKEN_TTL, STARTUP_TIMEOUT
set -euo pipefail

UPSTREAM_SRC="${UPSTREAM_SRC:-/tmp/anywhere-survey}"
# Writable copy of the read-only upstream clone: uv/alembic must write there
# (uv.lock, __pycache__), and /tmp/anywhere-survey must stay pristine.
REF_ROOT="${REF_ROOT:-/tmp/farwork-ref}"
REF_SRC="$REF_ROOT/server"
REF_VENV="${REF_VENV:-/tmp/farwork-ref-venv}"
REF_PY="$REF_VENV/bin/python"
RUN_DIR="$REF_ROOT/run"
LOG_FILE="$RUN_DIR/server.log"
PID_FILE="$RUN_DIR/server.pid"
TOKEN_FILE="$RUN_DIR/access-token.txt"
FILES_ROOT="$REF_ROOT/data/files"

PORT="${PORT:-8000}"
HOST="${HOST:-127.0.0.1}"
DB_HOST="${DB_HOST:-127.0.0.1}"
DB_PORT="${DB_PORT:-5432}"
DB_NAME="${DB_NAME:-agents_anywhere}"
DB_USER="${DB_USER:-agents_anywhere}"
DB_PASSWORD="${DB_PASSWORD:-agents_anywhere_dev_password}"
PGADMIN_USER="${PGADMIN_USER:-$(id -un)}"
REDIS_URL="${REDIS_URL:-redis://127.0.0.1:6379/0}"
REDIS_PREFIX="${REDIS_PREFIX:-farwork-ref}"
SERVER_SECRET="${SERVER_SECRET:-farwork-reference-dev-secret}"
ADMIN_EMAIL="${ADMIN_EMAIL:-admin@farwork.local}"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-RefAdmin!2026}"
SETUP_TOKEN_TTL="${SETUP_TOKEN_TTL:-900}"
STARTUP_TIMEOUT="${STARTUP_TIMEOUT:-90}"

DB_URL="postgresql+asyncpg://$DB_USER:$DB_PASSWORD@$DB_HOST:$DB_PORT/$DB_NAME"
BASE="http://$HOST:$PORT"
API="$BASE/api/v2"

section() { printf '\n=== %s ===\n' "$*"; }
ok()      { printf '  ok   %s\n' "$*"; }
note()    { printf '  ..   %s\n' "$*"; }
die()     { printf '\nFATAL: %s\n' "$*" >&2; exit 1; }

psql_admin() {
  PGHOST="$DB_HOST" PGPORT="$DB_PORT" PGUSER="$PGADMIN_USER" \
    psql -d postgres -v ON_ERROR_STOP=1 "$@"
}

# ---------------------------------------------------------------- 0. preflight
section "0. preflight"
for cmd in uv psql redis-cli curl rsync; do
  command -v "$cmd" >/dev/null || die "missing required command: $cmd"
done
[ -d "$UPSTREAM_SRC/server" ] || die "upstream server not found at $UPSTREAM_SRC/server (set UPSTREAM_SRC)"
pg_isready -h "$DB_HOST" -p "$DB_PORT" >/dev/null || die "PostgreSQL not reachable at $DB_HOST:$DB_PORT"
[ "$(redis-cli -u "$REDIS_URL" ping 2>/dev/null)" = "PONG" ] || die "Redis not reachable at $REDIS_URL"
mkdir -p "$RUN_DIR" "$FILES_ROOT"
ok "uv $(uv --version | awk '{print $2}'); postgres + redis reachable; target port $PORT"

# ------------------------------------------------------- 1. PostgreSQL role/db
section "1. PostgreSQL role + database (idempotent)"
if [ "$(psql_admin -tAc "SELECT 1 FROM pg_roles WHERE rolname='$DB_USER'")" != "1" ]; then
  psql_admin -c "CREATE ROLE \"$DB_USER\" LOGIN" >/dev/null
  ok "role $DB_USER created"
else
  ok "role $DB_USER already exists"
fi
# Re-assert the password every run so the URL below is always truthful.
psql_admin -c "ALTER ROLE \"$DB_USER\" WITH LOGIN PASSWORD '$DB_PASSWORD'" >/dev/null
if [ "$(psql_admin -tAc "SELECT 1 FROM pg_database WHERE datname='$DB_NAME'")" != "1" ]; then
  psql_admin -c "CREATE DATABASE \"$DB_NAME\" OWNER \"$DB_USER\"" >/dev/null
  ok "database $DB_NAME created (owner $DB_USER)"
else
  ok "database $DB_NAME already exists"
fi
PGPASSWORD="$DB_PASSWORD" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -tAc 'select 1' >/dev/null \
  || die "app role cannot log in over TCP: check pg_hba.conf for host connections"
ok "app role connects over TCP"

# --------------------------------------------- 2. writable copy + venv + deps
section "2. writable copy of upstream server + venv + dependencies"
STAMP_FILE="$REF_ROOT/.install-stamp"
SRC_STAMP="$REF_VENV $(md5 -q "$UPSTREAM_SRC/server/pyproject.toml" 2>/dev/null || md5sum "$UPSTREAM_SRC/server/pyproject.toml" | awk '{print $1}')"
if [ -f "$STAMP_FILE" ] && [ "$(cat "$STAMP_FILE")" = "$SRC_STAMP" ] && [ -x "$REF_PY" ]; then
  ok "dependencies already synced (stamp match) — skipping copy + uv sync"
else
  rsync -a --delete \
    --exclude '.venv' --exclude '__pycache__' --exclude '.ruff_cache' --exclude '.pytest_cache' \
    "$UPSTREAM_SRC/server/" "$REF_SRC/"
  ok "copied upstream server -> $REF_SRC (upstream clone untouched)"
  # 'uv venv' refuses an existing path, and the venv deliberately outlives
  # --reset-data (dropping data must not force a re-download of every wheel).
  [ -x "$REF_PY" ] || uv venv --python 3.12 "$REF_VENV" >/dev/null
  # NOTE: 'uv pip install -e .' FAILS on this project (no [build-system] ->
  # setuptools legacy backend; the flat layout has agent_server/ + migrations/ +
  # loadtests/ and setuptools refuses multiple top-level packages). 'uv sync'
  # treats a build-system-less project as non-package: deps only, and the code is
  # imported from the project root — so cwd MUST be $REF_SRC for every command.
  # UV_PROJECT_ENVIRONMENT keeps the venv outside the tree (no server/.venv).
  UV_PROJECT_ENVIRONMENT="$REF_VENV" uv sync --project "$REF_SRC" --python 3.12 --no-dev \
    >"$RUN_DIR/uv-sync.log" 2>&1 \
    || { tail -25 "$RUN_DIR/uv-sync.log"; die "uv sync failed (needs network for the git dep httpx-s3-client)"; }
  printf '%s' "$SRC_STAMP" >"$STAMP_FILE"
  ok "uv sync done ($("$REF_PY" -V)); log: $RUN_DIR/uv-sync.log"
fi

# ------------------------------------------------------------ 3. migrations
section "3. alembic migrations to head"
( cd "$REF_SRC" && AGENT_SERVER_DB_URL="$DB_URL" "$REF_PY" -m agent_server.infra.db.migrations upgrade ) \
  >"$RUN_DIR/migrate.log" 2>&1 || { tail -20 "$RUN_DIR/migrate.log"; die "migration failed"; }
ok "$(grep -o 'database schema is now .*' "$RUN_DIR/migrate.log" | tail -1)"
TABLE_COUNT="$(PGPASSWORD="$DB_PASSWORD" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -tAc \
  "select count(*) from information_schema.tables where table_schema='public'")"
ok "public tables in $DB_NAME: $TABLE_COUNT"

# ------------------------------------------------------- 4. start uvicorn
section "4. start server on $HOST:$PORT"
if [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
  note "server already running (pid $(cat "$PID_FILE")) — restarting"
  bash "$(dirname "$0")/reference-down.sh" >/dev/null
fi
rm -f "$PID_FILE"

: >"$LOG_FILE"
( cd "$REF_SRC" && exec env \
    AGENT_SERVER_DB_URL="$DB_URL" \
    AGENT_SERVER_REDIS_URL="$REDIS_URL" \
    AGENT_SERVER_REDIS_PREFIX="$REDIS_PREFIX" \
    AGENT_SERVER_SECRET="$SERVER_SECRET" \
    AGENT_SERVER_HOST="$HOST" \
    AGENT_SERVER_PORT="$PORT" \
    AGENT_SERVER_FILES_LOCAL_ROOT="$FILES_ROOT" \
    AGENT_SERVER_SETUP_TOKEN_TTL="$SETUP_TOKEN_TTL" \
    nohup "$REF_VENV/bin/uvicorn" agent_server.app:create_app --factory \
      --host "$HOST" --port "$PORT" ) >>"$LOG_FILE" 2>&1 &
SERVER_PID=$!
echo "$SERVER_PID" >"$PID_FILE"

for _ in $(seq 1 "$STARTUP_TIMEOUT"); do
  kill -0 "$SERVER_PID" 2>/dev/null || { tail -30 "$LOG_FILE" >&2; die "server exited during startup (log: $LOG_FILE)"; }
  [ "$(curl -s -o /dev/null -w '%{http_code}' "$API/auth/config" || true)" = "200" ] && break
  sleep 1
done
[ "$(curl -s -o /dev/null -w '%{http_code}' "$API/auth/config" || true)" = "200" ] \
  || { tail -30 "$LOG_FILE" >&2; die "server not healthy after $STARTUP_TIMEOUT s"; }
ok "server pid $SERVER_PID healthy; log $LOG_FILE"

# --------------------------------------------------------- 5. bootstrap admin
section "5. bootstrap first admin"
CONFIG="$(curl -s "$API/auth/config")"
ok "GET /auth/config -> $CONFIG"

json() { "$REF_PY" -c 'import json,sys;print(json.load(sys.stdin)[sys.argv[1]])' "$1"; }
mk_salt() {
  "$REF_PY" -c 'import secrets,base64;print(base64.urlsafe_b64encode(secrets.token_bytes(16)).decode().rstrip("="))'
}
# Mirrors web-next/src/features/auth/password-verifier.ts: PBKDF2-SHA256 over
# (password, salt), 120000 iterations, 32-byte output, base64url without padding.
# The salt travels to the server as-is and is stored inside the hash string.
mk_verifier() {
  "$REF_PY" -c 'import hashlib,base64,sys;print(base64.urlsafe_b64encode(hashlib.pbkdf2_hmac("sha256",sys.argv[1].encode(),sys.argv[2].encode(),120000,32)).decode().rstrip("="))' "$1" "$2"
}

NEEDS_BOOTSTRAP="$(json needsBootstrap <<<"$CONFIG")"
if [ "$NEEDS_BOOTSTRAP" = "True" ]; then
  SETUP_TOKEN="$(grep -o 'setup-token: [A-Za-z0-9_-]*' "$LOG_FILE" | tail -1 | awk '{print $2}')"
  [ -n "$SETUP_TOKEN" ] || die "needsBootstrap=true but no setup token in $LOG_FILE"
  note "setup token read from server log: $SETUP_TOKEN"
  SALT="$(mk_salt)"
  VERIFIER="$(mk_verifier "$ADMIN_PASSWORD" "$SALT")"
  REG_HTTP="$(curl -s -o "$RUN_DIR/register.json" -w '%{http_code}' -X POST "$API/auth/register" \
    -H 'content-type: application/json' \
    -d "{\"email\":\"$ADMIN_EMAIL\",\"displayName\":\"Ref Admin\",\"passwordVerifier\":\"$VERIFIER\",\"passwordSalt\":\"$SALT\",\"setupToken\":\"$SETUP_TOKEN\"}")"
  [ "$REG_HTTP" = "200" ] || { cat "$RUN_DIR/register.json"; die "bootstrap register failed (HTTP $REG_HTTP)"; }
  ok "first admin created: $ADMIN_EMAIL (role $(json role <"$RUN_DIR/register.json"))"
else
  ok "users already exist — skipping bootstrap"
fi

# ------------------------------------- 6. end-to-end verification of the chain
section "6. verify the auth chain end to end"
SALT="$(curl -s -X POST "$API/auth/password-salt" -H 'content-type: application/json' \
  -d "{\"email\":\"$ADMIN_EMAIL\"}" | json salt)"
[ -n "$SALT" ] || die "POST /auth/password-salt returned no salt"
ok "POST /auth/password-salt -> salt=$SALT"
VERIFIER="$(mk_verifier "$ADMIN_PASSWORD" "$SALT")"
LOGIN_HTTP="$(curl -s -o "$RUN_DIR/login.json" -w '%{http_code}' -X POST "$API/auth/login" \
  -H 'content-type: application/json' -d "{\"email\":\"$ADMIN_EMAIL\",\"passwordVerifier\":\"$VERIFIER\"}")"
if [ "$LOGIN_HTTP" != "200" ]; then
  cat "$RUN_DIR/login.json"
  die "login failed (HTTP $LOGIN_HTTP); if the admin was created with another password run: bash scripts/reference-down.sh --reset-data"
fi
ACCESS_TOKEN="$(json accessToken <"$RUN_DIR/login.json")"
printf '%s' "$ACCESS_TOKEN" >"$TOKEN_FILE"
ok "POST /auth/login -> 200; accessToken $(printf '%.24s' "$ACCESS_TOKEN")... ($(printf '%s' "$ACCESS_TOKEN" | wc -c | tr -d ' ') chars) saved to $TOKEN_FILE"

FAILED=0
check_200() {
  local method="$1" path="$2" body="" out code
  [ $# -ge 3 ] && body="$3"
  out="$RUN_DIR/$(echo "$path" | tr '/' '_').json"
  if [ -n "$body" ]; then
    code="$(curl -s -o "$out" -w '%{http_code}' -X "$method" "$API$path" \
      -H "authorization: Bearer $ACCESS_TOKEN" -H 'content-type: application/json' -d "$body")"
  else
    code="$(curl -s -o "$out" -w '%{http_code}' -X "$method" "$API$path" -H "authorization: Bearer $ACCESS_TOKEN")"
  fi
  if [ "$code" = "200" ]; then
    ok "$method $path -> 200  $(head -c 80 "$out")"
  else
    printf '  FAIL %s %s -> %s  %s\n' "$method" "$path" "$code" "$(head -c 200 "$out")"
    FAILED=1
  fi
}
check_200 GET /auth/me
check_200 GET /connectors
check_200 GET /projects
check_200 GET /sessions/list
# ws-ticket needs clientId plus exactly one scope; dashboard is the cheap one.
check_200 POST /ws-ticket '{"clientId":"farwork-ref-smoke","scope":{"dashboard":true}}'
[ "$FAILED" = "0" ] || die "one or more authenticated checks failed (payloads in $RUN_DIR)"

section "reference stack ready"
cat <<EOF
  server      : $BASE   (pid $(cat "$PID_FILE"), log $LOG_FILE)
  openapi     : $BASE/docs
  api prefix  : /api/v2
  database    : $DB_URL
  redis       : $REDIS_URL   (key prefix: $REDIS_PREFIX)
  admin       : $ADMIN_EMAIL / $ADMIN_PASSWORD
  token file  : $TOKEN_FILE
  stop        : bash scripts/reference-down.sh              (keeps data)
  stop + wipe : bash scripts/reference-down.sh --reset-data (drops the database)
EOF
