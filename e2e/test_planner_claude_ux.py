"""Model-free checks of evidence collection, never a fake Planner acceptance."""

import copy
import json
import io
from pathlib import Path
import tempfile
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
