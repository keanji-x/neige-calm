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
    def __init__(self, message, payload=None):
        super().__init__(message)
        self.payload = payload


def require_object(value, label):
    if not isinstance(value, dict):
        raise EvidenceError(f"{label} must be an object")
    return value


def scrub(value):
    """Keep private synthetic text; remove credential fields and image bytes."""
    if isinstance(value, dict):
        if value.get("type") == "image":
            if not isinstance(value.get("data"), str):
                return {"type": "image", "invalid_data_type": type(value.get("data")).__name__}
            try:
                image = base64.b64decode(value["data"], validate=True)
            except ValueError:
                # Failure evidence must remain writable even if the offending
                # transcript contains invalid base64; never preserve those bytes.
                return {"type": "image", "invalid_base64": True, "encoded_bytes": len(value["data"])}
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
            raise EvidenceError("malformed transcript page", {"earlier_rows": rows, "page": page})
        for row in page:
            if not isinstance(row, dict) or type(row.get("id")) is not int or row["id"] <= after:
                raise EvidenceError("transcript cursor did not advance", {"earlier_rows": rows, "row": row})
            try:
                params = json.loads(row["params"])
            except (KeyError, TypeError, json.JSONDecodeError) as error:
                raise EvidenceError("malformed transcript params JSON string", {"earlier_rows": rows, "row": row}) from error
            if not isinstance(params, dict) or not isinstance(params.get("item", {}), dict):
                raise EvidenceError("malformed transcript params object", {"earlier_rows": rows, "row": row})
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
    for call in calls.values():
        if call.get("result") is not None:
            require_object(call["result"], "MCP result")
        if str(call.get("tool", "")).startswith("calm.terminal."):
            arguments = require_object(call.get("arguments", {}), "terminal arguments")
            if call["tool"] == "calm.terminal.control" and arguments.get("action") not in ("claim", "release", "detach"):
                raise EvidenceError("terminal control action must be claim, release or detach")
            if call["tool"] == "calm.terminal.input" and "action" in arguments:
                action = require_object(arguments["action"], "terminal action")
                for field in ("type", "key", "text"):
                    if field in action and not isinstance(action[field], str):
                        raise EvidenceError(f"terminal action {field} must be a string")
    return list(calls.values())


def metadata(call):
    result = call.get("result")
    if not isinstance(result, dict):
        raise EvidenceError("terminal call has no structured result")
    value = result.get("structuredContent")
    if value is None:
        # Terminal tools keep the state only in structuredContent; the text
        # block is a one-line summary (#1618), so there is nothing to parse.
        raise EvidenceError("terminal result lacks structuredContent; content is a summary")
    if not isinstance(value, dict):
        raise EvidenceError("terminal metadata is not an object")
    return value


def final_texts(rows):
    return [row["params"]["item"]["text"] for row in rows
            if row.get("method") == "item/completed"
            and row["params"].get("item", {}).get("type") == "agentMessage"
            and row["params"]["item"].get("phase") == "final_answer"
            and isinstance(row["params"]["item"].get("text"), str)]


def stale_observation_result(call):
    """A completed input whose receipt says `outcome: "stale_observation"`.

    Since #1618 rounds 07/08 a stale observation with every other fence intact
    is a successful tool result carrying a fresh observation, not an RPC error;
    it still counts as an observation refusal because nothing was written.
    """
    if call.get("tool") != "calm.terminal.input" or not call.get("completed") or tool_failed(call):
        return False
    result = call.get("result")
    if not isinstance(result, dict) or not isinstance(result.get("structuredContent"), dict):
        return False
    return result["structuredContent"].get("outcome") == "stale_observation"


