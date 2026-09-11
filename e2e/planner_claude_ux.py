#!/usr/bin/env python3
"""Collect a real Planner's Claude TUI round; exit 3 means review is required.

Only normal REST Planner input is written. No terminal calls, model mocks,
shell execution, auth-file access, automatic login or claim of UX acceptance.
"""

import argparse
import base64
import collections
import hashlib
import json
from pathlib import Path
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


class EvidenceError(Exception):
    pass


def scrub(value):
    """Keep private synthetic text; remove credential fields and image bytes."""
    if isinstance(value, dict):
        if value.get("type") == "image" and isinstance(value.get("data"), str):
            try:
                image = base64.b64decode(value["data"], validate=True)
            except ValueError as error:
                raise EvidenceError("malformed image payload") from error
            return {"type": "image", "mimeType": value.get("mimeType"),
                    "bytes": len(image), "sha256": hashlib.sha256(image).hexdigest()}
        return {key: "[REDACTED]" if re.search(
            r"password|authorization|cookie|credential|api.?key|access.?token|refresh.?token|secret",
            key, re.I) else scrub(item) for key, item in value.items()}
    if isinstance(value, list):
        return [scrub(item) for item in value]
    if isinstance(value, str):
        value = re.sub(r"(?i)bearer\s+[^\s\"']+", "Bearer [REDACTED]", value)
        value = re.sub(r"\bsk-[\w-]{12,}", "[REDACTED]", value)
        value = re.sub(r"https?://[^\s\"'<>]*[?][^\s\"'<>]*", "[URL QUERY REDACTED]", value)
    return value


def write_json(path, value):
    path.write_text(json.dumps(scrub(value), ensure_ascii=False, indent=2) + "\n")
    path.chmod(0o600)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise EvidenceError("API redirect refused")


class Api:
    def __init__(self, url, cookie):
        parsed = urllib.parse.urlsplit(url)
        if (parsed.scheme != "http" or parsed.hostname != "127.0.0.1"
                or not parsed.port or parsed.username or parsed.password
                or parsed.path or parsed.query or parsed.fragment):
            raise EvidenceError("use the owned dedicated stack's loopback HTTP origin")
        self.url, self.cookie = url, cookie
        self.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}), NoRedirect())

    def call(self, method, path, body=None):
        headers = {"Content-Type": "application/json", "X-Calm-Actor": "user"}
        if self.cookie:
            headers["Cookie"] = self.cookie
        request = urllib.request.Request(self.url + path, method=method, headers=headers,
                                         data=None if body is None else json.dumps(body).encode())
        try:
            with self.opener.open(request, timeout=20) as response:
                return json.load(response)
        except (urllib.error.URLError, json.JSONDecodeError) as error:
            # Error pages, URLs and credentials are deliberately not logged.
            raise EvidenceError(f"{method} API request failed ({type(error).__name__})") from error


def read_items(api, card, after):
    rows = []
    for _ in range(50):
        page = api.call("GET", f"/api/cards/{card}/harness/items?after_id={after}&limit=500&direction=asc")
        if not isinstance(page, list) or len(page) > 500:
            raise EvidenceError("malformed transcript page")
        for row in page:
            if not isinstance(row, dict) or type(row.get("id")) is not int or row["id"] <= after:
                raise EvidenceError("transcript cursor did not advance")
            try:
                params = json.loads(row["params"])
            except (KeyError, TypeError, json.JSONDecodeError) as error:
                raise EvidenceError("malformed transcript params JSON string") from error
            if not isinstance(params, dict) or not isinstance(params.get("item", {}), dict):
                raise EvidenceError("malformed transcript params object")
            row = {**row, "params": params}
            after = row["id"]
            rows.append(row)
        if len(page) < 500:
            return rows
    raise EvidenceError("transcript pagination exceeded bound")


def completed_calls(rows):
    calls = {}
    for row in rows:
        item = row["params"].get("item", {})
        if not isinstance(item, dict):
            raise EvidenceError("malformed transcript item")
        if item.get("type") != "mcpToolCall":
            continue
        identifier = item.get("id")
        if not isinstance(identifier, str) or not identifier:
            raise EvidenceError("MCP call has no stable item id")
        call = calls.setdefault(identifier, {})
        call.update(item)
        call["row_id"] = row["id"]
        if row.get("method") == "item/completed":
            call["completed"] = True
    return list(calls.values())


