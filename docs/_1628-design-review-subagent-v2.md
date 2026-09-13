## #1628 设计 v2 评审（通道 A，第 2 轮）

基线核对：`review-a-1628` HEAD `ec0b3590` ← `1d2d3706` ← `c534bf6b` ✓，diff 只含三个 docs 文件。

### 0. 第 1 轮处置复核

逐条到代码与 v2 正文核对，结论：
- codex BLOCKER / A B1 构造 1-3：D2 步骤 2-4 引用的机制全部真实存在（`transport.rs:771` `plugin_tool_route` 返回 `(plugin_id, tool, kind)`；`connector.rs:44-55` 三变体；`manifest.rs:766-780` `annotations`）。但 `plugin_tool_route` **不返回 `ExposedTool`**，`readOnlyHint` 必须另做一次 registry 查找——"路由与 agent 路径同一函数"只覆盖一半，见 m3。
- "消失"两条（codex 8s/read、A M4）：理由成立，D4 read 确为纯 DB 读。
- "部分采纳"G11 的量化声明不成立，见 m2。
- A M3（`as_of` 上界）标"采纳"，但落点在 `kinds.rs` 纯函数里，载体不存在，见 M2。
- A M1 标"采纳"，但 7c 把"改 view hash 不变"写宽了，见 m1。
- 其余（F2 九处、F4.5、F4.10、F4.13、F7.3、F7.6、m3-m10）与代码一致。

### 1. 事实表核实（v2 新增/改动项）

全部打开核对，均与 `c534bf6b` 一致：`events.rs:787-789`（commit 后逐条 `emit_envelope`）、`card_fsm.rs:350-364`（`subscribe` + spawn 循环 + `Lagged` warn）、`sqlite/mod.rs:4`/`:264`（`.foreign_keys(true)`）、`transport.rs:771` 私有 `fn`、`cards.rs:573/588`、`mcp.rs:477/667/676/804/900`、`main.rs:52/2572/2590-2603`、`tracks.rs:2324/2717/2795/2848/4032-4037`、`track_report.rs:73/520/539/643/660`、0104:5、0106:4、`BUS_CAPACITY=1024`、版本常量四处。F2 穷举：重跑文档命令并去掉 card_kind/forge_action/注释后恰好 9 处生产入口 ✓；另核实 `PATCH /api/cards/{id}` 不能改 track-report payload（`calm-truth/src/db/sqlite/card.rs:253-258` 400 拒绝），"全部经 `validate_payload`"成立。§2 唯一小错：F3.7 `event_bus.rs` 在 `calm-truth`，不在 calm-server（文档未标 crate）。棘轮：`gate-1316-terminology-ratchet.sh` 在该树 OK。

### MAJOR

**M1. 钉住条件"回复 `as_of == 请求 as_of`"语义未定义：要么空洞、要么对非交易日永不满足。**
- 证据：D2 步骤 6 只要求回复 `as_of` 是日期且 ≥ 每条末点；D3 钉住要求相等；S3 未规定插件如何填 `as_of`。
- 构造：`as_of:"2026-09-06"`（周日）。(a) 插件回显请求值 → 恒等 → 检查空洞，插件回什么都钉；(b) 插件填末根 bar 日期 `09-04` → 永不相等 → `pinned=0`，每 6h 重投直到永远（G3 只说"可能"，实际上任何周末/假日 `as_of` 必然如此，而"as of 报告日"落周末很常见）。
- 处置：把钉住判据改成内核可验证的东西：每条 `ok` ∧ 每条末点日期 ≤ `as_of` ∧ 插件对每条声明 `complete_through >= as_of`（或等价的"该窗口无更多 bar"），并写明 S3 的 `as_of` 填法；A7b 的"回复 `as_of` 早于请求"用例改为对应新判据。

**M2. `as_of < 今天(UTC)` 放在 `calm-types::validate_chart_series`，但该 crate 没有时钟。**
- 证据：`rg "^chrono|^time" crates/calm-types/Cargo.toml` 零命中；`rg "Utc::now|SystemTime::now" crates/calm-types/src` 零命中；`validate_payload(kind, payload)` 是纯函数（`kinds.rs:89`）。
- 后果：要么给 9 处入口的公共签名加时钟参数（与"自动覆盖"矛盾），要么在纯校验里塞 `SystemTime::now()` + 手写 civil-date 算法（隐蔽的不纯，fixture 测试随日期漂移）。A2 的变异靶点当前指向不存在的代码。
- 处置：文档明确二选一：(i) `validate_chart_series` 只查格式，上界检查移到 `track_report_guard`（calm-server，可注入时钟）——但需证明 F2.1/F2.4 直接调 `render_data_block` 的两处也经过它，否则不是 9/9；(ii) 上界在 resolver 判（`unavailable, reason="as_of must be in the past"`），写端不拦。我倾向 (ii)+fe zod 提示，因为它保持纯函数且 fail-closed。

