"""Metric counters of the Planner Claude UX collector: each read from a completed
call's own arguments or result, never inferred (#1618/#1620/#1628/#1666/#1677).

Imported wholesale by `planner_claude_ux.py` (star import), which keeps the
transcript reading, the scenario checks and the round driver; the two files
are one program split so neither passes the file-size target.
"""

import collections


class EvidenceError(Exception):
    def __init__(self, message, payload=None):
        super().__init__(message)
        self.payload = payload


def require_object(value, label):
    if not isinstance(value, dict):
        raise EvidenceError(f"{label} must be an object")
    return value


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


def tool_failed(call):
    return bool(call.get("error") or call.get("status") == "failed"
                or (call.get("result") or {}).get("isError"))


WAIT_METRIC_KEYS = ("change_wait_requests", "change_wait_outcomes", "unsettled_change_waits",
                    "elapsed_wait_requests", "unmeasured_wait_observations", "drift_allowed_inputs",
                    "drift_observed_inputs", "implicit_observation_inputs")
SIGNAL_METRIC_KEYS = ("signal_wait_requests", "signal_wait_outcomes", "signal_repaint_outcomes", "submit_actions",
                      "open_with_claim", "open_with_permissions", "hooks_seen_observations", "signals_observed",
                      "signal_events_observed", "unmeasured_signal_observations")
SIGNAL_METRIC_TALLIES = ("signal_wait_outcomes", "signal_repaint_outcomes", "signal_events_observed")
ROUND_TRIP_METRIC_KEYS = ("text_wait_requests", "text_wait_outcomes", "sequence_actions", "sequence_steps",
                          "input_with_claim", "input_with_release", "below_cursor_allowed_inputs",
                          "below_cursor_tolerated_inputs")
OPEN_REPLACE_SUMMARY_METRIC_KEYS = ("open_with_wait", "open_wait_outcomes", "replace_actions", "replace_written",
                                    "summary_present")
TEXT_CONDITION_METRIC_KEYS = ("text_condition_requests", "signal_condition_outcomes")
HISTORY_SEARCH_METRIC_KEYS = ("history_search_requests", "history_search_found")
SUMMARY_METRIC_KEYS = (WAIT_METRIC_KEYS + SIGNAL_METRIC_KEYS + ROUND_TRIP_METRIC_KEYS
                       + OPEN_REPLACE_SUMMARY_METRIC_KEYS + TEXT_CONDITION_METRIC_KEYS
                       + HISTORY_SEARCH_METRIC_KEYS)
# The observe wait arguments every wait carrier accepts (#1677: open included; r16: wait_text_absent).
WAIT_ARGUMENT_KEYS = ("wait_for", "wait_ms", "settle_ms", "signal_events", "repaint_ms", "wait_text",
                      "wait_text_absent")
TEXT_CONDITION_KEYS = ("wait_text", "wait_text_absent")


def wait_outcome(state):
    """Return the observation's `wait.outcome`, or None when the server sent no `wait`."""
    if "wait" not in state:
        return None  # pre-#1618 server: unmeasured, never inferred
    wait = require_object(state["wait"], "observation wait")
    if not isinstance(wait.get("outcome"), str) or not isinstance(wait.get("settled"), bool):
        raise EvidenceError("observation wait must carry a string outcome and a boolean settled")
    return wait


def signal_repaint(wait):
    """Return a signal wait's `wait.repaint` block (#1628), or None when the server sent none.

    Only a `signal` outcome carries one; an older server omits it, which is
    tolerated (unmeasured), while a present block must be well formed.
    """
    if wait.get("outcome") != "signal" or "repaint" not in wait:
        return None
    repaint = wait["repaint"]
    if repaint is None:
        return None
    repaint = require_object(repaint, "observation wait.repaint")
    if not isinstance(repaint.get("outcome"), str) or type(repaint.get("waited_ms")) is not int:
        raise EvidenceError("observation wait.repaint must carry a string outcome and an integer waited_ms")
    return repaint


