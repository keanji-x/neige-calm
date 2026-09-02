# r2 confirm 轮 review — subagent 通道

> 总判：**rethink（限于 D1 载体的成本基线与写通道），其余 fix-then-ship。**
> 与 codex 通道**独立同结论**：新 kind 载体拿不到 report 的写通道、确定性 id 没有写口、`create_mode`/`persistence_invariants` 是死代码。

## BLOCKER

**B1 —— 块模型与 `wave-report` 是硬绑定，「复用」不是免费的。**
文档 §4 D1 称「payload 复用块模型，因此渲染器直接可用，不新做渲染路径」。渲染侧成立（`ReportDocumentProps.report: WaveReport | null`，纯值注入）；**写入侧不成立**：`db/sqlite/card.rs::card_update_with_crdt_tx` 开头就拒非 wave-report（错误串 `"card_update_with_crdt_tx is restricted to wave-report cards"`，由同文件测试 `crdt_update_seam_rejects_non_report_and_kind_change` 逐字钉死）。D1 把「渲染器可用」偷换成了「块模型可用」。

**B2 —— AI 根本没有写 daily-log 的通道；§3 那行事实是从 r1 原样搬来的。**
`wave_report_blocks.rs::commit_block_op` → `wave_report.rs::resolve_report_for_caller` → `load_report_for_wave`，按 `kind == "wave-report"` 找**那一张**卡（注释还引 `idx_cards_one_report_per_wave` 作背书）。`calm.report.blocks.*` 没有任何 target-card 参数。PR3 的闭环缺一整条写通道，而它不在 PR1/PR3 的预算里。

**B3 —— 确定性 card id 既没有写路径也没有 upsert 语义。**
`routes/cards.rs::create_card` 一律 `new_id()`，wire 上无 id 字段；`card_create_with_id_tx` 是裸 INSERT，**无 ON CONFLICT**，第二次同 id 是 UNIQUE 违例而非覆盖。引作先例的 `today.rs` 三处调用传的也都是 `new_id()`——先例只是「id 是参数」，不是「id 可派生」。

## MAJOR

**M1 —— `create_mode` / `persistence_invariants` 是死代码。**
全仓 grep 只命中定义处本身，零消费者，trait 上挂 `#[allow(dead_code)]`；`create_card` 只调 `validate_payload`。wave-report 的「内核铸造/不可删/每 wave 唯一」实际由 `card.rs` 手写的 `if p.kind == "wave-report"` 分支、`deletable` 列、migration 索引三处硬编码保证。后果：INV-001 的「唯一写口」拿不到 trait 保证，通用创建门 `POST /api/waves/{id}/cards` 今天接受任意 kind。

**M2 —— 「无条件 ensure」把读文档绑在 workspace materialize 与 harness 健康上。**
`ensure_today_launchpad` 在事务后还要 `materialize_workspace`，再 `submit("spec-harness-start")` 并 `.wait()`，失败返回 `Internal`。叠加 INV-002「任何失败都必须浮出」，codex 不可用时整页硬失败——而现在的 Today 不需要这些就能渲染。建议读路径与 ensure 解耦。

**M3 —— D4 第一层会把系统 wave 泄进日历。**
`list_waves_window` 唯一的可见性过滤是 `retain_user_visible_waves` → `user_visible_wave`，后者只排除 `COVE_CHAT_PURPOSE`；**`purpose='launchpad'` 通过**。今天的日历走客户端 `activeWavesOn(workspace.waves)`，而 workspace 只在可见 cove 上扇出，所以换成服务端窗口后，launchpad（及未来任何 system cove 里非 cove-chat 的 wave）会第一次出现在用户日历里。另：D4 没说 `activeWavesOn` 的去留，留着两份就是漂移。

