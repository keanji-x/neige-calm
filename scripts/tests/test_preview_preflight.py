"""Exercise real Make source discovery and preflight with costly tools fenced off."""
import os
from pathlib import Path
import shutil
import shlex
import time
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class PreviewPreflight(unittest.TestCase):
    def test_fe_build_uses_source_files_without_ignored_artifacts(self):
        with tempfile.TemporaryDirectory() as directory:
            sandbox = Path(directory)
            env = dict(os.environ)
            for name in ("MAKEFLAGS", "BASH_ENV", "ENV", "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
                env.pop(name, None)
            subprocess.run(["git", "init", "-q", str(sandbox)], env=env, check=True)
            baseline = time.time() - 100

            def write(path, text):
                file = sandbox / path
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text(text)
                os.utime(file, (baseline, baseline))
                return file

            write("Cargo.toml", "[workspace]\n")
            write(".gitignore", "ignored-build-artifacts/\n")
            source = write("fe/core/model.ts", "export const value = 1;\n")
            for path in ("fe/web/src/page.tsx", "fe/web/public/icon.svg", "fe/package.json",
                         "fe/package-lock.json", "fe/vite.config.ts", "fe/tools/pwa/vite-plugin.ts",
                         "fe/web/index.html"):
                write(path, "fixture\n")
            subprocess.run(["git", "-C", str(sandbox), "add", "."], env=env, check=True)
            patches = sandbox / "fe/patches"
            patches.mkdir()
            os.utime(patches, (baseline, baseline))
            stamp = write("fe/node_modules/.package-lock.json", "installed\n")
            os.utime(stamp, (baseline + 1, baseline + 1))
            dist = sandbox / "fe/web/dist"
            dist.mkdir()
            os.utime(dist, (baseline + 10, baseline + 10))
            ignored = write("fe/web/src/ignored-build-artifacts/image with spaces.png", "generated")
            os.utime(ignored, (baseline + 20, baseline + 20))
            tools = sandbox / "bin"
            tools.mkdir()
            marker = sandbox / "built"
            for name, body in (
                ("node", "exit 0"),
                ("npm", f"touch {shlex.quote(str(marker))} {shlex.quote(str(dist))}"),
            ):
                tool = tools / name
                tool.write_text(f"#!/bin/sh\n{body}\n")
                tool.chmod(0o755)
            env["PATH"] = f"{tools}{os.pathsep}{env['PATH']}"

            def build(expected=0):
                run = subprocess.run(
                    ["make", "--no-print-directory", "-f", str(ROOT / "Makefile"),
                     "fe-build", f"WORKTREE={sandbox}", "CALM_PORT=4315"],
                    cwd=sandbox, env=env, text=True, capture_output=True, timeout=15,
                )
                self.assertEqual(run.returncode, expected, run.stdout + run.stderr)

            build()
            self.assertFalse(marker.exists(), "ignored generated output is not a build input")
            os.utime(source, (baseline + 20, baseline + 20))
            build()
            self.assertTrue(marker.exists(), "tracked source edits must rebuild the bundle")
            marker.unlink()
            os.utime(source, (baseline, baseline))
            os.utime(dist, (baseline + 10, baseline + 10))
            new_source = write("fe/web/public/new graphic.svg", "new source")
            os.utime(new_source, (baseline + 20, baseline + 20))
            build()
            self.assertTrue(marker.exists(), "new untracked source must rebuild the bundle")
            marker.unlink()
            new_source.unlink()
            source.unlink()
            build()
            self.assertFalse(marker.exists(), "removed files are not missing Make targets")
            git = tools / "git"
            git.write_text("#!/bin/sh\nexit 72\n")
            git.chmod(0o755)
            build(expected=2)
            self.assertFalse(marker.exists(), "failed enumeration must not build from an incomplete graph")

    def test_build_preflight_orders_go_before_expensive_tools(self):
        with tempfile.TemporaryDirectory() as directory:
            sandbox = Path(directory)
            tools = sandbox / "bin"
            tools.mkdir()
            for name in ("find", "git", "uname", "awk", "mkdir", "dirname", "bash", "chmod", "python3", "make"):
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
