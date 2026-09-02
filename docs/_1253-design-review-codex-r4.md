
## MAJOR

1. **§3 的“渲染器直接可用”漏掉 canonical 初始 report 并不等于 FE 空态。**

   - 文档：[§3「✔ 直接可用」](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:64)；D7 要求未写过时显示“还没有今日进度”及触发按钮：[设计文档](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:142)。
   - 代码反证：launchpad 创建的是 `WaveReportPayload::initial()`，其 body 已包含维护注释和四个 H1，字符串非空：[wave_report.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/wave_report.rs:110)。`readWaveReport` 只在 summary/body/blocks 全空时返回 `null`，因此 canonical 初始卡必定返回非空 report：[report.ts](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/core/domain/report.ts:230)。`ReportDocument` 只有 `report === null` 才渲染 empty prop：[public.tsx](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/features/report/document/public.tsx:76)。
   - 建议：让只读 resolve 或专用 Today 投影返回内核计算的 `report_started`，直接复用 `report_startup_read_required()` 的 canonical 判据；不要在 FE 镜像初始 body 文本。PR1 增加“ensure 成功但从未汇总”的空态测试。

## MINOR

1. **INV-007 不能证明“显式自反排除”是 load-bearing。**

   - 文档：projection 先 join“用户可见 cove/wave”，又要求排除 launchpad 自身事件，并声称两条规则必须一起读：[设计文档](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:93)、[设计文档](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:107)。
   - 代码反证：launchpad 位于 system cove，而普通可见 cove 查询只返回 user cove：[coves.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/coves.rs:85)。因此按当前载体，即使删除显式 `scope_wave != launchpad`，可见性 join 也会先滤掉其 `wave.report_edited`；INV-007 仍可能全绿。事件本身确实带精确 launchpad wave scope，所以显式排除可实现：[wave_report.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/wave_report.rs:699)。
   - 建议：把它写成 defense-in-depth，或在可见性 join 之前对 raw allowlisted rows 做独立 mutation test；否则删掉自反谓词也不会红。

2. **§5.1 已干净解耦 harness，但 `report_card_id` 是当前解析链不需要的 DTO 改动。**

   - 文档：resolve 返回 `wave_id + report_card_id`，随后仍调用 wave detail：[设计文档](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:157)；D1 同时又写“不新建端点”：[设计文档](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:75)。
   - 代码反证：wave detail 已返回 cards，[waves.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/waves.rs:694)，而 `readWaveReport` 自己按 `kind === 'wave-report'` 定位卡。现有 Today router 只有 POST ensure，[today.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/today.rs:37)，所以新增只读 GET 是真实改动，但给既有 `TodayLaunchpad` 增加未消费字段不是必需条件。
   - 建议：新增窄 DTO `{ wave_id, report_started }`；保留 ensure DTO 不动。D1 改成“不新增写/CRDT 端点，只新增只读 resolve”。

D4 其余部分成立：四类事件均有可用 wave scope；allowlist 与 retention 描述正确；`[start,end)` 和只按 `at` 查询正确；transport 的 unresolved、cross-session、dormant、missing row、DB error 均可在 descriptor 隐藏 + 同一 async gate 的 list/call 双重检查下 fail-closed。§5.1 的只读 GET 本身也确实能完全避开 ensure 的 materialize、submit 和 wait。
tokens used
286,319
结论：**1 BLOCKER / 1 MAJOR / 2 MINOR；fix-then-ship**。载体方向成立，无需 rethink。全程只读，未修改文件。

## BLOCKER

