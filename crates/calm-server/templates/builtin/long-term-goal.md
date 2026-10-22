+++
id = "long-term-goal"
title = "长期目标"
description = "长期自主推进一个目标：按 Roadmap 分阶段交付，KR 由独立 worker 测量，自动合并，只在有问题时问你。"
instructions = """
把用户的目标写成「目标 + 关键结果 + Roadmap」，按阶段自主推进。
实现与评审用 Codex worker；同一 head 评审无阻塞、门禁与 CI 全绿即合并。
只在需要用户决定时写进「需要你」并提问。
"""
+++
<!-- neige:contract {"version":1,"sections":[{"h1":"规范"},{"h1":"需要你","omit_if_empty":true},{"h1":"目标"},{"h1":"关键结果"},{"h1":"Roadmap"},{"h1":"已交付"}]} -->
<!-- 维护约定（不渲染，AI 必读）
· 规范：用户给的长期指令，一行一条，原意不改；只有用户能增删。每次开工前先读。
· 需要你：只放等用户决定的事，一行一件，「问题？默认：…」。没有就删掉整段。
· 目标（O）：一句定性、有方向感的话，加时间框。不轻易改。
· 关键结果（KR）：3–5 个可测指标，表格写「KR / 怎么测（交付门槛）/ 当前 / 目标」。
  KR 是交付门槛：阶段交付前后按「怎么测」重新测量，测量由独立 Codex worker
  （只读任务）或命令完成，「当前」只引用测量结果；使任一 KR 变差的交付不合并。
· Roadmap：阶段一个接一个，每个阶段固定写：
  交付物（PR 等）/ 推动（哪个 KR）/ 上下文 预算 Xk（已用读下方运行数据） / 现在。
  状态用 `进行中` `待开始` `未开始`，做完移到「已交付」。
· 压缩检查点：阶段之间写「⏸ 压缩检查点」。到这里先更新本文档；若剩余上下文 <
  下一阶段的上下文预算，就声明一个 compact 任务块（key 唯一、kind compact、
  ready true）压缩自己；被压缩完成通知叫醒后先读本文档和任务列表，不要再压缩。
· 已交付：每个做完的阶段一行「名称 · 交付物链接 · 推动 KR · 上下文 预算Xk（运行数据见观测时间）」，
  最多 8 行。上下文读数与压缩任务数引用运行数据，不手写；预算仍手写。
· 不写过程；全文控制在一屏内。

工作方法
· 开工先读目标仓库的 AGENTS.md / CONTRIBUTING.md，按其中的评审分级、门禁、
  验证命令执行；把要求传给 worker。
· 一个功能可切 3–4 片 PR，不必第一片就端到端可用，但不要一二十个 PR 才能用。
· 实现、评审、修复都用 Codex worker；需要结构设计时用 Claude 只读任务。
· 每片：实现任务带门禁 → 评审（按仓库分级，L2 两路独立）→ 修阻塞 →
  plugin_gitforge_publish 发 PR → plugin_gitforge_gh_pr_checks 等 CI → plugin_gitforge_gh_pr_diff 核对 →
  plugin_gitforge_gh_pr_merge（expected_head_sha）直接合并，不再问用户。
· CI 失败读 plugin_gitforge_gh_ci_failure_evidence；与改动无关的失败用 plugin_gitforge_gh_run_rerun 重跑。
· 只有目标无法推进、需要越权或取舍时才问用户。
-->

# 规范

# 目标

# 关键结果

# Roadmap

```neige-block view
{
  "description": "上下文占用保留最近观测时间；压缩数只计本 track 已完成的 compact 任务。",
  "rows": [
    {
      "cells": [
        {
          "expects": "metrics",
          "id": "planner-context",
          "kind": "live",
          "source": "neige://plugin/kernel/planner.context"
        }
      ],
      "id": "planner-runtime",
      "layout": "one",
      "title": ""
    }
  ],
  "snapshot": null,
  "title": "Planner 运行数据",
  "version": 1
}
```

# 已交付
