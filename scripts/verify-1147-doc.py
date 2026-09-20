#!/usr/bin/env python3
"""Machine check that the workspace design doc still carries its section headings,
its measured contracts (by load-bearing keyword), and not the sentence contradicting N1."""
import pathlib
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
DOC = REPO / "docs" / "1147-workspace-design.md"

ITEMS = {
    "ownership marker is the test": ["neige-workspace", "第三方仓库", "误删用户仓库"],
    "marker written before git init": ["git init", "之前"],
    "non-empty: refuse vs repair": ["硬失败，绝不复用", "自己的半成品", "*.lock"],
    # NOT the old "worktree add fails" claim, which only holds on git < 2.42.0.
    "empty init commit is the D4 baseline": ["rev-list --count --all == 1", "基线", "2.42.0"],
    "exclude not gitignore": [".git/info/exclude", ".gitignore", "永假"],
    "git env isolation": ["GIT_TEMPLATE_DIR", "GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM", "hooks/"],
    "mutex + canonical prefix": ["per-path 互斥", "canonicalize", "符号链接", "starts_with"],
    "idempotency key carries path": ["幂等键必须包含路径摘要", "409"],
    "repoint intent is durable": ["可持久推断", "操作表"],
    "known gaps table": ["## 已知缺口", "N4", "N5", "N7", "N9", "N10", "N11", "不迁移"],
    # The exemption covers OLD DATA only, never within-one-run correctness (concurrency, crash-replay, idempotency).
    "no-old-data-migration premise": [
        "老数据不迁移",
        # Covers EVERY existing database including production; a "dev may be dropped, production must be migrated" split is the reading this token prevents.
        "所有现存库",
        "全新的库",
        "同一次运行内的正确性",
    ],
}

# Contradicts N1: a non-empty directory carrying our marker is repaired, not refused.
CONTRADICTS_N1 = "非空目录直接失败"


def main() -> int:
    doc = DOC.read_text()
    failures = []

    base = subprocess.run(
        ["git", "-C", str(REPO), "show", "origin/main:docs/1147-workspace-design.md"],
        capture_output=True, text=True,
    )
    if base.returncode == 0:
        headings = [l for l in base.stdout.split("\n") if l.startswith(("## ", "### "))]
        missing = [h for h in headings if h not in doc]
        if missing:
            failures.append(f"section headings dropped from the #1181 rewrite: {missing}")
        else:
            print(f"OK  {len(headings)} section headings from #1181 present")
    else:
        print("SKIP heading check (origin/main not available)")

    for name, tokens in ITEMS.items():
        absent = [t for t in tokens if t not in doc]
        if absent:
            failures.append(f"contract '{name}' lost these markers: {absent}")
        else:
            print(f"OK  {name}")

    if CONTRADICTS_N1 in doc:
        failures.append(
            f"the sentence contradicting N1 is back: {CONTRADICTS_N1!r} "
            "(a marked non-empty directory is repaired, not refused)"
        )
    else:
        print("OK  the N1-contradicting sentence is gone")

    for f in failures:
        print(f"FAIL {f}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
