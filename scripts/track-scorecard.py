#!/usr/bin/env python3
"""Print a scorecard for finished Planner Tracks, read from a calm database (#2170 A).

The window runs from the Track's creation to its first merged PR. A Track that has not merged
ends at closed_at, and a Track that is still open ends now. Every count covers that window only,
so the post-merge interview does not count as rounds or interventions.

Columns:
  minutes       window length
  rounds        completed Planner turns that started in the window ("-" when the Planner left
                no transcript)
  tasks         task attempts created in the window, by kind (codex/terminal/claude)
  failed        task attempts that ended failed
  rejected      task.failed events the Planner wrote (neige_task_reject verdicts)
  gate_red      task.gate_result events with passed=false
  publish       forge.pr.opened events (one per published head)
  ci_red        distinct heads whose forge.pr.checks concluded failure
  interventions the user's messages to the Planner after the kickoff message, plus ratify
                decisions
  ratify        ratify requests (and how many were denied)
  bypass        commands that write git history or GitHub outside the kernel's delivery path,
                counted once per terminal task, Planner shell command, Planner terminal input
                or worker shell command. A worker owns its checkout, so only its remote writes
                (git push, gh writes) count.

`bypass` matches command text, so a write inside a script the command runs (`bash x.sh`) is
not seen. Worker shells are read from Codex `exec` calls; Claude workers leave no
worker_flow_items rows to read. `--detail` lists every match and every terminal task, so a
reader can check.

Usage: scripts/track-scorecard.py --db PATH [--json | --detail] TRACK_ID...
       scripts/track-scorecard.py --selftest   (checks the bypass classifier; CI runs it)
The database is opened read-only. 4140's is ~/.local/share/neige-next/data/calm.db.
"""

import argparse
import json
import re
import sqlite3
import sys
import time

