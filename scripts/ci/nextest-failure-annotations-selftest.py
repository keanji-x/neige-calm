#!/usr/bin/env python3
"""Exercise the actual renderer, including two captured nextest failure shards."""
import importlib.util
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
RENDERER = HERE / "nextest-failure-annotations.py"
spec = importlib.util.spec_from_file_location("renderer", RENDERER)
renderer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(renderer)


class RendererTests(unittest.TestCase):
    def run_xml(self, xml):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "junit.xml"
            path.write_text(xml)
            return subprocess.run(["python3", str(RENDERER), str(path)], capture_output=True, text=True)

    def test_real_shards(self):
        titles = []
        for shard in (1, 2):
            result = subprocess.run(["python3", str(RENDERER), str(HERE / f"fixtures/nextest-failures/shard{shard}.xml")], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0)
            self.assertEqual(result.stdout.count("::error title=nextest failure%3A "), 1)
            self.assertNotIn("system-out", result.stdout)
            expected = ("gh_pr_checks_all_token_aliases_redacted_through_persistence_replay_wake" if shard == 1 else "gh_pr_checks_all_empty_deadline_is_partial")
            self.assertIn("calm-server%3A%3Aforge_template_e2e%3A%3Apr_checks%3A%3Adiagnostics%3A%3A" + expected, result.stdout)
            titles.append(result.stdout.split("::", 2)[1])
        self.assertNotEqual(*titles)

    def test_failure_error_and_green(self):
        result = self.run_xml('<testsuites><testsuite><testcase classname="bin" name="one"><failure message="short"/><error message="second">BODY_SECRET</error><system-out>LOG_SECRET</system-out></testcase><testcase classname="bin" name="two"><error/></testcase><testcase name="green"/></testsuite></testsuites>')
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.count("::error title="), 2)
        self.assertIn("nextest testcase failed (JUnit failure); see job log", result.stdout)
        self.assertNotIn("SECRET", result.stdout)
        result = self.run_xml('<testsuites><testsuite><testcase name="green"/></testsuite></testsuites>')
        self.assertNotIn("::error", result.stdout)
        self.assertIn("No failure/error", result.stdout)

    def test_escaping_and_incomplete(self):
        result = self.run_xml('<testsuite><testcase classname="bin,:" name="case%0A::error&#10;forged&#27;"><failure message="evil%0A::error&#13;&#10;token=secret"/></testcase><testcase name="missing-class"><error/></testcase></testsuite>')
        self.assertEqual(result.returncode, 1)  # XML forbids ESC; never reinterpret it.
        result = self.run_xml('<testsuite><testcase classname="bin,:" name="case%0A::error&#10;forged"><failure message="evil%0A::error&#13;&#10;token=secret"/></testcase><testcase name="missing-class"><error/></testcase></testsuite>')
        self.assertEqual(result.returncode, 0)
        self.assertNotIn("::error title=", result.stdout)
        self.assertNotIn("secret", result.stdout)
        self.assertEqual(len(result.stdout.splitlines()), 2)
        self.assertIn("truncated", result.stdout)
        self.assertEqual(renderer.escape("%\r\n:,", True), "%25%0D%0A%3A%2C")

    def test_limits(self):
        result = self.run_xml('<testsuite>' + ''.join(f'<testcase classname="bin" name="case{i}"><failure message="{chr(0x4e00) * 300}"/></testcase>' for i in range(100)) + '</testsuite>')
        self.assertLessEqual(result.stdout.count("::error title="), renderer.MAX_ANNOTATIONS)
        self.assertLessEqual(len(result.stdout.encode()), renderer.MAX_OUTPUT)
        self.assertIn("truncated", result.stdout)
        result = self.run_xml('<testsuite><testcase classname="bin" name="' + 'x' * 250 + '"><failure/></testcase></testsuite>')
        self.assertNotIn("::error title=", result.stdout)
        self.assertIn("truncated", result.stdout)

    def test_invalid_inputs(self):
        for xml in ('<testsuites>', '<unknown/>', '<!DOCTYPE x [<!ENTITY x "secret">]><testsuite/>', 'x' * (renderer.MAX_INPUT + 1)):
            result = self.run_xml(xml)
            self.assertEqual(result.returncode, 1)
            self.assertIn("test names not collected", result.stdout)
            self.assertNotIn("::error title=nextest", result.stdout)
        output, status = renderer.render(HERE / "does-not-exist.xml")
        self.assertEqual(status, 1)
        self.assertIn("missing", output)

    def test_sensitive_metadata_is_private(self):
        values = ("Authorization: Bearer fictional-opaque-credential", "https://user:fictional-password@logs.example/job", "https://logs.example/job?sig=fictional-signed-secret", "evil%0A::error forged")
        for value in values:
            for tag in ("failure", "error"):
                for field in ("message", "type", "body", "system-out", "classname", "name"):
                    attrs = {"classname": "bin", "name": "case"}
                    if field in attrs:
                        attrs[field] = value
                    failure_attrs = f' {field}="{value}"' if field in ("message", "type") else ""
                    body = value if field == "body" else ""
                    system = f"<system-out>{value}</system-out>" if field == "system-out" else ""
                    result = self.run_xml(f'<testsuite><testcase classname="{attrs["classname"]}" name="{attrs["name"]}"><{tag}{failure_attrs}>{body}</{tag}>{system}</testcase></testsuite>')
                    self.assertEqual(result.returncode, 0)
                    for secret in ("fictional-opaque-credential", "fictional-password", "fictional-signed-secret", "forged"):
                        self.assertNotIn(secret, result.stdout, (tag, field))
                    if field in attrs:
                        self.assertNotIn("::error title=", result.stdout)
                        self.assertIn("truncated", result.stdout)
                    else:
                        self.assertIn(f"nextest testcase failed (JUnit {tag}); see job log", result.stdout)

    def test_identity_protocol(self):
        for value in ("ghp_fictionalcredential", "case_ghp_fictionalcredential", "github_pat_fictionalcredential", "case_github_pat_fictionalcredential", "token::secret", "has space", "raw#identifier", "\u202eforged", "case,other", "case%0A", "case::error"):
            for field in ("classname", "name"):
                attrs = {"classname": "crate-name::test_binary", "name": "module::case"}
                attrs[field] = value
                result = self.run_xml(f'<testsuite><testcase classname="{attrs["classname"]}" name="{attrs["name"]}"><failure/></testcase></testsuite>')
                self.assertNotIn("::error title=", result.stdout)
                self.assertNotIn(value, result.stdout)
                self.assertIn("truncated", result.stdout)
        result = self.run_xml('<testsuite><testcase classname="crate-name::test_binary" name="module::case"><error/></testcase></testsuite>')
        self.assertIn("crate-name%3A%3Atest_binary%3A%3Amodule%3A%3Acase", result.stdout)

    def test_ci_both_paths_preserve_status(self):
        workflow = (HERE / "../../.github/workflows/ci.yml").read_text()
        self.assertEqual(workflow.count("id: rust_nextest"), 2)
        self.assertEqual(workflow.count("if: ${{ failure() && steps.rust_nextest.outcome == 'failure' }}"), 2)
        self.assertEqual(workflow.count("run: python3 scripts/ci/nextest-failure-annotations.py"), 2)
        self.assertNotIn("continue-on-error", workflow)
        lint = re.search(r"^  lint:\n(.*?)(?=^  [A-Za-z_][A-Za-z0-9_-]*:|\Z)", workflow, re.M | re.S).group(1)
        step = re.search(r"^      - name: nextest public annotation protocol selftest\n(.*?)(?=^      - |\Z)", lint, re.M | re.S).group(1)
        self.assertEqual(step.strip(), "run: python3 scripts/ci/nextest-failure-annotations-selftest.py")


if __name__ == "__main__":
    unittest.main()
