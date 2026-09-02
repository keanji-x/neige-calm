
2. M2 已真正关闭：INV-007、方法论备注及 §8 均明确限定该端点，没有残留全称。

**最终：fix-then-ship。**
tokens used
151,016
总判：**fix-then-ship**。不是 ship。B1、M2 已关闭，但仍有 1 个 BLOCKER。

## BLOCKER

1. **workspace digest 被掺进了错误的 key，会随 workspace re-point 派生出第二张 conversation 卡。**

   [D5](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:187)先用裸 `today-summary` 派生 `card_id`，[随后](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:198)又要求 key 掺 `workspace_key_digest(cwd)`。现有 create 路径中，同一个 `Idempotency-Key` 同时派生 card id 和 operation key：[conversation_keys.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/conversation_keys.rs:80)、[wave_conversations.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/wave_conversations.rs:176)。

   因此 cwd digest 一变，card id 也变；旧卡仍在，新卡会被创建，直接推翻“全生命周期一条汇总 conversation”。若仍按伪码查裸 key 的 card、却用 digest key 创建，创建出的又不是所查的 card。`today.rs` 的办法不能原样搬来，因为它的 key 不承担 conversation identity。

   必须明确解耦：稳定的裸 key 只决定 card id；workspace digest 只进入 start operation 的幂等 key。现有公共 conversation POST 不支持这种解耦，需要内部 helper/载体裁决。

## MAJOR

1. **INV-010 的信号可用，但正例仍是假的。**

   每次成功 `/spec/input` 都在返回前写一行事件：[cards.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/routes/cards.rs:859)。该 kind 不在 prune allowlist，确实永久且已有同型 SQL 测试：[events_prune.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-truth/src/events_prune.rs:93)。

   但第一次 summary trigger 会先由 create 写一行静态 bootstrap，再由无条件 spec input 写一行动态摘要；所以“三次触发 → 三行”实际是四行。应分别钉住“首次成功 → 2 行、已有卡时每次 → 1 行”，或增加能区分 trigger intent 的信号。§6 还残留“点三次 → 三次 turn”的旧判据：[设计文档](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:293)。

2. **M1 未真正关闭。**

   “最多 N 条 wave 明细”只限制行数，没有写死 N、DTO 字段及字符串长度；固定数量的可变长字符串仍不能证明 prompt ≤ 32,768。应写死纯计数 DTO，或给完整序列化字符预算与边界测试：[设计文档](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:170)。

3. **M3 仍把内容判据说成写历史。**

   helper 只判断当前 `summary/body` 是否不同于 initial：[wave_report.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/wave_report.rs:181)。写后恢复 initial、等内容写入、仅 revision/blocks 改变都仍为 false。因此“被任何人写过没有”“汇总跑过后空态永不回来”仍不成立；DTO 也仍叫 `report_started`，只是“建议”改名。应统一改成 `report_has_noninitial_content`，表中正例改为 canonical initial → 空态。

4. **dormant 恢复有代码载体，但没有真的挂进 INV-002。**

   `/spec/reset` 能按 Assistant profile 重启，规则站得住；但 INV-002 仍只测试 `ensure` 的 5xx 是否浮出，没有 dormant → reset → 单次重试的反例/正例：[不变量表](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/docs/1253-today-document-design.md:265)。

## MINOR

1. B1 的活动确实会在首次成功请求中送达，已关闭；但两条 `UserMessage` 都是 hard-fire，会绕过 250ms debounce，只取决于是否赶在下一次 50ms tick 前同时入队。可能合并，也可能先跑一个 bootstrap-only turn：[observation.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-types/src/observation.rs:139)、[run_loop.rs](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/harness/run_loop.rs:1034)。文档的 debounce 解释应修正。

2. M2 已真正关闭：INV-007、方法论备注及 §8 均明确限定该端点，没有残留全称。

**最终：fix-then-ship。**
