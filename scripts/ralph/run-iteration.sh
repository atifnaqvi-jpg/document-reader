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
#
# Events come back as {"items": [{kind, source, llm_message: {role, content: [{type, text}]}}]}
# — not {"events": [{kind, content}]}. `limit` is capped server-side (2000 silently yields zero
# items) and `page_id` is only a valid cursor together with sort_order=TIMESTAMP_DESC, so walk
# backwards from the newest event and stop at the first verdict instead of paging the whole,
# multi-megabyte history forwards. The key travels via the environment so it stays out of `ps`.
VERDICT="$(RALPH_SERVER="$SERVER" RALPH_CID="$CID" RALPH_API_KEY="$KEY" python3 - <<'PY'
import json, os, sys, urllib.request

server, cid, key = (os.environ[k] for k in ("RALPH_SERVER", "RALPH_CID", "RALPH_API_KEY"))


def page(page_id=None):
    url = f"{server}/api/conversations/{cid}/events/search?limit=100&sort_order=TIMESTAMP_DESC"
    if page_id:
        url += f"&page_id={page_id}"
    request = urllib.request.Request(url, headers={"X-Session-API-Key": key})
    with urllib.request.urlopen(request) as response:
        return json.load(response)


cursor = None
for _ in range(100):  # 10k events is far past any real iteration
    events = page(cursor)
    for event in events.get("items", []):
        if event.get("kind") != "MessageEvent" or event.get("source") != "agent":
            continue
        message = event.get("llm_message") or {}
        if message.get("role") != "assistant":
            continue
        text = "\n".join(
            part.get("text", "")
            for part in (message.get("content") or [])
            if isinstance(part, dict) and part.get("type") == "text"
        )
        for line in text.splitlines():
            if line.strip().startswith("ITERATION "):
                print(line.strip())
                sys.exit(0)
    cursor = events.get("next_page_id")
    if not cursor:
        break
sys.exit(1)
PY
)"

if [ -n "$VERDICT" ]; then
  printf '[ralph] %s\n' "$VERDICT"
else
  printf '[ralph] the iteration produced no ITERATION verdict line\n' >&2
fi

printf '[ralph] iteration ended with status %s\n' "$STATUS"
[ "$STATUS" = "finished" ] && [ -n "$VERDICT" ]
