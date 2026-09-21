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

Goal and inputs

- Template input: the track's bound `template_input` JSON is the task's source of truth,
  not the track title.
- Issue inspection: derive the track goal from gh.issue.view on input.repo /
  input.issue_number. Record the issue's requirements and constraints in the track
  report before dispatching any downstream task.
- notes: optional advisory context from the requester; it never overrides the issue or
  the gates.

Check the repository

Repo cross-check: before any repository write, compare input.repo
against `git remote get-url origin` run in the track cwd (owner/name after stripping the
host and a trailing .git). On mismatch do NOT proceed or declare execution tasks.
Record both observed repositories in 待你定 and ask the user to correct or confirm
the repository. In draft or planning, stop after reporting the mismatch: no
ratification is needed to ask this question, and calm.ratify.request is unavailable
there. If already working, move working->blocked via calm.ratify.request with
`reason:"repo_mismatch: input.repo=<owner/name>, cwd.origin=<owner/name>"`
(that exact prefix, then both observed values), and wait for the human decision.

Working method

Understand the source issue and propose an appropriate design. Review the design through two independent channels before implementation: channel a checks correctness, channel b checks failure paths. Implement and commit in a worktree, run verification, open a PR and converge both PR reviews through those same two perspectives before any merge. After an authorized merge, close the source issue. Create concrete tasks when delegation is needed; these are working requirements, not a fixed task list.

Review convergence

For this track, drive dual-review convergence for each review subject.

Record both verdicts

- After BOTH channels for a phase complete, call calm.review.round with
  subject:{phase,slice_id,pr_number?}, optional head_sha, n, cap, converged,
  channels:[both verdicts], and root_cause when known.
- Record each channel's verdict as the literal lowercase token `approved` or
  `changes_requested` (exactly those strings).
- converged is true only when EVERY channel verdict is `approved`.
- For PR subjects, head_sha is the reviewed forge.pr.diff.read head_sha; omit head_sha
  for design subjects.

Record root_cause each round; repeated facets should drive a class fix.

Review rounds and fixes

- For each subject, set n to the last observed review.round n for that same subject plus 1.
  cap is the fixed policy constant 8 for a subject's first review window; after a
  cap-exhaustion ratify grant it is the previous cap plus exactly 2 (see ASK-HUMAN
  below).
- Always re-review. Every fix re-dispatches BOTH channels before the next
  calm.review.round.

When the review limit is reached

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

Verification gates

gates: author each agent task's `gate` from the TARGET repo's own toolchain — detect it
(Cargo / npm / pytest / go / Make, etc.) and run that ecosystem's formatter, linter, and
tests where present; do not hardcode `cargo test`.

Merge and approval

Merge fence F4

Merge fence F4: call gh.pr.merge for a subject ONLY when that subject's latest
review.round has converged:true. Pass expected_head_sha equal to that round's head_sha.

Merge policy

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
-->

# 概要

# 待你定

# 已完成

# 决策
