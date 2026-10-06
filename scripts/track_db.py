"""Read-only access to a calm database for the Track scripts (#2170 A, #2206 T2).

track-scorecard.py and track-trace.py import this module, so they open the database, check its
schema, find a Planner's turns and classify bypass commands the same way.
"""

import json
import re
import sqlite3
import sys

# Every column either script reads. A schema change fails here, by name, not as a wrong number.
REQUIRED_COLUMNS = {
    "tracks": ["id", "area_id", "title", "created_at", "closed_at", "creator_track_id"],
    "cards": ["id", "track_id", "role"],
    "events": ["id", "kind", "payload", "actor", "at", "scope_track", "scope_card"],
    "tasks": ["track_id", "key", "kind", "goal", "status", "created_at_ms"],
    "harness_items": ["id", "card_id", "turn_id", "item_uuid", "item_type", "method", "params", "created_at_ms", "input_segments"],
    "worker_flow_items": ["track_id", "kind", "payload", "created_at_ms"],
}

GIT_OPTIONS = r"\bgit(?:\s+-[Cc]\s+\S+|\s+--(?:git-dir|work-tree|namespace)[=\s]\S+|\s+--[\w-]+(?:=\S+)?)*\s+"
GIT_WRITE = re.compile(
    GIT_OPTIONS + r"(push|commit|commit-tree|update-ref|merge|rebase|cherry-pick|revert|am)\b(?!-)"
)
GIT_REMOTE_WRITE = re.compile(GIT_OPTIONS + r"push\b")
GH_REPO = r"(?:(?:-R|--repo)[=\s]\S+\s+)?"
GH_WRITE = re.compile(
    r"\bgh\s+" + GH_REPO
    + r"(pr\s+(?:merge|create|edit|close|ready|comment|review)|issue\s+(?:create|edit|close|comment))\b"
)
GH_API = re.compile(r"\bgh\s+api\b")
GH_API_METHOD = re.compile(r"(?:-X|--method)[\s=]*([A-Za-z]+)")
GH_API_BODY = re.compile(r"(?:^|\s)(?:-f|-F|--field|--raw-field|--input)(?:\s|=|$)")
# Codex workers run shell through `tools.exec_command({cmd:"…"})` inside a JS snippet.
WORKER_CMD = re.compile(r"""cmd:\s*("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|`(?:[^`\\]|\\.)*`)""")
SEGMENT_SPLIT = re.compile(r"\|\||&&|[|;\n]")
LINE_CONTINUATION = re.compile(r"\\\n")


def gh_api_writes(segment, text):
    if not GH_API.search(segment):
        return False
    if re.search(r"\bgh\s+api\s+graphql\b", segment):
        # Every GraphQL call is a POST; only a mutation writes. The query may span lines.
        return "mutation" in text
    method = GH_API_METHOD.search(segment)
    if method:
        return method.group(1).upper() != "GET"
    # gh api defaults to POST once a field or body is given.
    return bool(GH_API_BODY.search(segment))


def write_segments(text, git_write=GIT_WRITE):
    """Return the shell segments of `text` that write git history (per `git_write`) or GitHub."""
    hits = []
    text = LINE_CONTINUATION.sub(" ", text)
    for segment in SEGMENT_SPLIT.split(text):
        segment = segment.strip()
        if git_write.search(segment) or GH_WRITE.search(segment) or gh_api_writes(segment, text):
            hits.append(segment)
    return hits


def worker_commands(exec_input):
    """The shell commands inside one Codex worker `exec` snippet."""
    return [
        json.loads(literal) if literal[0] == '"' else literal[1:-1] for literal in WORKER_CMD.findall(exec_input)
    ]


def check_schema(db):
    missing = []
    for table, columns in REQUIRED_COLUMNS.items():
        have = {row[1] for row in db.execute(f"PRAGMA table_info({table})")}
        missing += [f"{table}.{c}" for c in columns if c not in have]
    if missing:
        sys.exit("error: the database schema lacks " + ", ".join(missing) + "; update this script")


def open_db(path):
    """Open `path` read-only and check it has every column the scripts read."""
    db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    check_schema(db)
    return db


def planner_card(db, track_id):
    """The Track's Planner card id, or None."""
    row = db.execute("SELECT id FROM cards WHERE track_id = ? AND role = 'planner'", (track_id,)).fetchone()
    return None if row is None else row[0]


def planner_turns(db, card_id):
    """Every turn in the Planner's transcript, oldest first, as (turn_id, started_at, completed_at,
    turn/completed params or None). A turn starts with its first transcript item; older
    turn/completed rows carry no startedAt. completed_at is None while no turn/completed row exists."""
    rows = db.execute(
        "SELECT turn_id, min(created_at_ms) AS started,"
        " max(CASE WHEN method = 'turn/completed' THEN created_at_ms END),"
        " max(CASE WHEN method = 'turn/completed' THEN params END)"
        " FROM harness_items WHERE card_id = ? AND turn_id IS NOT NULL GROUP BY turn_id ORDER BY started, turn_id",
        (card_id,),
    ).fetchall()
    return [(turn, started, completed, None if params is None else json.loads(params)) for turn, started, completed, params in rows]


def bypass_hits(db, track_id, planner_card_id, start, end):
    """(source, command) for every bypass command timestamped in [start, end]."""
    hits = []

    def add(source, text, git_write=GIT_WRITE):
        segments = write_segments(text, git_write)
        if segments:
            hits.append((source, " ; ".join(segments)))

    for key, goal in db.execute(
        "SELECT key, goal FROM tasks WHERE track_id = ? AND kind = 'terminal' AND created_at_ms BETWEEN ? AND ?",
        (track_id, start, end),
    ):
        add("terminal task " + key, goal)
    for item_type, params in db.execute(
        "SELECT item_type, params FROM harness_items WHERE card_id = ? AND method = 'item/completed'"
        " AND item_type IN ('commandExecution', 'mcpToolCall') AND created_at_ms BETWEEN ? AND ?",
        (planner_card_id, start, end),
    ):
        item = json.loads(params)["item"]
        if item_type == "commandExecution":
            add("planner shell", item["command"])
        elif item["tool"] in ("neige_terminal_open", "neige_terminal_input"):
            args = item["arguments"] if isinstance(item["arguments"], dict) else json.loads(item["arguments"])
            text = args.get("program") or (args.get("action") or {}).get("text") or ""
            add("planner " + item["tool"], text)
    for (payload,) in db.execute(
        "SELECT payload FROM worker_flow_items WHERE track_id = ? AND kind = 'toolCall'"
        " AND created_at_ms BETWEEN ? AND ?",
        (track_id, start, end),
    ):
        call = json.loads(payload)
        if call.get("name") != "exec":
            continue
        for cmd in worker_commands(call.get("input") or ""):
            add("worker shell", cmd, GIT_REMOTE_WRITE)
    return hits
