# #985 切片 3c 交付报告

## 契约取舍

`Diagnostic` 采用 §12.2 C 的五项结构，同时保留既有 `message` 兼容字段。`path` 不是兼容
字段：它是 §6.5 判断在飞任务是否需要产出撤回诊断的判据载体。
理由是这两个字段已经由 `calm.report.read.taskDiagnostics` 暴露给 MCP spec agent，直接
删除会造成计划未定价的契约破坏。`message` 不是第二份真源：所有生产者只提交 `code`
与 `message_args`，统一由服务端 `render_diagnostic_message` 生成兼容文案。

报告页继续选择“已知 kind 的任一块不符合当前 schema 时整篇进入版本墙”，卡片快照路径则
保留逐块 `UnsupportedBlock`。两者不是同一读边界：前者是权威 HTTP 报告，拒绝把已知 kind
的损坏数据伪装成未来扩展；后者是非权威的卡片快照，优先保住同卡其余内容。`knownKinds`
必须从 `typedReportBlockSchema.options` 派生，避免新增判别项时把已知 kind 悄悄降级。

墓碑 UI 保留 `declared_by + tombstoned_by` 双署名审计线索；系统仍负责写入这两个字段，用户
看到的是“最初由谁提出、后来由谁留下不做记录”的自然语言，而不是可编辑的实现字段。

容量为零时没有任何获准块，因此 ceiling 诊断的 `related_block_ids` 必须为空，只由
`related_wave_id` 提供提高上限入口。此前测试要求 `b_0000` 的做法会制造指向被拒块自身的
跳转；本轮明确把该断言从错误行为改为诚实的空列表。

实现还纠正了一处代码与权威计划的偏差：读路径此前仍在未显式配置策略时根据用户墓碑
推导 `declare-and-wait`，这与 §6.6 和 §12.2 B2 已删除自动派生分支的裁决不符。本片将
生效条件收回为仅接受人显式 PATCH 的 `automation_policy`；墓碑与自动化策略恢复继续是
两个独立动作。

仓库中两条既有 acceptance 断言仍固定 B1（“墓碑自动推导等待、并连带删除其它任务”），
与权威计划 §6.1 / §6.6 / §12.2 B2 直接冲突。没有弱化其覆盖面：测试改为分别固定
“墓碑不改变策略”和“显式切换等待策略才删除未放行行”，并保留墓碑与恢复策略独立性的
端到端断言。请评审重点裁决这处按权威计划更新既有断言的处理。

## 变异验证

以下每项都临时改回错误实现、运行一次看到红灯，再恢复正确实现：

- 重复 key 的 `related_block_ids` 改为空：`projection_is_pure_and_reports_document_level_diagnostics` 因期望两个块 id、实际为空而变红。
- `declare_and_wait` 文案退化为 `Waiting.`：`gives declare_and_wait a human explanation and next action` 因缺少 “Allow this task” 变红。
- 隐藏 `released_by_user` 放行按钮：`renders task status, diagnostics and the release button without exposing ownership fields` 因找不到 “Allow this task” 按钮变红。
- B2 的“要”分支不再 PATCH：`asks on deleting a spec task and tightens the wave when the user chooses yes` 因 `patchWave` 零调用变红。
- 状态投影强制返回 `None`：`ceiling_i_a_inflight_is_input_and_holds_the_upper_bound` 因期望 `dispatched`、实际 `None` 变红。

这些测试分别从真实投影结果、渲染输出和注入的 HTTP 操作边界取证，没有与生产实现共享
文案表、关联块列表或请求结果 fixture。

## 修复轮 3 收敛

新增 `WaveReportPage` 级链路测试，从 `useWaveReportQuery` 的块、verdict 与诊断数据开始，
经过 `ReportContent → ReportMarkdown → ReportBlockView → ReportTaskBlock`，覆盖状态回显、
两条诊断、相关块/相关 wave 路由、worker 产出链接，以及放行、删除、清除墓碑、恢复自动化
四种动作的真实 API 参数。`CAP-REPORT-TASK-023` 的权威锚点已改指向该测试。

同时补齐 `task.dispatched` / `task.gate_result` 的精确与宽失效表驱动用例、
`useWaveReportQuery` 的畸形 verdict 拒绝契约、REST/MCP 两侧 gate 日志字段的 wire 筛除，
以及缺失 `tombstoned_by` 的容错读路径。`ceiling=0` 文案不再建议移动卡片，相关链接均走
TanStack Router。

### 修复轮 3 七项页面链路变异

以下七项逐项临时改坏并运行
`WaveReportPage > wires projected task UI and every task action through the page assembly`；每次均
确认该测试变红后用 `git checkout --` 恢复：

- （待验证）`taskActions` 整体断线。
- （待验证）`taskVerdict` 整体断线。
- （待验证）`RelatedBlocks` 直接返回 `null`。
- （待验证）删除整行诊断渲染。
- （待验证）删除 `Remove task` 按钮。
- （待验证）删除 `Open worker output` 链接。
- （待验证）页面级 `clearTombstone` 改为空函数。
