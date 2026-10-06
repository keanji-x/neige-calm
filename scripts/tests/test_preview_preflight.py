"""Exercise real Make entry points with Go absent and costly tools fenced off."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class PreviewPreflight(unittest.TestCase):
    def test_build_preflight_orders_go_before_expensive_tools(self):
        with tempfile.TemporaryDirectory() as directory:
            sandbox = Path(directory)
            tools = sandbox / "bin"
            tools.mkdir()
            for name in ("find", "uname", "awk", "mkdir", "dirname", "bash", "chmod", "python3", "make"):
                (tools / name).symlink_to(shutil.which(name))
            marker = sandbox / "expensive"
            for name in ("cargo", "node", "npm"):
                tool = tools / name
                tool.write_text(f'#!/bin/sh\nprintf started >> "{marker}"\nexit 91\n')
                tool.chmod(0o755)
            docker = tools / "docker"
            docker.write_text('#!/bin/sh\nprintf \'{"services":{"server":{"environment":{}}}}\\n\'\n')
            docker.chmod(0o755)
            env = dict(os.environ, PATH=str(tools))
            for name in ("MAKEFLAGS", "BASH_ENV", "ENV"):
                env.pop(name, None)
            for go_available in (False, True):
                if go_available:
                    go = tools / "go"
                    go.write_text("#!/bin/sh\nexit 0\n")
                    go.chmod(0o755)
                for target in ("build", "dev-bundles", "dev", "dev-fresh"):
                    for jobs in (1, 4):
                        with self.subTest(go_available=go_available, target=target, jobs=jobs):
                            marker.unlink(missing_ok=True)
                            run = subprocess.run(
                                [str(tools / "make"), f"-j{jobs}", "-B", target, "CALM_PORT=4315",
                                 f"XDG_DIRS={sandbox / 'state'}",
                                 "LOCAL_SHELL=/bin/sh", "CALM_HOST_PROXY_PORT=",
                                 "CALM_CODEX_HOST_BIN=/bin/true", "CALM_CODEX_CODE_MODE_HOST_BIN=/bin/true"],
                                cwd=ROOT, env=env, text=True, capture_output=True, timeout=15,
                            )
                            self.assertNotEqual(run.returncode, 0)
                            if go_available:
                                self.assertTrue(marker.exists(), run.stdout + run.stderr)
                                self.assertNotIn("Go toolchain is missing from PATH", run.stdout + run.stderr)
                            else:
                                self.assertIn("Go toolchain is missing from PATH", run.stdout + run.stderr)
                                self.assertFalse(marker.exists(), run.stdout + run.stderr)


if __name__ == "__main__":
    unittest.main()
