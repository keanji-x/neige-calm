+++
id = "issue-development"
title = "Issue development"
+++
<!-- neige:contract {"version":1,"sections":[{"h1":"概要"},{"h1":"待你定","omit_if_empty":true},{"h1":"已完成"},{"h1":"决策"}]} -->
<!-- 报告维护契约

这段注释在渲染时会被丢弃，用户在页面上看不到它；但它留在 body 源码里，
任何读源码的主体都读得到（你、worker 的 `neige cat report.md`、REST 读口、
track 的 VCS diff）。不要把秘密写进来。

这份报告自带的结构就是规则：维护它，不要重写它。

写作方式：
  · 这是一份工作简报，不是你的工作日志。假设读者今天第一次接触这个 track，
    3 分钟内要能搞清楚现状和下一步。
  · 报告反映当下的状态，不是历史。每次更新 REWRITE 相关章节，让陈旧条目消失；
    历史由内核的 event timeline 承载，不需要在这里复述。
  · 写产出，不写过程。不要写「重新读取了 track state」「分析了 worker 结果」
    「调用了 blocks.upsert」「incorporated the worker's analysis」这类描述你
    自己动作的句子。读者不关心你怎么运转的；他们想知道 *做成了什么*、
    *定下了什么*、*还差什么*。
    ✗ 不好：「重新读取 track state，确认 worker 完成了 demo 实现。」
    ✓ 好：「demo 已部署在 <preview URL>，PR #76 已开。」
  · 不要把对话历史 / 长引用 dump 进来 —— 摘要后写要点。
  · 散文正文（所有 prose 块的文字合计；非 prose 块在 body 里的 fence 投影不计入）
    控制在 1000 字以内。超了就 consolidate：合并相似条目、删掉已经不重要的细节、
    把长描述压成要点。

章节由下面这份清单定义。不要新增清单以外的 H1，不要重命名，不要调整顺序；
各章节内部的条目由你维护。不要因为「格式看起来不对」而整体重写本文档 ——
它就是它该有的样子。

各章节：
  · 概要 —— 1-3 句话：当前状态 + 下一步。读者哪怕只看这一段也能掌握局面。
    当前状态发生变化就重写它。
  · 待你定 —— 等用户拍板的事 / 阻塞项。紧排在概要之后，让用户最先看到需要他
    动作的事。被阻塞时在这里写明白具体要什么。没有就省略这个 section。
  · 已完成 —— 具体产出物：PR 链接、文件路径、部署地址、已成事实，每条带链接或
    具体引用。任务完成后挪到这里。不要 append 后不删 —— 旧条目失效就删掉，
    不要堆积。
  · 决策 —— 重要取舍，格式「决定 X，因为 Y」。候选 / 讨论过程不写在这里，
    只写已经定下来的事。做了一个决定就在这里加一行。

模板还可带有以下预置章节。各模板只维护正文中已有的章节，保持其顺序，
不补齐未使用的章节，也不要把这些独立章节合并回一个 Plan：
  · Purpose —— 模板用途与范围。
  · Goal and inputs —— 目标来源、输入与仓库核对。
  · Plan —— 预置计划与任务激活方式。
  · Review convergence —— 评审、修复与轮数限制。
  · Verification gates —— 仓库工具链与验证要求。
  · Merge and approval —— 合并条件与审批策略。
这些章节共同组成预置计划。里面的 `task` 块激活完之后，预置章节的散文
由上面「概要、待你定、已完成、决策」四节接手，相应的预置章节就可以移除。
除此之外不要动它们的结构；激活只针对 `task` 块，不是替换这些散文。
-->

# Goal and inputs

- Template input: the track's bound `template_input` JSON is the task's source of truth,
  not the track title.
- Ingest (inspect-issue): derive the track goal from gh.issue.view on input.repo /
  input.issue_number. Record the issue's requirements and constraints in the track
  report before dispatching any downstream task.
- notes: optional advisory context from the requester; it never overrides the issue or
  the gates.

