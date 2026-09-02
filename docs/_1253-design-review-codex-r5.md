
   - 文档：[§0b.59](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:59)删除全部截断纪律；D4 没定义 projection DTO/每 wave 行数/总字符上限。
   - 代码反证：create 的 `validate_first_message` 与 `spec/input` 都拒绝超过 **32,768 Unicode 字符**：[conversations_shared.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/conversations_shared.rs:39)、[cards.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/cards.rs:766)。动态摘要不走 arm (e)，但会直接撞 400；若塞回 create，则还会撞同 key/不同 SHA 的 409。
   - 建议：若 projection 只返回四个全局计数，把固定 O(1) DTO 写死；否则恢复确定性总字符预算，预算须覆盖模板文本，并加 32,768/32,769 与 CJK 边界测试。

2. **INV-007 不是全局闸；现有公共写入口可绕过。**

   - 文档：[D5](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:157)及[§8](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:269)把它表述成“没有素材时不写”。
   - 代码反证：公共 conversation POST 只排除 cove-chat wave，不排除 `purpose='launchpad'`：[wave_conversations.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/wave_conversations.rs:158)。它返回 assistant card id，而该卡明确允许公共 `/spec/input`：[model.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/model.rs:443)、[cards.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/cards.rs:814)。用户可在空窗口直接创建 launchpad assistant，或向既有汇总卡发送自带“摘要”并促使写 report。
   - 建议：二选一：把 INV-007 缩窄为“`POST /api/today/summary` 自身拒绝空窗口”，明确手动 conversation/input 是例外；或禁止外部 generic routes 操作保留的 summary card，由 server-only 内部入口发送。

3. **`report_startup_read_required()` 不是“写过没有”的判据。**

   - 文档：[D7](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:187)及[INV-003 备注](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:232)把它称为 canonical 写历史。
   - 代码反证：函数只比较 `summary + body`，明确忽略 `doc_rev + blocks`：[wave_report.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/wave_report.rs:179)。每次成功 persist——包括 content-equal replacement——都会增加 `doc_rev`：[wave_report.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/wave_report.rs:501)。因此写过再恢复初始正文、同内容写、仅改变 block identity/revision，都仍返回 false。
   - 建议：若 UX 真正要的是“当前仍为 canonical placeholder”，保留函数但把 DTO/不变量改名为 `report_has_noninitial_content`；若要“曾经汇总过”，必须使用 durable marker/event，不能复用该 helper。

## MINOR

无。

已验证成立：`derive_wave_conversation_keys(wave_id,key)` 是稳定纯 SHA-256 映射；中间态按 404 的 DTO 裁决可实现；`is_unique_constraint` 确实有两处错误调用，应分别改成 `coves.kind`、`waves.purpose`；INV-006 的 `[start,next_start)` 午夜测试可直接写。

INV-010 的第二反例也可直接写：预置汇总 harness 为 `TurnRunning`，连续 POST 三次，随后发 `TurnCompleted`，用现成 `turn_start_count_for_test` / `started_turns_for_test` 断言只新增了一轮。这个测试在当前设计下应当红，正好证明载体仍不满足不变量。
tokens used
214,844
结论：**2 BLOCKER / 3 MAJOR / 0 MINOR；fix-then-ship**。按 navi 追到了 route → harness run loop；全程只读，工作树 clean。

## BLOCKER

1. **首次触发没有一条确定路径把活动摘要交给 agent。**

   - 文档：[D5 四步与分支](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:154)说先注入摘要，再二选一；但[创建首条消息](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:170)又明确不得含摘要，摘要只走“重跑”的 `spec/input`。
   - 代码反证：conversation create 必须发送一条非空首消息，并将其逐字节 SHA-256 绑定 operation；成功后它确实立即调用 `send_spec_input`：[wave_conversations.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/wave_conversations.rs:153)、[wave_conversations.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/wave_conversations.rs:201)、[wave_conversations.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/wave_conversations.rs:245)。按表格实现，第一次只收到静态 bootstrap，活动摘要根本没发；若 create 后再补一次 `spec/input`，则首次触发可能跑一轮或两轮，取决于 50ms tick 是否已经 drain。
   - 建议：明确首次算法。若坚持“一次触发=一轮”，需要内部的“mint conversation without first turn”能力，再仅用一次 summary input；不能靠 create(static) 紧接 spec-input(dynamic) 的时序碰运气。