def observation_refused(call):
    if call.get("tool") != "calm.terminal.input":
        return False
    if stale_observation_result(call):
        return True
    messages = []
    if call.get("error") is not None:
        messages.append(require_object(call["error"], "MCP error").get("message"))
    result = call.get("result") or {}
    if result.get("isError") is True:
        content = result.get("content", [])
        if not isinstance(content, list):
            raise EvidenceError("MCP error content must be an array")
        for part in content:
            part = require_object(part, "MCP error content item")
            if part.get("type") == "text":
                messages.append(part.get("text"))
    # Exact production refusals in terminal_interaction/operations.rs. A broad
    # word-order regex misses the revision fence and counts unrelated errors.
    # The surface fence message continues with the changed properties; its
    # prefix is stable.
    refusals = ("observation expired; observe again",
                "observation belongs to another connection or expired",
                "terminal changed since observation; observe again",
                "terminal surface changed since observation",
                "no observation on this connection; observe first")
    return any(refusal in message for message in messages if isinstance(message, str)
               for refusal in refusals)


def tool_failed(call):
    return bool(call.get("error") or call.get("status") == "failed"
                or (call.get("result") or {}).get("isError"))


WAIT_METRIC_KEYS = ("change_wait_requests", "change_wait_outcomes", "unsettled_change_waits",
                    "elapsed_wait_requests", "unmeasured_wait_observations", "drift_allowed_inputs",
                    "drift_observed_inputs", "implicit_observation_inputs")


def wait_outcome(state):
    """Return the observation's `wait.outcome`, or None when the server sent no `wait`."""
    if "wait" not in state:
        return None  # pre-#1618 server: unmeasured, never inferred
    wait = require_object(state["wait"], "observation wait")
    if not isinstance(wait.get("outcome"), str) or not isinstance(wait.get("settled"), bool):
        raise EvidenceError("observation wait must carry a string outcome and a boolean settled")
    return wait


def wait_metrics(terminal):
    """#1618 counters, each read from a completed call's own arguments or result.

    A change wait is an `observe` call, or a control/input call requesting an
    `observe=true` readback, whose arguments say `wait_for: "change"`. Outcomes
    are read from the returned observation (`observe` result or readback
    `observation.state`); a failed call or unavailable readback returns no
    observation and therefore no outcome. Open results are not observations
    here. A settled/unchanged outcome is not application completion.
    """
    counts = collections.Counter()
    outcomes = collections.Counter()
    for call in terminal:
        if not call.get("completed"):
            continue
        args = call.get("arguments", {})
        tool = call["tool"]
        if tool == "calm.terminal.input":
            if args.get("allow_output_since_observation") is True:
                counts["drift_allowed_inputs"] += 1
            if "observation_id" not in args:
                counts["implicit_observation_inputs"] += 1
        observes = tool == "calm.terminal.observe" or (
            tool in ("calm.terminal.control", "calm.terminal.input") and args.get("observe") is True)
        wait_for, wait_ms = args.get("wait_for"), args.get("wait_ms")
        if observes and wait_for == "change":
            counts["change_wait_requests"] += 1
        elif observes and (wait_for == "elapsed" or (wait_for is None and type(wait_ms) is int and wait_ms > 0)):
            counts["elapsed_wait_requests"] += 1
        if tool_failed(call):
            continue
        data = metadata(call)
        if tool == "calm.terminal.input" and data.get("output_since_observation") is True:
            counts["drift_observed_inputs"] += 1
        state = observed_state(call, data) if tool != "calm.terminal.open" else None
        if state is None:
            continue
        wait = wait_outcome(state)
        if wait is None:
            counts["unmeasured_wait_observations"] += 1
        elif observes and wait_for == "change":
            outcomes[wait["outcome"]] += 1
            if wait["outcome"] == "changed" and wait["settled"] is False:
                counts["unsettled_change_waits"] += 1
    return {**{key: counts[key] for key in WAIT_METRIC_KEYS if key != "change_wait_outcomes"},
            "change_wait_outcomes": dict(sorted(outcomes.items()))}


