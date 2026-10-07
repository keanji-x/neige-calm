+++
id = "dev"
title = "Development"
description = "Develop a change, optionally linked to a GitHub issue, through verification and authorized delivery."
instructions = """
Understand the requested change and any optional issue context.
Follow the target repository's development and review guidelines.
Implement, verify, and deliver the change under the selected merge authorization.
"""
+++
<!-- neige:contract {"version":1,"sections":[{"h1":"概要"},{"h1":"待你定","omit_if_empty":true},{"h1":"已完成"},{"h1":"决策"}]} -->
<!-- 报告维护契约

这段注释在渲染时会被丢弃，用户在页面上看不到它；但它留在 body 源码里，
任何读源码的主体都读得到（你、worker 的 `neige track cat report.md`、REST 读口、
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

- Develop the user's requested change in the track's checkout. The title is not a
  substitute for requirements; record the outcome and acceptance in the report
  before dispatching downstream work. Ask for missing requirements only when they
  prevent meaningful progress.
- The bound `template_input` JSON supplies optional issue context and merge policy.
  When input.repo and input.issue_number are present, read plugin_gitforge_gh_issue_view and
  plugin_gitforge_gh_issue_comments and incorporate the issue's requirements and constraints.
  Use a new attempt when refreshing either read. With no issue, do not read,
  comment on, or close an issue, and do not create one unless the user or repository
  policy requires it.
- If an issue URL is supplied without its derived repo and issue_number, resolve
  those fields from that URL before using issue tools. A malformed or inconsistent
  issue reference is a blocking input problem; never guess an issue or repository.
  Apply the repository cross-check below to the resolved issue repository as well.
- notes: optional advisory context; it does not grant permission or bypass gates.

Check the repository

Repo cross-check: when input.repo is present, before any repository write, compare input.repo
against the first line of `git config --get-all remote.origin.url` run in the track cwd (the
configured origin URL, before any url.insteadOf rewrite; owner/name after
stripping the host and a trailing .git).
On mismatch do NOT proceed or declare execution tasks.
Record both observed repositories in 待你定, then ask with neige_user_ask, title
`repo_mismatch: input.repo=<owner/name>, cwd.origin=<owner/name>，用哪个仓库？`
(that exact prefix, then both observed values) and the two observed owner/name values
as options, and continue only with the repository the user answers.
Without input.repo, inspect the checkout and origin and use the user's request;
resolve any actual conflict in repository identity before writes. A local checkout
without a remote can still be developed; publication needs a confirmed remote.

Working method and repository policy

Read the target repository's AGENTS.md, applicable directory guidance, and
CONTRIBUTING.md before implementation, and pass their requirements to workers.
Those files own design requirements, review tiers, review/fix scope, coding rules,
verification commands, and preview requirements. Do not replace them with a
second development policy in this template. Record the applicable review tier
and reason in 决策 and the PR; arrange the review channels and follow-up checks
that repository policy requires. If the repository declares no review policy,
use one independent read-only PR review before merge.

Implement in a worktree, let the kernel commit worker changes, verify with the repository's required gates,
and publish a PR with plugin_gitforge_publish when the requested delivery calls for it.
Create concrete delegated tasks when needed, not a fixed task list.
Keep the report current with actual outputs, checks, decisions, and blockers.

Verification plan

Before the first implementing task, record the decided verification plan once in
决策 as a `table` block: each check repository policy requires for this change,
where it runs, the evidence it leaves, and when it counts as done. Unless
repository policy places a check elsewhere, each check runs in one place:
  · The implementing worker runs the focused tests that pin the change (red
    before the fix and green after, when there is behavior to pin) and quick
    formatter or lint fixes. Its goal names the filter or paths that select those
    tests; it reports the exact commands and does not run broad suites or repeat
    the gate's checks.
  · The task's gate runs the repository's required local gates for the changed
    surface and replays those focused tests. A replay step fails when it selects
    no test.
  · CI, where the repository has it, runs the broad suites; read it with
    plugin_gitforge_gh_pr_checks. Without CI they run in the gate.
  · Review channels get the implementing attempt's gate result and, once
    available, CI as evidence. They run a check only to settle a review hypothesis
    or when repository policy assigns it to them.
A check that fits none of these (policy keeps it off this host, it needs the
worker's environment, it changes files, or policy assigns it to a role) gets a row
naming where it runs instead, such as its own task. Verification ends when every
row has passing evidence for the delivered head (the PR head when there is one);
then stop verifying. Change the table only when the plan changes.

When an issue is attached, post relevant questions, progress, and results with
plugin_gitforge_gh_issue_comment, using a stable idem and unchanged body on retries and a new
idem for each new comment. A pending receipt does not confirm publication.
After an authorized merge, close the attached issue only when the change resolves it.

Read pull requests with the gitforge tools, not the gh CLI: plugin_gitforge_gh_pr_diff returns the
path of the file holding the patch. plugin_gitforge_gh_pr_checks waits for the head's CI and wakes
you (pass a new attempt for each wait); do not declare tasks to watch CI. A
`conflicting` PR gets no `pull_request` workflow run until it is synced with its base.

Merge and approval

- Merge only a head inspected with plugin_gitforge_gh_pr_diff and reviewed as repository policy requires, when no blocking finding is open
  and plugin_gitforge_gh_pr_checks is green. Pass that head_sha as expected_head_sha.
- merge_policy `auto-merge` allows plugin_gitforge_gh_pr_merge at that point without asking again.
- merge_policy `ask` — also the semantics whenever merge_policy is absent — first asks with
  neige_user_ask, options `合并` and `暂不合并`, and a title that carries the evidence for that head,
  each value read from a result for that head_sha:

      合并 PR #<n>（head <head_sha>）？
      - CI：<plugin_gitforge_gh_pr_checks conclusion>，失败检查：<failed_checks names, or 无>
      - 门禁：<implementing task key> 第 <n> 次 gate <通过 or 未通过>
      - 评审：<each review task key> → <its conclusion>；未关闭的阻塞发现：<无, or each one>
      - 可合并：<mergeable>

  Do not ask while a line is missing or comes from another head; obtain it first.
  Merge with plugin_gitforge_gh_pr_merge (expected_head_sha = that head_sha) only when the answer is `合并`
  and the head is unchanged. A new head needs the applicable checks and review again before a new ask.
-->

<!-- neige:input-form {
  "version": 1,
  "groups": [
    {
      "title": "Source issue",
      "description": "",
      "fields": [
        {
          "kind": "text",
          "key": "issue_url",
          "label": "Issue URL",
          "default": "",
          "required": false,
          "placeholder": "",
          "help": "Optional GitHub issue to associate with this change.",
          "format": {
            "kind": "github-issue-url",
            "outputs": {
              "issue_url": "issue_url",
              "repo": "repo",
              "issue_number": "issue_number"
            }
          }
        }
      ]
    },
    {
      "title": "Merge approval",
      "description": "",
      "fields": [
        {
          "kind": "toggle",
          "key": "merge_policy",
          "label": "Merge automatically",
          "default": "ask",
          "on_value": "auto-merge",
          "off_value": "ask",
          "on_description": "Allows the agent to merge after reviews and checks pass, without asking again.",
          "off_description": "The agent prepares the PR and waits for your approval to merge."
        }
      ]
    }
  ]
} -->

# 概要

# 待你定

# 已完成

# 决策