def metadata(call):
    result = call.get("result")
    if not isinstance(result, dict):
        raise EvidenceError("terminal call has no structured result")
    value = result.get("structuredContent")
    if value is None:
        # App-server releases may preserve MCP text while omitting the redundant
        # structuredContent field. ToolResult::structured writes the same JSON.
        content = result.get("content", [])
        if not isinstance(content, list) or not all(isinstance(item, dict) for item in content):
            raise EvidenceError("terminal content is malformed")
        texts = [item.get("text") for item in content if item.get("type") == "text"]
        if len(texts) != 1:
            raise EvidenceError("terminal result lacks one metadata JSON text")
        try:
            value = json.loads(texts[0])
        except (TypeError, json.JSONDecodeError) as error:
            raise EvidenceError("terminal metadata is not JSON") from error
    if not isinstance(value, dict):
        raise EvidenceError("terminal metadata is not an object")
    return value


def final_texts(rows):
    return [row["params"]["item"]["text"] for row in rows
            if row.get("method") == "item/completed"
            and row["params"].get("item", {}).get("type") == "agentMessage"
            and row["params"]["item"].get("phase") == "final_answer"
            and isinstance(row["params"]["item"].get("text"), str)]


def metrics(rows):
    calls = completed_calls(rows)
    terminal = [call for call in calls if str(call.get("tool", "")).startswith("calm.terminal.")]
    images = []

    def walk(value):
        if isinstance(value, dict):
            if value.get("type") == "image" and isinstance(value.get("data"), str):
                images.append(scrub(value))
            else:
                for item in value.values():
                    walk(item)
        elif isinstance(value, list):
            for item in value:
                walk(item)

    for call in calls:
        walk(call.get("result"))
    actions = [json.dumps(call.get("arguments", {}).get("action"), sort_keys=True)
               for call in terminal if call.get("tool") == "calm.terminal.input"]
    return {"mcp_tool_calls": len(calls), "terminal_tool_calls": len(terminal),
            "image_count": len(images), "image_bytes": sum(image["bytes"] for image in images),
            "normalized_transcript_json_bytes": len(json.dumps(rows, ensure_ascii=False).encode()),
            "repeated_identical_input_actions": sum(n - 1 for n in collections.Counter(actions).values()),
            "tool_errors": sum(bool(call.get("error") or call.get("status") == "failed"
                                    or (call.get("result") or {}).get("isError")) for call in calls),
            "observation_refusals": sum(bool(re.search(r"observation.*(?:expired|stale|changed)",
                                                       json.dumps(call.get("error")), re.I)) for call in terminal),
            "human_intervention": "not_measured", "token_savings": "not_measured"}


def terminal_evidence(rows, binding=None):
    calls = [call for call in completed_calls(rows)
             if str(call.get("tool", "")).startswith("calm.terminal.")]
    if not calls:
        raise EvidenceError("no actual Planner terminal MCP calls; prose is not evidence")
    observations, errors = [], []
    for call in calls:
        if not call.get("completed"):
            raise EvidenceError("terminal call never completed")
        if (call.get("error") or call.get("status") == "failed"
                or (call.get("result") or {}).get("isError")):
            errors.append(call["row_id"])
            continue
        args = call.get("arguments", {})
        if not isinstance(args, dict):
            raise EvidenceError("terminal call arguments are malformed")
        data = metadata(call)
        if call["tool"] in ("calm.terminal.open", "calm.terminal.observe"):
            current = {key: data.get(key) for key in
                       ("terminal_id", "terminal_session_id", "worker_session_id")}
            if not all(isinstance(value, str) and value for value in current.values()):
                raise EvidenceError("terminal observation lacks session identity")
            binding = current if binding is None else binding
            if current != binding:
                raise EvidenceError("terminal or session changed during UX round")
            if not isinstance(data.get("text"), str):
                raise EvidenceError("terminal observation has no text")
            observations.append({"row_id": call["row_id"], "text": data["text"]})
        if binding and args.get("terminal_id", binding["terminal_id"]) != binding["terminal_id"]:
            raise EvidenceError("Planner targeted another terminal")
    if not observations:
        raise EvidenceError("no successful terminal observations")
    return binding, observations, calls, errors


