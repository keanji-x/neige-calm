# r4 窄审 — subagent 通道

> 总判：**fix-then-ship**。PR1 可直接开工；PR2 开工前必须先改文档（四条段落级重写，不是重新设计）。

## BLOCKER

**B-1 —— D5「用 `Idempotency-Key` 复用当天那条汇总会话」会让第 2 次以后的点击变成静默 no-op。「同 key 复用」和「同日重跑」不是同一件事。**

`create_wave_conversation` 在 operation 成功后走 `conversations_shared.rs::user_message_already_enqueued`（`SELECT 1 FROM events WHERE kind='harness.user_message.enqueued' AND scope_wave=?1 AND scope_card=?2`）——**为真就跳过 `send_spec_input`**。arm (a) 的 utoipa 文档写死：*"replays the same conversation and **does not re-send the first message**"*。

后果：同 key 第二次点击 → 不发消息 → agent 不跑 → report 不更新，但 HTTP 201、UI 一切正常。**INV-010 会绿，功能是坏的。** 反过来若换新 key 就是三条会话，INV-010 红。两个目标在这个端点上互斥。

建议：D5 拆成两条路径——(1) 首次 `POST /api/waves/{id}/conversations`，key 确定性派生；(2) 重跑 resolve `derive_wave_conversation_keys(wave_id, key).card_id` 后 `POST /api/cards/{id}/spec/input`。INV-010 的反例要加「第二次点击必须真的产生一次新的 agent turn」。

**B-2 —— key 由谁生成没写；现有唯一生产者是随机 mint，且落在 #1225 的丢失窗口里。**

`app/router/idempotency-key.ts::mintIdempotencyKey` 每次返回新的随机 v4（明文 LAN 下 `randomUUID` 不可用，手搓）。#1225 确认 `held = { key, sentText }` 存在路由级 `useReducer` 里，`WaveRouteBody` 是 `key={wave.id}`，切走再回来必丢。所以「同 key」今天是**一个会话周期内的短命客户端状态**，撑不起「今天这一整天」。key 必须是**纯函数**（如 `today-summary:<YYYY-MM-DD>`），这时 #1225 自动绕开。

另注 arm (e)：同 key + 不同 `text` → 409 `conflict`（text 以 SHA-256 绑进 operation payload）。所以 prompt 文本必须**逐字节稳定**——若嵌了日期/活动摘要/时间戳，第二次就是 409。要写进 D5 并配断言。

**B-3 —— INV-007「空活动不发起」没有执行点。**

D4 第一层是 repo/service helper，第二层 `calm.day.activity` 是 MCP 面（只有 agent 能调）。**FE 没有任何 REST 面读得到这份 projection**，§6 的 PR2 也没有这样的端点。于是 INV-007 只能靠「FE 藏按钮」——绕过按钮直接 POST 就发起了，而 `create_wave_conversation` 不认识「今天有没有活动」。这属于 `feedback_statement_widened_past_carrier`：句子写得比载体宽。

建议：闸下沉到发起动作的**服务端**；或承认它是 UI 约束并改写可证死形式。

## MAJOR

**M-1 —— 「复用当天那条」= 每天恰好一条常驻 assistant 会话，r3 的成本没消掉，只是从「每次点击 +1」变成「每天 +1」。**

D5 先写「descope 后不是每天一条会话」，紧接着的裁决**正是**按天分键，两句打架。`recover_harnesses_on_boot` → `spawn_recovered_harness`（`idle` 算 active），每条起 `run_loop.rs` 的 50ms `interval`；`operations` 全仓无 pruner。这与 r3 判死「每天一条 assistant 会话」的论证逐字相同，只是没有 wave/工作区。

建议二选一并写进成本表：(a) 无日期的**单一**汇总会话；(b) 按日分键 + 明说「每天 +1 条常驻会话，无回收，可接受的理由是 X」。

**M-2 —— D4 第二层的 descriptor 形状在内置工具上没有那条缝。**

`registry.rs::descriptors_for_role` 就是 `filter(|d| d.visible_to_roles.contains(&role))`——`&[]` 的内置工具对**任何**角色永不出现在 `tools/list`，没有 per-identity 加回的钩子。唯一扩展点 `transport.rs::extend_plugin_tool_descriptors_for_role` 只处理**插件**工具，且其 `None` 分支注释明写 *"the shared scope function yields the union (F7 — discovery wide, dispatch strict)"*。

