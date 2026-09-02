# r5 窄审 — subagent 通道

> 总判：**fix-then-ship**。载体、D4 单层 projection、D7 的 `report_started` 判据、两处 `is_unique_constraint` 都经代码验证成立，PR 切分合理。全部是段落级修订，载体不动。

## BLOCKER

**B1 —— D5 内部自相矛盾：第一次点击的那一轮拿不到任何活动数据。**

D5 步骤 3 说「把活动摘要注入 prompt（不是让 agent 去查）」，同节又说「创建用的首条消息不得内嵌日期、活动摘要或时间戳——活动摘要走重跑路径的 spec input」。而 r5 已经删掉了 agent 侧的查询能力（D4 第二层），创建路径的 `text` 就是唯一送达 agent 的东西：`create_wave_conversation` 把 `text` 交给 `send_spec_input`，之后 `user_message_already_enqueued` 保证同一张卡不会再补发。

**所以「会话尚不存在」那一支跑完，agent 收到的是一条静态、无数据的消息，且没有别的渠道取活动。首次使用——也是用户唯一会看的第一印象——必然产出无素材的汇总。**

建议：创建那次只发**静态 bootstrap 文本**（保证 arm (e) 的字节稳定），然后**无条件**再走一次 `POST /api/cards/{id}/spec/input` 投递含活动的 prompt。即「创建 = 建会话，spec input = 唯一的 prompt 通道」，两条路径合并成「必要时先建，再一律 spec input」。同时给 INV-010 补断言：第一次点击后 harness 收到的 turn 文本必须含活动摘要——否则这个洞会绿着骗人，与 r4 那次同型。

**B2 —— INV-010 的正例「一条会话，三次 turn」被 harness 设计证伪。**

`run_loop.rs::maybe_issue_turn` 一次 `queue.drain(..)` 把**整个 pending 队列**拼成 `joined_observation_text` 发**一个** `turn_start`；发车前还有去抖 `debounce_min_idle=250ms` / `debounce_max_wait=5s`。汇总 turn 通常跑数秒，期间第 2、3 次 POST 全部排队，随后合并成第二个 turn——典型三连点 = **2 个 turn**，不是 3。队列满 256 时还会 `try_fold_pending_tail` 把相邻 UserMessage 拼成一条。

建议：把「有效性」拆成两条可证死的：(a) 每次 POST 必留一行 `harness.user_message.enqueued`（永久 kind，明写不在 prune allowlist 内）→ 三次 POST 三行；(b) 用既有的 `turn_start_count_for_test` / `started_turns_for_test` 断言，而不是「恰好等于 3」。反例改写成「第二次 POST 之后不再有新的 enqueued 行」。

## MAJOR

**M1 —— INV-007 又一次写宽过载体。** 陈述是「**服务端**在活动窗口为空时拒绝发起」，但闸只在 `POST /api/today/summary` 里。同一个已认证用户可以直接打 `POST /api/waves/{launchpad}/conversations`（唯一的 wave 级守卫只拒 cove-chat purpose，launchpad 不在其列）或 `POST /api/cards/{id}/spec/input`（只校验 text/角色）。这条全称陈述当前就是假的，而反例「绕过按钮直接 POST」还没说清 POST 哪个端点——按字面读它自己就红。
建议：收窄成「`POST /api/today/summary` 在空窗口下既不建会话也不发消息」，并写明其余两个端点故意不在射程内（用户手打不是要防的事）。

**M2 —— INV-010 第一句「全生命周期只有一条汇总会话」同病。** 内核对「每个 wave 几条 assistant 会话」没有任何上界：任意 `Idempotency-Key` 都派生出一张新卡，而 Today 页本来就有 Conversations 模块的 `+`。端到端数会话数量证不了它。
建议：改成纯函数性质——「固定 key 下 `derive_wave_conversation_keys(launchpad, "today-summary").card_id` 恒定」，用单测 golden 钉住（与 `conversation_keys.rs` 现成的两条 golden 同型），而不是数据库计数。

**M3 —— D5 重跑路径的错误面没裁决，而且不是「重试就好」。** `send_spec_input` → `ensure_live_spec_harness` 会在四种情况下失败：无 active runtime 行 / 无 thread / snapshot 形状损坏 → **409 `spec_harness_dormant`**（文案就是「reset to start a session」）；`Starting` → 503；共享 app-server 未运行 → 503；`observe` 是有界 `try_send`（`OBSERVATION_BUFFER = 256`）→ 503。
**在「全生命周期只有一条会话」的裁决下，一次 dormant 就让这个按钮永久死掉**，只能靠人手 reset。
建议：D5 明写恢复规则（收到 `spec_harness_dormant` 时对该卡重新提交 spec-harness-start / 走 `/spec/reset`），并挂进 INV-002。