def observation_signals(state):
    """Return the observation's `signals` block, or None when the server sent none (#1620)."""
    if "signals" not in state:
        return None  # pre-#1620 server: unmeasured, never inferred
    signals = require_object(state["signals"], "observation signals")
    events = signals.get("since_previous_observation")
    if (not isinstance(signals.get("hooks_seen"), bool) or type(signals.get("last_seq")) is not int
            or not isinstance(events, list) or not all(isinstance(event, dict) for event in events)):
        raise EvidenceError("observation signals must carry hooks_seen, last_seq and an event list")
    if not all(isinstance(event.get("event"), str) for event in events):
        raise EvidenceError("observation signals events must each carry a string event")
    return signals


def requests_observation(call):
    """True when a completed call returns an observation: observe and open always do (#1677: an open's
    wait arguments run as its final observation), control/input with an observe=true readback."""
    args = call.get("arguments", {})
    return call["tool"] in ("calm.terminal.open", "calm.terminal.observe") or (
        call["tool"] in ("calm.terminal.control", "calm.terminal.input") and args.get("observe") is True)


def signal_metrics(terminal):
    """#1620 hook-signal counters, each read from a completed call's own arguments or result.

    A signal wait is an observation-requesting call whose arguments say
    `wait_for: "signal"`; its outcome is the returned observation's
    `wait.outcome` (a `signal` outcome is an observation like any other, not
    application completion; `no_signal` (#1692) is a budget that ended without
    one and says nothing about the screen). `submit_actions` and
    `open_with_claim` describe requests, including failed ones.
    `hooks_seen_observations` and `signals_observed` are read from each
    observation's `signals` block, `signal_events_observed` (#1704) tallies
    its entries' `event` (so `signals_observed` is that tally's total);
    observations lacking it (older server) are `unmeasured_signal_observations`.
    `signal_repaint_outcomes` (#1628) tallies `wait.repaint.outcome` of the
    signal waits that ended on a signal; a missing block is tolerated.
    `open_with_permissions` (#1704) counts open requests whose arguments carry
    `claude_permissions`, failed ones included.
    """
    counts = collections.Counter()
    outcomes = collections.Counter()
    repaints = collections.Counter()
    events = collections.Counter()
    for call in terminal:
        if not call.get("completed"):
            continue
        args, tool = call.get("arguments", {}), call["tool"]
        action = args.get("action")
        if tool == "calm.terminal.input" and isinstance(action, dict) and action.get("type") == "submit":
            counts["submit_actions"] += 1
        if tool == "calm.terminal.open" and args.get("claim") is True:
            counts["open_with_claim"] += 1
        if tool == "calm.terminal.open" and "claude_permissions" in args:
            counts["open_with_permissions"] += 1
        signal_wait = requests_observation(call) and args.get("wait_for") == "signal"
        if signal_wait:
            counts["signal_wait_requests"] += 1
        if tool_failed(call):
            continue
        state = observed_state(call, metadata(call))
        if state is None:
            continue
        wait = wait_outcome(state)
        if signal_wait and wait is not None:
            outcomes[wait["outcome"]] += 1
            repaint = signal_repaint(wait)
            if repaint is not None:
                repaints[repaint["outcome"]] += 1
        signals = observation_signals(state)
        if signals is None:
            counts["unmeasured_signal_observations"] += 1
            continue
        if signals["hooks_seen"] is True:
            counts["hooks_seen_observations"] += 1
        counts["signals_observed"] += len(signals["since_previous_observation"])
        for event in signals["since_previous_observation"]:
            events[event["event"]] += 1
    return {**{key: counts[key] for key in SIGNAL_METRIC_KEYS if key not in SIGNAL_METRIC_TALLIES},
            "signal_wait_outcomes": dict(sorted(outcomes.items())),
            "signal_repaint_outcomes": dict(sorted(repaints.items())),
            "signal_events_observed": dict(sorted(events.items()))}


