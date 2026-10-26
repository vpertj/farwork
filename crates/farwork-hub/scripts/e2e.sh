#!/usr/bin/env bash
# End-to-end walk through the P0 hub against a real Postgres database.
#
#   createdb farwork
#   FARWORK_HUB_SETUP_TOKEN=setup-e2e-token cargo run -p farwork-hub &
#   BASE_URL=http://127.0.0.1:7300 SETUP_TOKEN=setup-e2e-token crates/farwork-hub/scripts/e2e.sh
#
# Requires: curl, jq, node (only for PBKDF2 and the WebSocket step). No Python.
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:7300}"
SETUP_TOKEN="${SETUP_TOKEN:-setup-e2e-token}"
EMAIL="${EMAIL:-ada+$(date +%s)@example.com}"
PASSWORD="${PASSWORD:-correct horse battery staple}"
TOKEN=""

say() { printf '\n=== %s ===\n' "$1"; }
auth_header() { if [ -n "$TOKEN" ]; then printf 'authorization: Bearer %s' "$TOKEN"; fi; }
api() { curl -sS -X "$1" "$BASE_URL$2" -H 'content-type: application/json' ${TOKEN:+-H "authorization: Bearer $TOKEN"} ${3:+-d "$3"}; }

say "healthz"
curl -sS "$BASE_URL/healthz" | jq -c .

say "auth/config (bootstrap)"
curl -sS "$BASE_URL/api/v2/auth/config" | jq -c '{needsBootstrap, registrationOpen, setupTokenExpiresAt}'

say "auth/password-salt"
SALT=$(curl -sS -X POST "$BASE_URL/api/v2/auth/password-salt" -H 'content-type: application/json' \
  -d "{\"email\":\"$EMAIL\"}" | jq -r .salt)
echo "salt=$SALT"