现有 `&[]` 内置工具的先例说明了用途：`tools/wave_history.rs` 头注释——*"Human-facing drill-in goes through `neige diff` …"*，是给 CLI 调的，不是给模型调的。**一个模型看不见的工具，prompt 里让它调是碰运气。**

建议三选一：(a) 新建内置工具的 per-identity augmentation 缝（成本进 PR2，且必须证明 unresolved 分支 fail-**closed**，与插件路径相反）；(b) `visible_to_roles: &[CardRole::Assistant]`，承认全仓每个 assistant 都看得见工具名，闸只在 `tools/call`；(c) **不走 MCP，改由 harness 在起会话时把当天活动注入 prompt** —— 这样第二层、`day_activity_allowed`、截断纪律整块消失，PR2 少一半。倾向 (c)：参数已写死「今天」、无 args、只读、单调用方，做成工具买不到任何东西，却买来「全 MCP 面第一个跨 cove 读」这个 §7 自认的最大风险。

**M-3 —— D4 第一层的两条过滤规则会互相吞掉。**

launchpad 在 system cove，而 §3 自己引用 #175「system cove 默认不在 `GET /api/coves` 里」。若「用户可见 cove」用同一判据，整个 system cove 已被排除，自反排除是死代码；若为看全活动而不按 cove 过滤，则「join 用户可见 cove/wave」这句是假的。两句必须挑一条做载重。

顺带：自反排除按 **wave** 粒度时，用户**手改** Today report 也不算活动。无害但要写明。

## MINOR

- **m-1** §3「全仓无按 `at` 的读者」不准确：`events_prune.rs` 已经是 `WHERE kind = ?1 AND at < ?2`。改成「无按 `at` 的**读**路径；唯一既有用户是 retention 的 DELETE」。
- **m-2** §3 缺一行：D4 的地基是 `events` 的 **scope 列**。已验证 `0007_events_scope.sql` 提供 `scope_kind/cove/wave/card` + 两个 partial index；`WaveReportEdited` 的 doc comment 明写 `scope_wave = wave_id`；`task.completed/failed` 经 `emit.rs::commit_worker_task_report_for_identity` 走 card scope。**结论是好的（D4 可行）**，但 0007 注释也写了老行一律 `scope_kind='system'` 且不回填——半开窗口跨过升级点会静默漏掉老事件，doc comment 该带这条。
- **m-3** §5.1 的顺手修只修了一半：`is_unique_constraint` 在**两处**被按索引名调用（`idx_coves_one_system`、`idx_waves_one_launchpad`），SQLite 报的是 `coves.kind` / `waves.purpose`。而 §7 要求「ensure 那条路径按新路径验，含并发撞唯一索引」——前者恰好在 system cove 首次并发 mint 的 race 上，必须一起修。
- **m-4** 只读 resolve 会看到 `ensure` 未走完的中间态（wave 行在、report 卡还没建）。`report_card_id` 无值时是 404 还是 `Option`，文档没说。
- **m-5** Q6 的答案是「都写」，直接定掉，不必留成开放问题。

## §3 事实表逐行核验（本轮实测）

✔ 亲自复验站得住的：ensure 幂等 + 已建 `wave-report` 卡；无生产调用方（全仓 grep，只有生成签名无调用）；materialize + submit + `.wait()`；DTO 不含 `report_card_id`；`harness.item.added` 在 prune allowlist、`task.*` 不在；文体契约确实种在 body 里且就是那四段（`initial_body` → `# 概要 / # 待你定 / # 已完成 / # 决策`）——**这行的 ✔ 是真的，r4 没有把旧载体的结论搬过来。**

⚠ 需要修辞降级：第 10 行（m-1）；第 7 行 `Idempotency-Key` 的 ✔（字面为真，但在 r4 载体下推不出 D5 想要的复用语义——B-1/B-2）。

✘ 反向过度纠正成「零改动」而实际有改动：D4 第二层的 tools/list 可见性（M-2）、INV-007 的服务端执行点（B-3）。

## 结论

PR1 不受任何一条阻塞，可直接开工（§5.1 的只读 resolve 确实把读路径与 harness 解耦干净），只需把 m-3、m-4 补进范围。PR2 开工前先改文档：B-1/B-2/B-3/M-1/M-2 五条段落级重写。若采纳 M-2 (c)，PR2 明显缩小，§7 最大风险直接消失。