1. **D5 把“同 key 重试”误当成“复用当天会话并再次汇总”。**

   - 文档：[D5「复用当天已有的那条汇总会话（按 Idempotency-Key…）」](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:129)，INV-010 又要求重复触发都落到同一条会话。
   - 代码反证：成功后的同 key 属于 arm (a)，只回放同一 conversation，明确“不重新发送首条消息”：[wave_conversations.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/wave_conversations.rs:102)。实际发送前还用永久 `harness.user_message.enqueued` 判断该卡是否发过消息，第二次会直接跳过 `send_spec_input`：[wave_conversations.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/wave_conversations.rs:245)。arm (b)–(e) 也只处理失败重试、stuck、64 次耗尽及不同正文冲突；都没有“成功后向旧会话发一个新 turn”的语义。card id 只是 `(wave_id, arbitrary key)` 的函数，日期不在其中：[conversation_keys.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/conversation_keys.rs:80)。
   - 生命周期也无解：key 固定则后续点击全是 no-op；key 按日则仍会每天新增 Assistant 卡/session；key 按点击则直接违反 INV-010。所有 active/idle Assistant session 都会在 boot 恢复，[session_projection.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/db/sqlite/session_projection.rs:495)，每个恢复出 50ms run loop，[run_loop.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/harness/run_loop.rs:516)。
   - 建议：裁决为 launchpad **全生命周期唯一一条汇总 conversation**。稳定 key 只负责首次创建；每次汇总通过现有 `/api/cards/{id}/spec/input` 向该卡发送新 prompt。若动作重试还要求 exactly-once，另给“本次触发”设计独立幂等键，不能复用 conversation-create 的 key。

## MAJOR

1. **§3 的“渲染器直接可用”漏掉 canonical 初始 report 并不等于 FE 空态。**

   - 文档：[§3「✔ 直接可用」](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:64)；D7 要求未写过时显示“还没有今日进度”及触发按钮：[设计文档](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:142)。
   - 代码反证：launchpad 创建的是 `WaveReportPayload::initial()`，其 body 已包含维护注释和四个 H1，字符串非空：[wave_report.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/wave_report.rs:110)。`readWaveReport` 只在 summary/body/blocks 全空时返回 `null`，因此 canonical 初始卡必定返回非空 report：[report.ts](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/core/domain/report.ts:230)。`ReportDocument` 只有 `report === null` 才渲染 empty prop：[public.tsx](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/fe/web/src/features/report/document/public.tsx:76)。
   - 建议：让只读 resolve 或专用 Today 投影返回内核计算的 `report_started`，直接复用 `report_startup_read_required()` 的 canonical 判据；不要在 FE 镜像初始 body 文本。PR1 增加“ensure 成功但从未汇总”的空态测试。

## MINOR

1. **INV-007 不能证明“显式自反排除”是 load-bearing。**

   - 文档：projection 先 join“用户可见 cove/wave”，又要求排除 launchpad 自身事件，并声称两条规则必须一起读：[设计文档](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:93)、[设计文档](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:107)。
   - 代码反证：launchpad 位于 system cove，而普通可见 cove 查询只返回 user cove：[coves.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/coves.rs:85)。因此按当前载体，即使删除显式 `scope_wave != launchpad`，可见性 join 也会先滤掉其 `wave.report_edited`；INV-007 仍可能全绿。事件本身确实带精确 launchpad wave scope，所以显式排除可实现：[wave_report.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/wave_report.rs:699)。
   - 建议：把它写成 defense-in-depth，或在可见性 join 之前对 raw allowlisted rows 做独立 mutation test；否则删掉自反谓词也不会红。

2. **§5.1 已干净解耦 harness，但 `report_card_id` 是当前解析链不需要的 DTO 改动。**

   - 文档：resolve 返回 `wave_id + report_card_id`，随后仍调用 wave detail：[设计文档](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:157)；D1 同时又写“不新建端点”：[设计文档](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:75)。
   - 代码反证：wave detail 已返回 cards，[waves.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/waves.rs:694)，而 `readWaveReport` 自己按 `kind === 'wave-report'` 定位卡。现有 Today router 只有 POST ensure，[today.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/today.rs:37)，所以新增只读 GET 是真实改动，但给既有 `TodayLaunchpad` 增加未消费字段不是必需条件。
   - 建议：新增窄 DTO `{ wave_id, report_started }`；保留 ensure DTO 不动。D1 改成“不新增写/CRDT 端点，只新增只读 resolve”。

D4 其余部分成立：四类事件均有可用 wave scope；allowlist 与 retention 描述正确；`[start,end)` 和只按 `at` 查询正确；transport 的 unresolved、cross-session、dormant、missing row、DB error 均可在 descriptor 隐藏 + 同一 async gate 的 list/call 双重检查下 fail-closed。§5.1 的只读 GET 本身也确实能完全避开 ensure 的 materialize、submit 和 wait。
