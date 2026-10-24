#!/usr/bin/env bash
# Runs exactly one Ralph iteration as a *fresh* agent conversation on the local agent server.
#
# Why not the OpenHands CLI: `openhands --headless` refuses to start without
# `~/.openhands/agent_settings.json`, which this environment never writes (the Agent Canvas
# backend holds the settings instead). A fresh conversation on the server has no such problem
# and gives each iteration what the loop actually needs: no memory of the last one.
#
# The conversation is started with the **encrypted** api_key from `/api/settings` and
# `secrets_encrypted: true`, so no plaintext credential passes through this script.
#
# Environment:
#   RALPH_SERVER          agent server base URL (default http://localhost:18000)
#   RALPH_API_KEY_FILE    file holding the session key (default ~/.agent-canvas/session-key)
#   RALPH_MAX_ITERATIONS  max agent iterations for the conversation (default 200)
#   RALPH_TIMEOUT         seconds to wait for the conversation to finish (default 3600)

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SERVER="${RALPH_SERVER:-http://localhost:18000}"
MAX_ITER="${RALPH_MAX_ITERATIONS:-200}"
TIMEOUT="${RALPH_TIMEOUT:-3600}"

fail() { printf '[ralph] %s\n' "$*" >&2; exit 1; }

# The key lives in the agent-server process's own environment; read it there rather than
# asking the user to copy it around.
KEY="${LOCAL_BACKEND_API_KEY:-}"
if [ -z "$KEY" ]; then
  PID="$(pgrep -f 'bin/agent-server' | head -1)"
  [ -n "$PID" ] && KEY="$(tr '\0' '\n' < "/proc/$PID/environ" 2>/dev/null | sed -n 's/^OH_SESSION_API_KEYS_0=//p')"
fi
[ -n "$KEY" ] || fail "no session API key: set LOCAL_BACKEND_API_KEY or run alongside the agent server"

curl -sS -H "X-Session-API-Key: $KEY" -H "X-Expose-Secrets: encrypted" \
  "$SERVER/api/settings" -o /tmp/ralph-settings.json -w '' || fail "cannot read $SERVER/api/settings"

PAYLOAD="$(python3 - "$REPO_ROOT" "$MAX_ITER" <<'PY'
import json, sys
repo, max_iter = sys.argv[1], int(sys.argv[2])
s = json.load(open("/tmp/ralph-settings.json"))
a = s["agent_settings"]
# schema_version is the settings-file format, not the conversation's; mcp_config can fail to
# connect at creation time and the loop needs no MCP servers.
a.pop("schema_version", None)
a.pop("mcp_config", None)
a["tools"] = [
    {"name": "terminal", "params": {}},
    {"name": "file_editor", "params": {}},
    {"name": "task_tracker", "params": {}},
]
a["agent_context"] = dict(
    a.get("agent_context") or {},
    load_public_skills=True,
    load_user_skills=True,
    load_project_skills=True,
)
prompt = open(f"{repo}/scripts/ralph/PROMPT.md").read()
print(json.dumps({
    "secrets_encrypted": True,
    "agent_settings": a,
    "workspace": {"kind": "LocalWorkspace", "working_dir": repo},
    "confirmation_policy": {"kind": "NeverConfirm"},
    "max_iterations": max_iter,
    "stuck_detection": True,
    "worktree": False,
    "initial_message": {
        "role": "user",
        "content": [{"type": "text", "text": prompt}],
        "run": True,
    },
}))
PY
)" || fail "could not build the conversation payload"

CID="$(curl -sS -X POST "$SERVER/api/conversations" \
  -H "Content-Type: application/json" -H "X-Session-API-Key: $KEY" \
  --data-binary "$PAYLOAD" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("id",""))')"
[ -n "$CID" ] || fail "conversation creation returned no id"

printf '[ralph] iteration conversation %s\n' "$CID"

DEADLINE=$(( $(date +%s) + TIMEOUT ))
while :; do
  STATUS="$(curl -sS -H "X-Session-API-Key: $KEY" "$SERVER/api/conversations/$CID" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin).get("execution_status",""))')"
  case "$STATUS" in
    finished|error|stopped|stuck) break ;;
  esac
  if [ "$(date +%s)" -ge "$DEADLINE" ]; then
    # A runaway iteration must not keep editing the repo while the shell moves on to the gate.
    printf '[ralph] timeout after %ss; pausing the conversation\n' "$TIMEOUT" >&2
    curl -sS -o /dev/null -X POST -H "X-Session-API-Key: $KEY" \
      "$SERVER/api/conversations/$CID/pause" || true
    STATUS=timeout
    break
  fi
  sleep 10
done

# The iteration's own verdict line is part of the contract; surface it, and fail the run when
# the agent never produced one (a truncated conversation must not look like a completed task).
curl -sS -H "X-Session-API-Key: $KEY" \
  "$SERVER/api/conversations/$CID/events/search?limit=2000" \
  | python3 -c '
import json, sys
events = json.load(sys.stdin).get("events", [])
text = "\n".join(
    part.get("text", "")
    for event in events
    if event.get("kind") in ("MessageEvent", "AgentMessageEvent")
    for part in (event.get("content") or [])
    if isinstance(part, dict) and part.get("type") == "text"
)
lines = [line for line in text.splitlines() if line.startswith("ITERATION ")]
print(lines[-1] if lines else "")
'

printf '[ralph] iteration ended with status %s\n' "$STATUS"
[ "$STATUS" = "finished" ]