# Every column the queries read. A schema change fails here, by name, not as a wrong number.
REQUIRED_COLUMNS = {
    "tracks": ["id", "title", "created_at", "closed_at"],
    "cards": ["id", "track_id", "role"],
    "events": ["id", "kind", "payload", "actor", "at", "scope_track", "scope_card"],
    "tasks": ["track_id", "key", "kind", "goal", "status", "created_at_ms"],
    "harness_items": ["card_id", "item_type", "method", "params", "created_at_ms"],
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


def window(db, track_id):
    row = db.execute("SELECT title, created_at, closed_at FROM tracks WHERE id = ?", (track_id,)).fetchone()
    if row is None:
        sys.exit(f"error: no track {track_id}")
    title, start, closed_at = row
    merged = db.execute(
        "SELECT min(at) FROM events WHERE scope_track = ? AND kind = 'forge.pr.merged'", (track_id,)
    ).fetchone()[0]
    if merged is not None:
        return title, start, merged, "merged"
    if closed_at is not None:
        return title, start, closed_at, "closed"
    return title, start, int(time.time() * 1000), "open"


def events(db, track_id, kinds, start, end):
    marks = ",".join("?" * len(kinds))
    return db.execute(
        f"SELECT kind, payload, actor, at FROM events WHERE scope_track = ? AND kind IN ({marks})"
        " AND at BETWEEN ? AND ? ORDER BY id",
        (track_id, *kinds, start, end),
    ).fetchall()


def ci_red_heads(db, track_id, start, end):
    """Heads whose checks concluded failure. Older events carry no head, so they take the head
    of the last publish before them."""
    head = None
    red = []
    for kind, payload, _, _ in events(db, track_id, ["forge.pr.opened", "forge.pr.checks"], start, end):
        data = json.loads(payload)
        if kind == "forge.pr.opened":
            head = data["head_sha"]
        elif data["conclusion"] == "failure":
            failed = (data.get("snapshot") or {}).get("head_sha") or head or "unknown"
            if failed not in red:
                red.append(failed)
    return red


def bypass_hits(db, track_id, planner_card, start, end):
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
        (planner_card, start, end),
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


def scorecard(db, track_id):
    title, start, end, end_reason = window(db, track_id)
    planner = db.execute("SELECT id FROM cards WHERE track_id = ? AND role = 'planner'", (track_id,)).fetchone()
    if planner is None:
        sys.exit(f"error: track {track_id} has no Planner card")
    planner_card = planner[0]
    # A turn starts with its first transcript item; older turn/completed rows carry no startedAt.
    # A Planner with no transcript at all has no round count, not zero rounds.
    rounds = db.execute(
        "SELECT CASE WHEN count(*) = 0 THEN NULL ELSE sum(started BETWEEN ? AND ? AND completed) END FROM ("
        " SELECT min(created_at_ms) AS started, max(method = 'turn/completed') AS completed"
        " FROM harness_items WHERE card_id = ? AND turn_id IS NOT NULL GROUP BY turn_id)",
        (start, end, planner_card),
    ).fetchone()[0]
    tasks = db.execute(
        "SELECT kind, status, key FROM tasks WHERE track_id = ? AND created_at_ms BETWEEN ? AND ?",
        (track_id, start, end),
    ).fetchall()
    by_kind = {}
    for kind, _, _ in tasks:
        by_kind[kind] = by_kind.get(kind, 0) + 1

    gate_red = sum(
        1 for _, p, _, _ in events(db, track_id, ["task.gate_result"], start, end) if not json.loads(p)["passed"]
    )
    publish = len(events(db, track_id, ["forge.pr.opened"], start, end))
    # The Planner's first message is the kickoff, whoever sent it. After that only the user's own
    # messages count: an AI-sent message is not an intervention, and side conversations with an
    # assistant card are not steering the Planner.
    messages = db.execute(
        "SELECT at, actor FROM events WHERE scope_track = ? AND scope_card = ?"
        " AND kind = 'harness.user_message.enqueued' ORDER BY id",
        (track_id, planner_card),
    ).fetchall()
    followups = sum(1 for at, actor in messages[1:] if start <= at <= end and json.loads(actor)["kind"] == "User")
    ratify_requested = len(events(db, track_id, ["ratify.requested"], start, end))
    resolved = [json.loads(p)["decision"] for _, p, _, _ in events(db, track_id, ["ratify.resolved"], start, end)]
    rejected = sum(
        1
        for _, _, actor, _ in events(db, track_id, ["task.failed"], start, end)
        if json.loads(actor)["kind"] in ("AiPlanner", "AiPlannerSession")
    )
    red_heads = ci_red_heads(db, track_id, start, end)
    hits = bypass_hits(db, track_id, planner_card, start, end)
    return {
        "track": track_id,
        "title": title,
        "end": end_reason,
        "minutes": round((end - start) / 60000),
        "rounds": rounds,
        "tasks": len(tasks),
        "tasks_by_kind": by_kind,
        "terminal_tasks": [key for kind, _, key in tasks if kind == "terminal"],
        "failed": sum(1 for _, status, _ in tasks if status == "failed"),
        "rejected": rejected,
        "gate_red": gate_red,
        "publish": publish,
        "ci_red": len(red_heads),
        "ci_red_heads": red_heads,
        "interventions": followups + len(resolved),
        "user_messages": followups,
        "ratify_requested": ratify_requested,
        "ratify_denied": sum(1 for d in resolved if d != "grant"),
        "bypass": len(hits),
        "bypass_hits": [{"source": s, "command": c} for s, c in hits],
    }


COLUMNS = [
    ("track", lambda c: c["track"][:8]),
    ("title", lambda c: c["title"][:24]),
    ("end", lambda c: c["end"]),
    ("minutes", lambda c: c["minutes"]),
    ("rounds", lambda c: "-" if c["rounds"] is None else c["rounds"]),
    ("tasks", lambda c: "{} ({})".format(c["tasks"], " ".join(f"{k}:{v}" for k, v in sorted(c["tasks_by_kind"].items())))),
    ("failed", lambda c: c["failed"]),
    ("rejected", lambda c: c["rejected"]),
    ("gate_red", lambda c: c["gate_red"]),
    ("publish", lambda c: c["publish"]),
    ("ci_red", lambda c: c["ci_red"]),
    ("interventions", lambda c: c["interventions"]),
    ("ratify", lambda c: "{} ({} denied)".format(c["ratify_requested"], c["ratify_denied"])),
    ("bypass", lambda c: c["bypass"]),
]


def print_table(cards):
    rows = [[name for name, _ in COLUMNS]] + [[str(get(c)) for _, get in COLUMNS] for c in cards]
    widths = [max(len(r[i]) for r in rows) for i in range(len(COLUMNS))]
    for row in rows:
        print("  ".join(cell.ljust(w) for cell, w in zip(row, widths)).rstrip())


def print_detail(cards):
    for c in cards:
        print(f"\n== {c['track']} {c['title']}")
        print("terminal tasks: " + (", ".join(c["terminal_tasks"]) or "-"))
        print("ci red heads:   " + (", ".join(h[:9] for h in c["ci_red_heads"]) or "-"))
        for hit in c["bypass_hits"]:
            print(f"bypass [{hit['source']}] {hit['command'][:160]}")


# (command, counted as a Planner or terminal write, counted as a worker write)
SELFTEST_CASES = [
    ("git push origin HEAD:refs/heads/x", True, True),
    ("git -C /tmp/wt commit -m 'Merge main'", True, False),
    ('git commit-tree "$tree" -p 162aff1d', True, False),
    ("git update-ref refs/heads/x abc", True, False),
    ("git rebase origin/main", True, False),
    ("gh pr merge 12 --squash", True, True),
    ("gh pr edit 2042 --body-file body.md", True, True),
    ("gh issue close 1868", True, True),
    ("gh api repos/o/r/pulls/1 --method PATCH --input -", True, True),
    ("gh api -X POST repos/o/r/issues/1/comments", True, True),
    ("gh api repos/o/r/issues/1/comments -f body=done", True, True),
    ("gh api graphql -f query='mutation { closeIssue }'", True, True),
    ("git merge --no-commit --no-ff abc", True, False),
    ("git merge-base HEAD origin/main", False, False),
    ("git log --merges --oneline", False, False),
    ("git status --porcelain", False, False),
    ("gh pr view 1 --json state", False, False),
    ("gh api repos/o/r/actions/runs --method GET -f head_sha=abc", False, False),
    ("gh api repos/o/r/pulls/1 --jq .body", False, False),
    ("gh api graphql -f query='query { repository { name } }'", False, False),
    ("gh api \\\n  repos/o/r/pulls/1 \\\n  --method PATCH --input body.json", True, True),
    ("gh api graphql -f query='\nmutation { closeIssue }'", True, True),
    ("gh --repo o/r pr merge 1 --squash", True, True),
    ("gh pr review 1 --approve", True, True),
    ("git --git-dir /x/.git push origin x", True, True),
    ("git cherry-pick abc", True, False),
]


def selftest():
    failures = []
    for command, planner, worker in SELFTEST_CASES:
        got = (bool(write_segments(command)), bool(write_segments(command, GIT_REMOTE_WRITE)))
        if got != (planner, worker):
            failures.append(f"{command!r}: expected {(planner, worker)}, got {got}")
    piped = write_segments("gh api repos/o/r/pulls/1 --jq .body | jq -Rs '{body: .}' | gh api repos/o/r/pulls/1 --method PATCH --input -")
    if piped != ["gh api repos/o/r/pulls/1 --method PATCH --input -"]:
        failures.append(f"pipeline: expected only the PATCH segment, got {piped}")
    snippet = 'text(await tools.exec_command({cmd:"git push origin x",max_output_tokens:2000}));\n' \
        "text(await tools.exec_command({cmd:'rg -n \\'push\\' docs'}));"
    if worker_commands(snippet) != ["git push origin x", "rg -n \\'push\\' docs"]:
        failures.append(f"worker snippet: got {worker_commands(snippet)}")
    for failure in failures:
        print("FAIL " + failure, file=sys.stderr)
    if failures:
        sys.exit(1)
    print(f"OK: the bypass classifier matches all {len(SELFTEST_CASES) + 2} cases")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--selftest", action="store_true", help="check the bypass classifier and exit")
    parser.add_argument("--db", help="path to calm.db (opened read-only)")
    output = parser.add_mutually_exclusive_group()
    output.add_argument("--json", action="store_true", help="print one JSON object per track")
    output.add_argument("--detail", action="store_true", help="also list terminal tasks, red heads and bypass commands")
    parser.add_argument("tracks", nargs="*", metavar="TRACK_ID")
    args = parser.parse_args()
    if args.selftest:
        selftest()
        return
    if args.db is None or not args.tracks:
        parser.error("--db and at least one TRACK_ID are required")

    db = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    check_schema(db)
    cards = [scorecard(db, t) for t in args.tracks]
    if args.json:
        for c in cards:
            print(json.dumps(c, ensure_ascii=False))
        return
    print_table(cards)
    if args.detail:
        print_detail(cards)


if __name__ == "__main__":
    main()
