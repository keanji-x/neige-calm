#!/usr/bin/env python3
"""#1147 — machine check that the S2 design contracts survived the #1181 doc rewrite.

The design document was condensed from 570 to 148 lines by #1181 while S2 was in
flight. The merge kept #1181's structure and ported S2's measured contracts into
it. Losing any of those contracts silently is the failure mode this guards: the
next slice would rediscover each one the hard way.

Checks three things:
  * every section heading #1181 introduced is still there (we did not "win" the
    merge by reverting someone else's editorial work);
  * every S2 contract is present, by load-bearing keyword;
  * the one sentence that contradicted the reviewed N1 behaviour is gone.
"""
import pathlib
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
DOC = REPO / "docs" / "1147-workspace-design.md"

ITEMS = {
    "ownership marker is the test": ["neige-workspace", "第三方仓库", "误删用户仓库"],
    "marker written before git init": ["git init", "之前"],
    "non-empty: refuse vs repair": ["硬失败，绝不复用", "自己的半成品", "*.lock"],
    # Version-independent rationale: the baseline for D4's emptiness predicate.
    # NOT the old "worktree add fails" claim, which only holds on git < 2.42.0.
    "empty init commit is the D4 baseline": ["rev-list --count --all == 1", "基线", "2.42.0"],
    "exclude not gitignore": [".git/info/exclude", ".gitignore", "永假"],
    "git env isolation": ["GIT_TEMPLATE_DIR", "GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM", "hooks/"],
    "mutex + canonical prefix": ["per-path 互斥", "canonicalize", "符号链接", "starts_with"],
    "idempotency key carries path": ["幂等键必须包含路径摘要", "409"],
    "repoint intent is durable": ["可持久推断", "操作表"],
    # N11 asked for a data migration until 2026-08-31, when the "old data is
    # not migrated" premise landed (see below) and the answer became "rebuild
    # the database". The token tracks that ruling: the table must still say
    # what happens to those rows, it just no longer says "migrate them".
    "known gaps table": ["## 已知缺口", "N4", "N5", "N7", "N9", "N10", "N11", "不迁移"],
    # 2026-08-31 premise. Both halves are load-bearing and the second is the
    # one that gets misread: the exemption covers OLD DATA only, never
    # within-one-run correctness (concurrency, crash-replay, idempotency).
    "no-old-data-migration premise": [
        "老数据不迁移",
        # Scope, and the half that gets softened first: it covers EVERY
        # existing database including production, i.e. deploy starts a fresh
        # one. A "dev may be dropped, production must be migrated" split is
        # exactly the reading this token exists to prevent.
        "所有现存库",
        "全新的库",
        "同一次运行内的正确性",
    ],
}

# #1181's step 1 read "创建一个不存在或为空的目标目录；非空目录直接失败", which
# contradicts N1: a non-empty directory carrying our marker is repaired, not refused.
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