## Check the repository

Repo cross-check (inspect-issue acceptance): before any write action, compare input.repo
against `git remote get-url origin` run in the track cwd (owner/name after stripping the
host and a trailing .git). On mismatch do NOT proceed: move working->blocked via
calm.ratify.request with `reason:"repo_mismatch: input.repo=<owner/name>, cwd.origin=<owner/name>"` (that exact prefix, then both observed values), and wait for
the human decision.

# Plan

Pre-set issue-development plan. Treat the `task` blocks as the authoritative plan.
Activate by replacing those task blocks and setting `ready: true` — use the read's block
ids and revision as replace anchors. Do not mint duplicate tasks. Prose blocks are NOT a plan to activate: maintain them per this document's own contract.

# Review convergence

For this track, drive dual-review convergence for each review subject.

## Record both verdicts

- After BOTH channels for a phase complete, call calm.review.round with
  subject:{phase,slice_id,pr_number?}, optional head_sha, n, cap, converged,
  channels:[both verdicts], and root_cause when known.
- Record each channel's verdict as the literal lowercase token `approved` or
  `changes_requested` (exactly those strings).
- converged is true only when EVERY channel verdict is `approved`.
- For PR subjects, head_sha is the reviewed forge.pr.diff.read head_sha; omit head_sha
  for design subjects.

Record root_cause each round; repeated facets should drive a class fix.

## Review rounds and fixes

- For each subject, set n to the last observed review.round n for that same subject plus 1.
  cap is the fixed policy constant 8 for a subject's first review window; after a
  cap-exhaustion ratify grant it is the previous cap plus exactly 2 (see ASK-HUMAN
  below).
- Always re-review. Every fix re-dispatches BOTH channels before the next
  calm.review.round.

## When the review limit is reached

If n == cap and the round is non-approving, do not merge.

- Either GIVE-UP by recording the terminal rationale in the report with
  calm.report.write and lifecycle failed for reviewing->failed; OR ASK-HUMAN by first
  moving reviewing->working with the normal lifecycle arg, then call calm.ratify.request
  with `reason:"cap_exhausted"` for working->blocked.
- On ratify.resolved grant the track is already back in working; resume
  working->reviewing and continue reviewing the exhausted subject with cap = previous
  cap + 2 on its next round.
- The kernel accepts this raise at most once per subject per grant; a grant may
  authorize this for each subject that was already cap-exhausted when it was issued.
- If the extended window also exhausts without convergence, GIVE-UP or ASK-HUMAN again.

# Verification gates

gates: author each agent task's `gate` from the TARGET repo's own toolchain — detect it
(Cargo / npm / pytest / go / Make, etc.) and run that ecosystem's formatter, linter, and
tests where present; do not hardcode `cargo test`.

# Merge and approval

## Merge fence F4

Merge fence F4: call gh.pr.merge for a subject ONLY when that subject's latest
review.round has converged:true. Pass expected_head_sha equal to that round's head_sha.

## Merge policy

- merge_policy: `auto-merge` allows gh.pr.merge as soon as merge fence F4 is satisfied.
- `hold-for-ratify` — also the semantics whenever merge_policy is absent — additionally
  requires a granted ratify BEFORE gh.pr.merge.
- Drive everything up to converged reviews + green checks, then move reviewing->working
  with the normal lifecycle arg (calm.ratify.request 400s outside working), and call
  calm.ratify.request with `reason:"merge_hold: pr #<n> converged at <head_sha>"` for
  working->blocked.
