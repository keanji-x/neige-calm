"""Model-free checks of evidence collection, never a fake Planner acceptance."""

import copy
import json
import unittest

import planner_claude_ux as ux


def row(identifier, tool="calm.terminal.observe", *, text="3141", terminal="t1"):
    return {"id": identifier, "method": "item/completed", "worker_session_id": "planner1",
            "params": {"item": {"id": f"call-{identifier}", "type": "mcpToolCall",
                                "tool": tool, "status": "completed", "arguments": {"terminal_id": terminal},
                                "result": {"structuredContent": {"terminal_id": terminal,
                                                                 "terminal_session_id": "pty1",
                                                                 "worker_session_id": "worker1", "text": text}}}}}


class CollectorTests(unittest.TestCase):
    def test_no_terminal_calls_does_not_turn_prose_into_success(self):
        for rows in ([], [row(1, "calm.track.cat")]):
            with self.subTest(rows=rows), self.assertRaisesRegex(ux.EvidenceError, "no actual Planner"):
                ux.terminal_evidence(rows)

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
            ux.check_scenario("short", [row(1, text="请只回答 3100 + 41 的结果")], None)

    def test_rewind_answer_without_rewind_action_is_incomplete(self):
        with self.assertRaisesRegex(ux.EvidenceError, "actual /rewind input absent"):
            ux.check_scenario("rewind", [row(1, text="松果 9123")], None)

    def test_actual_rewind_action_still_requires_review(self):
        action = row(2, "calm.terminal.input")
        action["params"]["item"]["arguments"]["action"] = {"type": "text", "text": "/rewind"}
        _, evidence = ux.check_scenario("rewind", [row(1, text="松果 9123"), action], None)
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