**M4 —— 确定性 key 冻结的不只是 text，`actor` 和 `cwd` 也在 payload hash 里。** 创建路径把整个 `SpecHarnessStartOperationPayload` 送进 `stable_payload_hash`，其中包含 `actor` 与 `cwd: wave.workspace.path`；`insert_operation` 对「同 idempotency_key + 不同 payload_hash」**硬 409，且 `operations` 无 pruner ⇒ 永久**。
**这正是 `today.rs` 里逐字记过的事故（「409, on every request, forever」），当时的解法是把 workspace digest 塞进 key。**固定 `today-summary` 把它请了回来：换一个登录身份（owner/dev 两个账号都存在）或一次 workspace re-point（launchpad 明确允许 re-point）落在「创建尚未成功」的窗口里，按钮就永久 409。
建议：汇总端点把 actor 固定成单一值（并按 `identity_migration_attribution_scope` 裁决这条汇总记在谁头上），key 里比照 `today.rs` 掺 `workspace_key_digest(cwd)`，另加兜底「创建返回 409 conflict ⇒ resolve 派生卡 ⇒ 转 spec input」。

**M5 —— INV-003 的判据本身没问题（已验证），但它答的问题和文档说的不是同一个。**
正面结论：**blocks-only 写不会漏判**——`calm.report.blocks.*` 每次落库都从 CRDT 重投影出 `body` 再写回 payload，所以只比 summary+body 的 `report_startup_read_required()` 会翻真。空态判据成立。
问题在语义：它答的是「这份 report 被**任何人**写过没有」，不是「今日汇总跑过没有」。汇总跑过一次以后空态永不回来，哪怕内容陈旧、或最后的写者是用户手改（甚至 D1 记的 concierge 第二写者复活）。descope 之后可以接受，但 D7/INV-003 现在读起来像是后者。
建议：D7 写明这层近似；并把**反方向**补进 INV-003——canonical 初始 payload 且 `doc_rev`/`blocks` 已被 CRDT 物化时（该函数**刻意**忽略这两者），`report_started` 仍须为 false。这正是 r4 判据翻车的那一格，也是最容易回归的一格。

## MINOR

- **m1 §5.1 的「中间态」论证不准确。** `today_launchpad_ensure_tx` 在**同一个事务**里建 wave 和 report 卡；adopt-legacy 那支在同一 tx 提交前也还没有 `purpose='launchpad'`，按 purpose 查的 resolve 根本看不见它。所以「wave 行在、report 卡还没建」经本路由**不可达**。404 规则本身没问题（便宜、fail-closed），但别把不可达状态写成裁决理由，否则又是一条删掉也不会红的假不变量。
- **m2 `is_unique_constraint` 两处确认是真 bug，但修它是行为变更。** 两个索引都是 partial，实跑 sqlite 确认报的是 `UNIQUE constraint failed: waves.purpose`，不含索引名 ⇒ 两个 `Err(...)` 分支现在都是**死代码**。改成列名形式会把「500 直抛」变成「重试并成功」，所以必须配**真并发或故障注入**用例，不能用手工构造的 `CalmError` 糊过去（`waves.rs` 已导出 `is_unique_constraint_for_test`，很容易只测到那层）。
- **m3 长度上限不是风险，但「丢消息」是。** 两个入口共用 `MAX_SPEC_INPUT_CHARS = 32_768` 字符，计数型摘要远够用。真正该写进 D4/D5 的是另一端：pending 队列满 256 且折叠后超过 `MAX_FOLDED_USER_MESSAGE_CHARS = 4×32768` 时，观测被**直接丢弃**（只留一条 warn 日志）——「点了但什么都没发生、且无错误返回」，和 INV-010 想防的是同一类。
- **m4 create-vs-rerun 的判据要写死。** 分支必须按 `card_get(derived.card_id)` 判，不能靠列表/启发式：`Stuck` 补偿会留下卡却没有 runtime（且卡是 `deletable: false`，用户删不掉），此时选错分支就撞 M3 的 dormant 死路。

## 结论

必须先改的是 B1（D5 段落级自相矛盾，会让首次使用直接失效）和 B2（INV-010 正例与 harness 的 drain+debounce 语义冲突，照写必红或必靠时序凑绿）；M1/M2 是同一条老病（陈述写宽过载体）的第五、第六次发作，属于改措辞 + 换证明层，成本很低；M3/M4 各需在 D5 里加一段裁决。