def metrics(rows):
    calls = completed_calls(rows)
    terminal = [call for call in calls if str(call.get("tool", "")).startswith("calm.terminal.")]
    images = []

    def walk(value):
        if isinstance(value, dict):
            if value.get("type") == "image":
                image = scrub(value)
                if "bytes" not in image:
                    raise EvidenceError("malformed image payload")
                images.append(image)
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
    readbacks = collections.Counter()
    requested_presses = extra_presses = unmeasured_requests = 0
    for call in terminal:
        if not call.get("completed"):
            continue
        if call["tool"] in ("calm.terminal.control", "calm.terminal.input") and not tool_failed(call):
            readback = action_readback(call, metadata(call))
            if readback is not None:
                readbacks[readback["status"]] += 1
        action = call.get("arguments", {}).get("action", {})
        if call["tool"] == "calm.terminal.input" and action.get("type") == "key":
            repeat = action.get("repeat", 1)
            if type(repeat) is int and 1 <= repeat <= 32:
                requested_presses += repeat
                extra_presses += repeat - 1
            else:
                # Preserve the refused caller request and interview; an invalid
                # count is unmeasured, never silently treated as one press.
                unmeasured_requests += 1
    return {"mcp_tool_calls": len(calls), "terminal_tool_calls": len(terminal),
            "image_count": len(images), "image_bytes": sum(image["bytes"] for image in images),
            "normalized_transcript_json_bytes": len(json.dumps(rows, ensure_ascii=False).encode()),
            "repeated_identical_input_actions": sum(n - 1 for n in collections.Counter(actions).values()),
            "tool_errors": sum(tool_failed(call) for call in calls),
            "readback_available": readbacks["available"], "readback_unavailable": readbacks["unavailable"],
            "requested_key_presses": requested_presses, "additional_repeated_key_presses": extra_presses,
            "unmeasured_key_press_requests": unmeasured_requests,
            "observation_refusals": sum(observation_refused(call) for call in terminal),
            **wait_metrics(terminal),
            "human_intervention": "not_measured", "token_savings": "not_measured"}


def action_readback(call, data):
    if call["tool"] not in ("calm.terminal.control", "calm.terminal.input") or "observation" not in data:
        return None
    readback = require_object(data["observation"], "action observation")
    if readback.get("status") == "available":
        require_object(readback.get("state"), "action observation state")
        return readback
    if readback.get("status") == "unavailable" and isinstance(readback.get("reason"), str):
        return readback
    raise EvidenceError("action observation must be available with state or unavailable with reason")


def observed_state(call, data):
    if call["tool"] in ("calm.terminal.open", "calm.terminal.observe"):
        return data
    readback = action_readback(call, data)
    return readback["state"] if readback is not None and readback["status"] == "available" else None