**M3. live 行会吞进盘中未收盘 bar，与非目标"不做盘中"矛盾。**
- 证据：D2 步骤 6 对 live 只要求"每点日期 ≤ 今天"；§2.5 腾讯 ifzq 在盘中返回当日实时更新的 bar（实测表未排除）；§2.8 "内核不理解交易日历"。
- 构造：15:00 UTC 解析 `US:NVDA` live → 末点为当日盘中价 → `summary.last/change_pct` 按盘中价算 → 存 6h → Planner 读到"close"实为半根 bar。
- 处置：live 规则改为末点日期 `< 今天(UTC)`（与 `as_of` 上界同形，最多晚一天，与日线语义一致）；S3 插件同样丢当日 bar；登记 CN 市场 UTC 日期偏移一天为 G。

**M4. resolver 生命周期的失败路径未规定，会 fail-locked 成永久 `pending`。**
- 证据：D2 只在步骤 8 `inflight.remove(key)`；步骤 7 写行可能因 FK/IO 失败（D3 说"任务 warn 并丢弃"）；drain 任务"首个 job 时 spawn"，无 panic/退出处理。
- 构造 1：步骤 7 返回 `Err` 提前退出 → key 留在 `inflight` → 之后所有 `enqueue` no-op → 该键直到重启都是 `pending`（读者兜底也被去重挡住）。构造 2：drain 任务 panic → mpsc receiver 丢 → `send` 失败 → 该插件所有后续 job 静默丢失。
- 处置：`inflight` 移除用 RAII guard；drain 任务用 `JoinHandle` 监督、`send` 失败时重建；A9 加"步骤 7 失败后同键再 enqueue 仍会调插件"的用例。

### MINOR

- m1. seq 7c / D1 "改 `view` hash 不变"过宽：`fields` 由 `view` 派生，`line↔candles` 切换必然换 hash、换行、frozen 重新解析。改为"`view ∈ {line,normalized,bar}` 之间切换 hash 不变"。
- m2. G11 "泄漏上界为 1" 为假：永不回复的插件下，每个 30s 超时留一个 responder 槽位（`mcp.rs:667`），每 6h TTL × 块数累积；同时插件单线程队列每次超时多一个未处理请求。改为"同一时刻未决 ≤1，累积由传输关闭清"。
- m3. D2 步骤 2 用 `format!("plugin.{id}_{tool}")` 再交 `plugin_tool_route` 反解：`source` 段字符集含 `_`（`kinds.rs:168`），`neige://plugin/a_b/c` 会被路由到插件 `a` 工具 `b_c`，`reason` 文本归错插件；且路由不返回 annotations。建议 S2 新增 `pub(crate) fn plugin_tool_entry(registry, plugin_id, tool) -> Option<(&ExposedTool, running)>` 精确查找，并断言 `source.plugin_id == 路由返回 id`。
- m4. `unavailable.reason` 回显插件 `isError` 文本（§5 示例），无长度上界（`MAX_SERIES_ROW_BYTES` 只盖 `data`）；Assistant 由此能读任意只读工具的错误文本。cap 256 字符并登记为 G（D4 对 Assistant 的驳回理由可站住，但这条间接通道应显式列在 KNOWN GAPS）。
- m5. 模板（F2.5）、fork（F2.6）、recipe（F2.7）写入不发 `TrackReportEdited`（`rg TrackReportEdited crates/calm-server/src` 只有 `write.rs:1098` 发）：这些块只靠读者兜底。D2 "触发三处"应注明，seq 12 的 live 块"按 pending 重新解析"依赖首次 read。
- m6. 按插件 id 建 mpsc 队列 + spawn，而 id 来自文档 `source`：一篇报告写 1000 个不存在的 plugin_id → 1000 条队列与任务。只为 `plugin_tool_route` 命中的插件建队列，未命中直接写 `unavailable`。
- m7. `ok` 序列 `points=[]` 未定义：`n=0` 时 `first/last` 为 null，且能被钉住。校验清单加"`ok` 项至少 1 点"，否则该条 `unavailable`。
- m8. seq 3b：source 插件在跑但 track owner 不可用（`TrackPluginScope::None`）时 reason 写"plugin X is not running"是假话；改为"track owner unavailable"。
- m9. 3d 标 S3，但"块级 ok、`pinned=false`"是内核行为，由 S2 的 A7b 测；行应拆为 3d(S3 回复形状)/3d'(S2 处理)。
- m10. seq 10 "字节相等"只在两次读之间无 TTL 刷新时成立；断言改为"同 `resolved_at` ⇒ 同字节"。
- m11. `ts_ms` → `YYYY-MM-DD` 的 UTC 折算约定未写（港/A 股若用本地零点会退一天）；规定 `ts_ms` = 交易日 UTC 零点。

### 切片与纪律

S1→(S2∥S3)→S4 各自可合入判断成立（fe 只有 `document/public.tsx:332` 与 `report.ts:250` 两个 kind switch，均在 S1；web/ 落 opaque；两份 OpenAPI 在 S4）。无已发布迁移改动；迁移号推迟；不 bump 三个版本常量的判断成立；`Resolved` 用枚举 ✓。

REVISE