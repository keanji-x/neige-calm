#!/bin/bash
# A fake `claude -p` for the #1791 session tests (tests/cases/claude_planner_session.rs). The test
# copies it into a private directory next to a `scenario` file; the spawn's environment is an
# allowlist, so everything the fake needs or records lives in that directory:
#   in:  scenario, version (optional; default 2.1.280), result_line (optional),
#        auth (optional; the `auth status --json` answer, default logged-in)
#   out: spawns, pid, argv, env, stdin, instructions, orphan, emitted, mcp_reply,
#        auth-pid, auth-env, auth-calls
# Needs on PATH: bash, jq, setsid, sleep, seq, touch, yes; `flood` (F_SETPIPE_SZ) and `mcp` (a unix
# socket client) also need python3.
D=$(cd "$(dirname "$0")" && pwd)
SCENARIO=$(cat "$D/scenario")

if [ "$1" = "auth" ]; then
  # #1817: `auth status --json`. The real CLI prints more fields (auth method, the account's
  # email); neige must read `loggedIn` only, so the logged-in answer carries an email to leak.
  [ "$2 $3" = "status --json" ] || { echo "fake claude: unexpected auth argv: $*" >&2; exit 2; }
  echo "$$" > "$D/auth-pid"
  echo "$$" >> "$D/auth-calls"
  env > "$D/auth-env"
  case "$(cat "$D/auth" 2>/dev/null || echo logged-in)" in
    logged-in) echo '{"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty", "email": "owner@example.invalid", "orgName": "fake org"}' ;;
    # The parse does not depend on the exit status; this answer also exits 1.
    logged-out) echo '{"loggedIn": false, "authMethod": "none", "apiProvider": "firstParty"}'; exit 1 ;;
    no-field) echo '{"authMethod": "none", "apiProvider": "firstParty"}' ;;
    not-bool) echo '{"loggedIn": "true"}' ;;
    not-json) echo 'Logged in as owner@example.invalid' ;;
    # Answers only once the test creates `release-auth`, so concurrent readers overlap.
    hold) touch "$D/auth-entered"
      while [ ! -e "$D/release-auth" ]; do sleep 0.05; done
      echo '{"loggedIn": true}' ;;
    hang) exec sleep 300 ;;
    flood) exec yes '{"loggedIn": true}' ;;
    *) echo "fake claude: unknown auth answer" >&2; exit 2 ;;
  esac
  exit 0
fi

if [ "$1" = "--version" ]; then
  # `hold-version` answers only once the test creates `release-version`.
  if [ "$SCENARIO" = "hold-version" ]; then
    touch "$D/version-entered"
    while [ ! -e "$D/release-version" ]; do sleep 0.05; done
  fi
  echo "$(cat "$D/version" 2>/dev/null || echo 2.1.280) (Claude Code)"
  if [ "$SCENARIO" = "vanish-after-version" ]; then rm -f -- "$0"; fi
  exit 0
fi

echo "$$" >> "$D/spawns"
echo "$$" > "$D/pid"
printf '%s\n' "$@" > "$D/argv"
env > "$D/env"
SID=""
PF=""
while [ $# -gt 0 ]; do
  case "$1" in
    --session-id|--resume) SID="$2"; shift ;;
    --append-system-prompt-file) PF="$2"; shift ;;
  esac
  shift
done
if [ -n "$PF" ]; then cat "$PF" > "$D/instructions"; fi

case "$SCENARIO" in
  stall) exec sleep 300 ;;
  immediate-exit) exit 1 ;;
esac

IFS= read -r LINE || exit 3
printf '%s\n' "$LINE" >> "$D/stdin"

if [ "$SCENARIO" = "undecodable" ]; then
  echo "this is not a stream-json line"
  exec sleep 300
fi
if [ "$SCENARIO" = "invalid-utf8" ]; then
  printf '\xff\xfe not utf-8\n'
  exec sleep 300
fi

INIT='{"type":"system","subtype":"init","session_id":"%s","claude_code_version":"2.1.280",'
INIT+='"model":"claude-haiku-4-5","capabilities":["interrupt_receipt_v1"],'
SKILLS='[]'
# A skill fails the init check after the CLI has already named (and created) the session.
if [ "$SCENARIO" = "bad-init" ]; then SKILLS='["dataviz"]'; fi
INIT+='"mcp_servers":[{"name":"calm","status":"connected"}],"skills":'"$SKILLS"','
INIT+='"plugins":[{"name":"telemetry","source":"telemetry@builtin"}]}\n'
# `slow-bind` names the session a second late and then works for seven more seconds, so a test
# can hold the database across the bind of `agent_session_id` and release it before the result.
if [ "$SCENARIO" = "slow-bind" ]; then sleep 1; fi
# shellcheck disable=SC2059
printf "$INIT" "$SID"
jq -c '. + {isReplay: true}' <<< "$LINE"
if [ "$SCENARIO" = "slow-bind" ]; then sleep 7; SCENARIO=exit; fi

USAGE='"usage":{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,'
USAGE+='"output_tokens":5,"iterations":[{"input_tokens":10,"cache_creation_input_tokens":0,'
USAGE+='"cache_read_input_tokens":0,"output_tokens":5}]},"modelUsage":{"claude-haiku-4-5":{"contextWindow":200000}}'
TEXT='{"type":"assistant","uuid":"0b6d1d4e-6f5a-4c2e-9d8e-2a51f3c7b001",'
TEXT+='"message":{"content":[{"type":"text","text":"done"}]}}'
SUCCESS='{"type":"result","subtype":"success","is_error":false,"result":"done",'
SUCCESS+="$USAGE"',"terminal_reason":"completed"}'
ABORTED='{"type":"result","subtype":"error_during_execution","errors":[],"terminal_reason":"aborted_streaming"}'