say "derive verifier (PBKDF2-SHA256 / 120000 iterations / 32 bytes / base64url)"
VERIFIER=$(node -e '
  const crypto = require("node:crypto");
  const [password, salt] = process.argv.slice(1);
  process.stdout.write(crypto.pbkdf2Sync(password, salt, 120000, 32, "sha256").toString("base64url"));
' "$PASSWORD" "$SALT")
echo "verifier=$VERIFIER"

say "auth/register (first account becomes admin)"
REGISTER=$(curl -sS -X POST "$BASE_URL/api/v2/auth/register" -H 'content-type: application/json' \
  -d "{\"email\":\"$EMAIL\",\"displayName\":\"Ada\",\"passwordVerifier\":\"$VERIFIER\",\"passwordSalt\":\"$SALT\",\"setupToken\":\"$SETUP_TOKEN\"}")
echo "$REGISTER" | jq -c '{userId, role, email, tokenType}'
TOKEN=$(echo "$REGISTER" | jq -r .accessToken)
echo "token=${TOKEN:0:24}..."

say "auth/login"
LOGIN=$(curl -sS -X POST "$BASE_URL/api/v2/auth/login" -H 'content-type: application/json' \
  -d "{\"email\":\"$EMAIL\",\"passwordVerifier\":\"$VERIFIER\"}")
echo "$LOGIN" | jq -c '{userId, role, tokenType}'
TOKEN=$(echo "$LOGIN" | jq -r .accessToken)

say "auth/me"
api GET /api/v2/auth/me | jq -c '{userId, role, disabled}'

say "connectors: create"
CONNECTOR=$(api POST /api/v2/connectors '{"name":"Ada laptop","connectorKind":"cli"}')
echo "$CONNECTOR" | jq -c '{connector: {id: .connector.id, status: .connector.status}, tokenPrefix}'
CONNECTOR_ID=$(echo "$CONNECTOR" | jq -r .connector.id)

say "projects: create"
PROJECT=$(api POST /api/v2/projects "{\"name\":\"farwork\",\"connectorId\":\"$CONNECTOR_ID\",\"workspacePath\":\"/srv/farwork\"}")
echo "$PROJECT" | jq -c '{project: {id: .project.id, workspacePath: .project.workspacePath, activeSessionCount: .project.activeSessionCount}}'
PROJECT_ID=$(echo "$PROJECT" | jq -r .project.id)

say "sessions/create-and-start"
STARTED=$(api POST /api/v2/sessions/create-and-start \
  "{\"connectorId\":\"$CONNECTOR_ID\",\"projectId\":\"$PROJECT_ID\",\"runtime\":\"codex\",\"title\":\"first session\",\"content\":\"hello from the e2e script\",\"clientMessageId\":\"cm_e2e_1\"}")
echo "$STARTED" | jq -c '{session: {id: .session.id, status: .session.status, takeover: .session.takeover, updatedSeq: .session.updatedSeq}, connectorResult: .connectorResult.code, eventCursor}'
SESSION_ID=$(echo "$STARTED" | jq -r .session.id)

say "sessions: list (limit<=100)"
api GET '/api/v2/sessions?limit=100' | jq -c '{count: (.sessions|length), hasMore, first: .sessions[0].id}'

say "runtime/messages (persist a user message as a timeline item)"
MESSAGE=$(api POST "/api/v2/sessions/$SESSION_ID/runtime/messages" '{"content":"second message","clientMessageId":"cm_e2e_2"}')
echo "$MESSAGE" | jq -c '{ok, eventCursor: .result.eventCursor, item: {id: .result.item.id, role: .result.item.role, updatedSeq: .result.item.updatedSeq, text: .result.item.content.text}}'

say "runtime/messages replay with the same clientMessageId (idempotent)"
api POST "/api/v2/sessions/$SESSION_ID/runtime/messages" '{"content":"second message","clientMessageId":"cm_e2e_2"}' \
  | jq -c '{ok, duplicate: .result.duplicate, sameItem: .result.item.id}'

say "snapshot"
api GET "/api/v2/sessions/$SESSION_ID/snapshot" \
  | jq -c '{items: (.timeline.items|length), nextSeq: .timeline.nextSeq, eventCursor, capabilities: (.effectiveCapabilities.capabilities|length)}'

say "events?after=seq:0 (cursor catch-up, not polling)"
api GET "/api/v2/sessions/$SESSION_ID/events?after=seq:0" \
  | jq -c '{events: [.events[] | {type, sequence, cursor}], nextCursor, snapshotRequired}'

say "runtime/state + runtime/capabilities"
api GET "/api/v2/sessions/$SESSION_ID/runtime/state" | jq -c '{status: .state.status, updatedSeq: .state.updatedSeq}'
api GET "/api/v2/sessions/$SESSION_ID/runtime/capabilities" | jq -c '{connectorId, revision: .capabilitySet.revision, capabilities: (.capabilitySet.capabilities|length)}'

say "not implemented: steer answers 501"
api POST "/api/v2/sessions/$SESSION_ID/runtime/steer" '{"content":"nope"}' | jq -c '{error, detail}'

say "ws-ticket + session socket (session.subscribed, then a live delta)"
TICKET=$(api POST /api/v2/ws-ticket "{\"clientId\":\"e2e-client\",\"scope\":{\"sessionId\":\"$SESSION_ID\"}}" | jq -r .ticket)
TOKEN="$TOKEN" node -e '
  const [base, sessionId, ticket] = process.argv.slice(1);
  const ws = new WebSocket(base.replace(/^http/, "ws") + "/api/v2/sessions/" + sessionId + "/ws?ticket=" + ticket);
  let seen = 0;
  const done = (code) => { ws.close(); setTimeout(() => process.exit(code), 50); };
  ws.addEventListener("message", (event) => {
    const frame = JSON.parse(event.data);
    if (frame.type === "keepalive") return;
    console.log(JSON.stringify({type: frame.type, cursor: frame.cursor, payloadKeys: Object.keys(frame.payload || {})}));
    seen += 1;
    if (seen >= 2) setTimeout(() => done(0), 200);
  });
  ws.addEventListener("open", () => {
    fetch(base + "/api/v2/sessions/" + sessionId + "/runtime/messages", {
      method: "POST",
      headers: {"content-type": "application/json", "authorization": "Bearer " + process.env.TOKEN},
      body: JSON.stringify({content: "delta from the ws test"}),
    }).then((response) => response.json()).then((body) => console.log("post ok=" + body.ok));
  });
  ws.addEventListener("error", () => done(1));
  setTimeout(() => done(seen >= 2 ? 0 : 2), 15000);
' "$BASE_URL" "$SESSION_ID" "$TICKET"

say "done"
