"""Model-free checks of evidence collection, never a fake Planner acceptance."""

import copy
import json
import io
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import planner_claude_ux as ux


def row(identifier, tool="calm.terminal.observe", *, text=("3141",), terminal="t1"):
    return {"id": identifier, "method": "item/completed", "worker_session_id": "planner1",
            "params": {"item": {"id": f"call-{identifier}", "type": "mcpToolCall",
                                "tool": tool, "status": "completed", "arguments": {"terminal_id": terminal},
                                "result": {"structuredContent": {"terminal_id": terminal,
                                                                 "terminal_session_id": "pty1",
                                                                 "worker_session_id": "worker1", "text": list(text)}}}}}


class CollectorTests(unittest.TestCase):
    def test_metrics_count_available_and_unavailable_action_readbacks_separately(self):
        state = row(1)["params"]["item"]["result"]["structuredContent"]
        calls = []
        for identifier, observation in ((1, {"status": "available", "state": state}),
                                         (2, {"status": "unavailable", "reason": "readback timeout"})):
            call = row(identifier, "calm.terminal.input")
            call["params"]["item"]["arguments"]["action"] = {"type": "key", "key": "Enter"}
            call["params"]["item"]["result"] = {"structuredContent": {
                "terminal_id": "t1", "request_id": f"request-{identifier}", "outcome": "written",
                "application_result": "unverified", "observation": observation}}
            calls.append(call)
        result = ux.metrics(calls)
        self.assertEqual(result["readback_available"], 1)
        self.assertEqual(result["readback_unavailable"], 1)
        self.assertEqual(result["tool_errors"], 0)

    def test_metrics_count_requested_key_presses_without_redefining_repeated_actions(self):
        calls = []
        for identifier, action in enumerate(({"type": "key", "key": "Left", "repeat": 4},
                                             {"type": "key", "key": "Right"},
                                             {"type": "key", "key": "Backspace", "repeat": 2},
                                             {"type": "key", "key": "Enter"},
                                             {"type": "key", "key": "Left", "repeat": 4}), start=1):
            call = row(identifier, "calm.terminal.input")
            call["params"]["item"]["arguments"]["action"] = action
            calls.append(call)
        result = ux.metrics(calls)
        self.assertEqual(result["requested_key_presses"], 12)
        self.assertEqual(result["additional_repeated_key_presses"], 7)
        self.assertEqual(result["repeated_identical_input_actions"], 1)

    def test_invalid_repeat_on_failed_call_is_preserved_and_unmeasured(self):
        for repeat in (None, "4", 2.5, True, 0, 33):
            call = row(1, "calm.terminal.input")
            item = call["params"]["item"]
            item["arguments"]["action"] = {"type": "key", "key": "Left", "repeat": repeat}
            item["status"], item["error"] = "failed", {"message": "invalid repeat"}
            original = copy.deepcopy(call)
            with self.subTest(repeat=repeat):
                result = ux.metrics([call])
                self.assertEqual(result["requested_key_presses"], 0)
                self.assertEqual(result["additional_repeated_key_presses"], 0)
                self.assertEqual(result["unmeasured_key_press_requests"], 1)
                self.assertEqual(result["tool_errors"], 1)
                self.assertEqual(call, original)

    def test_available_action_readback_is_actual_observation_without_changing_receipt(self):
        state = row(1)["params"]["item"]["result"]["structuredContent"]
        state.update({"observation_id": "fresh-observation", "control_id": "owner1", "role": "owner"})
        for tool, action, receipt in (
                ("calm.terminal.control", "claim", {"terminal_id": "t1", "connection_id": "c1", "control_id": "owner1"}),
                ("calm.terminal.input", {"type": "key", "key": "Enter"},
                 {"terminal_id": "t1", "request_id": "r1", "outcome": "written", "application_result": "unverified"}),
                ("calm.terminal.input", {"type": "key", "key": "Enter"},
                 {"terminal_id": "t1", "request_id": "r1", "outcome": "unknown", "repeat_input": False})):
            observed = row(1, tool)
            item = observed["params"]["item"]
            item["arguments"].update({"action": action, "observe": True})
            item["result"] = {"structuredContent": {**receipt, "observation": {"status": "available", "state": state}}}
            original = copy.deepcopy(observed)
            with self.subTest(tool=tool, receipt=receipt):
                _, evidence = ux.check_scenario("short", [observed], None)
                self.assertEqual(evidence["observations"][0]["text"], "3141")
                self.assertEqual(evidence["status"], "review_required")
                self.assertEqual(observed, original)
                self.assertEqual(ux.metrics([observed])["image_count"], 0)

    def test_unavailable_readback_preserves_written_receipt_without_inventing_view(self):
        written = row(2, "calm.terminal.input")
        written["params"]["item"]["arguments"]["action"] = {"type": "key", "key": "Enter"}
        receipt = {"terminal_id": "t1", "request_id": "r1", "outcome": "written", "application_result": "unverified",
                   "observation": {"status": "unavailable", "reason": "connection lost after write"}}
        written["params"]["item"]["result"] = {"structuredContent": receipt}
        original = copy.deepcopy(written)
        _, observations, _, errors = ux.terminal_evidence([row(1), written])
        self.assertEqual([view["row_id"] for view in observations], [1])
        self.assertEqual(errors, [])
        self.assertEqual(ux.metrics([written])["tool_errors"], 0)
        self.assertEqual(written, original)
        with self.assertRaisesRegex(ux.EvidenceError, "no successful terminal observations"):
            ux.terminal_evidence([written])

    def test_receipt_text_without_available_readback_is_not_observation(self):
        receipt = row(1, "calm.terminal.input")
        receipt["params"]["item"]["result"]["structuredContent"]["outcome"] = "written"
        with self.assertRaisesRegex(ux.EvidenceError, "no successful terminal observations"):
            ux.terminal_evidence([receipt])

    def test_readback_reuses_exact_session_and_shape_validation(self):
        state = row(1)["params"]["item"]["result"]["structuredContent"]
        binding, *_ = ux.terminal_evidence([row(1)])
        for observation in (None, {"status": "available", "state": []}, {"status": "unavailable"},
                            {"status": "unavailable", "reason": 123}, {"status": "unknown", "state": state},
                            {"status": "available", "state": {**state, "terminal_session_id": "replacement"}}):
            call = row(2, "calm.terminal.input")
            call["params"]["item"]["result"] = {"structuredContent": {"outcome": "written", "observation": observation}}
            with self.subTest(observation=observation), self.assertRaises(ux.EvidenceError):
                ux.terminal_evidence([call], binding)

    def test_bootstrap_posts_user_input_before_waiting_for_same_live_session(self):
        calls = []

        class ApiResponses:
            def call(self, method, path, body=None):
                calls.append((method, path, body))
                return {("GET", "/api/version"): {"buildSha": "source"},
                        ("POST", "/api/areas"): {"id": "area"},
                        ("POST", "/api/tracks"): {"id": "track"},
                        ("GET", "/api/tracks/track/cards"): [{"id": "planner", "payload": {"planner_harness": True}}],
                        ("GET", "/api/cards/planner/planner/run"): {"worker_session_id": "session", "phase": "idle"},
                        ("POST", "/api/cards/planner/planner/input"): {"worker_session_id": "session"}}[(method, path)]

        class BootstrapReached(Exception):
            pass

        def wait(round_, name, expected_session=None):
            self.assertEqual(name, "bootstrap")
            self.assertEqual(calls[-1][:2], ("POST", "/api/cards/planner/planner/input"))
            self.assertIn("Reply ready", calls[-1][2]["text"])
            self.assertEqual(expected_session, "session")
            self.assertEqual(round_.session, "session")
            raise BootstrapReached()

        with tempfile.TemporaryDirectory() as directory:
            args = SimpleNamespace(source_sha="source", workspace="/synthetic", artifacts=Path(directory),
                                   claude_version="test", codex_version="test")
            with patch.object(ux.Round, "wait_turn", wait), self.assertRaises(BootstrapReached):
                ux.Round(ApiResponses(), args).run()

    def test_bootstrap_input_rejects_a_changed_planner_session(self):
        class ApiResponse:
            def call(self, *_):
                return {"worker_session_id": "replacement"}

        round_ = ux.Round(ApiResponse(), None)
        round_.card, round_.session = "planner", "original"
        with patch.object(round_, "wait_turn") as wait, self.assertRaisesRegex(ux.EvidenceError, "another session"):
            round_.send("bootstrap", "Reply ready")
        wait.assert_not_called()

    def test_real_control_actions_and_result_shapes_remain_collectable(self):
        # terminal_interaction::control: action is a string; detach returns
        # the #1618 identity receipt (older servers: {detached:true}), while
        # claim/release return connection/control IDs.
        for action, result in (("claim", {"terminal_id": "t1", "connection_id": "c1", "control_id": "owner1"}),
                               ("release", {"terminal_id": "t1", "connection_id": "c1", "control_id": None}),
                               ("detach", {"detached": True}),
                               ("detach", {"detached": True, "had_client": True, "terminal_id": "t1",
                                           "connection_id": "c1", "terminal_session_id": "pty1"})):
            control = row(2, "calm.terminal.control")
            control["params"]["item"]["arguments"]["action"] = action
            control["params"]["item"]["result"] = {"structuredContent": result}
            with self.subTest(action=action):
                self.assertEqual(ux.metrics([row(1), control])["terminal_tool_calls"], 2)
                _, _, calls, errors = ux.terminal_evidence([row(1), control])
                self.assertEqual(len(calls), 2)
                self.assertEqual(errors, [])

    def test_change_wait_outcomes_are_read_from_returned_observations_only(self):
        # Request side: wait_for=change on observe, and on an observe=true
        # input readback. Outcome side: the observation's own wait block.
        def wait(outcome, settled=True):
            return {"wait": {"mode": "change", "outcome": outcome, "waited_ms": 812, "settled": settled},
                    "changed_since_previous_observation": outcome == "changed"}
        calls = []
        for identifier, outcome, settled in ((1, "changed", True), (2, "changed", False), (3, "unchanged", False)):
            call = row(identifier)
            call["params"]["item"]["arguments"].update({"wait_for": "change", "wait_ms": 5000})
            call["params"]["item"]["result"]["structuredContent"].update(wait(outcome, settled))
            calls.append(call)
        state = {**row(4)["params"]["item"]["result"]["structuredContent"], **wait("exited")}
        readback = row(4, "calm.terminal.input")
        readback["params"]["item"]["arguments"].update({"action": {"type": "key", "key": "Enter"},
                                                         "observe": True, "wait_for": "change"})
        readback["params"]["item"]["result"] = {"structuredContent": {
            "terminal_id": "t1", "request_id": "r4", "outcome": "written", "application_result": "unverified",
            "observation_id_used": "obs-3", "output_since_observation": False,
            "observation": {"status": "available", "state": state}}}
        calls.append(readback)
        # Refused change wait: a request, but no observation and no outcome.
        refused = row(5)
        refused["params"]["item"]["arguments"]["wait_for"] = "change"
        refused["params"]["item"]["status"], refused["params"]["item"]["error"] = "failed", {"message": "wait_ms out of range"}
        calls.append(refused)
        # Unavailable readback with wait_for=change: a request without an observation.
        unavailable = row(6, "calm.terminal.input")
        unavailable["params"]["item"]["arguments"].update({"action": {"type": "key", "key": "Enter"},
                                                            "observe": True, "wait_for": "change"})
        unavailable["params"]["item"]["result"] = {"structuredContent": {
            "terminal_id": "t1", "request_id": "r6", "outcome": "written", "application_result": "unverified",
            "observation": {"status": "unavailable", "reason": "readback timeout"}}}
        calls.append(unavailable)
        original = copy.deepcopy(calls)
        result = ux.metrics(calls)
        self.assertEqual(result["change_wait_requests"], 6)
        self.assertEqual(result["change_wait_outcomes"], {"changed": 2, "exited": 1, "unchanged": 1})
        self.assertEqual(result["unsettled_change_waits"], 1)
        self.assertEqual(result["elapsed_wait_requests"], 0)
        self.assertEqual(result["unmeasured_wait_observations"], 0)
        self.assertEqual(result["readback_available"], 1)
        self.assertEqual(result["tool_errors"], 1)
        self.assertEqual(calls, original)
        # Existing definitions are untouched by the new fields.
        self.assertEqual(result["terminal_tool_calls"], 6)
        self.assertEqual(result["observation_refusals"], 0)

    def test_change_wait_only_counts_calls_that_produce_an_observation(self):
        # wait_for=change on a control call without observe=true requests no
        # readback, so it is neither a change nor an elapsed wait request.
        control = row(1, "calm.terminal.control")
        control["params"]["item"]["arguments"].update({"action": "claim", "wait_for": "change"})
        control["params"]["item"]["result"] = {"structuredContent": {
            "terminal_id": "t1", "connection_id": "c1", "control_id": "owner1"}}
        result = ux.metrics([control])
        self.assertEqual(result["change_wait_requests"], 0)
        self.assertEqual(result["elapsed_wait_requests"], 0)
        self.assertEqual(result["unmeasured_wait_observations"], 0)

    def test_elapsed_wait_requests_are_explicit_or_positive_wait_ms_without_mode(self):
        cases = ({"wait_for": "elapsed"}, {"wait_ms": 500}, {"wait_for": "elapsed", "wait_ms": 0},
                 {"wait_ms": 0}, {}, {"wait_for": "change", "wait_ms": 500}, {"wait_ms": "500"})
        calls = []
        for identifier, arguments in enumerate(cases, start=1):
            call = row(identifier)
            call["params"]["item"]["arguments"].update(arguments)
            call["params"]["item"]["result"]["structuredContent"].update({
                "wait": {"mode": arguments.get("wait_for", "elapsed"), "outcome": "elapsed",
                         "waited_ms": 0, "settled": True},
                "changed_since_previous_observation": False})
            calls.append(call)
        result = ux.metrics(calls)
        self.assertEqual(result["elapsed_wait_requests"], 3)
        self.assertEqual(result["change_wait_requests"], 1)
        self.assertEqual(result["change_wait_outcomes"], {"elapsed": 1})
        self.assertEqual(result["unmeasured_wait_observations"], 0)

    def test_observation_without_wait_field_is_unmeasured_not_a_crash(self):
        # Pre-#1618 server: no wait block on observe results or readback states.
        plain = row(1)
        requested = row(2)
        requested["params"]["item"]["arguments"]["wait_for"] = "change"
        state = row(3)["params"]["item"]["result"]["structuredContent"]
        readback = row(3, "calm.terminal.input")
        readback["params"]["item"]["arguments"].update({"action": {"type": "key", "key": "Enter"}, "observe": True})
        readback["params"]["item"]["result"] = {"structuredContent": {
            "terminal_id": "t1", "request_id": "r3", "outcome": "written", "application_result": "unverified",
            "observation": {"status": "available", "state": state}}}
        result = ux.metrics([plain, requested, readback])
        self.assertEqual(result["unmeasured_wait_observations"], 3)
        self.assertEqual(result["change_wait_requests"], 1)
        self.assertEqual(result["change_wait_outcomes"], {})
        self.assertEqual(result["unsettled_change_waits"], 0)
        _, evidence = ux.check_scenario("short", [plain, requested, readback], None)
        self.assertEqual([view["row_id"] for view in evidence["observations"]], [1, 2, 3])

    def test_malformed_wait_block_is_rejected(self):
        for wait in ([], "changed", {"outcome": "changed"}, {"outcome": 1, "settled": True},
                     {"outcome": "changed", "settled": "yes"}):
            observed = row(1)
            observed["params"]["item"]["result"]["structuredContent"]["wait"] = wait
            with self.subTest(wait=wait), self.assertRaisesRegex(ux.EvidenceError, "wait"):
                ux.metrics([observed])

    def test_input_without_observation_id_is_implicit_and_still_evidence(self):
        state = row(1)["params"]["item"]["result"]["structuredContent"]
        state.update({"observation_id": "obs-2", "wait": {"mode": "change", "outcome": "changed",
                                                          "waited_ms": 40, "settled": True},
                      "changed_since_previous_observation": True})
        explicit = row(1, "calm.terminal.input")
        explicit["params"]["item"]["arguments"].update({"action": {"type": "key", "key": "Enter"},
                                                        "observation_id": "obs-1", "request_id": "r1"})
        explicit["params"]["item"]["result"] = {"structuredContent": {
            "terminal_id": "t1", "request_id": "r1", "outcome": "written", "application_result": "unverified",
            "observation_id_used": "obs-1", "output_since_observation": False}}
        implicit = row(2, "calm.terminal.input")
        implicit["params"]["item"]["arguments"].update({"action": {"type": "key", "key": "Enter"},
                                                        "request_id": "r2", "observe": True})
        implicit["params"]["item"]["result"] = {"structuredContent": {
            "terminal_id": "t1", "request_id": "r2", "outcome": "written", "application_result": "unverified",
            "observation_id_used": "obs-1", "output_since_observation": False,
            "observation": {"status": "available", "state": state}}}
        original = copy.deepcopy([explicit, implicit])
        result = ux.metrics([explicit, implicit])
        self.assertEqual(result["implicit_observation_inputs"], 1)
        self.assertEqual(result["drift_allowed_inputs"], 0)
        self.assertEqual(result["drift_observed_inputs"], 0)
        self.assertEqual(result["readback_available"], 1)
        _, evidence = ux.check_scenario("short", [explicit, implicit], None)
        self.assertEqual([view["row_id"] for view in evidence["observations"]], [2])
        self.assertEqual(evidence["status"], "review_required")
        self.assertEqual([explicit, implicit], original)

    def test_drift_allowed_and_drift_observed_inputs_are_counted_from_arguments_and_receipts(self):
        def input_call(identifier, allow, receipt, failed=False):
            call = row(identifier, "calm.terminal.input")
            item = call["params"]["item"]
            item["arguments"].update({"action": {"type": "key", "key": "Escape"}, "observation_id": "obs-1",
                                      "request_id": f"r{identifier}"})
            if allow is not None:
                item["arguments"]["allow_output_since_observation"] = allow
            item["result"] = {"structuredContent": {"terminal_id": "t1", "request_id": f"r{identifier}",
                                                    "outcome": "written", "application_result": "unverified",
                                                    "observation_id_used": "obs-1", **receipt}}
            if failed:
                item["status"], item["error"] = "failed", {"message": "terminal changed since observation; observe again"}
            return call
        calls = [
            input_call(1, True, {"output_since_observation": True,
                                 "observation_drift": {"observed_revision": 7, "input_revision": 9}}),
            input_call(2, True, {"output_since_observation": False}),
            input_call(3, None, {"output_since_observation": False}),
            input_call(4, False, {"output_since_observation": True}),
            # Refused: the flag was requested, but no receipt reports drift.
            input_call(5, True, {"output_since_observation": True}, failed=True),
            # Older server: receipt has no drift field at all; nothing is inferred.
            input_call(6, True, {}),
        ]
        result = ux.metrics(calls)
        self.assertEqual(result["drift_allowed_inputs"], 4)
        self.assertEqual(result["drift_observed_inputs"], 2)
        self.assertEqual(result["implicit_observation_inputs"], 0)
        self.assertEqual(result["tool_errors"], 1)
        self.assertEqual(result["observation_refusals"], 1)

    def test_review_wait_summary_is_the_per_scenario_counter_subset(self):
        observed = row(1)
        observed["params"]["item"]["arguments"]["wait_for"] = "change"
        observed["params"]["item"]["result"]["structuredContent"].update({
            "wait": {"mode": "change", "outcome": "unchanged", "waited_ms": 5000, "settled": False},
            "changed_since_previous_observation": False})
        result = ux.metrics([observed])
        summary = {key: result[key] for key in ux.SUMMARY_METRIC_KEYS}
        self.assertEqual(summary, {"change_wait_requests": 1, "change_wait_outcomes": {"unchanged": 1},
                                   "unsettled_change_waits": 0, "elapsed_wait_requests": 0,
                                   "unmeasured_wait_observations": 0, "drift_allowed_inputs": 0,
                                   "drift_observed_inputs": 0, "implicit_observation_inputs": 0,
                                   "signal_wait_requests": 0, "signal_wait_outcomes": {}, "submit_actions": 0,
                                   "open_with_claim": 0, "hooks_seen_observations": 0, "signals_observed": 0,
                                   "unmeasured_signal_observations": 1})
        self.assertEqual(json.loads(json.dumps(summary)), summary)
        self.assertEqual(ux.SUMMARY_METRIC_KEYS, ux.WAIT_METRIC_KEYS + ux.SIGNAL_METRIC_KEYS)

    # #1620 hook-signal counters.
    @staticmethod
    def signals(hooks_seen=True, events=()):
        return {"hooks_seen": hooks_seen, "last_seq": 7, "since_previous_observation": [
            {"seq": 7 - len(events) + index + 1, "event": event, "notification_type": None,
             "message": None, "received_at_ms": 1780977421069} for index, event in enumerate(events)]}

    def signal_wait_state(self, outcome="signal", event="Stop"):
        state = row(1)["params"]["item"]["result"]["structuredContent"]
        wait = {"mode": "signal", "outcome": outcome, "waited_ms": 812, "settled": True}
        if outcome == "signal":
            wait["signal"] = {"seq": 7, "event": event, "notification_type": None, "message": None,
                              "received_at_ms": 1780977421069}
        state.update({"wait": wait, "signals": self.signals(True, (event,))})
        return state

    def test_signal_wait_requests_and_outcomes_are_read_from_arguments_and_observations(self):
        signalled = row(1)
        signalled["params"]["item"]["arguments"].update({"wait_for": "signal", "signal_events": ["Stop"]})
        signalled["params"]["item"]["result"]["structuredContent"] = self.signal_wait_state()
        budget = row(2)
        budget["params"]["item"]["arguments"].update({"wait_for": "signal", "wait_ms": 5000})
        budget["params"]["item"]["result"]["structuredContent"] = self.signal_wait_state("elapsed")
        readback = row(3, "calm.terminal.input")
        readback["params"]["item"]["arguments"].update({"action": {"type": "submit", "text": "3100 + 41"},
                                                         "observe": True, "wait_for": "signal"})
        readback["params"]["item"]["result"] = {"structuredContent": {
            "terminal_id": "t1", "request_id": "r3", "outcome": "written", "application_result": "unverified",
            "observation": {"status": "available", "state": self.signal_wait_state()}}}
        # Refused signal wait: a request, but no observation and no outcome.
        refused = row(4)
        refused["params"]["item"]["arguments"]["wait_for"] = "signal"
        refused["params"]["item"]["status"], refused["params"]["item"]["error"] = "failed", {"message": "unsupported"}
        # wait_for=signal on a control call without observe=true requests no observation.
        control = row(5, "calm.terminal.control")
        control["params"]["item"]["arguments"].update({"action": "claim", "wait_for": "signal"})
        control["params"]["item"]["result"] = {"structuredContent": {"terminal_id": "t1", "connection_id": "c1", "control_id": "o1"}}
        # A change wait that happens to observe a signal outcome stays in the change tally only.
        change = row(6)
        change["params"]["item"]["arguments"]["wait_for"] = "change"
        change["params"]["item"]["result"]["structuredContent"] = self.signal_wait_state("changed")
        calls = [signalled, budget, readback, refused, control, change]
        original = copy.deepcopy(calls)
        result = ux.metrics(calls)
        self.assertEqual(result["signal_wait_requests"], 4)
        self.assertEqual(result["signal_wait_outcomes"], {"elapsed": 1, "signal": 2})
        self.assertEqual(result["change_wait_requests"], 1)
        self.assertEqual(result["change_wait_outcomes"], {"changed": 1})
        self.assertEqual(result["submit_actions"], 1)
        self.assertEqual(result["readback_available"], 1)
        self.assertEqual(result["tool_errors"], 1)
        self.assertEqual(result["unmeasured_signal_observations"], 0)
        self.assertEqual(calls, original)
        # Missing wait_for: neither a request nor an outcome, even with a signal-shaped result.
        plain = row(7)
        plain["params"]["item"]["result"]["structuredContent"] = self.signal_wait_state()
        result = ux.metrics([plain])
        self.assertEqual(result["signal_wait_requests"], 0)
        self.assertEqual(result["signal_wait_outcomes"], {})

    def test_signal_wait_on_pre_1618_observation_is_a_request_without_outcome(self):
        requested = row(1)
        requested["params"]["item"]["arguments"]["wait_for"] = "signal"
        result = ux.metrics([requested])
        self.assertEqual(result["signal_wait_requests"], 1)
        self.assertEqual(result["signal_wait_outcomes"], {})
        self.assertEqual(result["unmeasured_wait_observations"], 1)
        self.assertEqual(result["unmeasured_signal_observations"], 1)

    def test_submit_actions_are_counted_from_input_arguments_only(self):
        calls = []
        for identifier, action in enumerate(({"type": "submit", "text": "/rewind"},
                                             {"type": "text", "text": "/rewind"},
                                             {"type": "key", "key": "Enter"},
                                             {"type": "submit", "text": "again"}), start=1):
            call = row(identifier, "calm.terminal.input")
            call["params"]["item"]["arguments"]["action"] = action
            calls.append(call)
        failed = row(5, "calm.terminal.input")
        failed["params"]["item"]["arguments"]["action"] = {"type": "submit", "text": "refused"}
        failed["params"]["item"]["status"], failed["params"]["item"]["error"] = "failed", {"message": "no observation on this connection; observe first"}
        calls.append(failed)
        result = ux.metrics(calls)
        self.assertEqual(result["submit_actions"], 3)
        self.assertEqual(result["repeated_identical_input_actions"], 0)
        self.assertEqual(result["observation_refusals"], 1)
        self.assertEqual(ux.metrics([row(1), row(2, "calm.terminal.input")])["submit_actions"], 0)

    def test_open_with_claim_counts_only_true_claim_arguments(self):
        calls = []
        for identifier, arguments in enumerate(({"claim": True}, {"claim": False}, {}, {"claim": "true"}), start=1):
            opened = row(identifier, "calm.terminal.open")
            opened["params"]["item"]["arguments"] = {"command": "claude", **arguments}
            opened["params"]["item"]["result"]["structuredContent"].update({"control_id": "o1", "role": "owner"})
            calls.append(opened)
        unavailable = row(5, "calm.terminal.open")
        unavailable["params"]["item"]["arguments"] = {"command": "claude", "claim": True}
        unavailable["params"]["item"]["result"]["structuredContent"]["claim"] = {"status": "unavailable", "reason": "owned by a human"}
        calls.append(unavailable)
        result = ux.metrics(calls)
        self.assertEqual(result["open_with_claim"], 2)
        self.assertEqual(result["terminal_tool_calls"], 5)
        self.assertEqual(result["tool_errors"], 0)

    def test_hooks_seen_and_signals_observed_are_read_from_observation_signals(self):
        seen = row(1)
        seen["params"]["item"]["result"]["structuredContent"]["signals"] = self.signals(True, ("UserPromptSubmit", "PreToolUse", "Stop"))
        silent = row(2)
        silent["params"]["item"]["result"]["structuredContent"]["signals"] = self.signals(False)
        state = row(3)["params"]["item"]["result"]["structuredContent"]
        state["signals"] = self.signals(True, ("Notification",))
        readback = row(3, "calm.terminal.input")
        readback["params"]["item"]["arguments"].update({"action": {"type": "submit", "text": "hi"}, "observe": True})
        readback["params"]["item"]["result"] = {"structuredContent": {
            "terminal_id": "t1", "request_id": "r3", "outcome": "written", "application_result": "unverified",
            "observation": {"status": "available", "state": state}}}
        # Pre-#1620 server: no signals block is unmeasured, never inferred.
        older = row(4)
        # Open results and unavailable readbacks are not observations here.
        opened = row(5, "calm.terminal.open")
        opened["params"]["item"]["result"]["structuredContent"]["signals"] = self.signals(True, ("Stop",))
        unavailable = row(6, "calm.terminal.input")
        unavailable["params"]["item"]["arguments"].update({"action": {"type": "key", "key": "Enter"}, "observe": True})
        unavailable["params"]["item"]["result"] = {"structuredContent": {
            "terminal_id": "t1", "request_id": "r6", "outcome": "written", "application_result": "unverified",
            "observation": {"status": "unavailable", "reason": "readback timeout"}}}
        calls = [seen, silent, readback, older, opened, unavailable]
        original = copy.deepcopy(calls)
        result = ux.metrics(calls)
        self.assertEqual(result["hooks_seen_observations"], 2)
        self.assertEqual(result["signals_observed"], 4)  # 3 + 0 + 1 events over 3 measured observations
        self.assertEqual(result["unmeasured_signal_observations"], 1)
        self.assertEqual(result["signal_wait_requests"], 0)
        self.assertEqual(calls, original)
        self.assertEqual(ux.metrics([older])["hooks_seen_observations"], 0)
        self.assertEqual(ux.metrics([older])["signals_observed"], 0)

    def test_malformed_signals_block_is_rejected(self):
        for signals in ([], "seen", {"hooks_seen": True}, {"hooks_seen": "yes", "last_seq": 1, "since_previous_observation": []},
                        {"hooks_seen": True, "last_seq": "1", "since_previous_observation": []},
                        {"hooks_seen": True, "last_seq": 1, "since_previous_observation": {}},
                        {"hooks_seen": True, "last_seq": 1, "since_previous_observation": ["Stop"]}):
            observed = row(1)
            observed["params"]["item"]["result"]["structuredContent"]["signals"] = signals
            with self.subTest(signals=signals), self.assertRaisesRegex(ux.EvidenceError, "signals"):
                ux.metrics([observed])

    def test_signal_wait_outcome_is_an_accepted_observation(self):
        signalled = row(1, text=["松果 9123"])
        signalled["params"]["item"]["arguments"].update({"wait_for": "signal", "signal_events": ["Stop"]})
        state = self.signal_wait_state()
        state["text"] = ["松果 9123"]
        signalled["params"]["item"]["result"]["structuredContent"] = state
        original = copy.deepcopy(signalled)
        binding, observations, _, errors = ux.terminal_evidence([signalled])
        self.assertEqual(binding["terminal_id"], "t1")
        self.assertEqual([view["text"] for view in observations], ["松果 9123"])
        self.assertEqual(errors, [])
        self.assertEqual(signalled, original)
        submit = row(2, "calm.terminal.input")
        submit["params"]["item"]["arguments"]["action"] = {"type": "submit", "text": "/rewind"}
        _, evidence = ux.check_scenario("rewind", [signalled, submit], None)
        self.assertEqual(evidence["status"], "review_required")
        self.assertEqual([view["row_id"] for view in evidence["observations"]], [1])

    def test_submit_action_satisfies_scenario_input_checks_like_text_plus_enter(self):
        for name, text in (("short", "请只回答 3100 + 41 的结果。"), ("edit", "请只回答 7200 + 19 的结果。"), ("rewind", "/rewind ")):
            answer = {"short": "3141", "edit": "7219", "rewind": "9123"}[name]
            submit = row(2, "calm.terminal.input")
            submit["params"]["item"]["arguments"]["action"] = {"type": "submit", "text": text}
            correction = row(3, "calm.terminal.input")
            correction["params"]["item"]["arguments"]["action"] = {"type": "key", "key": "Backspace"}
            with self.subTest(name=name):
                _, evidence = ux.check_scenario(name, [row(1, text=[answer]), submit, correction], None)
                self.assertEqual(evidence["status"], "review_required")
        # A submit of other text is not a rewind; a submit without text is tolerated but is not one either.
        for action in ({"type": "submit", "text": "/help"}, {"type": "submit"}):
            other = row(2, "calm.terminal.input")
            other["params"]["item"]["arguments"]["action"] = action
            with self.subTest(action=action), self.assertRaisesRegex(ux.EvidenceError, "actual /rewind input absent"):
                ux.check_scenario("rewind", [row(1, text=["9123"]), other], None)

    def test_changed_since_observation_production_refusal_is_counted(self):
        bad = row(1, "calm.terminal.input")
        bad["params"]["item"]["status"] = "failed"
        bad["params"]["item"]["error"] = {"message": "terminal changed since observation; observe again"}
        self.assertEqual(ux.metrics([bad])["observation_refusals"], 1)

    def test_observation_refusal_variants_and_error_envelopes(self):
        for message in ("observation expired; observe again",
                        "observation belongs to another connection or expired",
                        "terminal changed since observation; observe again",
                        "terminal surface changed since observation (size, input modes or alternate screen); observe again",
                        "no observation on this connection; observe first"):
            for envelope in ("error", "result", "both"):
                failed = row(1, "calm.terminal.input")
                item = failed["params"]["item"]
                if envelope in ("error", "both"):
                    item["status"] = "failed"
                    item["error"] = {"message": f"MCP error: -32403: {message}"}
                if envelope in ("result", "both"):
                    item["result"] = {"isError": True, "content": [{"type": "text", "text": message}]}
                with self.subTest(message=message, envelope=envelope):
                    self.assertEqual(ux.metrics([failed])["observation_refusals"], 1)

    def test_refusal_metric_ignores_unrelated_errors_and_success_output(self):
        unrelated = row(1, "calm.terminal.input")
        unrelated["params"]["item"]["error"] = {"message": "observation renderer changed resolution"}
        other_tool = row(2, "calm.track.cat")
        other_tool["params"]["item"]["error"] = {"message": "observation expired; observe again"}
        success = row(3, "calm.terminal.input")
        # A successful kernel result carries JSON metadata, even when some
        # returned text happens to quote a refusal message.
        data = {"terminal_id": "t1", "outcome": "written", "next": "terminal changed since observation; observe again"}
        success["params"]["item"]["result"] = {"isError": False, "structuredContent": data, "content": [
            {"type": "text", "text": json.dumps(data)}]}
        self.assertEqual(ux.metrics([unrelated, other_tool, success])["observation_refusals"], 0)

    def test_main_preserves_incomplete_transcript_when_wait_turn_metrics_rejects_shapes(self):
        for field, value in (("arguments", "{}"), ("arguments", None),
                             ("arguments", {"action": []}),
                             ("arguments", {"action": {"type": "text", "text": ["bad"]}}),
                             ("result", ["malformed"]), ("result", []),
                             ("result", {"content": [{"type": "image", "data": "invalid"}]}),
                             ("result", {"content": [{"type": "image", "data": []}]})):
            bad = row(1, "calm.terminal.input")
            bad["params"]["item"][field] = value
            final = {"id": 2, "worker_session_id": "planner1", "method": "item/completed",
                     "params": {"item": {"id": "final", "type": "agentMessage",
                                         "phase": "final_answer", "text": "Done"}}}
            wire = [{**item, "params": json.dumps(item["params"])} for item in [bad, final]]

            class ApiResponses:
                def call(self, method, path):
                    if "/harness/items?" in path:
                        return copy.deepcopy(wire)
                    return {"worker_session_id": "planner1", "phase": "turn_completed"}

            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                argv = ["collector", "--url", "http://127.0.0.1:4900", "--workspace", "/synthetic",
                        "--claude-bin", "/bin/claude", "--claude-version", "test", "--codex-version", "test",
                        "--source-sha", "test", "--artifacts", directory]
                # Enter the real wait_turn and top-level failure writer without
                # creating a Planner, invoking a model or imitating its behavior.
                with patch.object(ux, "Api", return_value=ApiResponses()), \
                     patch.object(ux.Round, "run", lambda self: self.wait_turn("malformed")), \
                     patch.object(ux.sys, "argv", argv), patch.object(ux.sys, "stdin", io.StringIO()), \
                     patch.object(ux.sys, "stderr", io.StringIO()):
                    self.assertEqual(ux.main(), 1)
                artifact = json.loads((Path(directory) / "incomplete.json").read_text())
                self.assertEqual(artifact["status"], "incomplete")
                self.assertEqual(artifact["transcript"], ux.scrub([bad, final]))

    def test_production_terminal_text_rows_are_joined_without_mutating_transcript(self):
        observed = row(1)
        # Frame.text: Vec<String>, emitted directly by terminal_interaction::observe.
        observed["params"]["item"]["result"]["structuredContent"]["text"] = ["Claude", "3141", ""]
        original = copy.deepcopy(observed)
        _, evidence = ux.check_scenario("short", [observed], None)
        self.assertEqual(evidence["observations"][0]["text"], "Claude\n3141\n")
        self.assertEqual(observed, original)

    def test_no_terminal_calls_does_not_turn_prose_into_success(self):
        for rows in ([], [row(1, "calm.track.cat")]):
            with self.subTest(rows=rows), self.assertRaisesRegex(ux.EvidenceError, "no actual Planner"):
                ux.terminal_evidence(rows)

    def test_terminal_text_rejects_scalar_and_nonstring_rows(self):
        for malformed in ("3141", [3141], ["3141", None], None, {}):
            observed = row(1)
            observed["params"]["item"]["result"]["structuredContent"]["text"] = malformed
            with self.subTest(text=malformed), self.assertRaisesRegex(ux.EvidenceError, "array of strings"):
                ux.terminal_evidence([observed])

    def test_completed_agent_message_uses_real_item_phase_and_text(self):
        # Current app-server shape captured in plannerChatItems.test.ts; the
        # kernel persists item fields verbatim, including phase.
        final = {"id": 1, "method": "item/completed", "params": {"completedAtMs": 1780977421069,
                 "item": {"id": "msg_agent", "phase": "final_answer", "text": "Done", "type": "agentMessage"},
                 "threadId": "thread", "turnId": "turn"}}
        commentary = copy.deepcopy(final)
        commentary["params"]["item"]["phase"] = "commentary"
        started = copy.deepcopy(final)
        started["method"] = "item/started"
        missing_phase = copy.deepcopy(final)
        missing_phase["params"]["item"].pop("phase")
        self.assertEqual(ux.final_texts([commentary, started, missing_phase, final]), ["Done"])

    def test_session_replacement_is_rejected(self):
        binding, *_ = ux.terminal_evidence([row(1)])
        for key in ("terminal_id", "terminal_session_id", "worker_session_id"):
            changed = row(2)
            changed["params"]["item"]["result"]["structuredContent"][key] = "different"
            with self.subTest(key=key), self.assertRaisesRegex(ux.EvidenceError, "session changed"):
                ux.terminal_evidence([changed], binding)

    def test_started_arguments_survive_completed_frame(self):
        started = row(1, "calm.terminal.input")
        started["method"] = "item/started"
        started["params"]["item"].pop("result")
        finished = copy.deepcopy(started)
        finished["id"], finished["method"] = 2, "item/completed"
        finished["params"]["item"].pop("arguments")
        calls = ux.completed_calls([started, finished])
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]["arguments"], {"terminal_id": "t1"})
        self.assertTrue(calls[0]["completed"])

    def test_malformed_result_is_not_an_empty_observation(self):
        for result in (None, {"structuredContent": []}, {"content": ["bad"]},
                       {"content": [{"type": "text", "text": "not json"}]}):
            bad = row(1)
            bad["params"]["item"]["result"] = result
            with self.subTest(result=result), self.assertRaises(ux.EvidenceError):
                ux.terminal_evidence([bad])

    def test_summary_only_result_is_rejected_not_parsed_as_metadata(self):
        value = row(1)["params"]["item"]
        metadata = value["result"]["structuredContent"]
        for text in (json.dumps(metadata), "terminal t1 observation o1 revision 3 owner 80x24 cursor 0,0 wait elapsed; full state in structuredContent"):
            value["result"] = {"content": [{"type": "text", "text": text}]}
            with self.subTest(text=text), self.assertRaisesRegex(ux.EvidenceError, "lacks structuredContent; content is a summary"):
                ux.metadata(value)

    def test_drift_and_implicit_observation_refusals_are_counted_exactly(self):
        for message, counted in (
                ("terminal surface changed since observation (size, input modes or alternate screen); observe again", 1),
                ("no observation on this connection; observe first", 1),
                ("terminal control changed; observe before input", 0),
                ("observe first", 0)):
            failed = row(1, "calm.terminal.input")
            failed["params"]["item"]["status"] = "failed"
            failed["params"]["item"]["error"] = {"message": f"MCP error: -32403: {message}"}
            with self.subTest(message=message):
                self.assertEqual(ux.metrics([failed])["observation_refusals"], counted)

    def test_stale_observation_result_counts_as_refusal_and_its_fresh_state_is_evidence(self):
        state = row(1)["params"]["item"]["result"]["structuredContent"]
        state.update({"observation_id": "fresh", "previous_observation_revision": "41",
                      "wait": {"mode": "elapsed", "outcome": "elapsed", "waited_ms": 0, "settled": False,
                               "baseline_revision": "41"}})
        stale = row(2, "calm.terminal.input")
        stale["params"]["item"]["arguments"]["action"] = {"type": "key", "key": "Enter"}
        stale["params"]["item"]["result"] = {"structuredContent": {
            "terminal_id": "t1", "request_id": "enter", "outcome": "stale_observation",
            "application_result": "unverified", "observation_id_used": "old",
            "observed_revision": 41, "current_revision": 42, "next": "inspect observation.state",
            "observation": {"status": "available", "state": state}}}
        original = copy.deepcopy(stale)
        result = ux.metrics([stale])
        self.assertEqual(result["observation_refusals"], 1)
        self.assertEqual(result["tool_errors"], 0)
        self.assertEqual(result["readback_available"], 1)
        self.assertEqual(result["implicit_observation_inputs"], 1)
        self.assertEqual(result["drift_observed_inputs"], 0)
        # The fresh observation is real terminal evidence, like any readback.
        binding, observations, _, errors = ux.terminal_evidence([stale])
        self.assertEqual(binding["terminal_id"], "t1")
        self.assertEqual([view["row_id"] for view in observations], [2])
        self.assertEqual(errors, [])
        self.assertEqual(stale, original)
        # Both refusal shapes in one round add up; a written receipt does not.
        failed = row(3, "calm.terminal.input")
        failed["params"]["item"]["status"] = "failed"
        failed["params"]["item"]["error"] = {"message": "terminal changed since observation; observe again"}
        written = row(4, "calm.terminal.input")
        written["params"]["item"]["result"] = {"structuredContent": {
            "terminal_id": "t1", "request_id": "enter", "outcome": "written", "application_result": "unverified"}}
        self.assertEqual(ux.metrics([stale, failed, written])["observation_refusals"], 2)

    def test_stale_observation_outcome_is_not_counted_on_failed_or_started_calls(self):
        for status, completed in (("failed", True), ("completed", False)):
            call = row(1, "calm.terminal.input")
            item = call["params"]["item"]
            item["result"] = {"structuredContent": {"terminal_id": "t1", "outcome": "stale_observation"}}
            if status == "failed":
                item["status"] = "failed"
                item["error"] = {"message": "unrelated failure"}
            if not completed:
                call["method"] = "item/started"
            with self.subTest(status=status, completed=completed):
                self.assertEqual(ux.metrics([call])["observation_refusals"], 0)
        other = row(2, "calm.terminal.control")
        other["params"]["item"]["arguments"]["action"] = "claim"
        other["params"]["item"]["result"] = {"structuredContent": {"terminal_id": "t1", "outcome": "stale_observation"}}
        self.assertEqual(ux.metrics([other])["observation_refusals"], 0)

    def test_release_readback_without_text_is_tolerated_but_adds_no_observation(self):
        state = row(1)["params"]["item"]["result"]["structuredContent"]
        del state["text"]
        state.update({"observation_id": "o2", "text_omitted": "unchanged since previous observation o1"})
        released = row(2, "calm.terminal.control")
        released["params"]["item"]["arguments"]["action"] = "release"
        released["params"]["item"]["result"] = {"structuredContent": {
            "terminal_id": "t1", "connection_id": "c1", "control_id": None,
            "observation": {"status": "available", "state": state}}}
        binding, observations, calls, errors = ux.terminal_evidence([row(1), released])
        self.assertEqual([view["row_id"] for view in observations], [1])
        self.assertEqual(len(calls), 2)
        self.assertEqual(errors, [])
        self.assertEqual(ux.metrics([row(1), released])["readback_available"], 1)
        # The identity checks still apply to a text-less state.
        foreign = copy.deepcopy(released)
        foreign["params"]["item"]["result"]["structuredContent"]["observation"]["state"]["terminal_session_id"] = "other"
        with self.assertRaisesRegex(ux.EvidenceError, "terminal or session changed"):
            ux.terminal_evidence([row(1), foreign])
        # Without text_omitted a missing or malformed text is still an error,
        # and text_omitted must be a string.
        for patch_state in ({"text_omitted": None}, {"text_omitted": 7}, {}):
            broken = copy.deepcopy(released)
            broken_state = broken["params"]["item"]["result"]["structuredContent"]["observation"]["state"]
            broken_state.pop("text_omitted", None)
            broken_state.update(patch_state)
            with self.subTest(patch=patch_state), self.assertRaisesRegex(ux.EvidenceError, "text must be an array"):
                ux.terminal_evidence([row(1), broken])
        # Text-less states alone are not a successful observation.
        with self.assertRaisesRegex(ux.EvidenceError, "no successful terminal observations"):
            ux.terminal_evidence([released])

    def test_tool_errors_retained_as_review_findings(self):
        bad = row(2, "calm.terminal.input")
        bad["params"]["item"]["status"] = "failed"
        bad["params"]["item"]["error"] = {"message": "observation expired; observe again"}
        _, evidence = ux.check_scenario("short", [row(1), bad], None)
        self.assertEqual(evidence["status"], "review_required")
        self.assertEqual(evidence["tool_error_rows"], [2])
        self.assertEqual(ux.metrics([bad])["observation_refusals"], 1)
        with self.assertRaisesRegex(ux.EvidenceError, "no successful terminal"):
            ux.terminal_evidence([bad])

    def test_prompt_echo_without_actual_answer_is_incomplete(self):
        with self.assertRaisesRegex(ux.EvidenceError, "answer absent"):
            ux.check_scenario("short", [row(1, text=["请只回答 3100 + 41 的结果"])], None)

    def test_rewind_answer_without_rewind_action_is_incomplete(self):
        with self.assertRaisesRegex(ux.EvidenceError, "actual /rewind input absent"):
            ux.check_scenario("rewind", [row(1, text=["松果 9123"])], None)

    def test_actual_rewind_action_still_requires_review(self):
        action = row(2, "calm.terminal.input")
        action["params"]["item"]["arguments"]["action"] = {"type": "text", "text": "/rewind"}
        _, evidence = ux.check_scenario("rewind", [row(1, text=["松果 9123"]), action], None)
        self.assertEqual(evidence["status"], "review_required")

    def test_paginated_rows_are_complete_and_cursor_advances(self):
        class Pages:
            def __init__(self):
                self.paths = []

            def call(self, method, path):
                self.paths.append(path)
                rows = [row(i) for i in range(1, 501)] if len(self.paths) == 1 else [row(501)]
                # Transcript params are JSON encoded inside the REST JSON row
                # (crates/calm-types/src/model.rs), not an embedded object.
                return [{**item, "params": json.dumps(item["params"])} for item in rows]

        pages = Pages()
        self.assertEqual(len(ux.read_items(pages, "card", 0)), 501)
        self.assertIn("after_id=500", pages.paths[1])

    def test_malformed_or_repeated_page_fails(self):
        class Page:
            def __init__(self, page):
                self.page = page

            def call(self, *_):
                return self.page

        for page in ({}, [row(0)], [{"id": 1, "params": "bad"}], [row(2), row(1)]):
            with self.subTest(page=page), self.assertRaises(ux.EvidenceError):
                ux.read_items(Page(page), "card", 0)

    def test_malformed_wire_row_retains_preceding_rows_and_bad_payload(self):
        good = row(1)
        bad = {"id": 2, "params": "invalid json"}

        class Page:
            def call(self, *_):
                return [{**good, "params": json.dumps(good["params"])}, bad]

        with self.assertRaises(ux.EvidenceError) as raised:
            ux.read_items(Page(), "card", 0)
        self.assertEqual(raised.exception.payload, {"earlier_rows": [good], "row": bad})

    def test_sanitization_removes_credentials_and_image_bytes(self):
        value = ux.scrub({"authorization": "private", "content": [
            {"type": "image", "data": "YWJj", "mimeType": "image/png"},
            {"type": "text", "text": "Bearer private https://example.test/?token=secret sk-1234567890123"}]})
        self.assertEqual(value["authorization"], "[REDACTED]")
        self.assertNotIn("data", value["content"][0])
        self.assertEqual(value["content"][0]["bytes"], 3)
        self.assertNotIn("private", str(value))
        self.assertNotIn("token=secret", str(value))

    def test_api_rejects_remote_or_credential_bearing_origin(self):
        for origin in ("https://example.test", "http://127.0.0.1:4900/api", "http://u:p@127.0.0.1:4900"):
            with self.subTest(origin=origin), self.assertRaises(ux.EvidenceError):
                ux.Api(origin, "private")


if __name__ == "__main__":
    unittest.main()