def round_trip_metrics(terminal):
    """#1666 round-trip counters, each read from a completed call's own arguments or result.

    A text wait is an observation-requesting call whose arguments say
    `wait_for: "text"`; its outcome is the returned observation's
    `wait.outcome` (`matched` is an observation like any other, not
    application completion). `sequence_actions` counts input requests whose
    action type is `sequence` (failed ones included) and `sequence_steps`
    the steps those requests carried (a non-list `steps` adds none).
    `input_with_claim` / `input_with_release` / `below_cursor_allowed_inputs`
    count input requests whose arguments say `claim`, `release` or
    `allow_output_below_cursor` is true; `below_cursor_tolerated_inputs`
    counts non-failed input receipts whose `observation_drift.tolerance` is
    `below_cursor` (the server admitted the write through the row
    comparison). Nothing is inferred from screen text.
    """
    counts = collections.Counter()
    outcomes = collections.Counter()
    for call in terminal:
        if not call.get("completed"):
            continue
        args, tool = call.get("arguments", {}), call["tool"]
        text_wait = requests_observation(call) and args.get("wait_for") == "text"
        if text_wait:
            counts["text_wait_requests"] += 1
        if tool == "calm.terminal.input":
            action = args.get("action")
            if isinstance(action, dict) and action.get("type") == "sequence":
                counts["sequence_actions"] += 1
                if isinstance(action.get("steps"), list):
                    counts["sequence_steps"] += len(action["steps"])
            for flag, key in (("claim", "input_with_claim"), ("release", "input_with_release"),
                              ("allow_output_below_cursor", "below_cursor_allowed_inputs")):
                if args.get(flag) is True:
                    counts[key] += 1
        if tool_failed(call):
            continue
        data = metadata(call)
        if tool == "calm.terminal.input":
            drift = data.get("observation_drift")
            if isinstance(drift, dict) and drift.get("tolerance") == "below_cursor":
                counts["below_cursor_tolerated_inputs"] += 1
        state = observed_state(call, data)
        if state is None:
            continue
        wait = wait_outcome(state)
        if text_wait and wait is not None:
            outcomes[wait["outcome"]] += 1
    return {**{key: counts[key] for key in ROUND_TRIP_METRIC_KEYS if key != "text_wait_outcomes"},
            "text_wait_outcomes": dict(sorted(outcomes.items()))}


def open_replace_summary_metrics(terminal):
    """#1677 counters, each read from a completed call's own arguments or result.

    `open_with_wait` counts `open` calls whose arguments carry any wait
    argument (failed ones included); `open_wait_outcomes` tallies those
    calls' returned `wait.outcome` (a missing block, older server, adds
    none). `replace_actions` counts input requests whose action type is
    `replace` (failed ones included) and `replace_written` the non-failed
    ones whose receipt `outcome` is `written` (an acknowledgement, not an
    application result). `summary_present` counts non-failed control/input
    receipts carrying a `summary` object. Nothing is inferred from screen
    text.
    """
    counts = collections.Counter()
    outcomes = collections.Counter()
    for call in terminal:
        if not call.get("completed"):
            continue
        args, tool = call.get("arguments", {}), call["tool"]
        open_wait = tool == "calm.terminal.open" and any(key in args for key in WAIT_ARGUMENT_KEYS)
        if open_wait:
            counts["open_with_wait"] += 1
        action = args.get("action")
        replace = tool == "calm.terminal.input" and isinstance(action, dict) and action.get("type") == "replace"
        if replace:
            counts["replace_actions"] += 1
        if tool_failed(call):
            continue
        data = metadata(call)
        if replace and data.get("outcome") == "written":
            counts["replace_written"] += 1
        if tool in ("calm.terminal.control", "calm.terminal.input") and isinstance(data.get("summary"), dict):
            counts["summary_present"] += 1
        if open_wait:
            wait = wait_outcome(data)
            if wait is not None:
                outcomes[wait["outcome"]] += 1
    return {**{key: counts[key] for key in OPEN_REPLACE_SUMMARY_METRIC_KEYS if key != "open_wait_outcomes"},
            "open_wait_outcomes": dict(sorted(outcomes.items()))}