case "$SCENARIO" in
  exit)
    echo "$TEXT"; echo "$SUCCESS"
    cat > /dev/null
    exit 0 ;;
  mcp)
    # One real MCP handshake with the spawn's own credential against the kernel's socket (the
    # reply lands in `mcp_reply`), then the calm tool call and its result as Claude reports them.
    python3 - "$D/mcp_reply" <<'PY' || { echo "mcp: handshake script failed" >&2; exit 6; }
import json, os, socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(os.environ["NEIGE_MCP_SOCKET"])
f = s.makefile("rw")
f.write(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
    "protocolVersion": "2024-11-05", "capabilities": {},
    "clientInfo": {"name": "fake-claude", "version": "0"},
    "_meta": {"dev.neige/auth": {"token": os.environ["NEIGE_MCP_TOKEN"]}}}}) + "\n")
f.flush()
open(sys.argv[1], "w").write(f.readline())
PY
    CALL='{"type":"assistant","uuid":"0b6d1d4e-6f5a-4c2e-9d8e-2a51f3c7b003","message":{"content":'
    CALL+='[{"type":"tool_use","id":"toolu_mcp","name":"mcp__calm__calm_report_write","input":{"text":"x"}}]}}'
    RESULT='{"type":"user","uuid":"0b6d1d4e-6f5a-4c2e-9d8e-2a51f3c7b004","message":{"role":"user","content":'
    RESULT+='[{"tool_use_id":"toolu_mcp","type":"tool_result","content":[{"type":"text","text":"ok"}]}]},'
    RESULT+='"tool_use_result":[{"type":"text","text":"ok"}]}'
    echo "$CALL"; echo "$RESULT"; echo "$TEXT"; echo "$SUCCESS"
    cat > /dev/null
    exit 0 ;;
  hold-orphan)
    # `hold`, with a detached marked child that outlives this process (a Bash tool's own session).
    setsid sleep 300 < /dev/null > /dev/null 2>&1 &
    echo "$!" > "$D/orphan"
    TOOL='{"type":"assistant","uuid":"0b6d1d4e-6f5a-4c2e-9d8e-2a51f3c7b002","message":{"content":'
    TOOL+='[{"type":"tool_use","id":"toolu_hold","name":"Bash","input":{"command":"sleep 20"}}]}}'
    echo "$TOOL"
    IFS= read -r CONTROL || exit 4
    printf '%s\n' "$CONTROL" >> "$D/stdin"
    echo "$ABORTED"
    cat > /dev/null
    exit 0 ;;
  exit-with-orphan)
    setsid sleep 300 < /dev/null > /dev/null 2>&1 &
    echo "$!" > "$D/orphan"
    echo "$TEXT"; echo "$SUCCESS"
    cat > /dev/null
    exit 0 ;;
  linger)
    echo "$TEXT"; echo "$SUCCESS"
    exec sleep 300 ;;
  hold)
    # Start a Bash tool call and work until the next stdin line (an interrupt), then end as the
    # CLI does after one.
    TOOL='{"type":"assistant","uuid":"0b6d1d4e-6f5a-4c2e-9d8e-2a51f3c7b002","message":{"content":'
    TOOL+='[{"type":"tool_use","id":"toolu_hold","name":"Bash","input":{"command":"sleep 20"}}]}}'
    echo "$TOOL"
    IFS= read -r CONTROL || exit 4
    printf '%s\n' "$CONTROL" >> "$D/stdin"
    echo "$ABORTED"
    cat > /dev/null
    exit 0 ;;
  ignore-interrupt)
    # Take the interrupt and keep working: only the stop timer ends this turn.
    IFS= read -r CONTROL || exit 4
    printf '%s\n' "$CONTROL" >> "$D/stdin"
    exec sleep 300 ;;
  flood|flood-no-result)
    # Shrink this end's stdin pipe to one page, then send more control requests than that page
    # holds answers for, then the result; the CLI never reads stdin again, so the session is stuck
    # writing an answer while the result already waits on stdout (all output stays under the
    # 8 KiB a pipe gets even when the user's pipe budget is exhausted).
    python3 -c 'import fcntl, sys; fcntl.fcntl(0, 1031, 4096); sys.exit(fcntl.fcntl(0, 1032) != 4096)' || {
      echo "flood: this host cannot give stdin a one-page (4096-byte) pipe" >&2
      exit 5
    }
    for i in $(seq 1 40); do
      echo '{"type":"control_request","request_id":"r-'"$i"'","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{}}}'
    done
    if [ "$SCENARIO" = flood ]; then echo "$SUCCESS"; fi
    touch "$D/emitted"
    exec sleep 300 ;;
  bad-init)
    exec sleep 300 ;;
  chatty-after-interrupt)
    # Ignore the interrupt and keep stdout full of ignored records (`yes` outruns any reader).
    IFS= read -r CONTROL || exit 4
    printf '%s\n' "$CONTROL" >> "$D/stdin"
    exec yes '{"type":"system","subtype":"status","status":"requesting"}' ;;
  stubborn-after-interrupt)
    # Ignore the interrupt AND SIGTERM (the disposition survives exec), and keep stdout full.
    IFS= read -r CONTROL || exit 4
    printf '%s\n' "$CONTROL" >> "$D/stdin"
    trap '' TERM
    exec yes '{"type":"system","subtype":"status","status":"requesting"}' ;;
  interrupt-result-line)
    IFS= read -r CONTROL || exit 4
    printf '%s\n' "$CONTROL" >> "$D/stdin"
    cat "$D/result_line"
    cat > /dev/null
    exit 1 ;;
esac
echo "unknown scenario $SCENARIO" >&2
exit 2
