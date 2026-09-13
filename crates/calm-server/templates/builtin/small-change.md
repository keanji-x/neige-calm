+++
id = "small-change"
title = "Small change"
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

# Purpose

Short inspect → implement → verify loop.

# Plan

Treat these task blocks as the authoritative pre-set plan. Activate by replacing those
task blocks, authoring a real `gate` from the target repo toolchain (formatter, linter,
tests), and setting `ready: true`. Do not mint duplicate tasks. Prose blocks are NOT a plan to activate: maintain them per this document's own contract.

```neige-block task
{
  "acceptance": "The change request and the current code path are captured in the track report.",
  "declared_by": "spec",
  "depends_on": [],
  "goal": "Read the requested change and the current code that it touches. Record constraints in this report before writing.",
  "key": "inspect",
  "kind": "codex",
  "no_gate_reason": "inspect does not produce a repo change to verify",
  "ready": false
}
```

```neige-block task
{
  "acceptance": "The change is committed in the track worktree.",
  "declared_by": "spec",
  "depends_on": ["inspect"],
  "goal": "Implement the change and commit it.",
  "key": "implement",
  "kind": "codex",
  "no_gate_reason": "author a real gate from the target repo toolchain (formatter, linter, tests) before activating; this reason is not a permanent skip",
  "ready": false
}
```

```neige-block task
{
  "acceptance": "The repository toolchain's standard test/verification command passed.",
  "declared_by": "spec",
  "depends_on": ["implement"],
  "goal": "Run the repository's standard tests and record the result.",
  "key": "verify",
  "kind": "codex",
  "no_gate_reason": "author a real gate from the target repo toolchain (formatter, linter, tests) before activating; this reason is not a permanent skip",
  "ready": false
}
```

