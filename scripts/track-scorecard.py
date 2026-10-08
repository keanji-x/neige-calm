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
  runs          gate runs the workers asked for (task-gate-run operations, #2464)
  reused        task.gate_result verdicts that reused a passing worker run (evidence `reused`)
  publish       forge.pr.opened / forge.pr.published events (one per published head)
  ci_red        distinct heads whose forge.pr.checks concluded failure; --detail and --json also
                name each failed check per head (events before #2170 did not record names)
  interventions the user's messages to the Planner after the kickoff message, plus answered
                questions (ask.answered) and historical ratify decisions
  asks          questions the Planner asked (ask.requested), and how many the user answered
  ratify        historical ratify requests before #2209 (and how many were denied)
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
import sys
import time

sys.dont_write_bytecode = True  # importing track_db must not leave scripts/__pycache__ behind
from track_db import GIT_REMOTE_WRITE, bypass_hits, open_db, planner_card, planner_turns, worker_commands, write_segments  # noqa: E402


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
    """Heads whose checks concluded failure, each with its failed check names (None when the
    events did not record them). Older events carry no head, so they take the head of the last
    publish before them."""
    head = None
    red = {}
    for kind, payload, _, _ in events(db, track_id, ["forge.pr.opened", "forge.pr.published", "forge.pr.checks"], start, end):
        data = json.loads(payload)
        if kind in ("forge.pr.opened", "forge.pr.published"):
            head = data["head_sha"]
        elif data["conclusion"] == "failure":
            failed = (data.get("snapshot") or {}).get("head_sha") or head or "unknown"
            names = red.setdefault(failed, None)
            if "failed_checks" in data:
                names = names or []
                for check in data["failed_checks"]:
                    if check["name"] not in names:
                        names.append(check["name"])
                red[failed] = names
    return red


def scorecard(db, track_id):
    title, start, end, end_reason = window(db, track_id)
    planner = planner_card(db, track_id)
    if planner is None:
        sys.exit(f"error: track {track_id} has no Planner card")
    # A Planner with no transcript at all has no round count, not zero rounds.
    turns = planner_turns(db, planner)
    rounds = None if not turns else sum(1 for _, started, completed, _ in turns if start <= started <= end and completed is not None)
    tasks = db.execute(
        "SELECT kind, status, key FROM tasks WHERE track_id = ? AND created_at_ms BETWEEN ? AND ?",
        (track_id, start, end),
    ).fetchall()
    by_kind = {}
    for kind, _, _ in tasks:
        by_kind[kind] = by_kind.get(kind, 0) + 1

    gate_results = [json.loads(p) for _, p, _, _ in events(db, track_id, ["task.gate_result"], start, end)]
    gate_red = sum(1 for result in gate_results if not result["passed"])
    reused = sum(
        1 for result in gate_results if ((result.get("target") or {}).get("evidence") or {}).get("kind") == "reused"
    )
    runs = db.execute(
        "SELECT count(*) FROM operations WHERE kind = 'task-gate-run'"
        " AND json_extract(payload_json, '$.track_id') = ? AND created_at_ms BETWEEN ? AND ?",
        (track_id, start, end),
    ).fetchone()[0]
    publish = len(events(db, track_id, ["forge.pr.opened", "forge.pr.published"], start, end))
    # The Planner's first message is the kickoff, whoever sent it. After that only the user's own
    # messages count: an AI-sent message is not an intervention, and side conversations with an
    # assistant card are not steering the Planner.
    messages = db.execute(
        "SELECT at, actor FROM events WHERE scope_track = ? AND scope_card = ?"
        " AND kind = 'harness.user_message.enqueued' ORDER BY id",
        (track_id, planner),
    ).fetchall()
    followups = sum(1 for at, actor in messages[1:] if start <= at <= end and json.loads(actor)["kind"] == "User")
    asks = len(events(db, track_id, ["ask.requested"], start, end))
    answered = len(events(db, track_id, ["ask.answered"], start, end))
    ratify_requested = len(events(db, track_id, ["ratify.requested"], start, end))
    resolved = [json.loads(p)["decision"] for _, p, _, _ in events(db, track_id, ["ratify.resolved"], start, end)]
    rejected = sum(
        1
        for _, _, actor, _ in events(db, track_id, ["task.failed"], start, end)
        if json.loads(actor)["kind"] in ("AiPlanner", "AiPlannerSession")
    )
    red_heads = ci_red_heads(db, track_id, start, end)
    hits = bypass_hits(db, track_id, planner, start, end)
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
        "runs": runs,
        "reused": reused,
        "publish": publish,
        "ci_red": len(red_heads),
        "ci_red_heads": [{"head": h, "failed_checks": names} for h, names in red_heads.items()],
        "interventions": followups + answered + len(resolved),
        "user_messages": followups,
        "asks": asks,
        "asks_answered": answered,
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
    ("runs", lambda c: c["runs"]),
    ("reused", lambda c: c["reused"]),
    ("publish", lambda c: c["publish"]),
    ("ci_red", lambda c: c["ci_red"]),
    ("interventions", lambda c: c["interventions"]),
    ("asks", lambda c: "{} ({} answered)".format(c["asks"], c["asks_answered"])),
    ("ratify", lambda c: "{} ({} denied)".format(c["ratify_requested"], c["ratify_denied"])),
    ("bypass", lambda c: c["bypass"]),
]


def print_table(cards):
    rows = [[name for name, _ in COLUMNS]] + [[str(get(c)) for _, get in COLUMNS] for c in cards]
    widths = [max(len(r[i]) for r in rows) for i in range(len(COLUMNS))]
    for row in rows:
        print("  ".join(cell.ljust(w) for cell, w in zip(row, widths)).rstrip())


def red_head_text(red):
    names = red["failed_checks"]
    return red["head"][:9] + (" (checks not recorded)" if names is None else " (" + ", ".join(names) + ")")


def print_detail(cards):
    for c in cards:
        print(f"\n== {c['track']} {c['title']}")
        print("terminal tasks: " + (", ".join(c["terminal_tasks"]) or "-"))
        print("ci red heads:   " + (", ".join(red_head_text(r) for r in c["ci_red_heads"]) or "-"))
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

    db = open_db(args.db)
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
