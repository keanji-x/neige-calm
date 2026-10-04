+++
id = "issue-development"
title = "Issue development"
description = "Develop a GitHub issue through implementation, review and an authorized merge."
instructions = """
Read the issue and confirm the repository and requirements.
When a design is needed, have it reviewed once, then implement and verify the change.
Open a PR and review it. Merge only under your selected authorization.
"""
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
against the first line of `git config --get-all remote.origin.url` run in the track cwd (the
configured origin URL, before any url.insteadOf rewrite; owner/name after
stripping the host and a trailing .git).
On mismatch do NOT proceed or declare execution tasks.
Record both observed repositories in 待你定, ask the user to correct or confirm
the repository with neige.ratify.request and
`reason:"repo_mismatch: input.repo=<owner/name>, cwd.origin=<owner/name>"`
(that exact prefix, then both observed values), and wait for the human decision.

Working method

Understand the source issue. Write a short design before implementing only when the change is large, risky, or crosses an authority or persistence boundary; otherwise implement directly. A design gets one read-only review task; after a revision, that reviewer re-checks only the changed parts. Implement and commit in a worktree, open a PR with neige.track.publish, review it, and merge under your selected authorization. After an authorized merge, close the source issue. Create concrete tasks when delegation is needed; these are working requirements, not a fixed task list.

Read the issue discussion with gh.issue.comments; pass a new attempt when refreshing
comments or the gh.issue.view body. Post relevant questions, progress, and results
with gh.issue.comment, using a stable idem and unchanged body on retries and a
new idem for each new comment. A pending receipt does not confirm publication.

Read pull requests with the git-forge tools, not the gh CLI: gh.pr.diff returns the path of the file holding the patch, and gh.pr.checks returns the conclusion (pass a new attempt on each re-read).

Review

- Choose the review level for the PR's code change (design review stays at one task) and
  record it with a one-line reason in 决策: one read-only review task for an ordinary
  change; two independent review tasks when the change crosses an authority,
  persistence, isolation or security boundary, adds a database migration, or is large.
- Give reviewers the implementing attempt's gate result and gh.pr.checks as mechanical
  evidence; a reviewer runs a check only to test its own hypothesis.
- A finding blocks only when it is a defect this change introduces or a structural
  problem in the approach. Fix cheap in-scope findings. Post the other findings worth
  keeping as one gh.issue.comment on the source issue; drop pure style.
- A removal stays a removal: it adds no new mechanism, hand-built fixture or extra test,
  and leaves historical design documents unchanged; reviewers raise either as a
  structural finding.
- After a fix, the reviewer that raised the finding re-checks the fix; with two review
  tasks, both re-check.
- When blocking findings still arrive after three review rounds, stop patching them one
  by one: find the structural problem in the approach and change or narrow it, or ask
  the user with neige.ratify.request.

Verification gates

gates: author each `gate` from the TARGET repo's own toolchain — detect it
(Cargo / npm / pytest / go / Make, etc.) and run that ecosystem's formatter, linter, and
tests where present; do not hardcode `cargo test`.

前端预览 (frontend preview)

When the change touches calm's web frontend (`fe/`), put the running result in the report.
Open a terminal in the task worktree with neige.terminal.open and start, each on a free port:
- the backend: `CALM_PUBLISH_ADDR=127.0.0.1 CALM_DEV_AUTOLOGIN=true make dev-fresh DEV_ID=<short track id> CALM_PORT=<port>`.
  Autologin is required: the preview gateway never forwards calm's session cookie, so the
  dev stack must not ask for a login. Autologin makes anyone who can reach the port the
  owner, so the stack is published on loopback only (make refuses autologin otherwise).
  It first compiles release binaries (several minutes, CPU-heavy: on a busy host set
  `CARGO_BUILD_JOBS`) and builds the tailnet helper with Go: if it stops because `go` is
  not found, put the Go toolchain on PATH and rerun.
- the frontend (after `npm --prefix fe ci` if needed):
  `FE_API_PROXY_TARGET=http://127.0.0.1:<CALM_PORT> FE_DEV_HOST=127.0.0.1 FE_DEV_PORT=<port> npm --prefix fe run dev`.
  The gateway only reaches 127.0.0.1, and vite's default host `localhost` may bind IPv6 only.
Then call neige.preview.register {key:"fe", target_port:<FE_DEV_PORT>, title} and put its
block_hint into the report with `path:"/next/"` added (a block upsert or a report commit). When the work is done, stop both and call
neige.preview.unregister {key:"fe"}.
Self-checks against the dev stack (curl, Playwright) run in a worker task, not in your own
shell.

Merge and approval

- Merge only a head that a review read with gh.pr.diff, when no blocking finding is open
  and gh.pr.checks is green. Pass that head_sha as expected_head_sha.
- merge_policy `auto-merge` allows gh.pr.merge at that point without asking again.
- `hold-for-ratify` — also the semantics whenever merge_policy is absent — first calls
  neige.ratify.request with `reason:"merge_hold: pr #<n> at <head_sha>"`; on
  ratify.resolved grant, merge that head with gh.pr.merge (expected_head_sha = that
  head_sha). A new head needs review again before a new ratify.
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
          "required": true,
          "placeholder": "",
          "help": "The GitHub issue this track should resolve.",
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
          "default": "hold-for-ratify",
          "on_value": "auto-merge",
          "off_value": "hold-for-ratify",
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