**M4 —— 「一 wave 挂 N 张同 kind 卡」确实撞到一处 FE 读路径，但不是 `readWaveReport`。**
`readWaveReport` 硬绑 kind，**看不到** daily-log（所以「渲染器直接可用」这句最需要补的一行是：先把它拆成 card→`WaveReport` 的纯函数）。真正出问题的是 `systems/cards/builtins/headless-filter.ts::partitionWaveCards`：无适配器的卡进 `unknown`，唯一例外是硬编码的 `wire.kind !== 'wave-report'`。daily-log 不注册适配器就会 N 张全部列进 CARDS 面板。卡片网格只吃 `visible`，网格安全；面板不安全。
反向证据：`load_report_for_wave` 的注释写着 "waves are small (single digits of cards in practice)"——D1 恰好在 spec agent 常驻的那个 wave 上证伪它。

**M5 —— 没有单卡读端点，一天一卡会把「读一天」变成「读全年」。**
`routes/cards.rs::router()` 只有 `GET /api/waves/{wave_id}/cards` 与 `PATCH/DELETE /api/cards/{id}`——**没有 `GET /api/cards/{id}`**，也没有按 kind/时间的筛选。wave detail 返回全部 cards 及完整 payload。这才是 Q1 的真实内容，而它并不站在「一天一卡」那边。

**M6 —— r1 两条 MAJOR：一条真关，一条关了一半。**
`waves_window` 闲置端点**真关**；「跨 cove 读是新类别」**关了措辞、授权归属没关**——D4 说 MCP 层是第一层的「薄封装」，而第一层的过滤是给人用的（session 中间件 + `retain_user_visible_waves`）。要写死：复用查询函数，**不复用**人向可见性与 session 门。

## MINOR

- **D6 引错先例。** `calm.admin.wave_gc` 是 agent 触发、绑定自身 wave、清 VCS commit 的工具——恰恰是 D6 要反对的脆链。真正的后台保留循环先例是 `events_prune.rs`，其模块注释还独立说了 D6 那条诚实标注："The pruner never VACUUMs..."。
- **GC 会持续产生永久事件。** 删卡走 `card.*` 结构性事件，而 prune allowlist 明写结构性 kind "untouchable by construction"。
- **INV-003 的变异对象选错了。** 日期身份落在 id 上之后，危险面是「谁算这个日期」，变异应打时钟/时区。
- **`report.ts` 已 1164 行**，PR1 若要在其中拆分，按文件尺寸治理线该点名。

## §9 五问推荐

- **Q1** 维持一天一份，但必须补读端点（M5）。只有 id 携带日期才满足 INV-003；一卡多块的日期键必然落回内容解析。
- **Q2** 按天取 30 天，并绑定到 `events_prune` 的默认 horizon——让日志不比它总结的证据活得更久，N 就有推导而不是拍脑袋。
- **Q3** 状态条在前。§8.1 的直接推论已足够；补充：状态条高度 O(1)，不把文档推出首屏；「文档是主角」由面积和视觉权重表达。
- **Q4** 服务端固定「今天」，不接受 agent 传参。与「窗口至多一日」天然一致，参数面清零。
- **Q5** 服务端本地日，服务端算、服务端铸 id。证据：全仓 `crates/` grep 不到 `timezone`，时间一律 epoch ms；分日逻辑当前只在浏览器侧。理由：(a) id 是持久事实，不能取决于哪台浏览器最后碰过它——两个时区的客户端会为同一天铸两份，直接打穿 INV-006；(b) 活动窗口本就在服务端算，两侧日界必须同源；(c) 引入工作区时区是独立产品决策，不该被 #1253 顺手绑架。代价（跨时区错位何时不再无害）要写进 doc comment。

## 开工前必须关闭的最小集

B1（CRDT 放宽还是无 CRDT）、B2（写通道形状）、B3（确定性 id 的写口与 upsert 事务）、M2（ensure 不能无条件调用）、M5（读端点进 PR1）。M3/M4 是 PR 内的具体补丁，不必回设计轮。