- On ratify.resolved grant the track is already back in working: the grant authorizes
  merging that already-converged head — no fresh review round is required for the hold
  itself; resume working->reviewing and call gh.pr.merge per fence F4 (expected_head_sha
  = the converged round's head_sha).

```neige-block task
{
  "acceptance": "The issue requirements and constraints are captured for the track AND the track cwd's origin remote matches input.repo (mismatch is reported, not proceeded past).",
  "context": {
    "tools": ["gh.issue.view"]
  },
  "declared_by": "spec",
  "depends_on": [],
  "goal": "Read the bound template input, view the source issue via gh.issue.view, and cross-check input.repo against the git remote of the track cwd.",
  "key": "inspect-issue",
  "kind": "codex",
  "no_gate_reason": "inspect does not produce a repo change to verify",
  "ready": false
}
```

```neige-block task
{
  "acceptance": "Channel a records a design verdict.",
  "context": {
    "channel": "a",
    "reviewer_role": "design-correctness"
  },
  "declared_by": "spec",
  "depends_on": ["inspect-issue"],
  "goal": "Review the proposed design for correctness before implementation.",
  "key": "review-design-a",
  "kind": "codex",
  "no_gate_reason": "design review does not produce a repo change to verify",
  "ready": false
}
```

```neige-block task
{
  "acceptance": "Channel b records a design verdict.",
  "context": {
    "channel": "b",
    "reviewer_role": "design-failure-path"
  },
  "declared_by": "spec",
  "depends_on": ["inspect-issue"],
  "goal": "Review the proposed design for failure paths before implementation.",
  "key": "review-design-b",
  "kind": "codex",
  "no_gate_reason": "design review does not produce a repo change to verify",
  "ready": false
}
```

```neige-block task
{
  "acceptance": "The change is committed in the track worktree.",
  "context": {
    "tools": ["git.worktree.add", "git.commit"]
  },
  "declared_by": "spec",
  "depends_on": ["review-design-a", "review-design-b"],
  "goal": "Create a worktree, implement the change, and commit the result.",
  "key": "implement-change",
  "kind": "codex",
  "no_gate_reason": "author a real gate from the target repo toolchain (formatter, linter, tests) before activating; this reason is not a permanent skip",
  "ready": false
}
```

```neige-block task
{
  "acceptance": "A pull request exists with readable diff and check status.",
  "context": {
    "tools": ["gh.pr.create", "gh.pr.list", "gh.pr.diff", "gh.pr.checks"]
  },
  "declared_by": "spec",
  "depends_on": ["implement-change"],
  "goal": "Open a pull request and check its diff/check status.",
  "key": "open-pr",
  "kind": "codex",
  "no_gate_reason": "opening a PR is verified by forge status, not a local toolchain gate",
  "ready": false
}
```

```neige-block task
{
  "acceptance": "Channel a records a PR verdict.",
  "context": {
    "channel": "a",
    "reviewer_role": "pr-correctness"
  },
  "declared_by": "spec",
  "depends_on": ["open-pr"],
  "goal": "Review the pull request for correctness.",
  "key": "review-pr-a",
  "kind": "codex",
  "no_gate_reason": "PR review does not produce a repo change to verify",
  "ready": false
}
```

```neige-block task
{
  "acceptance": "Channel b records a PR verdict.",
  "context": {
    "channel": "b",
    "reviewer_role": "pr-failure-path"
  },
  "declared_by": "spec",
  "depends_on": ["open-pr"],
  "goal": "Review the pull request for failure paths.",
  "key": "review-pr-b",
  "kind": "codex",
  "no_gate_reason": "PR review does not produce a repo change to verify",
  "ready": false
}
```

```neige-block task
{
  "acceptance": "Either the PR is merged (F4 converged and any policy-required ratify grant held) and the issue is closed, or — hold-for-ratify with no grant yet — the track is parked at the merge_hold ratify request with no merge performed.",
  "context": {
    "tools": ["gh.pr.merge", "gh.issue.close"]
  },
  "declared_by": "spec",
  "depends_on": ["review-pr-a", "review-pr-b"],
  "goal": "Merge the pull request and close the issue only after merge fence F4 has converged AND any merge_policy-required ratify grant is held; under hold-for-ratify with no grant yet, park at the merge_hold ratify request instead of merging.",
  "key": "merge",
  "kind": "codex",
  "no_gate_reason": "merge is gated by review fence F4 and forge, not a local toolchain gate",
  "ready": false
}
```

