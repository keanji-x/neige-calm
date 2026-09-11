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
                "application_completed": False, "observation": observation}}
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
                 {"terminal_id": "t1", "request_id": "r1", "outcome": "written", "application_completed": False}),
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
        receipt = {"terminal_id": "t1", "request_id": "r1", "outcome": "written", "application_completed": False,
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
        # {detached:true}, while claim/release return connection/control IDs.
        for action, result in (("claim", {"terminal_id": "t1", "connection_id": "c1", "control_id": "owner1"}),
                               ("release", {"terminal_id": "t1", "connection_id": "c1", "control_id": None}),
                               ("detach", {"detached": True})):
            control = row(2, "calm.terminal.control")
            control["params"]["item"]["arguments"]["action"] = action
            control["params"]["item"]["result"] = {"structuredContent": result}
            with self.subTest(action=action):
                self.assertEqual(ux.metrics([row(1), control])["terminal_tool_calls"], 2)
                _, _, calls, errors = ux.terminal_evidence([row(1), control])
                self.assertEqual(len(calls), 2)
                self.assertEqual(errors, [])

    def test_changed_since_observation_production_refusal_is_counted(self):
        bad = row(1, "calm.terminal.input")
        bad["params"]["item"]["status"] = "failed"
        bad["params"]["item"]["error"] = {"message": "terminal changed since observation; observe again"}
        self.assertEqual(ux.metrics([bad])["observation_refusals"], 1)

    def test_observation_refusal_variants_and_error_envelopes(self):
        for message in ("observation expired; observe again",
                        "observation belongs to another connection or expired",
                        "terminal changed since observation; observe again"):
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

    def test_text_only_mcp_metadata_is_parsed(self):
        import json
        value = row(1)["params"]["item"]
        metadata = value["result"]["structuredContent"]
        value["result"] = {"content": [{"type": "text", "text": json.dumps(metadata)}]}
        self.assertEqual(ux.metadata(value), metadata)

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
                # HarnessItem.params is JSON encoded inside the REST JSON row
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
