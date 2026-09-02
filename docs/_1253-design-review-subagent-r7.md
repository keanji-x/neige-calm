# r7 最终收敛确认 — subagent 通道

> 总判：**fix-then-ship**，唯一一条 BLOCKER 已在 r9 修掉 ⇒ 等价 ship。设计本身收敛，无架构级问题。

## 裸 key 方案的代码验证 —— 成立

- `derive_wave_conversation_keys` 确实用同一 digest 同时产 `card_id = conv-{digest[..32]}` 与 `operation_key = wave-conversation-{digest}`，doc comment 逐字如 r8 所述。**r8 推翻我 r5/M4 的理由成立，我 r6 的 m7 判断作废是对的。**
- `retryable_operation_key` 只在 `phase == Failed` 时给 `#N`；`insert_operation` 同 kind+key、hash 相同 → 复用，不同 → `idempotency_payload_conflict`。**残余窗口描述准确。**
- 分支判据可靠：`plan_compensation` 的注释逐字写明补偿第一次出错就 `Stuck`、不再重驱，遗留卡带 `deletable: false`。**「卡存在就不会再走 create」撑得住。**
- `MAX_SPEC_INPUT_CHARS = 32_768`、`spec_harness_dormant` 错误码均存在；首次 2 行 enqueued 的推导与 `create_wave_conversation`（wait → claim → `send_spec_input`）一致。

## BLOCKER

**§6 切片表 PR2 行仍在教实现者做 r8 刚推翻的那件事。** 逐字写着「掺 workspace digest 的确定性 key」以及「三次触发 → 三次新增」，与 D5（「key 必须是裸 `today-summary`」）、INV-011、INV-010 的 2+1+1 直接矛盾。按它派活会派生第二张 `deletable:false` 的会话卡，正是 r8 论证要防的后果。

**这是我 r6 那条 MAJOR 的同一处、同一形状第二次没关闭**——切片表才是拆 brief 的载体。修正是一行，但改完前不能 dispatch。

## MINOR

- **payload 字段枚举不完整。** 文档写六个，实际 `SpecHarnessStartOperationPayload` 有 11–12 个字段，漏了 `goal` / `reset_harness_items` / `force_new_thread` / `profile` / `create_card` / `first_message_sha256`。前五个在固定 key 下恒定，但 **`first_message_sha256` 是第三个真变量**——它被「bootstrap 必须静态」那条管住了，可「只剩两个变量」的论证本身把它漏了。
- **actor 那条的理由略偏。** `Actor::to_actor_id()` 把 `"user"` 和一切非 `ai:codex` 的值都映射成 `ActorId::User`，中间件只放行 `user` / `ai:<id>`。所以「owner/dev 两个账号各点一次就永久 409」到不了——两个人类账号本就同为 `ActorId::User`；真正的变量通道是客户端自带 `X-Calm-Actor: ai:<id>`。另附实现约束：想按 kernel 归属就不能经 `to_actor_id()`，必须直接构造。
- **§10 标题与表停在「四轮」**，无 r6–r8 行；纯存档性。

## 结论

r6 的七条 MINOR 六条真关闭（命名已全量落地，全文无 `report_started`），m7 被 r8 正当推翻。只差切片表那一处两句的同步——改完即 ship，不需要再开一轮。