def condition_state(wait):
    """Return a signal wait's `wait.conditions` block (#1677 r16), or None when the server sent none.

    A present block must carry `present` and `absent`, each true, false or
    null (a side that was not asked).
    """
    if "conditions" not in wait:
        return None
    conditions = require_object(wait["conditions"], "observation wait.conditions")
    for side in ("present", "absent"):
        if conditions.get(side) is not None and not isinstance(conditions[side], bool):
            raise EvidenceError("observation wait.conditions sides must be true, false or null")
    return conditions


def text_condition_metrics(terminal):
    """#1677 r16 counters, each read from a completed call's own arguments or result.

    `text_condition_requests` counts observation-requesting calls (observe,
    open, control/input with observe=true) whose arguments say `wait_for:
    "signal"` and carry `wait_text` or `wait_text_absent` (failed ones
    included). `signal_condition_outcomes` tallies, over the non-failed ones
    that returned an observation whose wait ended on a signal with a
    `conditions` block, the string `<repaint outcome>/<held|not_held|untested>`:
    held when every asked side is true, not_held when one is false, untested
    when the repaint was `skipped` or an asked side came back null (the
    server never tested the screen; review r1 H); a missing repaint or
    conditions block (older server) adds nothing. A held condition is a
    screen fact, not an application result.
    """
    counts = collections.Counter()
    outcomes = collections.Counter()
    for call in terminal:
        if not call.get("completed"):
            continue
        args = call.get("arguments", {})
        conditioned = (requests_observation(call) and args.get("wait_for") == "signal"
                       and any(key in args for key in TEXT_CONDITION_KEYS))
        if not conditioned:
            continue
        counts["text_condition_requests"] += 1
        if tool_failed(call):
            continue
        state = observed_state(call, metadata(call))
        if state is None:
            continue
        wait = wait_outcome(state)
        if wait is None:
            continue
        repaint = signal_repaint(wait)
        conditions = condition_state(wait)
        if repaint is None or conditions is None:
            continue
        asked = [side for side, key in (("present", "wait_text"), ("absent", "wait_text_absent")) if key in args]
        if repaint["outcome"] == "skipped" or any(conditions.get(side) is None for side in asked):
            verdict = "untested"
        elif all(conditions[side] is not False for side in asked):
            verdict = "held"
        else:
            verdict = "not_held"
        outcomes[f"{repaint['outcome']}/{verdict}"] += 1
    return {"text_condition_requests": counts["text_condition_requests"],
            "signal_condition_outcomes": dict(sorted(outcomes.items()))}


def history_search_metrics(terminal):
    """#1710 counters, each read from a completed call's own arguments or result.

    `history_search_requests` counts observe calls whose arguments carry
    `scroll_to_text` (failed ones included); `history_search_found` the
    non-failed ones whose result carries a `scroll_to` block with `status`
    `"found"` (a missing block, older server, adds none). A found row is a
    screen fact, never an application result.
    """
    counts = collections.Counter()
    for call in terminal:
        if not call.get("completed") or call["tool"] != "calm.terminal.observe":
            continue
        if "scroll_to_text" not in call.get("arguments", {}):
            continue
        counts["history_search_requests"] += 1
        if tool_failed(call):
            continue
        scroll_to = metadata(call).get("scroll_to")
        if scroll_to is None:
            continue
        if require_object(scroll_to, "observation scroll_to").get("status") == "found":
            counts["history_search_found"] += 1
    return {key: counts[key] for key in HISTORY_SEARCH_METRIC_KEYS}


def wait_metrics(terminal):
    """#1618 counters, each read from a completed call's own arguments or result.

    A change wait is an `observe` call, or a control/input call requesting an
    `observe=true` readback, whose arguments say `wait_for: "change"` (#1677:
    an `open` too, whose wait runs as its final observation). Outcomes are
    read from the returned observation (`open`/`observe` result or readback
    `observation.state`); a failed call or unavailable readback returns no
    observation and therefore no outcome. A settled/unchanged outcome is not
    application completion.
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
        observes = requests_observation(call)
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
        state = observed_state(call, data)
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