2. **INV-010 的“一次触发对应一次新 turn”不受 `/spec/input` 保证。**

   - 文档：[INV-010](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:227)要求点三次确实跑三轮。
   - 代码反证：`send_spec_input` 的 200 只表示 `try_send` 进 observation channel：[cards.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/cards.rs:786)、[cards.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/cards.rs:859)。run loop 会把 pending queue **整批 drain**、拼成一个 input、只调用一次 `turn_start`：[run_loop.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/harness/run_loop.rs:1250)。因此当前 turn 运行时连点三次，三条都会排队，但完成后合成一轮。更坏时 pending queue 满且全是 hard-fire，incoming observation 会被静默丢弃：[run_loop.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/harness/run_loop.rs:600)。
   - harness 状态还会导致：starting/daemon unavailable/ingress queue 满 → 503；dormant/unrecoverable 或 channel shutting down → 409；只有 live/recoverable session 才排队：[cards.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/cards.rs:1196)。
   - 建议：为 summary trigger 增加服务端串行化和可观察的 trigger/turn id，响应至少确认独立 `turn_start`；或把不变量降为“每次成功请求都保留一条 intent，允许合并 turn”。若仍要求三轮，现有裸 `spec/input` 不能直接承载。

## MAJOR

1. **删除截断纪律后，服务端 prompt 没有可证明的长度界。**

   - 文档：[§0b.59](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:59)删除全部截断纪律；D4 没定义 projection DTO/每 wave 行数/总字符上限。
   - 代码反证：create 的 `validate_first_message` 与 `spec/input` 都拒绝超过 **32,768 Unicode 字符**：[conversations_shared.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/conversations_shared.rs:39)、[cards.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/cards.rs:766)。动态摘要不走 arm (e)，但会直接撞 400；若塞回 create，则还会撞同 key/不同 SHA 的 409。
   - 建议：若 projection 只返回四个全局计数，把固定 O(1) DTO 写死；否则恢复确定性总字符预算，预算须覆盖模板文本，并加 32,768/32,769 与 CJK 边界测试。

2. **INV-007 不是全局闸；现有公共写入口可绕过。**

   - 文档：[D5](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:157)及[§8](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:269)把它表述成“没有素材时不写”。
   - 代码反证：公共 conversation POST 只排除 cove-chat wave，不排除 `purpose='launchpad'`：[wave_conversations.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/wave_conversations.rs:158)。它返回 assistant card id，而该卡明确允许公共 `/spec/input`：[model.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/model.rs:443)、[cards.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/cards.rs:814)。用户可在空窗口直接创建 launchpad assistant，或向既有汇总卡发送自带“摘要”并促使写 report。
   - 建议：二选一：把 INV-007 缩窄为“`POST /api/today/summary` 自身拒绝空窗口”，明确手动 conversation/input 是例外；或禁止外部 generic routes 操作保留的 summary card，由 server-only 内部入口发送。

3. **`report_startup_read_required()` 不是“写过没有”的判据。**

   - 文档：[D7](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:187)及[INV-003 备注](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:232)把它称为 canonical 写历史。
   - 代码反证：函数只比较 `summary + body`，明确忽略 `doc_rev + blocks`：[wave_report.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/wave_report.rs:179)。每次成功 persist——包括 content-equal replacement——都会增加 `doc_rev`：[wave_report.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/wave_report.rs:501)。因此写过再恢复初始正文、同内容写、仅改变 block identity/revision，都仍返回 false。
   - 建议：若 UX 真正要的是“当前仍为 canonical placeholder”，保留函数但把 DTO/不变量改名为 `report_has_noninitial_content`；若要“曾经汇总过”，必须使用 durable marker/event，不能复用该 helper。

## MINOR

无。

已验证成立：`derive_wave_conversation_keys(wave_id,key)` 是稳定纯 SHA-256 映射；中间态按 404 的 DTO 裁决可实现；`is_unique_constraint` 确实有两处错误调用，应分别改成 `coves.kind`、`waves.purpose`；INV-006 的 `[start,next_start)` 午夜测试可直接写。

INV-010 的第二反例也可直接写：预置汇总 harness 为 `TurnRunning`，连续 POST 三次，随后发 `TurnCompleted`，用现成 `turn_start_count_for_test` / `started_turns_for_test` 断言只新增了一轮。这个测试在当前设计下应当红，正好证明载体仍不满足不变量。