def check_scenario(name, rows, binding):
    binding, observations, calls, errors = terminal_evidence(rows, binding)
    actions = [call.get("arguments", {}).get("action", {}) for call in calls
               if call["tool"] == "calm.terminal.input"]
    # These answers are deliberately absent from the supplied TUI prompts.
    # A matching answer is still only supporting evidence, never rewind proof.
    answer = {"short": "3141", "edit": "7219", "rewind": "9123"}[name]
    if not any(re.search(rf"(?<!\d){answer}(?!\d)", view["text"]) for view in observations):
        raise EvidenceError(f"{name}: actual terminal answer absent")
    if name == "edit" and not any(action.get("type") == "key" and action.get("key")
                                  in ("Backspace", "Delete", "Ctrl+U") for action in actions):
        raise EvidenceError("edit: no actual input correction action")
    if name == "rewind" and not any(action.get("type") == "text"
                                    and action.get("text", "").strip() == "/rewind" for action in actions):
        raise EvidenceError("rewind: actual /rewind input absent")
    return binding, {"candidate_answer_observed": True, "tool_error_rows": errors,
                     "observations": observations, "status": "review_required"}


class Round:
    def __init__(self, api, args):
        self.api, self.args = api, args
        self.card = self.session = None
        self.cursor = 0
        self.current = []

    def wait_turn(self, name, expected_session=None):
        started = time.monotonic()
        self.current = []
        while time.monotonic() - started < self.args.turn_timeout:
            self.current.extend(read_items(self.api, self.card, self.cursor))
            if self.current:
                self.cursor = self.current[-1]["id"]
            snapshot = self.api.call("GET", f"/api/cards/{self.card}/planner/run")
            if not isinstance(snapshot, dict):
                raise EvidenceError("malformed Planner run")
            session = snapshot.get("worker_session_id")
            if expected_session and session != expected_session:
                raise EvidenceError("Planner session changed or became dormant")
            if snapshot.get("phase") == "wedged" or snapshot.get("blocked_reason"):
                raise EvidenceError("Planner is wedged or blocked")
            if final_texts(self.current) and snapshot.get("phase") in ("idle", "turn_completed"):
                sessions = {row.get("worker_session_id") for row in self.current}
                if sessions != {session} or not session:
                    raise EvidenceError("transcript mixed Planner sessions")
                self.session = session
                write_json(self.args.artifacts / f"{name}.json", {
                    "elapsed_seconds": round(time.monotonic() - started, 3),
                    "planner_card_id": self.card, "planner_session_id": session,
                    "planner_model": snapshot.get("model"), "metrics": metrics(self.current),
                    "transcript": self.current})
                return self.current
            time.sleep(2)
        raise EvidenceError(f"{name}: Planner did not finish within the bounded timeout")

    def send(self, name, goal):
        self.current = []
        response = self.api.call("POST", f"/api/cards/{self.card}/planner/input", {"text": goal})
        if response.get("worker_session_id") != self.session:
            raise EvidenceError("Planner input was accepted by another session")
        return self.wait_turn(name, self.session)

    def run(self):
        args = self.args
        version = self.api.call("GET", "/api/version")
        if version.get("buildSha") != args.source_sha:
            raise EvidenceError("running build SHA does not match the committed source")
        area = self.api.call("POST", "/api/areas", {"name": "Planner Claude UX", "color": "#4a90d9"})
        track = self.api.call("POST", "/api/tracks", {
            "area_id": area["id"], "cwd": args.workspace, "attach_folder": True,
            "theme": {"fg": [220, 220, 220], "bg": [30, 30, 30]},
            "title": "Disposable Planner terminal usability test. Do not dispatch workers or modify files. Reply ready and wait for the next instruction."})
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            cards = self.api.call("GET", f"/api/tracks/{track['id']}/cards")
            planners = [card for card in cards if card.get("payload", {}).get("planner_harness") is True]
            if len(planners) == 1:
                self.card = planners[0]["id"]
                break
            time.sleep(2)
        if not self.card:
            raise EvidenceError("production Planner card did not appear")
        write_json(args.artifacts / "environment.json", {
            "source_sha": args.source_sha, "server": version,
            "claude_version": args.claude_version, "codex_version": args.codex_version,
            "track_id": track["id"], "planner_card_id": self.card})
        self.wait_turn("bootstrap")
        common = ("This is an authorized disposable usability test. Operate the actual Claude TUI "
                  "through your production terminal tools. Do not use shell/exec tools outside that "
                  "terminal, call model APIs directly, impersonate tool output, write files, read "
                  "credentials/settings, change permissions, or dispatch another worker. If Claude "
                  "needs login or unsupported access, stop and explain. Observe text by default; use "
                  "an image only when visual highlighting is necessary. A write receipt is not an "
                  "application result. Finish each scenario with an honest final report, recording "
                  "terminal identity, actual evidence, ambiguity and friction. ")
        goals = {
            "short": common + f"Open one terminal running the actual executable {args.claude_bin!r}. "
            "In Claude submit exactly this synthetic prompt once: 记住暗号松果。请只回答 3100 + 41 的结果。 "
            "Wait for the actual answer and preserve this terminal for the next scenario.",
            "edit": common + "Reuse the same Claude terminal and conversation. Type but DO NOT "
            "submit: 请只回答 7200 + 11 的结果。 Correct the unsubmitted 11 to 19 using the TUI "
            "editor, preserving Chinese punctuation. Observe the corrected draft, then submit "
            "exactly once. Wait for the actual answer. Keep the conversation open.",
            "rewind": common + "In that same terminal, use Claude's actual /rewind command and "
            "menu to return to the point before the second prompt, preserving the first exchange. "
            "Observe the menu and restored input/history. Replace the restored draft with: "
            "请先写出之前的暗号，再写出 9100 + 23 的结果。 Submit once and observe the answer. "
            "Do not simulate rewind by starting a fresh session or by asking Claude to pretend. "
            "If the installed version cannot restore the conversation as requested, report it "
            "as blocked. State what proves the first exchange survived and the second was replaced."}
        binding, findings = None, {}
        for name, goal in goals.items():
            rows = self.send(name, goal)
            try:
                binding, findings[name] = check_scenario(name, rows, binding)
            except EvidenceError as error:
                findings[name] = {"status": "incomplete", "reason": str(error)}
                break
        # Interview the very same Planner after success OR a scenario-level
        # failure, while the original tool experience is still in its context.
        interview = self.send("interview", "Stop terminal actions. Based only on your actual "
                              "experience in this round, explain in Chinese: which operations "
                              "succeeded or failed; every cumbersome/redundant step; when an image "
                              "was necessary; confusing states, stale observations or workarounds; "
                              "one concrete improvement per pain point; and what remains unproven "
                              "about rewind. Cite actual tool item IDs when available. Distinguish "
                              "interface friction, Claude behavior and environment failures. Do "
                              "not claim a hypothetical improvement was tested.")
        if completed_calls(interview):
            findings["interview"] = {"status": "incomplete", "reason": "interview performed extra tool actions"}
        write_json(args.artifacts / "review.json", {
            "status": "review_required", "acceptance": "not_established",
            "binding": binding, "scenarios": findings, "interview": final_texts(interview),
            "review_requirements": ["actual Claude UI, not prompt echo or shell substitution",
                                    "unsubmitted correction and exactly one submission",
                                    "real rewind menu, restored history and resubmission",
                                    "same terminal/terminal-session/worker-session throughout",
                                    "interview pain points matched to actual tool evidence"]})
        print(f"REVIEW_REQUIRED: private evidence at {args.artifacts}")
        return 3


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("url", "workspace", "claude-bin", "claude-version", "codex-version", "source-sha"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--turn-timeout", type=int, default=360)
    args = parser.parse_args()
    if not 1 <= args.turn_timeout <= 900:
        parser.error("--turn-timeout must be 1..900 seconds")
    args.artifacts.mkdir(mode=0o700, parents=True, exist_ok=True)
    round_ = Round(Api(args.url, sys.stdin.read().strip()), args)
    try:
        return round_.run()
    except (EvidenceError, KeyError, TypeError) as error:
        write_json(args.artifacts / "incomplete.json", {
            "status": "incomplete", "acceptance": "not_established",
            "reason": str(error), "transcript": round_.current})
        print(f"INCOMPLETE: {scrub(str(error))}; private evidence at {args.artifacts}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