def terminal_evidence(rows, binding=None):
    calls = [call for call in completed_calls(rows)
             if str(call.get("tool", "")).startswith("calm.terminal.")]
    if not calls:
        raise EvidenceError("no actual Planner terminal MCP calls; prose is not evidence")
    observations, errors = [], []
    for call in calls:
        if not call.get("completed"):
            raise EvidenceError("terminal call never completed")
        if tool_failed(call):
            errors.append(call["row_id"])
            continue
        args = call.get("arguments", {})
        if not isinstance(args, dict):
            raise EvidenceError("terminal call arguments are malformed")
        # `observation_id` may be omitted (#1618 C3); the receipt's
        # `observation_id_used` is informational and not checked here.
        # A fresh readback is observable state even when the physical receipt
        # remains unknown/refused. Do not rewrite or infer application completion.
        data = observed_state(call, metadata(call))
        if data is not None:
            current = {key: data.get(key) for key in
                       ("terminal_id", "terminal_session_id", "worker_session_id")}
            if not all(isinstance(value, str) and value for value in current.values()):
                raise EvidenceError("terminal observation lacks session identity")
            binding = current if binding is None else binding
            if current != binding:
                raise EvidenceError("terminal or session changed during UX round")
            lines = data.get("text")  # Frame.text is Vec<String>, not a scalar.
            if lines is None and isinstance(data.get("text_omitted"), str):
                # A release readback of a screen unchanged since the previous
                # observation (#1618 rounds 07/08) carries no text on purpose;
                # its identity was checked above but it adds no view.
                pass
            elif not isinstance(lines, list) or not all(isinstance(line, str) for line in lines):
                raise EvidenceError("terminal observation text must be an array of strings")
            else:
                observations.append({"row_id": call["row_id"], "text": "\n".join(lines)})
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
                    "planner_model_selection": snapshot.get("model"),
                    "planner_reasoning_effort_selection": snapshot.get("reasoning_effort"),
                    "metrics": metrics(self.current),
                    "transcript": self.current})
                return self.current
            time.sleep(2)
        raise EvidenceError(f"{name}: Planner did not finish within the bounded timeout")

    def send(self, name, goal):
        self.current = []
        response = require_object(self.api.call("POST", f"/api/cards/{self.card}/planner/input", {"text": goal}), "Planner input response")
        if response.get("worker_session_id") != self.session:
            raise EvidenceError("Planner input was accepted by another session")
        return self.wait_turn(name, self.session)

    def run(self):
        args = self.args
        version = require_object(self.api.call("GET", "/api/version"), "version response")
        if version.get("buildSha") != args.source_sha:
            raise EvidenceError("running build SHA does not match the committed source")
        area = self.api.call("POST", "/api/areas", {"name": "Planner Claude UX", "color": "#4a90d9"})
        track = self.api.call("POST", "/api/tracks", {
            "area_id": area["id"], "cwd": args.workspace, "attach_folder": True,
            "theme": {"fg": [220, 220, 220], "bg": [30, 30, 30]},
            "title": "Planner Claude terminal usability test"})
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            cards = self.api.call("GET", f"/api/tracks/{track['id']}/cards")
            if not isinstance(cards, list):
                raise EvidenceError("cards response must be an array")
            planners = [card for card in cards if require_object(require_object(card, "card").get("payload"), "card payload").get("planner_harness") is True]
            if len(planners) == 1:
                self.card = planners[0]["id"]
                snapshot = require_object(self.api.call("GET", f"/api/cards/{self.card}/planner/run"), "Planner run")
                if snapshot.get("blocked_reason") or snapshot.get("phase") == "wedged":
                    raise EvidenceError("Planner startup is blocked")
                session = snapshot.get("worker_session_id")
                if isinstance(session, str) and session and snapshot.get("phase") in ("idle", "turn_completed"):
                    self.session = session
                    break
            time.sleep(2)
        if not self.session:
            raise EvidenceError("production Planner did not become ready with a live session")
        write_json(args.artifacts / "environment.json", {
            "source_sha": args.source_sha, "server": version,
            "claude_version": args.claude_version, "codex_version": args.codex_version,
            "track_id": track["id"], "planner_card_id": self.card})
        # #1211: Track title is a label. Only normal user input starts a turn.
        self.send("bootstrap", "Disposable Planner terminal usability test. Do not dispatch workers "
                  "or modify files. Reply ready and wait for the next instruction.")
        common = ("This is an authorized disposable usability test. Operate the actual Claude TUI "
                  "through your production terminal tools. Do not use shell/exec tools outside that "
                  "terminal, call model APIs directly, impersonate tool output, write files, read "
                  "credentials/settings, change OS/account/filesystem permissions, or dispatch another worker. "
                  f"You may approve Claude's workspace-trust dialog only for this disposable workspace: {args.workspace!r}. "
                  "Do not trust another folder or change account permissions. If Claude "
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
        binding, findings, wait_summary = None, {}, {}
        for name, goal in goals.items():
            rows = self.send(name, goal)
            wait_summary[name] = {key: metrics(rows)[key] for key in WAIT_METRIC_KEYS}
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
            "binding": binding, "scenarios": findings, "wait_summary": wait_summary,
            "interview": final_texts(interview),
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
            "reason": str(error), "transcript": round_.current,
            "malformed_payload": error.payload if isinstance(error, EvidenceError) else None})
        print(f"INCOMPLETE: {scrub(str(error))}; private evidence at {args.artifacts}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
