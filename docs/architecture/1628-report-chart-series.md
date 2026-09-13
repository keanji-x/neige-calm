# 声明式图表 `chart.series`（#1628）— 设计 v6

基线：`origin/main` = `c534bf6b`（工作树 `feat/report-chart-series`）。所有 file:line 都在该基线上实测（[实测]），未核实的写 "未核实"。v1 → v2 → v3 → v4 → v5 → v6 的每条改动登记在 §11。

**v2 的模型变化（用户 2026-09-12 拍板）**：解析是**后台任务**，`calm.report.read` 与浏览器只读**已存储的行**，读路径上永不调用插件。需要即时数据时 Planner 自己调 `market.series`。

**v3 的模型变化（编排者 2026-09-13 裁决）**：**解析的唯一触发是读**。v2 的 bus 订阅者被第 2 轮证明拿不到块 id（`TrackReportEdited` 只带平铺 `body_after`，`BlockSlice` 只有 `raw`，F3.6/F1.11），且 bus 有损、无重放、无启动扫描；与其让订阅者再去加载权威块，不如删掉它。写路径什么都不做；`calm.report.read` 与 HTTP GET 读到无行 / 过期行时 `enqueue`。理由：读者兜底本来就是保证，订阅者只是优化；Planner 每轮都 read（CAS 必经，F3.3），人打开报告时浏览器 `pending` 轮询 3s，两个读者都会在几秒内触发。同时 v3 修正了第 1 轮两条被证伪的处置（超时泄漏"上界为 1"、2 MiB "内存上界"），把 `as_of` 改为**截止日**语义并用 `complete_through` 做钉住判据，删掉写端的时钟检查。

**v4 的修正（编排者 2026-09-13 第 3 轮裁决）**：两通道各自独立发现 v3 的 `complete_through` 模型有两处缺陷：(1) 「插件总是拉最新 N 根再按 `as_of` 过滤」本身就是缺陷——旧 `as_of` 的 frozen 块被永久钉在截断的窗口上或永远 `unavailable`；v4 把 `range` 定义为**相对截止日的窗口** `[as_of − RANGE_DAYS, as_of]`，插件按窗口取数（spike 证实源支持 start/end），`complete_through` 来自单独一次探测。(2) `complete_through >= as_of` 在相等时钉住的是一根内核无法证明已收盘的 bar（Binance 必然返回当前未收盘日 K [实测]）；v4 改**严格 `complete_through > as_of`**，且 `complete_through` 定义为源未过滤的最新**日线** bar 日期、与 `period` 无关。另：周/月线只输出周期结束日 ≤ 截止日的完整周期；路由预检未命中的一次性任务携带否定结果、不再二次查找；lane 通道改 unbounded 且检查-重建-投递在锁内；§2.8 保证语句改写为带例外清单。

**v5 的修正（编排者 2026-09-13 第 4 轮裁决）**：(1) **探测先于取数**（codex R4-1）：v4 的「窗口取数 + 单独探测」没有规定顺序，探测晚于取数会把取数时刻尚未收盘的 bar 用之后的探测认证并钉住；v5 规定插件先探测 `complete_through` 再取窗口（含全部分页），`complete_through` 记录的是探测时刻的观察，缓存页各自携带其取数前的探测值、回复取最小值。(2) **纳入规则统一为「结束日 ≤ `as_of` 且 < `complete_through`」**（codex R4-2）：v4 只按历法判「完整」，未来截止日下当前半周 / 半月照样输出；v5 要求存在更晚的日线 bar 才纳入，日线同样适用，插件聚合时与内核校验清单两处执行，§2.8「未完成周期永不出现」由此可验证；代价登记 G21。(3) **in-flight 键改 `(track, block)`、不含 hash**（codex R4-3）：v4 的 `(track, block, hash)` 让同一块反复改写并读取时排队 job 无界；v5 出队时才由当前 payload 派生请求与 hash，排队 job ≤ 被读过且未出队的块数。(4) **否定结果不落行**（A M2）：路由预检未命中与作用域 `None` 时读端直接返回 `pending, reason`，不排队、不 spawn、不写行（v4 的一次性任务删除）；真正调用失败写下的 `unavailable` 行用独立短 TTL `SERIES_UNAVAILABLE_TTL = 2 min`。(5) `as_of` 在 S1 查历法（A M1）。另：A9f 改用 `try_lock` 断言与确定性同步；`{range:"1M", period:"month"}` 结构性拒绝；§5.1 登记每片的实现约束。

**v6 的修正（编排者 2026-09-13 第 5 轮裁决；计划中的最后一轮修订）**：(1) **行 TTL 按 series 结果选**（codex R5-1）：v5 的 `ok` 行一律 6h，一条 series 的瞬时 `unavailable` 被块级 `ok, pinned=false` 的 6h 锁住；v6 规定行内**任一** series 非 `ok` → `SERIES_UNAVAILABLE_TTL`（2 min），全部 `ok` 才 6h，成功的 series 数据保留在行里（`row_ttl`，D3）。(2) **live 股票日线放宽**（codex R5-2 / R5-3 + 通道 A §1 逐市场判断，取两家更保守者的并集）：请求加 `mode: "live" | "frozen"`；**只有 `live ∧ period = day ∧ venue ∉ {CRYPTO}`** 放宽为「bar 日期 ≤ 截止日（昨天 UTC）」——美/港/A 股日期为 D 的常规时段都在 D+1 00:00 UTC 前收盘（§2.5 收盘时刻表）；CRYPTO 任何 mode、live 周/月线、frozen 全部保持「结束日 < `complete_through`」；插件缓存页键含取数 UTC 日期、永不跨午夜复用；内核校验按 `(mode, period)` 分支、venue 分支只在插件；美股盘后成交是否被源日线吸收登记为 U9（S3 spike，若吸收 US 回退严格）；G21 改写。(3) 三条 codex MINOR 与通道 A 六条 MINOR 全部落为 §5.1 的编号实现约束（准入与读端按全主键选行、锁内原子 `insert`、period_end 语义与 live 周/月负例、`sqlite_pool()` `None` 分支、同源探测/取窗、默认摘要读不取 `data` 列、series 查询不随 `track.report_edited` 前缀失效）。(4) 收尾：§5.1 整理为实现简报可直接引用的编号清单（S<n>.<k>）；§9 只留 S3 spike 才能定的项。

## 1. 目标与非目标

**目标。** 让投研报告里的图表只*命名*数据与视图：Planner / 人写一个 `chart.series` 块（`{series, field, range, view, as_of?}`），内核在块被**读到**且无结果时投一个后台任务向 market 插件解析"资产 × 字段 × 区间 → 序列"并把结果存成一行；内核对每个这样的块提供两态（`as_of` 缺席 = live，截止日 = 解析时的昨天 UTC，按 TTL 随源流动；`as_of` 存在 = frozen，截止日 = `as_of`，源发布出**晚于**截止日的日线 bar 后钉住）；`calm.report.read` 对数据块返回存储行的 `resolved` 摘要（默认）或原始序列（按块显式要），read 是纯 DB 读、永不因插件失败而失败；前端 fe/ 用现有 SVG 路线画 line / normalized / bar / candles；钉住的行对人与 AI 字节相等。

**非目标。** 不在正文里发明宏语法；不给内核加行情源（数据仍由插件解析）；不把 `chart.candles` 的已存文档做数据迁移；不给 legacy `web/` 加新渲染器（它按现有规则显示 `unsupported block kind chart.series`，见 §4 D5）；不做缩放/联动/图表库懒加载；不做盘中序列（live 截止日 = 昨天 UTC，D2）；不做跨币种换算（每条序列自带 `currency`，normalized 视图天然可比，line 视图按原值画并标币种）；**不承诺 read 即时、不承诺有界新鲜度**（§2.8）；不在写时触发解析；不加固 MCP 传输层字节上界（#1634）；不输出源尚未证明已收盘的 bar / 周期（结束日 < `complete_through`，D2 步骤 6、§2.5 S3 约束 3；**唯一例外是 live 股票日线**：按 §2.5 收盘时刻表，日期 ≤ 昨天 UTC 的美/港/A 股日线 bar 在解析时刻必已收盘，v6 对它只查 `≤ as_of`）；不在截止日当天钉住（严格 `>`，D3）。

## 2. 事实表

### 2.1 block kind 词汇与校验（`crates/calm-types/src/report_blocks/`）

| # | 事实 | 位置 |
|---|---|---|
| F1.1 | 数据 kind 闭集 `DATA_KINDS = [chart.candles, table, app, task]`；`is_data_kind` 只认这四个 | [实测] `kinds.rs:58-62` |
| F1.2 | `validate_payload(kind, payload)` 按 kind 分派；未知 kind 本身是错误（列出 `DATA_KINDS`）；形状合法后再量一次 `canonical_json` 的字节数，上限 `MAX_CANONICAL_BYTES = 256KB` | [实测] `kinds.rs:89-134`，常量 `:46-55` |
| F1.3 | caps：`MAX_CHART_CANDLES=5000`、`MAX_TABLE_COLUMNS=32`、`MAX_TABLE_ROWS=500`、`MAX_STRING_CHARS=2048` | [实测] `kinds.rs:46-55` |
| F1.4 | live `source` 两段语法：`neige://plugin/<plugin_id>/<overlay_kind>`，前缀常量 `LIVE_SOURCE_PREFIX`，每段字符集 `[A-Za-z0-9._-]`（**含 `_` 与大写**，比 manifest 的 plugin id 规则宽，见 F4.16），**只查形状不查存在**（"插件尚未安装是正常状态"） | [实测] `kinds.rs:137-177`，字符集 `:162-175` |
| F1.5 | `validate_chart`：allow-list `symbol, period, candles, overlays, caption`；`period ∈ day/week/month`；candles 行 5 或 6 个数；`overlays ∈ ma20/ma60` | [实测] `kinds.rs:483-539` |
| F1.6 | `validate_table` 用 `source` 的*存在*选 live 形态，live 形态只允许 `source, caption`（两种形态互斥） | [实测] `kinds.rs:547-570` |
| F1.7 | fence 形状：```` ```neige-block <kind> ```` 开头、JSON **object** 内容、裸 ```` ``` ```` 收尾；`render_fence ∘ parse_fence` 幂等；`canonical_json` 键排序、2 空格、纯标量数组单行 | [实测] `fence.rs:33-77`、`:84-136` |
| F1.8 | `flatten(split_body(body)) == body` 字节级不变式；格式错误的 neige fence 读成 prose，写端必须用 `invalid_neige_fences` 拒绝 | [实测] `mod.rs:3-7`、`:43-63`、proptest `:387-388` |
| F1.9 | "一个块能装什么"的唯一定义：`check_prose_markdown` / `render_data_block`（先 `validate_payload` 再 `render_fence`）/ `unknown_kind_message` | [实测] `mod.rs:196-233` |
| F1.10 | 现有 chart 校验测试 `chart_payload_valid_and_invalid`；live table 三条测试 | [实测] `kinds_tests.rs:5`、`:385/:406/:425` |
| F1.11 | 解析 fence 的公共函数 `split_body(body) -> Vec<BlockSlice>`、`parse_fence(raw) -> Option<NonProseFence>`；**`BlockSlice { raw }` 只有原文、`NonProseFence { kind, payload }` 只有 kind 与 payload，都不带块 id**——从 body 文本无法得到持久块 id（v2 订阅者因此被删，§11 R2-4） | [实测] `mod.rs:36-38`、`:48`；`fence.rs:28-31`、`:52` |
| F1.12 | `calm-types` **没有时钟**：`Cargo.toml` 无 `chrono`/`time`/`jiff`（`grep -n "chrono\|^time\|jiff" crates/calm-types/Cargo.toml` 零命中）；`grep -rn "Utc::now\|SystemTime::now" crates/calm-types/src` 零命中；crate 头注 "NO sqlx, NO axum, NO tokio"。`validate_payload` 是纯函数 | [实测] `calm-types/Cargo.toml:8-11`；`kinds.rs:89` |

### 2.2 写端入口（`crates/calm-server/src/`）

穷举命令（在工作树跑）：`grep -rn "validate_payload\|render_data_block\|check_prose_markdown\|validate_body_fences" crates --include='*.rs' | grep -v "report_blocks/kinds.rs\|report_blocks/kinds_tests.rs\|report_blocks/mod.rs\|/tests/"`，再去掉注释行、测试模块、`card_kind`/`forge_action_adapter` 的同名无关函数。**生产**写入口共 **9 处**（v1 写 7 处，通道 A 指出漏项，§11 M6）：

| # | 入口 | payload 校验 | 位置 |
|---|---|---|---|
| F2.1 | `calm.report.blocks.upsert` 与 `calm.report.commit` 的每个 `upsert` op 共用 `resolve_upsert_content`：prose 走 `check_prose_markdown`，数据 kind 走 `render_data_block` | 是 | [实测] `mcp_server/tools/track_report_blocks.rs:581`、`:603` |
| F2.2 | op 层复核：`apply_upsert_existing` / `apply_upsert_new` 在 `validate_caller_content` 时再跑 `track_report_guard::validate_block_content`（prose → `check_prose_markdown`；fence → `check_fence_payload` → `validate_payload`）。**所以 F2.1 被绕过时这里仍拒绝**（§11 C12） | 是 | [实测] `track_report.rs:520`、`:539`；`track_report_guard.rs:215-222`、`:128` |
| F2.3 | `ReportDocOp::Replace`（`calm.report.write` / `.edit` shim）与 `ReportDocOp::WriteMarkdown`（`calm.report.write_markdown`）两臂各调一次 `validate_body_fences`（`invalid_neige_fences` + 每个 fence `validate_payload`） | 是 | [实测] `track_report.rs:643`、`:660`；`track_report_guard.rs:138` |
| F2.4 | 人用 HTTP：`POST /api/tracks/{id}/report/blocks` 等，`block_content` 同样 `check_prose_markdown` / `render_data_block` | 是 | [实测] `routes/track_report_blocks.rs:132`、`:142` |
| F2.5 | 建 track 时的模板 body：`validate_body_fences` | 是 | [实测] `routes/tracks.rs:1006`（`:940-941` 是注释） |
| F2.6 | fork：prose 块内 fence `validate_body_fences`（`:2795`）+ 每个非 prose 块 `validate_payload`（`:2848`），`prepare_fork_report` 保留 block id、换 track_id | 是 | [实测] `routes/tracks.rs:2717`、`:2795`、`:2848` |
| F2.7 | recipe HTTP 写口 `validate_recipe_body` → `validate_body_fences` | 是 | [实测] `routes/track_recipes.rs:274-276` |
| F2.8 | 内核自写的 task 块：`track_report/user_start.rs:56`、`track_report/dispatch.rs:212`、`track_report/repair.rs:13` 都过 `render_data_block` | 是 | [实测] 同列 |
| F2.9 | `report_backlinks.rs:559` 测试内 `render_data_block`（非生产，列出以示穷举） | — | [实测] |
| F2.10 | **schema 自描述**只有一处：`contracts.rs::kinds_table()`，`blocks.kinds` 直接返回它；`upsert` 与 `commit` 的 `kind` enum 由 `block_kind_enum()` 从同一张表读出，测试锁死三者相等 | — | [实测] `contracts.rs:41`、`:444-455`、测试 `:695-711` |
| F2.11 | `kinds_descriptor` 的 description 文字**手写**列了四个 kind 名，`upsert_descriptor` 同样手写 "`chart.candles` / `table` / `app` / `task`" | — | [实测] `contracts.rs:20-24`、`:311-313` — 新 kind 要改这两段文字，无测试覆盖 |
| F2.12 | `chart.candles` 的 usage 文字宣称 "The kernel has no market-data source: include every candle" | — | [实测] `contracts.rs:92-100` — 本设计落地后这句要改 |

结论：新 kind 只需进 `DATA_KINDS` + 一个 `validate_chart_series` + `kinds_table` 一项，上述 9 处入口全部经 `validate_payload` 自动覆盖。**`validate_chart_series` 只做形状与历法校验**（历法 = 闰年 + 每月天数，纯函数，§11 第 4 轮 A M1；F1.12：该 crate 无时钟，任何"与今天比"的检查都放不进这里）。

### 2.3 读端与写后钩子

| # | 事实 | 位置 |
|---|---|---|
| F3.1 | `ReportReadSnapshot { updated_at, schema_version, doc_rev, summary, body, blocks: Vec<ReportBlock>, task_diagnostics }`，一次 `card_get_with_body_crdt` 单行读 | [实测] `track_report_read.rs:12-20`、`:43-60` |
| F3.2 | `ReportBlock { id, kind, rev, payload }` — 快照里**有** payload 与持久块 id | [实测] `calm-types/src/track_report.rs:16-22` |
| F3.3 | `calm.report.read` 响应 `{ text, body(alias), summary, schemaVersion, docRev, updated_at, blocks: [{id, kind, rev}], taskDiagnostics?(Planner only) }`；入参只有 `with_markers`；Planner 与 Assistant 都能 read；注释明说 read 是 `docRev`/`rev` 的**唯一来源**、每次写前必 read（所以 read 路径不能变慢；也所以 Planner 每轮至少 read 一次） | [实测] `mcp_server/tools/track_report.rs:142-222`，角色 `:148-153`，index `:190-194` |
| F3.4 | **live `source` 在读端不解析**：handler 只把 `flat_text` 拼进 `text`，块索引不带 payload | [实测] `track_report.rs:172-194` |
| F3.5 | **没有面向 agent 的 overlay 读取工具**：`grep -rln overlay crates/calm-server/src/mcp_server/` 只命中 `contracts.rs`（描述文字） | [实测] |
| F3.6 | 报告写入走 `write_with_actor_events_typed`，calm-truth 在 `tx.commit().await?` **之后**逐条 `bus.emit_envelope`；`Event::TrackReportEdited` 带 `track_id, card_id, author, edit_id, summary_before/after, body_before, body_after: String`——**平铺文本，无块 id 列表** | [实测] `track_report/write.rs:832-880`；`calm-truth/src/db/sqlite/events.rs:787-789`；`calm-types/src/event.rs:579-597` |
| F3.7 | 事务外后台反应的既有形状：`bus.subscribe()` + 自己的 `tokio::spawn` 循环，`RecvError::Lagged` 只 warn 继续（bus 是 lossy 的 `broadcast`；`subscribe()` 只收订阅之后的信封，无重放） | [实测] `card_fsm.rs:350-364`；`dispatcher/mod.rs:1118-1127`（poke scheduler）；`calm-truth/src/event_bus.rs:109-131`、`:185-189` |
| F3.8 | 报告块快照在事务内可读：`report_blocks_snapshot_tx(tx, track_id)` | [实测] `track_report.rs:73` |
| F3.9 | `Event::TrackReportEdited { .. }` 的**生产构造点只有一处** `track_report/write.rs:1098`（`grep -rn "TrackReportEdited" crates/calm-server/src --include='*.rs' | wc -l` = **26** [实测 2026-09-13]：`grep -rn "Event::TrackReportEdited {" crates/calm-server/src --include='*.rs' | grep -v tests.rs` = 5 处 = `write.rs:1098` 构造 + 4 处 match 消费（`dispatcher/mod.rs:117/1142/1646`、`decision_sink.rs:1220`）；`dispatcher/tests.rs:162/682/1053/1596` 是测试构造；其余 17 处是 doc 注释与描述文字。v3 写的「共 7 处」是错的，§11 A m6）。模板（F2.5）、fork（F2.6）、recipe（F2.7）写入不经 `write.rs` 的这条路径——v2 订阅者对它们本就只能靠读者兜底 | [实测] |

### 2.4 overlay 机制与内核↔插件调用面

| # | 事实 | 位置 |
|---|---|---|
| F4.1 | `neige.overlay.set` 回调：`entity_kind` 必须 externally_writable（`card`/`track`），manifest `overlays_write` 授权，`validate_overlay_payload` 只校验内核自有 kind（插件 kind 不透明、**无字节上限**），`plugin_id` 由内核注入 | [实测] `plugin_host/callbacks.rs:268-320`；`calm-truth/src/validation.rs:417-445`、`:575-577` |
| F4.2 | 存储：`overlays` 表 `UNIQUE(plugin_id, entity_kind, entity_id, kind)`，**无外键**；upsert 为 `ON CONFLICT ... DO UPDATE` | [实测] `calm-truth/migrations/0001_init.sql:42-52`；`db/sqlite/overlay.rs:7-16` |
| F4.3 | 读：`overlays_for(entity_kind, entity_id)` / `overlays_by_kind(entity_kind)`；`GET /api/overlays?entity_kind=track`（不带 id）**返回全工作区该 kind 的所有 overlay，侧栏用这个形态**；`GET /api/tracks/{id}` 详情也带该 track 全部 overlays | [实测] `db/mod.rs:83-84`；`routes/overlays.rs:106-112`、`:125-134`；`routes/tracks.rs:1102-1121` |
| F4.4 | 事件 `Event::OverlaySet(Overlay)`（带整个 payload）/ `OverlayDeleted`，wire 名 `overlay.set` / `overlay.deleted` | [实测] `calm-types/src/event.rs:600-608` |
| F4.5 | overlay 写方：外部只有插件回调（`callbacks.rs:310`）与手测路由（`routes/overlays.rs:202`）；**内核内部写者**另有 `card_fsm.rs:558/:683`、track structure creation、`child_track_adapter`（路由头注自列） | [实测] `grep -rn overlay_upsert_tx crates/calm-server/src`；`routes/overlays.rs:80-82` |
| F4.6 | 内核→插件请求-响应面：`McpClient::tools_call(name, arguments, track_id)` 把 track 放在 `params._meta`；`McpClient::call` **无超时**（只有 `initialize` 包了 10s，`:477`）；`call` 先把 responder 插进 `responders: Arc<Mutex<HashMap<RequestId, oneshot::Sender<…>>>>`（`:233` 类型，`:667` 插入）再 `rx.await`（`:676`）；**调用方超时/取消（future 被 drop）不会移除 responder**——只有 writer 已死（`:672`）、对端回复（`:804` `remove`）或传输关闭（`flush_responders_with_error :867`，读失败/EOF 时 `drain`）才清；读循环 `read_line` 先把整行读进内存再 `parse_frame`，**无字节上限** | [实测] `plugin_host/mcp.rs:233`、`:655-679`、`:783-810`、`:867-874`、`:900` |
| F4.7 | 按插件 id 取客户端：`PluginHost::connector_client(id) -> Option<ConnectorClient>`（Running 才有）、`mcp_client(id)`（只给 stdio）；`running_plugin_ids() -> BTreeSet<String>` | [实测] `plugin_host/mod.rs:3260`、`:3296-3320` |
| F4.8 | `ConnectorClient` 三变体：`Stdio(Arc<McpClient>)`（本地子进程，`tools_call` 三参带 track）、`Http(Arc<HttpMcpClient>)`（远端 mcp-http，`tools_call` 双参、注释明说 "somebody else's service"）、`Cli(Arc<CliQueryRuntime>)`（本地 exec，双参） | [实测] `plugin_host/connector.rs:44-55`；`mcp_server/transport.rs:722-738` |
| F4.9 | agent 路由 `dispatch_plugin_tools_call`：身份先于路由 → `plugin_tool_route(registry, name, running_ids) -> Result<Option<(plugin_id, tool, Option<ToolKind>)>>`（**不返回 `ExposedTool`，拿不到 `annotations`**）只认 manifest `exposes_tools` 里的工具 → `plugin_scope_for_track(...).allows(plugin_id)` → `require_role_any(PLUGIN_TOOL_ROLES = [Planner, Worker])` → `kind == None` 走 `connector_client` 三变体分派；`Some(ForgeAction)` 走 `trusted_forge_plugin` + `mcp_client` 专用臂；`plugin_tool_route` 是私有 `fn`，靠 `plugin.{id}_{tool}` 字符串前缀反解，注释自述"plugin ids cannot contain `_`" | [实测] `transport.rs:88`、`:569`、`:667-762`、`:771-820`（注释 `:805`）；`forge_trust.rs:20` |
| F4.10 | **内核作为调用者已有先例**：HTTP 建卡路径 `routes/cards.rs:588` 直接 `mcp.tools_call(&via.tool_name, via.arguments, None)`，前置门是插件权限 `cards_create`（`:573`），不经 `PLUGIN_TOOL_ROLES` | [实测] |
| F4.11 | `TrackPluginScope`：无 track / 未绑定 → `All`；绑定且 owner 运行∧可信 → `Only(id)`；绑定但 owner 不可用 → `None`（`allows` 恒 false，fail-closed） | [实测] `tool_visibility.rs:50-70`、`:136-149` |
| F4.12 | `ExposedTool { name, description?, kind?: Option<ToolKind>(只有 ForgeAction), input_schema?, annotations?: Option<Value> }`；`readOnlyHint` 是 `annotations` JSON 里的键；没有"仅内核可调"的标记 | [实测] `plugin_host/manifest.rs:766-780`；`grep -n "hidden\|internal" tool_visibility.rs` 零命中 |
| F4.13 | 插件侧回调 `Rpc::call` 有 15s 超时；插件把 tools/call 送到**单**工作线程串行处理（`mpsc::channel::<Value>()` :2602，worker `for frame in tool_queue` :2616-2626）；**内核超时后请求仍留在该队列里、按序被处理**（插件收不到取消）；未知工具回 `tool_error("unknown tool …")`（`isError`） | [实测] `plugins/market/main.rs:52`、`:2572`、`:2590-2626`、`tool_error :2372` |
| F4.14 | `neige.kv.set` 配额按整个 keyset 的 JSON 文本长度计；market manifest 设 `kv_quota_bytes: 262144` | [实测] `callbacks.rs:792-835`；`plugins/market/manifest.json` permissions |
| F4.15 | SQLite 连接池 `PRAGMA foreign_keys = ON` 每连接；`REFERENCES tracks(id) ON DELETE CASCADE` 有先例（0104、0106）；track 删除在一个事务里先做 overlay 等显式清理再 `track_delete_tx` | [实测] `calm-truth/src/db/sqlite/mod.rs:4`、`:264`；`migrations/0104_candidate_review.sql:5`、`0106_candidate_repair.sql:4`；`routes/tracks.rs:4032-4037` |
| F4.16 | manifest plugin id 规则 `^[a-z0-9][a-z0-9.-]{1,63}$`（小写、**不含 `_`**，测试 `bad_id_rejected_illegal_char` 锁死）；`PluginRegistry::get(id) -> Option<Manifest>` 精确查找、`list() -> Vec<Manifest>` 快照。所以 `source` 段（F1.4 允许 `_`/大写）里任何含 `_` 或大写的 plugin_id **不可能**匹配任何 manifest——用 `get` 精确查找它就是干净的 `None`；用 `plugin.{id}_{tool}` 反解则 `neige://plugin/a_b/c` 会命中插件 `a` 的工具 `b_c`（§11 A m3） | [实测] `manifest.rs:2303-2314`、`:3172-3177`；`registry.rs:292`、`:299` |
| F4.17 | `#1634`（OPEN）："hardening(plugin_host): MCP 传输读端无字节上限、超时取消不清 responder"——传输层字节上界归它；responder 清理由本设计 S2 落地（D2 步骤 5），S2 合入后 #1634 只剩字节上界一项 | [实测] `gh issue view 1634` |
| F4.18 | `plugin_scope_for_track(ctx, Some(track_id))` 的成本：一次 `repo.track_get` + 一次 plugin host 表读（`resolve_track_owner_binding`），头注自述它已在 tools/list 与 tools/call 热路径上按调用执行；track 不存在 / 读失败 → `None`（fail-closed）。读端 `enqueue` 的预检可以承担它（D2 enqueue 步骤 2，一次 read 只算一次） | [实测] `tool_visibility.rs:87-152` |

回答：overlay 是推送模型；请求-响应通道已存在且内核作为调用者已有先例（F4.6/F4.10）。新的是"内核在后台任务里作为调用者，经 manifest 精确查找与只读约束"（§4 D2）。

### 2.5 market 插件

| # | 事实 | 位置 |
|---|---|---|
| F5.1 | 规范身份 `AssetId { venue, symbol }`，`canonical()` = `<VENUE>:<SYMBOL>`；venue 闭集 `CRYPTO/US/HK/SH/SZ`，`CN:` 只为读回旧数据、永不定价；`canonical_symbol` 把 HK 数字代码折成一种写法（`9988` 与 `09988` 同身份） | [实测] `main.rs:175-250`、`parse_asset :295`、`canonical_symbol :349-360` |
| F5.2 | `quote_asset`：CRYPTO → Binance `/api/v3`，其余 → Sina `hq.sinajs.cn/list=<symbol>`（需 `Referer`，GBK）；只取**现价**字段 | [实测] `main.rs:699-702`、`:834-866`、`:1125-1160` |
| F5.3 | 结算：`Currency` 与 `FxLeg`（Sina `fx_s*` 行）、USDT=1 USD 是假设 | [实测] `main.rs:622-668`、`:1244`、`:1281-1312` |
| F5.4 | **历史序列今天没有源**：模块头 "History is forward-only ... never back-fills from klines"；`history/<track_id>` KV 存的是组合总值逐 tick 点（≤500） | [实测] `main.rs:33-36`、`:55`、`:64-65`、`:1974-2033` |
| F5.5 | 推送：`push_overlay` → `neige.overlay.set{entity_kind:"track", entity_id, kind, payload}` | [实测] `main.rs:2045-2061` |
| F5.6 | 工具：`market.quote`（`readOnlyHint: true`）、`market.holdings.set`（`false`）、`market.holdings.list`（`true`）；track 由内核 `_meta` 注入 | [实测] `manifest.json:29/:56/:71`；`main.rs:494-500` |
| F5.7 | 已有测试基建：`sina_fixture.rs`、进程级 `tests/cases/market_plugin_process.rs`（`boot_sources`、`call_tool(id, name, args, track)`）；假插件基建 `boot_plugin_host`（`tests/cases/mcp_plugin_tools.rs:927`） | [实测] `market_plugin_process.rs:124`、`:294` |

**历史日线源 [实测 2026-09-12 编排者]**（本机直连、无代理、UA=Mozilla/5.0；v1 的 U1 由此关闭）：

| 源 | 端点 | 覆盖 | 结果 |
|---|---|---|---|
| 腾讯 ifzq | `https://web.ifzq.gtimg.cn/appstock/app/fqkline/get?param=<sym>,day,,,<n>,qfq` | us/hk/sh/sz 一个端点 | 全部 200 JSON。sh/sz 数组键为 `qfqday`，us/hk 为 `day`；行 `[date, open, close, high, low, volume]`（**列序 o,c,h,l,v**）。usNVDA 请求 3 条只回 2 条且首行是 2011-06-02（疑似复权基准行），实现时按日期过滤不按条数信任 |
| 新浪 A 股 | `https://quotes.sina.cn/cn/api/json_v2.php/CN_MarketDataService.getKLineData?symbol=sh600519&scale=240&ma=no&datalen=3` | sh/sz | 200，`[{day,open,high,low,close,volume}]`（需 Referer） |
| 新浪美股 | `https://stock.finance.sina.com.cn/usstock/api/json_v2.php/US_MinKService.getDailyK?symbol=nvda` | us | 200，从 1999 起全量，`{d,o,h,l,c,v,a}`，`___qn` 不限条数 |
| 新浪港股 | `.../hkstock/api/json_v2.php/HK_MinKService.getDailyK?symbol=09988` | hk | `{"__ERROR":3,"__ERRORMSG":"Service not valid"}` — 不可用 |
| Binance | `https://data-api.binance.vision/api/v3/klines?symbol=BTCUSDT&interval=1d&limit=2` | crypto | 200，标准 klines 数组 |


**按截止日取窗口 [实测 2026-09-13 编排者]**（本机直连、无代理、UA=Mozilla/5.0；第 3 轮 spike，回答 A M1 / codex R3-2 的「源能否按窗口取」）：

| 调用 | 结果 | 含义 |
|---|---|---|
| ifzq `usNVDA,day,,2026-03-10,3,qfq`（只给 end） | 只回 2011-06-02 那一行基准 | **只给 end 不可用**（美股），必须 start+end 都给 |
| ifzq `hk09988,day,2026-03-01,2026-03-10,50,qfq`（start+end） | 7 根，2026-03-02 … 2026-03-10 | **按窗口取可用**，行数以窗口内交易日为准 |
| ifzq `sh600519,day,,,1300,qfq`（要 1300 根） | 只回 640 根（2024-01-22 … 2026-09-11） | **单次上限约 640 根**；5Y 日线（≈1260）必须按 start/end 分页 |
| ifzq `usNVDA,week,,,3,qfq` / `month` | 各只回 1 行（2026-09-11） | **周/月端点不可用**（只给当前一根，且日期是最新日线日期）；周/月线由插件从日线自聚合 |
| Binance `klines?...&endTime=1772000000000&limit=2` | 2 根，都 ≤ endTime | `endTime` 可用；`startTime` 同理（Binance 文档，本次未测） |
| Binance `klines?symbol=BTCUSDT&interval=1d&limit=2`（不带 end）[实测 2026-09-13 02:39 UTC 修订者] | 末根 `closeTime` 在未来 | **不带 end 的探测必然含当前未收盘日 K**（A M2 的断言成立）；`complete_through` = 今天，严格 `>` 判据下无害 |

**常规时段收盘时刻（UTC）**[第 5 轮两通道引用交易所官方时刻表（NYSE / HKEX / SSE 交易时间页、Binance klines 契约）；本仓未实测；「交易所收盘」与「源发布 / 定稿」不是同一件事（codex R5 venue 表的限定）——US 由 U9 spike 补，HK / CN 的余量 ≥ 15h 足以覆盖批量更新滞后]（v6 live 日线放宽的依据，§2.5 S3 约束 3 与 D2 步骤 6）：

| venue | bar 日期 D 的标注 | 常规时段收盘（UTC） | 距 D+1 00:00 UTC 的余量 | live 日线纳入规则 |
|---|---|---|---|---|
| US（ifzq 主源，Sina 兜底） | ET 交易日 = 同一 UTC 日 | EDT 20:00 / EST 21:00；半日 17:00 / 18:00 | 常规 ≥ 3h；**盘后交易 16:00–20:00 ET = 冬令时 21:00–01:00 UTC 跨过零点** | 放宽（`≤ as_of`）；**若 U9 spike 证明源日线吸收盘后成交 → 该 venue 回退严格**（插件侧一行，与 CRYPTO 同臂） |
| HK（ifzq，无兜底 G9） | 本地日 = UTC 日 | 08:00 收市，收市竞价至 08:10；半日市 04:00 | ≥ 15h | 放宽（`≤ as_of`） |
| SH / SZ（ifzq，Sina 兜底） | 本地日 = UTC 日 | 07:00；盘后固定价格交易至 07:30 | ≥ 16h | 放宽（`≤ as_of`） |
| CRYPTO（Binance） | UTC | **恰在 D+1 00:00:00**（`closeTime` = 23:59:59.999） | **0** | **严格（任何 mode）**：放宽零收益（不带 end 的探测必含今天的未收盘 K [实测 §2.5]，`complete_through` = 今天 > 昨天恒成立，昨天的 bar 在严格规则下任何时刻都纳入）、只添时钟偏斜窗口（内核比 Binance 快 s 秒 ⇒ 00:00:00–00:00:0s 内的 live 解析把未收盘的 D 根写进行）；严格规则在偏斜下反而安全（内核以为 D+1、Binance 仍在 D ⇒ 探测最新 = D ⇒ `as_of` = D 被排除） |

S3 约束（v4 按第 3 轮 spike 重写；v5 加执行顺序、纳入规则、探测样本、缓存与近端检查；v6 加 `mode` 分支、缓存日期键、同源探测）：腾讯 ifzq 为股票三市场主源（单端点、复权；解析时 `qfqday`/`day` 两个键都要认、列序按 o,c,h,l,v 重排、按日期过滤）；Binance klines 为 crypto；新浪只作 A 股/美股兜底，**港股无兜底**（登记 G9，与 #1556 D3″ 同形）。取数规则（**每条 series 的执行顺序：1 探测 → 2 取窗口 → 4 深度 / 近端检查 → 3 聚合与纳入；探测必须最先**）：

1. **先探测 `complete_through`**（§11 第 4 轮 codex R4-1）：不带 start/end 的「最新 3 根」调用（n=3：us 代码首行可能是 2011 复权基准行 U6，spike 里 n=3 只回 2 行且含基准行，n=2 未测；要求至少一根非基准行，否则该条 `unavailable, reason:"probe returned no recent bar"`，§11 第 4 轮 A m5），取其中日期最大者为**日线** `complete_through`；Binance `limit=3` 末根是当前未收盘 K 线，日期即今天。与 `period` 无关：周/月请求的 `complete_through` 也是这个日线日期。**`complete_through` 记录的是探测时刻的观察，之后才取窗口。任何 mode 都探测并回传**（live 日线放宽分支也要它：CRYPTO 严格判据、UI 的 "source data through"、内核形状校验）。理由：若探测时 `complete_through > as_of`，则每个 ≤ `as_of` 的 bar 在探测时刻已收盘（D3 前提：bar 日期正确、按时间顺序发布），之后任何时刻取到的都是终态（源事后修正除外，G1）；反过来（先取数、后探测）会把取数时刻仍在变的 bar 用之后的探测认证——v4 没有规定顺序，codex R4-1 的构造（23:59:59 取到当日未收盘 bar，00:00:01 探测到次日 bar，严格 `>` 钉住盘中值）成立，分页只会拉长这个窗口。
2. **再取窗口**：请求带内核算好的 `start`（= `as_of − RANGE_DAYS[range]`，D1）与 `as_of`；插件向源要 `[start − SERIES_FETCH_MARGIN_DAYS(=14), as_of]`（ifzq 给 start+end 两个日期；Binance 给 `startTime`/`endTime`），再过滤到 `[start, as_of]`。单次超过约 600 根就按日期分页拼接（去重按日期），**全部分页都在探测之后**。**内存缓存**（§5 S3 的项）服从同一顺序：缓存页随页记录「取该页之前的探测值」`page.observed_complete_through`；一次回复用到了缓存页时，回复的 `complete_through` = min(本次探测值, 每个用到的缓存页的 `observed_complete_through`)——每个 bar 仍只被「取它那一页之前」的探测认证；规则 3 的纳入与 D3 的钉住判据用的都是这个取 min 之后的值，对缓存页照样成立。**缓存页键含取数时刻的 UTC 日期**（`fetched_on`），查找只命中 `fetched_on == 今天 UTC` 的页——**永不跨 UTC 午夜复用**（§11 第 5 轮 codex R5-2：23:59 取的页含当日半根 bar、`observed_complete_through` = 当日，次日 live 的截止日推进到该日，放宽分支会把缓存里的盘中值当已收盘值输出；「之后的时间」不能让「之前取到的字节」定稿。日期键对全部 venue 与 mode 执行，不只对被放宽的分支）。**同一条 series 的探测与取窗必须同源**：ifzq 探测后取窗失败切到 Sina 兜底 → 对 Sina **重新探测**再取窗；缓存页的 `observed_complete_through` 按源分别记（§11 第 5 轮 A m3）。**不再「拉最新 N 根再过滤」**——那句话是 v3 的缺陷本身（§11 第 3 轮交叉命中 1）。
3. **纳入规则（按 `(mode, period, venue)` 分支，v6）**：请求带 `mode`（D2 请求形状）。**默认（严格）分支**：一根 bar 或一个周期只在 **`period_start ≥ start` ∧ 结束日 ≤ `as_of` ∧ 结束日 < `complete_through`** 时输出（§11 第 4 轮 codex R4-2：存在更晚的日线 bar 才证明该 bar / 周期已闭合；v4 的「结束日 ≤ 截止日」只是历法完整——周三请求 `as_of` = 周日时，周一到周三聚成的半根周 K 结束日 ≤ 周日、`ts_ms` 是周一、`complete_through` = 周三 ≥ `ts_ms`，v4 的每条检查都过，图上却是半周；未来的月末截止日同样放进当前半月）。日线（严格分支）：bar 日期 < `complete_through`——`as_of` = 今天时今天的 bar 在明天的 bar 出现前不纳入。**唯一放宽分支——`mode = live ∧ period = day ∧ venue ∉ {CRYPTO}`**（venue 由插件 `parse_asset` 判，F5.1；内核无 venue 知识）：bar 只需 **`start ≤ 日期 ≤ as_of`**（= 昨天 UTC），不要求更晚的 bar 存在。依据是 §2.5 收盘时刻表：美/港/A 股日期为 D 的常规时段在 D+1 00:00 UTC 前收盘，所以「日期 ≤ 昨天 UTC」的 bar 在解析时刻已收盘；错误存活 ≤ 6h 且 live 行永不钉住（D3）。**CRYPTO 任何 mode 严格**（余量为 0，见表）。**live 周/月线严格**（§11 第 5 轮 A §1 构造：周一 00:30 UTC，live `as_of` = 周日，ISO 周结束日 = 周日 ≤ `as_of`，但 ifzq 批量更新滞后、周五的日线 bar 尚未发布 → 周 K 会由周一至周四聚成、close = 周四、标成完整周；「存在更晚日线 bar」正是「该周期所有成员 bar 已发布」的证明，放宽后周/月聚合失去这个证明；日线不受此影响——bar 要么缺、要么已收盘）。**frozen 严格**（探测先于取数 ∧ ≤ `as_of` ∧ < `complete_through`，钉住判据依赖它，D3）。**结束日语义**（§11 第 5 轮 codex MINOR）：截止比较的是**周期结束日**（day：bar 日期；week：ISO 周日；month：月末），不是存储的 `ts_ms`（周一 / 1 日）——否则周三的 live 周线请求会把周一至周二聚成的半根周 K 当作「起始日 ≤ 昨天」放进去；两层都查 `period_start ≥ start` 与周期结束日截止。周/月线由插件从日线聚合（源端周/月端点不可用）：ISO 周（周一至周日）/ 自然月，UTC 日期；输出 `period_start ≥ start ∧ period_end ≤ as_of ∧ period_end < complete_through` 的周期；`ts_ms` = 周期起始日（周一 / 1 日）UTC 零点；聚合值 open = 首日 open、high = max、low = min、close = 末日 close、volume = sum。本周在下周一（或更晚）的日线 bar 出现前不纳入；不要求源已发布到周期结束日当天（周日不是交易日）。**内核校验清单执行同一组规则（D2 步骤 6，按 `(mode, period)` 分支；venue 分支只在插件），插件与内核两处都执行**；§2.8「未完成周期永不出现」由此成为可验证的语句。代价：严格分支下最新一根已收盘的 bar 要等下一根出现才进图（G21，v6 收窄到 live 周/月与加密日线）。
4. **超出源深度与窗口近端**：分页向前直到覆盖 `start − SERIES_FETCH_MARGIN_DAYS` 或某页返回零根；若得到的最早 bar 日期 > `start + SERIES_FETCH_MARGIN_DAYS` → 该条 `unavailable, reason:"lookback exceeds source depth"`（插件分不清「源没那么深」与「标的上市/复牌晚于窗口起点」，两者同落此态，登记 G18）。**对称检查**（§11 第 4 轮 A m4）：过滤到 `[start, as_of]` 后、聚合与纳入规则之前，若最晚的日线 bar 日期 < `as_of − SERIES_FETCH_MARGIN_DAYS` → 该条 `unavailable, reason:"no data near cutoff"`（退市 / 长期停牌 / 源只保留最早 N 根时的近端截断都落此态，G3/G18）。**ifzq 单次超过约 640 根时截哪一端未实测**（spike 第 3 行没带 start/end，U8）：S3 先 spike、fixture 照实模拟；若源保留最早 N 根、静默截掉近端，「分页向前直到覆盖 `start − 14`」第一页就停，内核清单（日期 ∈ 窗口、≤ max_points、≥ 2 点）抓不到近端缺口，只有这条对称检查能抓到。

### 2.6 前端

| # | 事实 | 位置 |
|---|---|---|
| F6.1 | fe 蜡烛图走 **SVG**，头注三条理由：token 可用（`currentColor`/`var(--…)`）、零依赖零懒加载 chunk、是 markup；CN 极性红涨绿跌 | [实测] `fe/web/src/features/report/candles/public.tsx:1-22` |
| F6.2 | 蜡烛图区间客户端过滤 `1M/3M/6M/1Y/All`，`viewBox` 740×256，MA20/60 客户端算 | [实测] `candles/public.tsx:27-58`、`:89-125` |
| F6.3 | live table 三态：无 resolver / 未推送 / 推送了但不是 table，都渲染 caption + 一行说明 | [实测] `table/public.tsx:19-56` |
| F6.4 | resolver 来源：`router/public.tsx` 用 `liveTableOverlayPayload(track.id, overlays, source)`，按 `(entity_kind='track', entity_id, plugin_id, kind)` 匹配 | [实测] `fe/web/src/app/router/public.tsx:3049-3052`；`fe/core/domain/track.ts:110-131` |
| F6.5 | zod：`chartCandlesPayloadSchema` strictObject；`payloadSchemaFor(kind)` switch，未知 kind → `{kind:'unsupported', declaredKind}`；`ReportBlock` 是闭合联合 | [实测] `fe/core/domain/report.ts:52-59`、`:101-118`、`:230-272` |
| F6.6 | 渲染 switch `BlockBody` 无 default（加 zod 分支不加 case 会 TS 不过）；`unsupported` 渲染 "unsupported block kind X" | [实测] `fe/web/src/features/report/document/public.tsx:326-345` |
| F6.7 | 查询 key `overlaysByKind`、`trackReportPrefix`；`track.report_edited` 失效 `['track-report']`、`['track', id]` 等；`overlay.set` 失效 `['overlays', kind]` 与 `['track', id]` | [实测] `fe/web/src/app/providers/queries.ts:172-173`；`fe/core/events/invalidation-plan.ts:291-304` |
| F6.8 | legacy `web/`：`typedReportBlockSchema` 只认五个 kind，其它落 `opaqueReportBlockSchema`；未知 kind → `unsupported block kind {kind}` | [实测] `web/src/cards/builtins/track-report.tsx:200-226`；`web/src/pages/report-blocks/index.tsx:60-61` |
| F6.9 | 两份签入 OpenAPI：`fe/core/api/generated/openapi.json` 与 `web/src/api/openapi.json` | [实测] |
| F6.10 | 生产是明文 http LAN ⇒ 浏览器里 `crypto.subtle` 不可用（不安全上下文），**fe 不能算 sha256**（影响 D5 的请求绑定载体） | [实测] 项目记忆 `project_insecure_context_web_apis`；本设计不在 fe 做哈希 |

### 2.7 迁移、版本常量与时钟

| # | 事实 | 位置 |
|---|---|---|
| F7.1 | 迁移目录 `crates/calm-truth/migrations/`，最新 `0106_candidate_repair.sql`；**本设计 S2 需要一张新表，号最后才定** | [实测] `ls … \| tail` |
| F7.2 | `SYNC_EVENT_VERSION = 20`；门禁 `gate-sync-event-version-lockstep.sh`。**本设计不新增 Event kind**（行写入不发事件，§4 D3），不 bump | [实测] `calm-types/src/event.rs:258` |
| F7.3 | `REST_API_VERSION = "7"`（`crates/calm-types/src/compatibility.rs:5`）；`WEB_COMPAT_VERSION = 27`（`routes/version.rs:116`），门禁只查三处相等。新增一条 GET 路由是加法，不 bump | [实测]（v1 把 `compatibility.rs` 写在 calm-server，§11 m1） |
| F7.4 | `TrackReportPayload::SCHEMA_VERSION = 4`；新 kind 不改 payload 形状，不 bump | [实测] `calm-types/src/track_report.rs:146-152` |
| F7.5 | #1316 术语棘轮扫 `docs`（`RATCHETED_SCOPES=(crates fe docs e2e)`，`git grep` 只看已跟踪文件）；本文档避免退役词 | [实测] `scripts/gate-1316-terminology-ratchet.sh:459` |
| F7.6 | 仓内已有 `sha2::Sha256` 用法（观察 hash）：`track_report_doc.rs:232-235`；`request_hash` 复用同一 crate，不再引入依赖 | [实测]（v1 U4 关闭，§11 m2） |
| F7.7 | 时钟在 calm-server / calm-truth 这一层：`calm_truth::model::now_ms() -> i64` 是内核给 `*_at` 列盖戳的规范函数；calm-server 依赖 `chrono 0.4`；"可注入时刻的纯核心 + 薄壳取 `now`"的既有形状是 `planner_attachments/gc.rs` 的 `sweep_staging` / `sweep_staging_at(staging, now, ttl)`。**"今天 UTC"的计算落在 calm-server 的 resolver**，不在 calm-types（F1.12） | [实测] `calm-truth/src/model.rs:540-546`；`calm-server/Cargo.toml:182`；`gc.rs:58-64` |
| F7.8 | 读事务与写事务：`write_in_tx` / `write_in_tx_typed` 用 `begin_immediate_tx`（`BEGIN IMMEDIATE`，事务开始即取 SQLite 写锁，`events.rs:872-874`、`infra.rs:10-15`）；只读访问的既有形状是 `repo.sqlite_pool()`（`state.rs:686`）+ `pool.begin()`（sqlx 默认 `BEGIN` = DEFERRED，只读不取写锁）或直接在 pool 上 `fetch`（先例 `isolated_codex/settled.rs:148-151`）；`report_blocks_snapshot_tx` 只要 `&mut Transaction<'_, Sqlite>`（`track_report.rs:73-76`），两种事务都能传；journal 是 WAL（`sqlite/mod.rs:279`），读者不阻塞写者。D2 步骤 1 的准入用读事务（§11 第 4 轮 A m6） | [实测] |

### 2.8 简化假设（用户："水合只需要给一个够用的简化就行"）

- 只做日线及以上（`period ∈ day/week/month`），不做盘中：**live 的截止日是解析时的昨天 UTC**，当天的 bar 永远不进 live 行（D2）。纳入规则按 `(mode, period, venue)` 分支（§2.5 S3 约束 3、D2 步骤 6）：**live 股票日线**（US/HK/SH/SZ）按「日期 ≤ 昨天 UTC」纳入，不滞后——昨天的 bar 只要源已列出就在图上（三个市场都在 UTC 零点后，G14）；**其它全部**（frozen、live 周/月、CRYPTO 任何 mode）只在源已发布出更晚的日线 bar 后才纳入（结束日 < `complete_through`）：live 周/月线只含结束日 ≤ 截止日且已被更晚的日线 bar 证明闭合的周期——**live 周线最多滞后一周加一个交易日、月线最多滞后一个月加一个交易日**（截止日落在周期中间时，该周期不出现）；加密 live 日线的昨天 bar 依赖今天的 K 出现（Binance 全天候出 K，实际不滞后，G21）。
- **解析在写后的第一次读触发，不在写时触发；写完立刻读到的是 `pending`。** 触发点只有两个读者（`calm.report.read`、HTTP GET），写路径不做任何事。
- **保证语句（带例外清单，§11 第 3 轮 codex R3-MINOR-1）**：一个块被读过之后，其 job 在该插件 lane 里排到时，`tools_call` 至多 30s（`tokio::time::timeout(SERIES_RESOLVE_TIMEOUT)`，D2 步骤 5）返回或超时，之后**一次写行**得到 `ok` 或 `unavailable`；lane 延迟 = 排在前面的 job 数 × 各自 ≤ 30s（+ 每 job 一次短事务准入与写行）。**例外**（都以「下一次读重投」收口，不另加机制）：(a) 步骤 7 写行失败（FK、IO）→ 无行，warn，键释放；(b) drain 任务 panic → 该 lane 队列里的 job 随 receiver drop 丢失、键释放；(c) `enqueue` 在 `lanes` 锁内替换了一条 lane → 被替换 lane 队列里的 job 同 (b)；(d) 路由预检未命中 / 作用域 `None` 时**不排队、不落行**，读端返回 `pending, reason`——插件之后启动，要等下一次读（浏览器 `pending` 轮询、Planner 下一轮 read）才排队（§11 第 4 轮 A M2）；(e) `lanes`/`inflight` 全在内存，进程重启丢掉全部排队 job，靠下一次读重投（§11 第 4 轮 A m2）。**从未被读的块不解析。不承诺有界新鲜度**：过期行要等下一次读才重投，浏览器停轮询后过期行可见到下一次 fetch。
- 解析失败不重试风暴：`unavailable` 行与**含任一非 `ok` series 的 `ok` 行**按 `SERIES_UNAVAILABLE_TTL = 2 min` 才再投（全部 series `ok` 的行 6h；`row_ttl`，D3，§11 第 5 轮 codex R5-1；2 min ≫ 30s 超时且只在被读时重投，§11 第 4 轮 A M2）；同一块 `(track, block)` 同一时刻至多一个任务（in-flight 去重，键不含 hash，同一块反复改写只占一个位置，D2）；job 出队时行已新鲜或已钉住 → 丢弃（drain 准入，D2 步骤 1）。**TTL 是时长不是日历**：23:59 写入的行 00:01 仍新鲜，「过夜后第一次打开」只在距上次解析 ≥ 6h 时才刷新。
- 人与 AI 读同一行：摘要在写入行时由同一个 Rust 函数算好存下，两个读者只做序列化。**钉住的行**任何两个读者任何时刻字节相等；**未钉住的行**同一次读拿到同一行、两次读之间可被后台刷新替换，`resolved_at` 暴露这一点（D3）。
- 行写入不发事件：浏览器在 `pending` 时短轮询，其它时候靠既有失效与刷新（D5）。
- 内核不理解 venue、不理解交易日历、不做复权声明；这些归插件源。内核只比较日期字符串与 `ts_ms` 整数，外加历法算术（`start = as_of − RANGE_DAYS`、ISO 周一 / 月首末日，calm-server 的 `chrono`，F7.7）——历法不是交易日历。

## 3. Oracle trace

Planner 写 `chart.series` → 内核校验落盘（不触发任何事）→ 某个读者读到无行 → `enqueue` → 任务调插件、写一行 → 前端/Planner 读那一行。事件 kind 均为真实 `Event` 变体。状态：✅ 今天已成立 / ⚠️ 本设计新增或改动 / ❌ 今天为假、本设计修正。**每个 ⚠️/❌ 行恰好一个切片**（"片"列）。状态词：块级只用 `resolved.status ∈ {ok, pending, unavailable}`；`unknown_asset` 是插件回复里**每条 series** 的状态词，只在 3d 出现、归 S3。

| seq | phase | actor | trigger / MCP tool | 效果 | 可观察事件 | 不变式断言 | 状态 | 片 |
|---|---|---|---|---|---|---|---|---|
| 1 | discover | Planner | `calm.report.blocks.kinds` | 返回含 `chart.series` 的 kinds 表 | 无 | `upsert.kind.enum == commit.ops.kind.enum == kinds_table.kinds`（`contracts.rs:695-711`） | ⚠️ | S1 |
| 2 | write | Planner | `calm.report.commit{ops:[{op:"upsert", kind:"chart.series", payload:{source:"neige://plugin/dev-neige-market/market.series", series:["US:NVDA","HK:9988"], view:"normalized", range:"1Y"}}], if_doc_rev, message}` | `validate_payload` 通过 → canonical fence 落盘，docRev+1；**事务里不调插件、不写 `report_series`、不 enqueue** | `CardUpdated` + `TrackReportEdited` 恰好各一 | `flatten(split_body(body))==body`；`parse_fence` 回同 payload；假插件 tools/call 计数 0 | ⚠️ | S1 |
| 2n | write-neg | Planner | 同上但 `series:["NVDA"]`（无 venue）/ 9 条 / `view:"candles"` 配 2 条 / `source:"https://…"` / `as_of:"2026/09/10"`（形状错） | `-32602` 字段级错误；不写不发事件 | 无 | 拒绝发生在 `validate_chart_series`（纯形状，无时钟），F2 的 9 个入口都经它 | ⚠️ | S1 |
| 3 | resolve | 内核任务 | drain 任务取出 job `(track, block)`：准入（读事务里重读块、由**当前** payload 派生请求与 hash、行不存在或已过期且未钉住、`(plugin_id, tool)` 仍属本 lane）→ `plugin_tool_entry` 命中 → `connector_client` 为本地变体 → `timeout(30s, tools_call("market.series", args{…, mode, start, as_of, deadline_ms}, Some(track_id)))` | 回复通过校验清单 → 事务内 `INSERT … ON CONFLICT DO UPDATE … WHERE pinned=0` 一行 `status=ok`，`summary` 算好存下 | 无 | 同一 `(track,block)` 同一时刻至多一个任务（行身份仍是 `(track,block,hash)`）；写入受 FK 约束 | ⚠️ | S2 |
| 3a | resolve-admit | 内核任务 | 两个读者各对同一过期行 `enqueue`；第一个 job 写了新行后第二个 job 出队 | 第二个 job 重读行：`resolved_at` 在 TTL 内 → **丢弃，零次 tools/call** | 无 | in-flight 去重之外还有出队准入；TTL 在 drain 侧执行 | ⚠️ | S2 |
| 3a′ | resolve-coalesce | 内核任务 | lane 被一次慢调用占住；同一块被改写 5 次（`series` 各不同），每次改写后各读一次 | 5 次读只留下**一个**排队 job（键 `(track,block)` 已在 in-flight）；出队时准入读到第 5 版 payload，插件收到第 5 版的 `series`，行以第 5 版 hash 写入 | 无 | 排队 job ≤ 被读过且未出队的块数（§11 第 4 轮 codex R4-3）；出队时解析的是最新 payload，不是入队时的 | ⚠️ | S2 |
| 3b | resolve-neg | 读者 / 内核任务 | (i) `source` 的插件未安装 / 未运行 / 未暴露该工具；(ii) 绑定 track 的 owner 不可用（`TrackPluginScope::None`，source 插件可能在跑）；(iii) 绑定 track 的 owner 是另一个插件（`Only(other)`，`tool_visibility.rs:141`） | (i)(ii) **读端**直接返回 `resolved.status="pending"` 带 `reason`（`"plugin dev-neige-market is not installed"` / `"… is not running"` / `"… does not expose market.series"` / `"track owner plugin unavailable"`），**不排队、不 spawn、不写行**（§11 第 4 轮 A M2：v4 的一次性任务写 6h `unavailable` 行，会把 calm-server 重启后插件 `Spawning` 那几秒内被浏览器重连 / Planner 首轮 read 到的每个块冻结六小时）；有旧行时照常返回旧行，预检结果只决定不排队；(iii) lane 里的 job 在 resolve 步骤 3 写 `unavailable, reason="plugin dev-neige-market is outside this track's plugin scope"`（永久，G17） | 无 | (i)(ii) 零插件调用、零行、零 spawn；插件启动后的下一次读（浏览器 3s 轮询）就排队并解析（A9e）；reason 说的是本次读时刻的真话（m8） | ⚠️ | S2 |
| 3c | resolve-neg | 内核任务 | 超时 / `isError` / 回复非 object（S3 前 `market.series` 不在 manifest → 走 3b(i) 的 `NotExposed`，读端 `pending, reason`，到不了插件的 `unknown tool` 分支，§11 R3-MINOR-3） | 写一行 `unavailable, reason`（≤256 字符），TTL = `SERIES_UNAVAILABLE_TTL`（2 min，D3） | 无 | 超时只影响该插件队列，读者不等；**超时后 `McpClient.responders` 里该 id 已被 guard 移除**（假插件永不回复，两次超时后 `pending_responders() == 0`） | ⚠️ | S2 |
| 3d | resolve-partial | 插件 | 请求 `series` 里有 `US:NOPE` | 回复 `series[j].status="unknown_asset", reason`，其它条 `ok`（各带 `complete_through`） | 无 | 插件不因一条未知资产拒绝整个请求 | ⚠️ | S3 |
| 3d′ | resolve-partial | 内核任务 | 收到 3d 的回复 | 内核块级 `ok`，`pinned=false`，`summary.series[j]` 保留 `unknown_asset`；**行 TTL = `SERIES_UNAVAILABLE_TTL`（2 min）**——任一 series 非 `ok` 就用短 TTL（`row_ttl`，D3，§11 第 5 轮 codex R5-1），成功的 series 数据保留在行里 | 无 | 部分失败不拖垮整块，不钉住，**也不被 6h 锁住** | ⚠️ | S2 |
| 3e | resolve-neg | 内核任务 | 回复超 `MAX_SERIES_REPLY_BYTES` / 资产不一一对应 / `ts_ms` 非严格升序或非 UTC 零点 / 非有限数 / bar 日期 > 请求 `as_of` / bar 日期 < 请求 `start` / **bar 或周期的结束日 ≥ 该条 `complete_through`**（日线末点日期 == `complete_through` 也算；**`mode = live ∧ period = day` 不查这一条**，只查 ≤ `as_of`，D2 步骤 6）/ 周或月 `ts_ms` 非周期起始日 / 周期结束日 > `as_of` / 点数 > 区间上界 / `ok` 项 `points.len() < 2` / `ok` 项缺 `complete_through` | 整行 `unavailable, reason` | 无 | 校验清单在内核边界（D2 步骤 6） | ⚠️ | S2 |
| 3f | resolve-neg | 内核任务 / 读者 | `source` 指向 `kind: ForgeAction` 工具 / `readOnlyHint != true` / `ConnectorClient::Http`（都是路由命中后才知道、对该块永久的否定）；plugin_id 段含 `_`（`neige://plugin/aa_b/c`，fixture 插件 `aa` 暴露 `b_c`） | 前三者：整行 `unavailable, reason`，**零次** tools/call（2 min TTL 重投也只做 DB 判定、零调用）；`aa_b`：`registry.get("aa_b")` 为 `None` = `NotInstalled` → 读端 `pending, reason:"plugin aa_b is not installed"`（reason 里的 plugin id 是 `source` 里的原字符串），零行、零调用 | 无 | 精确查找 `plugin_tool_entry(registry, running, plugin_id, tool)`，不反解字符串；与 `plugin_tool_route` 的集合相等元测试（A19） | ⚠️ | S2 |
| 3g | resolve-deadline | 插件 | 出队时 `deadline_ms` 已过（内核早已超时） | 不打网络，回 `tool_error("deadline exceeded")`（内核侧 responder 已清，回复被 `mcp.rs:804` 的 else 臂 warn 丢弃） | 无 | 插件按自身时钟丢弃过期请求；fixture 源计数 0 | ⚠️ | S3 |
| 3h | resolve-fail | 内核任务 | 步骤 7 写行失败（测试 failpoint）/ drain 任务 panic（测试 failpoint） | in-flight 键随 RAII guard 释放；lane 在下一次 `enqueue` 时于 `lanes` 锁内因 `JoinHandle::is_finished()` 或 `send` 失败重建；被替换 lane 队列里的 job 随 receiver drop 一起 drop（tokio `Rx::drop` 排空缓冲区）→ 键释放 | 无 | 同键再 `enqueue` 仍会调插件（不 fail-locked 成永久 `pending`）；任何时刻每插件至多一条 lane 在跑 | ⚠️ | S2 |
| 3i | resolve-window | 插件 | 请求 `start=2024-12-31−366d`, `as_of="2024-12-31"`（两年前的 frozen）；fixture 源有 2011 起全量 | 回复窗口 `[start, as_of]` 内的日线，`complete_through` = **先**探测到的最新日线日期（2026-09-11） | 无 | 窗口相对截止日取，不是「最新 N 根」；fixture 源记录的请求顺序里探测请求先于全部窗口请求；收到带 start+end 的请求且分页拼接后无重复日期 | ⚠️ | S3 |
| 3i′ | resolve-window-neg | 插件 | 同上但 fixture 源最早只有 `start + 30d` 起的 bar | 该条 `unavailable, reason:"lookback exceeds source depth"` | 无 | 最早 bar 晚于 `start + 14d` 即报深度不足（G18） | ⚠️ | S3 |
| 3j | resolve-period | 插件 | frozen：`period:"week"`，`as_of` = 周日 2026-09-13，请求发生在周三 09-09（fixture 日线到 09-09；探测得 `complete_through` = 09-09） | 回复只到 ISO 周 08-31…09-06（`ts_ms` = 08-31 UTC 零点）；含周三在内的当前周（结束日 09-13 ≤ `as_of` 但 ≥ `complete_through`）**不出现**；fixture 推进到 09-14 出现日线 bar 后再解析 → 09-07 那周出现 | 无 | 纳入规则 = 结束日 ≤ `as_of` ∧ < `complete_through`（§11 第 4 轮 codex R4-2）；`complete_through` 仍是日线日期 | ⚠️ | S3 |
| 3k | resolve-order | 插件 | fixture 源在插件的**第一次** HTTP 请求之后推进一天：09-13 的 bar 从盘中值 `v_partial` 变成收盘值 `v_close`，并出现 09-14 的 bar；请求 `as_of` = 09-13 | 探测在前 → `complete_through` = 09-13 → 09-13 的 bar 按纳入规则不输出、行未钉住；下一次解析（fixture 已推进）→ `complete_through` = 09-14、09-13 = `v_close`、钉住 | 无 | 钉住的 09-13 值永远是 `v_close`；先取数后探测的实现会钉住 `v_partial`（§11 第 4 轮 codex R4-1） | ⚠️ | S3 |
| 3j′ | resolve-period-live | 插件 | `mode = live`、`period:"week"`，请求发生在周一 09-14 00:30 UTC（live `as_of` = 周日 09-13）；fixture 日线只到周四 09-10（周五 09-11 的 bar 尚未发布；探测得 `complete_through` = 09-10） | 09-07…09-13 那周**不出现**（结束日 09-13 ≤ `as_of` 但 ≥ `complete_through`）；fixture 补上 09-11 与 09-14 的 bar 后再解析 → 出现且 close = 周五 | 无 | live 周/月线保持严格（§11 第 5 轮 A §1 构造）：放宽会把周一至周四聚成的半根周 K 标成完整周；截止比较用周期结束日不用 `ts_ms` | ⚠️ | S3 |
| 3l | resolve-live-day | 插件 | `mode = live`、`period:"day"`、`as_of` = 昨天；fixture 对 `US:NVDA` / `HK:9988` / `SH:600519` 与 `CRYPTO:BTC` 的最新日线都止于昨天（无今天的 bar；探测得 `complete_through` = 昨天） | 三个股票 venue 的回复**含**昨天的 bar（放宽分支）；`CRYPTO:BTC` 的回复**不含**昨天的 bar（严格，`complete_through` 不晚于它） | 无 | venue 分支在插件（F5.1）；同一请求里两种规则并存、按条判；同一 fixture 下 `mode = frozen` 四条都不含昨天的 bar | ⚠️ | S3 |
| 4 | read-pending | Planner | `calm.report.read{}`，该块尚无行 | `resolved.status="pending"`；read 顺手 `enqueue`（路由预检 + 作用域 + 内存集合 + 可能建 lane，不写 DB；预检未命中 → `pending` 带 `reason`、不排队） | 无 | read 期间假插件收到零次 tools/call；read 永远 200 | ❌→⚠️ | S2 |
| 4a | read-summary | Planner | 同上，行已存在 | `blocks[i].resolved` = 行里的 `status/as_of/resolved_at/pinned/summary` | 无 | 纯 DB 读，无超时无并发预算 | ⚠️ | S2 |
| 4b | read-full | Planner | `calm.report.read{resolve:{"b_x":"full"}}` | 该块附 `series[j].points`；未点名的仍 summary | 无 | `points` 来自同一行的 `data` | ⚠️ | S2 |
| 5 | render-fetch | 浏览器 | `GET /api/tracks/{id}/report/series/{block_id}?rev=<n>`（NEW 路由） | 返回同一行（默认带 points）；无行 → `pending` 并 enqueue | 无 | `rev` ≠ 当前块 rev → 409 `{current_rev}`，旧数据不会贴到新参数上 | ⚠️ | S4 |
| 5a | render | fe | `ReportSeriesBlock` | SVG line/normalized/bar/candles；`pending`/`unavailable` 各渲染 caption + 一行文字；`pending` 时 3s 轮询直到非 pending；未钉住的 frozen 图标 "source data through <complete_through>" | 无 | 不含字面颜色；normalized 首点 ≤ 0 的序列显式标"不可归一化" | ⚠️ | S4 |
| 6 | live-stale | 任一读者 | 读到 live 行 `resolved_at < now - 6h` | 返回旧行 + `enqueue`（stale-while-revalidate） | 无 | 正在跑的块不再投（in-flight 去重） | ⚠️ | S2 |
| 7 | freeze-write | Planner | `commit` 写 `as_of:"2026-09-06"`（周日） | 存 fence；同 seq 2 | 同 seq 2 | 内核查 `YYYY-MM-DD` 形状与历法（`2026-02-30` 拒绝）；不与今天比较 | ⚠️ | S1 |
| 7a | freeze-pin | 内核任务 | 回复每条 `ok` ∧ 每条末点日期 ≤ `as_of` ∧ 每点结束日 < `complete_through`（校验清单）∧ 每条 `complete_through > as_of`（严格，§11 第 3 轮交叉命中 2） | 写行 `pinned=true` | 无 | 之后 enqueue 对该键是 no-op；`DO UPDATE … WHERE pinned=0` 拒绝覆盖 | ⚠️ | S2 |
| 7b | freeze-hold | 源 | 钉住后假插件换数据、TTL 过期 | 行**不变**，`resolved` 仍是钉住的数据 | 无 | 两个并发首解析竞争者只有一个能写、之后谁都不能覆盖 | ⚠️ | S2 |
| 7c | freeze-rehash | Planner | 改 `range`/`series`/`as_of`/`source`（hash 变）或 `view` 在 `candles` 与其它之间切换（`fields` 变 → hash 变）；改 `caption`/`overlays` 或 `view` 在 `line↔normalized↔bar` 之间切换（hash 不变） | 前者新行、旧行留到 track 删除；后者复用原行 | `CardUpdated`+`TrackReportEdited` | 行身份 = `(track,block,request_hash)`，不是 rev；指纹含 `plugin_id, tool`；in-flight 键是 `(track,block)`（D2），与行身份不同 | ⚠️ | S2 |
| 7d | freeze-incomplete | 内核任务 | frozen 块回复里有 `unknown_asset`，或某条 `complete_through ≤ as_of`（源最新日线 bar 是周五、`as_of` 是周日；**或 `as_of` 就是周五本身**） | 写行 `ok, pinned=false`（`as_of` = 周五的例子里周五的 bar 本身也按纳入规则不在回复里，D2 步骤 6）；TTL 后再投；源发布出**晚于** `as_of` 的日线 bar（周一）后 `complete_through > as_of` → 钉住，数据仍止于 ≤ `as_of` | 无 | 不完整快照永不钉住；钉住发生在「更晚的日线 bar 出现 ∧ 之后一次读 ∧ TTL 已过」；退市/停牌标的永不出现更晚 bar → 永不钉住（G3） | ⚠️ | S2 |
| 8 | live-drift | 假插件 fixture | live 行过期后（注入 `now` 前进一天）fixture 多给一根 bar（日期 = 新的昨天 UTC；`complete_through` **可以就等于该日期**——`live ∧ day` 的内核校验只查 ≤ `as_of`，§11 第 5 轮裁决 2） | 新行 `summary.last` 日期前进；行 `as_of` = 新的 `yesterday_utc(now)`（**前进**，§11 A m7）；docRev **不变**；`pinned` 仍 0 | 无 | 用受控 fixture 与注入时钟断言，不用日历时间 | ⚠️ | S2 |
| 9 | delete | 人 | 删 track | `ON DELETE CASCADE` 清掉 `report_series`；迟到的任务写入被 FK 拒绝 | `TrackDeleted` | 无孤儿行；in-flight 键随 guard 释放 | ⚠️ | S2 |
| 10 | human-read | 人 | 打开报告 | **钉住行**：seq 5 的字节 = seq 4a 的 `resolved`（任何时刻）；**未钉住行**：`resolved_at` 相同 ⇒ 字节相同 | — | 人与 AI 同源的精确边界 | ⚠️ | S4 |
| 11 | legacy | 人（web/） | 打开报告 | `unsupported block kind chart.series` | — | 差异被声明（§7 G5） | ✅ | — |
| 12 | fork | 人 | fork track | fork 事务里 `INSERT … SELECT` 复制源 track **全部** `report_series` 行到新 track（block id 不变，F2.6） | 现有事件 | 钉住行 fork 后不变；未钉住行在两个 track 里各按 TTL 刷新、可能分叉 | ⚠️ | S2 |

## 4. 决策

### D1 契约：`chart.series` payload

**问题。** 精确 JSON 形状；`chart.candles` 是否收编。

**备选。** (a) 新 kind `chart.series`，只命名数据；`chart.candles` 原样保留。 (b) `chart.candles` 扩成双形态。 (c) 新 kind 并废弃/迁移 `chart.candles`。

**裁决：(a)。** 形状（strict，未知字段拒绝，字符串 ≤ `MAX_STRING_CHARS`）：

```jsonc
{
  "source":  "neige://plugin/<plugin_id>/<tool>",   // 必填。同 F1.4 两段语法（复用 validate_live_source）；
                                                    // 第二段是插件 *tool 名*（如 market.series），不是 overlay kind
  "series":  ["US:NVDA", "HK:9988"],                 // 必填，1..MAX_CHART_SERIES(=8)，字面去重；
                                                    // 每条 ^[A-Z]{2,8}:[A-Za-z0-9._-]{1,32}$（内核只查形状，venue 语义归插件 F5.1）
  "field":   "close",                                // 可选，close|open|high|low|volume，默认 close；view=candles 时必须缺席
  "range":   "1Y",                                   // 可选，1M|3M|6M|1Y|2Y|5Y，默认 1Y；定义窗口 [as_of − RANGE_DAYS, as_of]（下）
  "period":  "day",                                  // 可选，day|week|month，默认 day；week/month 只含完整周期（D2）
  "view":    "line",                                 // 可选，line|normalized|bar|candles，默认 line；candles 要求 series.len()==1
  "as_of":   "2026-09-10",                           // 可选，截止日。查 YYYY-MM-DD 形状与历法（四位-两位-两位、月 01-12、日 ≤ 该月天数、
                                                    // 闰年 2 月 29；纯函数，§11 第 4 轮 A M1），不与今天比较；存在 = frozen；缺席 = live（截止日 = 解析时的昨天 UTC）
  "overlays": ["ma20"],                              // 可选，ma20|ma60，只对 line/candles 生效
  "caption": "…"                                     // 可选
}
```

- 不允许 inline 数据：数据只由 `source` 解析。要内联的数据继续用 `chart.candles`。
- `series` 去重按字面；`HK:9988` 与 `HK:09988` 在插件里是同一身份（F5.1 `canonical_symbol`），**内核不折叠**（不复制插件的 venue 规则），插件回复里两条同资产序列由校验清单的"资产一一对应"规则接受（回复 `asset` 必须逐项等于请求字符串）。
- **`as_of` 是截止日（cutoff），不是"最后一根 bar 的日期"**。历法非法的 `as_of`（`2026-02-30`、`2027-02-29`）在 S1 拒绝——否则 D2 的 `start = as_of − RANGE_DAYS` 没有定义的出口，最可能落成每 TTL 重投一次的永久 `unavailable`（§11 第 4 轮 A M1）；「calm-types 无时钟」推不出「不能查历法」，闰年 + 每月天数是纯函数。插件返回窗口 `[start, as_of]` 内的所有 bar（`start` 见下一条）；每条序列的末点日期可以早于 `as_of`（周末、假日、停牌）。**没有 `as_of` 上界**：写端不与今天比较（F1.12 该 crate 无时钟，§11 A M2）；未来日期的 `as_of` 只是"源尚未发布出晚于截止日的 bar"的 frozen 块——`complete_through ≤ as_of` → 未钉住，按 TTL 重投，源发布出更晚的日线 bar 后自动钉住（D3）。v2 担心的"`as_of:"2099-01-01"` 让钉住退化成钉在第一个任务跑的时刻"由 `complete_through` 判据消解，不需要时钟。fe zod 对未来日期可**提示**，不拒绝。
- **`range` 是相对截止日的窗口**（§11 第 3 轮交叉命中 1）：`RANGE_DAYS = {1M: 31, 3M: 92, 6M: 183, 1Y: 366, 2Y: 731, 5Y: 1827}`（历日，与 `max_points` 用同一张表），窗口 = `[as_of − RANGE_DAYS[range], as_of]`（两端含），live 即 `[昨天 − RANGE_DAYS, 昨天]`。内核在请求里显式填 `start`（calm-server `chrono` 做日期减法，F7.7），插件不自己算窗口（一个算法一个实现）；校验清单查每点日期 ∈ `[start, as_of]`。旧 `as_of`（如两年前）的 frozen 块窗口非空，v3 的「拉最新 N 根再过滤」对它是 0 点 → 永久 `unavailable`，那句话已删。
- `chart.candles` **不收编、不迁移**：已存文档、两个前端各有渲染器（F6.1、F6.8）、有校验与集成测试。折衷：`kinds_table` 里 `chart.candles` 的 usage 改为"内联数据的逃生口；行情能由插件解析的标的请用 `chart.series`"（F2.12 那句删掉）。**这是与 issue 方向 1 的出入**（§8）。
- caps：`MAX_CHART_SERIES = 8`（新常量，放 `kinds.rs:46-55` 旁）。点数上界不再复用 `MAX_CHART_CANDLES`，改为按窗口自然大小 `max_points(range, period)`（D2 步骤 6，用同一张 `RANGE_DAYS`）。**结构上装不下 2 个完整周期的组合在 S1 拒绝**：只有 `{range:"1M", period:"month"}`（32 个历日至多含 1 个完整自然月；其它组合的结构下限见 §5.1 S1）→ `-32602 "range 1M cannot hold two complete month periods"`（§11 第 4 轮 A m3：否则该块永久 `unavailable` 且每 TTL 重投）。
- 校验落点：`kinds.rs` 新 `validate_chart_series`（挨着 `validate_chart :483`，**纯函数：形状 + 历法 + 组合，无时钟**），`DATA_KINDS` 变 5 项，`KIND_CHART_SERIES` 常量；`contracts.rs::kinds_table` 加一项并改 F2.11 两段手写文字；`fe/core/domain/report.ts` 加 `chartSeriesPayloadSchema` + `payloadSchemaFor` 分支；`web/` 不加（落 opaque，F6.8）。
- Rust 侧派生（calm-server）：`SeriesRequest::from_payload(&Value) -> (SeriesRequest, request_hash)`（读端与出队都调；`start` 不在这里算，见 D2 步骤 1），`fields` 由 `view` 派生（candles → `[open,high,low,close,volume]`，否则 `[field]`）；**`request_hash = hex(sha256(canonical_json({plugin_id, tool, series, fields, range, period, as_of})))`**——`plugin_id, tool` 入指纹（§11 R2-3：换源必须换行，否则 P 的结果会钉在 Q 的声明下）；`as_of` 是 payload 里的值（live 缺席，**live 的运行时截止日不入指纹**，否则每天一行）；请求的 `mode` 由 `as_of` 的有无派生（存在 = `frozen`、缺席 = `live`），**不另入指纹**（`as_of` 的有无已在指纹里）。`caption/overlays` **不入指纹**（表现层）；`view` 只通过 `fields` 间接入指纹：`line↔normalized↔bar` 互换 `fields` 不变 → 复用行，`candles` 与其它互换 → 换行（§11 R2-MINOR / A m1）。

**依据。** F1.1-F1.6、F1.4、F1.12、F5.1、F6.5/F6.8。

### D2 解析任务：内核如何、何时向插件要序列

**问题。** v1 在 read 里同步调插件；v2 用 bus 订阅者在写后触发，但订阅者从 `body_after` 拿不到块 id（F1.11、F3.6），且 bus 有损无重放（F3.7），模板/fork/recipe 写入根本不发该事件（F3.9）。

**备选。** (a) 保持 read 内同步解析。 (b) 写后 bus 订阅者 + 读者兜底。 (b′) 订阅者改为收到事件后加载权威块快照再 enqueue。 (b″) **只由读触发**。 (c) 插件预推全部历史成 overlay。

**裁决：(b″)。** (b′) 被否：它让订阅者做读者已经在做的事（加载快照、算 hash、比行），而 bus 的丢失/重启/无启动扫描三个问题一个都没解决，最终仍靠读者兜底；(b″) 删掉这层，保证语句反而更诚实（§2.8）。机制：

- **触发**（两处，都只 `enqueue`，不调插件、不写 DB）：
  1. `calm.report.read`（D4）：对每个 `chart.series` 块，按全主键读行（hash 由当前 payload 派生）；无行 / 过期行（`row_ttl`，D3）且未钉住 → `enqueue` 并返回现状；`enqueue` 回 `Miss(reason)` 且无行 → `pending` 带 `reason`。
  2. HTTP GET（D5）：同上。
  3. 写路径不做任何事；不做启动扫描、不做定时扫描。
- **`enqueue(track_id, block_id, request) -> Queued | InFlight | Miss(reason)`**（NEW `SeriesResolver`，挂在 `AppContext`；`request` 是读端由当前 payload 派生的 `SeriesRequest`，`enqueue` 只用其 `plugin_id, tool` 做预检与选 lane，不带进 job）：
  1. `inflight: Mutex<HashSet<Key>>`，**`Key = (TrackId, BlockId)`，不含 hash**（**检查与插入是锁内一次 `HashSet::insert`，原子**：返回 `false` → `InFlight`；返回 `true` → 立即构造 guard，再做步骤 2 的预检，预检未命中 → guard drop 释放键、返回 `Miss`——键在预检期间被短暂占用，此时到达的另一个读者得到 `InFlight`（`pending` 无 reason）而不是 `Miss(reason)`，它的下一次读会拿到 reason；§11 第 5 轮 codex MINOR：v5 的「先查、预检命中后再插入」让两个读者都能穿过检查-插入之间的空隙、各建一个 job；§11 第 4 轮 codex R4-3：v4 的 `(track, block, hash)` 让「lane 被慢调用占住时反复改写并读取同一块」每次都产生新键，一个块可以挂着上千个排队 job，陈旧 hash 只在出队后才丢弃、拦不住累积，删块也留着；改键后同一块至多一个排队 job，出队时读的是当时的最新 payload）；`insert` 返回 `false` → `InFlight`。只有 `insert` 成功者构造 `InflightGuard { set, key }`（`Drop` 时 `remove`）随 job 走——job 无论正常完成、提前 `return Err`、panic 展开、还是随 lane receiver 一起被 drop，键都释放（§11 A M4 构造 1）。
  2. 路由预检 `plugin_tool_entry(registry, &running_ids, plugin_id, tool)`（定义见 resolve 步骤 2）与作用域 `plugin_scope_for_track(ctx, Some(track_id))`（F4.18；一次 read 里对所有块只算一次）：`NotInstalled | NotRunning | NotExposed` 或作用域 `None` → **释放键、返回 `Miss(reason)`，不建 lane、不 spawn、不写行**（§11 第 4 轮 A M2：v4 用一次性任务写下 6h 的 `unavailable` 行，calm-server 重启后 market 插件 `Spawning`（`running_plugin_ids` 明写不含 `Spawning`，`plugin_host/mod.rs:3256-3262`）的那几秒内，浏览器 WS 重连的 refetch 与 Planner 首轮 read 会把每个块都冻结六小时，用户刷新无效；否定结果现在只活在这一次读的响应里）。`Found(entry)` 且作用域 `All | Only(_)` → `Job { key, guard, lane_plugin: plugin_id }` 投该插件的 lane（`Only(other)` 也进 lane，由 resolve 步骤 3 写永久 `unavailable`，G17——这是修订者对 A M2 处置的延伸，见 §11 第 4 轮末行）。所以 lane 只为路由命中的插件存在，一篇写了 1000 个不存在 plugin_id 的报告零条 lane、零个任务、零行（§11 A m6）。
  3. lane：`lanes: std::sync::Mutex<HashMap<PluginId, Lane { tx: mpsc::UnboundedSender<Job>, drain: JoinHandle<()> }>>`。**通道 unbounded**：`enqueue` 的投递是同步非阻塞的 `UnboundedSender::send`，读者永远不等通道容量（§11 第 3 轮 codex R3-5：v3 的有界 `mpsc::Sender` + `send().await` 会在一个挂住 30s 的调用后面让读者等容量，与「enqueue 不等插件」矛盾）。**队列上界来自 in-flight 集合**，不来自通道：每个 `(track, block)` 键同一时刻至多一个 job（步骤 1），所以 `inflight.len()` = 排队 job 数 + 执行中 job 数（每 lane ≤ 1；执行中的 job 也持 guard，§11 第 5 轮 codex MINOR），排队 job 总数 ≤ `inflight.len()` ≤ **被读过且 job 尚未结束的块数**——同一块被改写 100 次再读 100 次仍只占一个位置，块被删除后它的 job 在出队准入处丢弃、键随之释放（§11 第 4 轮 A m1：v4 写的「≤ 工作区块数（每块一个当前 hash）」在旧 hash 的 job 仍在飞时不成立；这是执行上界的机制，不是估计）。**检查-重建-投递在 `lanes` 锁内一次完成**（§11 A m1）：`let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner); let lane = lanes.entry(id).or_insert_with(spawn_lane); if lane.drain.is_finished() || lane.tx.send(job).is_err() { rebuild_lane(&mut lanes, id).tx.send(job) }`——锁内没有 `.await`（spawn 与 send 都是同步的），两个并发 `enqueue` 不可能各建一条 lane，任何时刻每插件至多一条 drain 在跑；`rebuild_lane` 以 `&mut MutexGuard<'_, HashMap<…>>` 为参数（一个 `MutexGuard` 只能来自 `lock()`；这是防误用，不是证明——证明是 A9f 的 `try_lock` 断言，§11 第 4 轮 codex R4-MINOR）。被替换 lane 的 `UnboundedReceiver` 随 panic 掉的 drain 任务一起 drop，tokio 1.52.3 的 `Rx::drop` 关闭并排空缓冲区（`sync/mpsc/chan.rs:487-508` `drain`）→ 队列里的 job 被 drop → 各自的 `InflightGuard` 释放键 → 下次读重投（§2.8 例外 (b)(c)）。两把 `std::sync::Mutex` 都用 `unwrap_or_else(PoisonError::into_inner)`（§11 第 4 轮 A m7：`InflightGuard::drop` 在 unwind 中跑，`lock().unwrap()` 遇 poison 会在 Drop 里二次 panic → abort；`lanes` 一旦 poison，读路径上每次 `enqueue` 都 panic）。**按插件串行、跨 track 全局**（G20）——与 market 插件自身的单工作线程（F4.13）同构。测试 seam：`SeriesResolver::new_unstarted()` 只记录 `enqueue` 的调用与返回值（`Queued / InFlight / Miss`）与 job、不 drain、不建 lane（A9e/A10/A10b 用，测试可手动执行记录下的 job）；`#[cfg(test)] failpoints { fail_write_once, panic_drain_once, hold_in_rebuild, hold_in_precheck }` 与 `#[cfg(test)] fn lanes_try_lock()`（A9c/A9d/A9f 用）；`now: fn() -> i64` 注入（默认 `calm_truth::model::now_ms`，A8 用）；`SERIES_RESOLVE_TIMEOUT` 是 `SeriesResolver` 的字段而非常量（A5 注入毫秒级）。
- **任务 `resolve(job)`**：
  1. **准入（读事务）**：`repo.sqlite_pool()` 是 `Option`（trait 默认 `None`，`calm-truth/src/db/mod.rs:1236-1238`；`state.rs:686` 透传）：**`None` → 丢弃 job + warn，不落行**（§11 第 5 轮 A m2）；`Some(pool)` → `pool.begin()`（DEFERRED，只读不取写锁，F7.8；**不用 `write_in_tx_typed`**——它是 `BEGIN IMMEDIATE`，准入只读却会与报告写入争 SQLite 单写者，§11 第 4 轮 A m6）里用 `report_blocks_snapshot_tx(tx, track_id)`（F3.8，`track_report.rs:73`，返回带 payload 的 `Vec<ReportBlock>`）重读块。**不用 `load_report_read_snapshot`**（`track_report_read.rs:43-52`：它先 `load_settings` 再按 `task_budget` 算 `task_diagnostics`，那是 read 面的诊断，与准入无关，§11 A m4）。块不存在 / kind 不再是 `chart.series` → 丢弃。**由当前 payload 派生 `SeriesRequest`（含 `mode`）与 hash（出队时算，不信任入队时的观察，§11 第 4 轮 codex R4-3）**；`(plugin_id, tool)` ≠ 本 lane 的插件（块在排队期间被改到别的源）→ 丢弃，下次读投到正确的 lane。`start = as_of − RANGE_DAYS[range]` 在这里算（calm-server `chrono`，F7.7）：`as_of` 经 S1 历法校验后 `NaiveDate::from_ymd_opt` 不会失败，若失败（只能是绕过校验的数据）→ 丢弃并 warn，不落行。然后**按全主键 `(track_id, block_id, request_hash)` 精确选行**——hash 先派生、再选该 hash 的行，**不按 `(track, block)` 任取一行**（§11 第 5 轮 codex MINOR：旧行故意保留（G8），h1 钉住后块改成 h2，按 `(track, block)` 查到 h1 的钉住行就永远压住 h2）。判定后 rollback，不跨插件调用持有。行存在且（`pinned = 1` 或 `resolved_at ≥ now − row_ttl(row)`：`row_ttl` 是 S2 的一个纯函数、读端与准入共用——`status = ok` ∧ `summary.series` 全部 `ok` → `SERIES_TTL` 6h；否则（`unavailable` 行、或任一 series 非 `ok` 的 `ok` 行）→ `SERIES_UNAVAILABLE_TTL` 2 min，D3，§11 第 5 轮 codex R5-1）→ 丢弃（§11 R2-5：TTL 在 drain 侧执行，不信任读者的旧观察）。
  2. **路由**（出队时再查一次：插件在排队期间停止 → `NotInstalled | NotRunning | NotExposed` → **丢弃 job、不落行**，与读端同一规则「否定结果不落行」，下次读返回 `pending, reason`）：NEW `pub(crate) fn plugin_tool_entry(registry: &PluginRegistry, running_ids: &BTreeSet<String>, plugin_id: &str, tool: &str) -> ToolEntry` 放在 `transport.rs` `plugin_tool_route` 旁；实现 = `registry.get(plugin_id)`（F4.16 精确查找，**不拼 `plugin.{id}_{tool}` 再反解**）→ `manifest.exposes_tools.iter().find(name == tool)` → `running_ids.contains(plugin_id)`。返回枚举 `NotInstalled | NotRunning | NotExposed | Found(ExposedTool)`（一个布尔装不下四种结论）。三个否定态的 reason 文字（读端 `pending` 用）：`NotInstalled` → `"plugin <id> is not installed"`；`NotRunning` → `"… is not running"`；`NotExposed` → `"… does not expose <tool>"`。`Found(entry)` 且 `entry.kind.is_some()` → 行 `unavailable, reason="tool is not an ordinary read-only tool"`；`entry.annotations["readOnlyHint"] != true` → 同上（这两条对该块是永久的、只有改块或改 manifest 才变，落行让 fe 停止轮询；2 min TTL 重投只做 DB 判定、零调用）。**G7 由此关闭**（未安装/未运行/未暴露三态可分）。`plugin_tool_route` 不改（不需要 `pub(crate)`）；S2 加一条元测试：对 fixture registry 里每个 `(id, tool)` 与若干不存在的组合，`plugin_tool_entry(...) is Found` ⇔ `plugin_tool_route(registry, "plugin.{id}_{tool}", running) == Ok(Some((id, tool, kind)))`（A19）。
  3. **作用域**：`plugin_scope_for_track(ctx, Some(track_id)).allows(plugin_id)`（与 agent 路由同一规则 F4.11）。`TrackPluginScope::None`（绑定 track 的 owner 不可用）→ **丢弃 job、不落行**（读端已对这一态返回 `pending, reason="track owner plugin unavailable"`——不是"plugin X is not running"，source 插件可能正在跑，§11 A m8；出队时再查是因为排队期间 owner 可能停止，fail-closed 不调用）；`Only(other)` → `unavailable, reason="plugin <id> is outside this track's plugin scope"`——插件模板创建的 Owned track 上，非 owner 插件的 `chart.series` **永久** `unavailable`（G17，seq 3b(iii)；§11 A m2 维持保守规则）。影响面：track 只在「创建时某个运行中的可信插件用 manifest `templates` 认领了该模板 key」时才带 owner（`routes/tracks.rs:1683-1688`、`:1691-1695`、`:1750-1752`；`grep -n '"templates"' plugins/*/manifest.json` 只有 `git-forge` 认领 `issue-development`），投研模板 `investment-research`（#1626，origin/main `7754fd32`，不在基线）是内核花名册项、无插件认领 → `plugin_scope = NULL` → `All`。
  4. **客户端**：`connector_client(plugin_id)`：`None`（两次采样之间停止）→ 丢弃、不落行；`Http(_)` → `unavailable, reason="remote connectors are not series sources"`（远端服务不该由文档内容驱动被内核请求，§11 B1 构造 2）；`Stdio(c)` → `c.tools_call(tool, args, Some(&track_id))`；`Cli(c)` → `c.tools_call(tool, args)`。
  5. **超时与 responder 清理**：`tokio::time::timeout(SERIES_RESOLVE_TIMEOUT = 30s, …)`。理由：后台执行、按插件串行，长超时的代价是该插件队列的延迟而不是任何读者的等待；8 条资产逐条打腾讯/Binance 各 ≤3s 的最坏情况在 30s 内。**超时不再泄漏 responder**：S2 在 `McpClient::call` 里、`responders.insert`（`mcp.rs:667`）与 `rx.await`（`:676`）之间加一个 `ResponderSlot { map: &ResponderMap, id }` RAII guard，`Drop` 时 `map.lock().remove(&id)`（对端已回复时 `:804` 已 `remove`，再 `remove` 是 no-op）；超时取消 = future 被 drop = guard 被 drop = 槽位移除。这是对**所有**内核→插件调用的修复（agent 路由 `transport.rs:722-738`、`cards.rs:588` 一并受益）；`#[cfg(test)] pub(crate) fn pending_responders(&self) -> usize` 给 A5b 用。v2 的"串行化把泄漏封顶为 1"是假的：永不回复的插件下每次超时留一个槽位，跨块、跨 TTL 周期无界累积（§11 R2-1，第 1 轮处置作废）。**插件侧**：内核超时不通知插件取消（登记 G11），请求仍留在插件单工作线程队列里按序处理（F4.13）；缓解在 S3——请求带 `deadline_ms`（内核 `now_ms() + 30_000`），`market.series` 出队时若自身时钟已过 `deadline_ms` → 不打网络，回 `tool_error("deadline exceeded")`（seq 3g）。
  6. **回复校验清单**（内核边界，任一不过 → 整行 `unavailable, reason`）：`isError != true`；`structuredContent` 是 object；序列化整个 `CallToolResult` ≤ `MAX_SERIES_REPLY_BYTES = 2 MiB`（**这是内核接受并存储的回复上限，是校验，不是内存上界**——传输层 `read_line` 在此之前已把整行读进内存，字节上界见 #1634 / G11）；`series` 数组长度 == 请求长度且第 j 项 `asset` == 请求第 j 条（一一对应）；每项 `status ∈ {ok, unknown_asset, unavailable}`；`unknown_asset`/`unavailable` 项的 `reason` 是字符串（存储时截到 256 字符）；`ok` 项：`complete_through` 是 `YYYY-MM-DD`；**`points.len() >= 2`**（图与摘要都需要两点；空或单点由插件自己报 `unavailable, reason:"no data in range"`，内核收到 `ok` 配 <2 点视为 malformed，§11 R2-9）；每点长度 == 1 + `fields.len()`；`ts_ms` 是整数、**`ts_ms % 86_400_000 == 0`（= 交易日 UTC 零点，§11 A m11）**、严格升序；数值全部有限（`f64::is_finite`）；每点日期（`ts_ms / 86_400_000` 折成 `YYYY-MM-DD`）∈ **`[请求 start, 请求 as_of]`**（两态统一，因为 live 请求也带 `start`/`as_of`）；`period = week` → 每点是周一（`(ts_ms / 86_400_000 + 3) % 7 == 0`，1970-01-01 是周四）且 `ts_ms 日期 + 6d ≤ as_of`；`period = month` → 每点是 1 日且该月最后一日 ≤ `as_of`（`chrono` 历法，§11 第 3 轮 codex R3-3：未完成周期在内核边界也拒绝，不只靠插件）；**每点的周期结束日 < 该条 `complete_through`**（day：bar 日期；week：`ts_ms` 日期 + 6d；month：该月最后一日；§11 第 4 轮 codex R4-2——末点日期 == `complete_through` 也是 malformed：那根 bar 没有更晚的 bar 证明已收盘）——**这一条只在 `(mode = frozen ∨ period ≠ day)` 执行；`mode = live ∧ period = day` 不查它**，只查上面的「每点日期 ∈ `[start, as_of]`」（§11 第 5 轮裁决 2：内核无 venue 知识，加密日线的严格由插件执行、内核的弱检查对它照样通过；放宽的正确性由 §2.5 收盘时刻表与「live 行永不钉住 + 6h 自愈」承担）。这一组与插件的纳入规则（§2.5 S3 约束 3）是同一组规则的两处执行，两层都查 `period_start ≥ start`（= 每点日期 ≥ `start`）与周期结束日 ≤ `as_of`；点数 ≤ `max_points(range, period)`（`RANGE_DAYS[range] / period_days + 2`，period_days day=1 / week=7 / month=30：1Y day = 368、5Y day = 1829、5Y week = 263、5Y month = 62）；存储 `data` 序列化 ≤ `MAX_SERIES_ROW_BYTES = 1 MiB`。
  7. 写行（D3，`write_in_tx_typed` 写事务），`resolved_at = now`，`summary` 由 `summarize(&Series) -> Summary` 在写入前算出并一起存。写失败（FK、IO）→ warn 并返回；键由 guard 释放。
  8. （无显式清理步骤：`InflightGuard` 在 job 作用域结束时 drop。）
- **请求窗口**：内核在每个请求里显式填 `start` 与 `as_of`（出队时算，步骤 1）：frozen `as_of` = payload `as_of`；**live `as_of` = 昨天 UTC**（`yesterday_utc(now_ms)`，calm-server 用 `chrono` 算，F7.7）；`start = as_of − RANGE_DAYS[range]`（D1）。请求不带 `range`：窗口只在内核算一次，插件按 `[start, as_of]` 取数（§2.5 S3 约束 2）。请求另带 **`mode`**（payload 有 `as_of` = `"frozen"`、无 = `"live"`，内核填；§11 第 5 轮 codex R5-3：两态请求同形时插件无法区分「frozen 截止日恰是昨天」与 live，而两者的纳入规则不同；从截止日反推 mode 会削弱 frozen）；两态都带 `start`/`as_of`，不再有「无 `as_of`」的请求形状；插件按 `(mode, period, venue)` 选纳入规则（§2.5 S3 约束 3）；当天的盘中半根 bar 永远不进 live 行（§11 A M3）。代价：CN/HK 市场按 UTC 日期可能多滞后一天（G14）。
- **请求形状**（内核由 payload 派生）与**回复形状**（`structuredContent`）：
  ```jsonc
  // 请求（frozen 与 live 同形；mode 由内核按 payload 是否有 as_of 填；live 的 as_of 由内核填昨天 UTC；start = as_of − RANGE_DAYS[range]，内核算好）
  { "series": ["US:NVDA","HK:9988"], "fields": ["close"], "period": "day", "mode": "frozen",
    "start": "2025-09-09", "as_of": "2026-09-10", "deadline_ms": 1789000000000 }
  // 回复（无顶层 as_of——回显请求值是空洞检查，§11 A M1）
  { "series": [
      { "asset": "US:NVDA", "currency": "USD", "status": "ok",
        "complete_through": "2026-09-11",           // 源未过滤的最新**日线** bar 日期，来自**先于**窗口取数的一次不带 end 的探测（§2.5 S3 约束 1）；与 period 无关
        "points": [[ts_ms, close], …] },             // ts_ms = 交易日（week/month：周期起始日）UTC 零点；全部日期 ∈ [start, as_of]；结束日 < complete_through（live ∧ day ∧ 非 CRYPTO 除外，§2.5 S3 约束 3）；≥ 2 点
      { "asset": "HK:9988", "status": "unknown_asset", "reason": "…" },
      { "asset": "US:NEW",  "status": "unavailable",   "reason": "no data in range" } ] }
  ```
- 插件 manifest `exposes_tools` 加 `market.series`（`readOnlyHint: true, openWorldHint: true`）。它同时对 agent 可见为 `plugin.dev-neige-market_market.series`（F4.12 无隐藏机制）——这是接受的，也是"要即时数据自己调"的通道（§2.8）。
- 与 overlay 推送的关系：互不替代。表继续用推送；序列用内核拉取。两者共享 `neige://plugin/<id>/<x>` 语法，第二段含义不同（overlay kind vs tool 名）；kinds 表与 schema description 必须写清。

**依据。** F1.11/F3.6/F3.9（订阅者拿不到块 id、事件只覆盖一条写路径）、F3.7（bus 有损无重放）、F3.3（Planner 每轮必 read → 读触发够快）、F4.16（`registry.get` 精确查找）、F4.6（取消不清 responder → 本设计加 guard）、F4.13（插件单线程 → 内核端也串行；内核超时插件不知情 → `deadline_ms`）、F4.14（KV 配额 256KB，插件不能把历史存 KV）、F7.7（时钟在 calm-server）。(a) 被否：read 是 CAS 握手的必经路径（F3.3），任何插件延迟都放大到每次写；(c) 被否：撑大 overlay 且插件不知道该推哪些标的。

### D3 存储与 frozen / live

**问题。** 行存哪、身份是什么、谁写、什么时候钉住、删除与 fork。

**备选。** (a) 新表 `report_series`。 (b) 复用 `overlays` 表（`plugin_id = "kernel"` + 新内核 kind）。

**评估 (b)**：省一张迁移，但：① `GET /api/overlays?entity_kind=track` 不带 id 是**全工作区**拉取、侧栏用它（F4.3）；`GET /api/tracks/{id}` 也带全部 overlays——每个图 1Y 日线 ≈ 6KB、candles ≈ 12KB、8 条 5Y candles ≈ 730KB，都会随每次 `overlay.set` 失效被侧栏重新拉下来；② 每次行写入会发 `Event::OverlaySet(Overlay)` 带整个 payload（F4.4）进 WS 与插件 firehose；③ overlay 键 `(plugin_id, entity_kind, entity_id, kind)` 没有 `request_hash` 位，要把 `block_id:hash` 编进 `kind` 字符串；④ overlay 无外键（F4.2），删除竞态靠显式清理。四条里 ①② 是硬伤。

**裁决：(a)。** 迁移（号最后定，F7.1）：

```sql
CREATE TABLE report_series (
    track_id     TEXT    NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    block_id     TEXT    NOT NULL,
    request_hash TEXT    NOT NULL,                 -- hex sha256, D1（含 plugin_id, tool）
    status       TEXT    NOT NULL,                 -- 'ok' | 'unavailable'
    reason       TEXT,                             -- unavailable 时，≤ 256 字符
    as_of        TEXT    NOT NULL,                 -- 请求截止日 YYYY-MM-DD：frozen = payload；live = 解析时的昨天 UTC
    resolved_at  INTEGER NOT NULL,                 -- ms
    pinned       INTEGER NOT NULL DEFAULT 0,
    summary      TEXT,                             -- JSON，ok 行写入时算好；unavailable 行为 NULL（§11 A m3）
    data         TEXT,                             -- JSON：series[] 含 points，ok 时
    PRIMARY KEY (track_id, block_id, request_hash)
);
```

- **外键 vs 事务内存在性校验**：选外键 `ON DELETE CASCADE`。理由：`foreign_keys = ON` 每连接、同形先例 0104/0106（F4.15）；删除时 `track_delete_tx` 级联清掉行，迟到的任务写入在约束处失败（任务 warn 并丢弃，键由 guard 释放），没有"先查后插"的窗口。
- **写入**：任务在 `write_in_tx_typed` 里 `INSERT … ON CONFLICT(track_id, block_id, request_hash) DO UPDATE SET status, reason, as_of, resolved_at, pinned, summary, data = excluded.* WHERE report_series.pinned = 0`。`pinned` 行在 DB 层不可覆盖（A7 的变异靶点）。不发事件（F7.2 不 bump）。
- **`pending` 不是行的状态**：无行 = `pending`（读时词）。行的 `status` 只有 `ok | unavailable`。
- **live**：payload `as_of` 缺席。每次解析的截止日 = 昨天 UTC（D2），存进 `as_of` 列。TTL = `SERIES_TTL = 6h`：日线一天一根、收盘后源才更新；6h 让一天内被反复打开的报告最多刷新 4 次（读触发 + drain 准入共同保证，D2 步骤 1），一天开一次的报告刷新一次；**TTL 是时长不是日历**——过夜后第一次打开只在距上次解析 ≥ 6h 时刷新（23:59 写入的行 00:01 仍新鲜，§2.8）。**TTL 按行状态与 series 结果选——`row_ttl(row)`，S2 的一个纯函数，读端与 drain 准入共用**：`status = ok` ∧ `summary.series` 全部 `ok` → `SERIES_TTL` 6h；`unavailable` 行、**或任一 series 非 `ok`（`unavailable` / `unknown_asset`）的 `ok` 行** → `SERIES_UNAVAILABLE_TTL = 2 min`（§11 第 4 轮 A M2 (b)：一次瞬时 5xx / 超时不该让该块 6h 无图；§11 第 5 轮 codex R5-1：两资产回复里一条 `ok` 一条瞬时 `unavailable` 通过校验、块级 `ok, pinned=false`，v5 给它 6h，源立刻恢复也要等 6h 才补那条——A M2 的同胞分支；成功的 series 数据保留在行里，重投成功后整行替换；2 min ≫ 30s 超时，且只在被读时重投，串行 lane 下每块每 2 min 至多一次调用，不构成风暴。代价：`unknown_asset`（打错代码）也按 2 min 重投直到块被改，G22）。**`pinned` 行不过期**（钉住要求全部 `ok`，下段）。**live 行永远 `pinned = 0`**：钉住判据只对 frozen 评估（Binance 的探测让 live 行也能满足 `complete_through > 昨天`，但 live 的语义是随源流动，不钉）。
- **frozen**：payload `as_of` 存在；请求带它；插件只返回日期 ∈ `[start, as_of]` 的 bar。**钉住条件**（内核可验证，不依赖插件回显）：每条 series `status == ok` **∧** 每条末点日期 ≤ `as_of`（校验清单已保证）**∧** 每条 **`complete_through > as_of`**（严格，§11 第 3 轮交叉命中 2：源已发布出**晚于**截止日的日线 bar ⇒ 截止日当日的 bar 已收盘、"≤ as_of 的集合"是终态；`>=` 在 `complete_through == as_of` 时钉住的是内核无法证明已收盘的那一根——Binance 不带 end 的探测必然含当前未收盘日 K [实测 2026-09-13]，ifzq 盘中是否列当日 bar 未测但已无关）→ `pinned = 1`，此后 `enqueue` 对该键 no-op、写入被 `WHERE pinned = 0` 拒绝。配合校验清单的「每点结束日 < `complete_through`」与插件的「探测先于取数」（§2.5 S3 约束 1/3），钉住的行里每个 bar 都是在源已发布出更晚 bar **之后**取到的（§11 第 4 轮 codex R4-1 / R4-2）。前提假设：源的 bar 日期正确且按时间顺序发布（更晚日期的 bar 出现 ⇒ 更早日期的 bar 已收盘）；判据不证明历史完整性，也不阻止源事后修正（前复权基准变化，G1）。不满足 → `pinned = 0` 的 `ok` 行（图能画，但标"未钉住，source data through <min complete_through>"），按 TTL 再投直到满足。**逐场景**：`as_of` = 周日、源最新日线 = 周五 → 周五 > 周日假 → 未钉住；周一收盘出 bar → 周一 > 周日 → 下次 TTL 重投钉住，数据仍止于周五。`as_of` = 周五（最常见写法：最近一个交易日）→ 周五 > 周五假 → **也要等周一的 bar**（周末最多延迟 3-4 天；若源在周一盘中就列出周一 bar 则周一早上即可）；`as_of` = 昨天 → 若探测含今天的 bar 立即钉住（昨天已收盘），否则等今天收盘；`as_of` = 今天 → 永不在当天钉住；未来 `as_of` → 等到那天之后。**代价**：`as_of` = 最近交易日的钉住统一推迟到下一根日线 bar 出现，且需要「之后一次读 ∧ TTL 已过」（§2.8）；退市/停牌标的永不出现更晚 bar → 永不钉住（`pinned=false` 但数据可画、图上有标记，G3）。`complete_through` 是**日线**日期与 `period` 无关（A M2：若周/月请求用聚合 bar 的日期，进行中的周 bar 标成周五会在周三就骗过判据）。并发首解析：串行队列 + in-flight 去重使同键同一时刻只有一个任务；即便测试直接并发调用两次 `resolve`，第一个写成 `pinned=1` 后第二个的 `DO UPDATE … WHERE pinned=0` 是 no-op。
- **身份**：`(track_id, block_id, request_hash)`（in-flight 键是 `(track_id, block_id)`，D2，不是行身份）。改 `caption/overlays`、`view` 在 `line↔normalized↔bar` 间切换 → 不换行；改 `series/range/period/as_of/source`、`view` 与 `candles` 互换 → 换行，旧行留到 track 删除（G8）。
- **人/AI 同源的精确边界**：钉住行——任何两个读者任何时刻拿到同一字节（DB 层不可覆盖）；未钉住行——同一次读拿到同一行，两次读之间行可被后台刷新替换（`resolved_at` 变），无法取回旧版本（§11 R2-7：v2 的"同一份字节"对 live 行不成立，收窄而不是措辞）。
- **fork**：`routes/tracks.rs:2322` 的 fork 在事务里、block id 保留（F2.6）——同一事务里 `INSERT INTO report_series SELECT <new_track_id>, block_id, request_hash, status, reason, as_of, resolved_at, pinned, summary, data FROM report_series WHERE track_id = <source>`，**复制全部行**（不只 pinned：未钉住的 frozen 行也是人看到的图，复制后子 track 不会空一段再解析出修订价，§11 R2-8）。保证边界：钉住行 fork 后不变；未钉住行在两个 track 里各按 TTL 刷新，可能分叉。
- **track 删除**：级联，无需改 `routes/tracks.rs:4032-4037` 的显式清理列表。

**依据。** F4.2-F4.4（overlay 的读取与事件体量）、F4.15（FK 先例与 PRAGMA）、F2.6（fork 保留 id）、F7.6（sha256 已在仓内）。

### D4 读端水合

**问题。** `calm.report.read` 响应里 `resolved` 的形状、`resolve` 入参、谁能拿。

**裁决。**

- 入参：`{ with_markers?: bool, resolve?: { [block_id]: "full" | "none" } }`。默认每个 `chart.series` 块与 live `table` 块给 `summary`；`"full"` 附 `points` / table 全部行；`"none"` 跳过。未知 block_id 忽略。**没有 `"summary"` 值**：它是默认，不是选项。
- 响应：`blocks[i]` 从 `{id, kind, rev}` 变 `{id, kind, rev, resolved?}`。Rust 类型是枚举（m8）：
  ```rust
  enum Resolved { Pending { reason: Option<String> }, Unavailable { reason, resolved_at }, Ok { as_of, resolved_at, pinned, summary: Summary, points: Option<…> } }
  ```
  序列化摊平：
  ```jsonc
  { "status": "ok" | "pending" | "unavailable",
    "reason": "…",                     // unavailable（≤ 256 字符）；pending 时可选：路由预检未命中 / 作用域 None（D2 enqueue 步骤 2）
    "resolved_at": "2026-09-12T08:00:00Z",   // ok / unavailable
    "as_of": "2026-09-11", "pinned": false,  // ok；as_of = 请求截止日（live 行 = 解析时的昨天 UTC）
    "view": "normalized", "field": "close", "period": "day", "range": "1Y",
    "series": [
      { "asset": "US:NVDA", "currency": "USD", "status": "ok", "complete_through": "2026-09-11",
        "n": 251, "first": ["2025-09-11", 118.2], "last": ["2026-09-11", 176.9],
        "change_pct": 49.66, "high": 181.3, "low": 101.4,
        "points": [[ts_ms, v], …] },   // 仅 "full"
      { "asset": "HK:9988", "status": "unknown_asset", "reason": "…" } ] }
  ```
  `status/reason/as_of/resolved_at/pinned/series[].summary` 全部**直接来自那一行**（`summary` 列原样反序列化），read 不重算。
- live `table` 的 `resolved`：`{status, resolved_at, columns: n, rows: n, caption?}`，来源 `repo.overlays_for("track", track_id)` 按 `(plugin_id, kind)` 匹配（与 F6.4 同一规则，DB 读）。
- 摘要定义（`summarize`，写行时执行一次）：`n` = 点数（≥ 2，校验清单）；`first/last` = `[YYYY-MM-DD, value]`（candles 视图 value=close）；`change_pct = (last-first)/first*100` 两位小数，`first == 0` → null；`high/low` = value 极值（candles 用 high/low 列）；`complete_through` 原样带出。HTTP 路由（D5）返回**同一行的同一字节**。
- **读路径零插件调用、零 DB 写**：read 只做 `load_report_read_snapshot` + 每个 `chart.series` 块一次 `report_series` **按全主键 `(track_id, block_id, request_hash)`** 的查询（hash 由当前 payload 派生，D1；**默认摘要读只 SELECT `status, reason, as_of, resolved_at, pinned, summary`，不取 `data` 列**，`resolve: full` 才取 `data`——G8 不 GC，改过 50 次参数的块有 50 行 × ≤ 1 MiB 落在 CAS 必经的 read 路径上，§11 第 5 轮 A m1 / codex MINOR）+ 可选 `enqueue`（路由预检 `registry.get` / `running_plugin_ids().await` + 作用域 `plugin_scope_for_track`（一次 `track_get`，F4.18）+ 内存集合 + 可能建 lane）。无超时、无并发预算。`load_report_read_snapshot` 的错误仍是 internal（文档本身读不到，与今天一致）。
- **谁能拿 `resolved`**：Planner 与 Assistant 都拿。裁量按报告内容可见性：解析由内核固定身份在后台执行（先例 F4.10：内核调用不经 `PLUGIN_TOOL_ROLES`），工具被 D2 限定为 manifest 内只读工具，结果是存进 `report_series` 的报告内容，与 `text` 里的 fence 同一可见级。`PLUGIN_TOOL_ROLES = [Planner, Worker]`（F4.9）约束的是 **agent 以自己身份发起** tools/call；这里没有 agent 身份发起的调用。Assistant 能通过写块让内核调只读工具——这与它今天能写 live `table` 的 `source` 同级（写权已在 `[Planner, Assistant]`，`track_report_blocks.rs:121-296`），且它拿到的只是 Planner 也能直接调工具拿到的东西。副作用登记 G15：Assistant 可经 `unavailable.reason` 读到只读工具的错误文本（≤ 256 字符）。`taskDiagnostics` 的划线（F3.3）不动。
- token 体量估算不变：summary 每条 ≈ 50 token；full 1Y 日线 ≈ 1.5k token/条、candles ≈ 3.1k、5Y ≈ 7.5k/15.7k——`full` 只按块显式索取。

**依据。** F3.3（read 是 CAS 必经路径 → 纯 DB 读）、F3.5、F4.9/F4.10（角色门的适用边界）。

### D5 前端渲染

**裁决。**
- fe/：NEW `fe/web/src/features/report/series/public.tsx` + `series.module.css`，沿 F6.1 的 SVG 路线：`line`（多序列各一条 `<polyline>`，token 上色）、`normalized`（每条 rebase 到首点=100；**首点 ≤ 0 的序列不画线，图例处标 "cannot normalize (first value ≤ 0)"**，其余序列照画）、`bar`（单序列柱，多序列分组柱，≤8）、`candles`（把 `candles/public.tsx:89-190` 的绘制体抽成 `CandlesFigure({candles, overlays})` 共用）。区间切换不再客户端过滤；图上显示 `range`/`as_of`/币种/`pinned` 标记；frozen 且 `pinned=false` 时标 "not pinned — source data through <min complete_through>"。
- 数据：NEW 路由 `GET /api/tracks/{id}/report/series/{block_id}?rev=<n>&detail=full|summary`（默认 `full`；`routes/track_report_series.rs`），Principal 鉴权同其它 track 路由。服务端：读快照找块；`rev` 与当前块 `rev` 不等 → **409** `{current_rev}`；无块 → 404；算 hash → **按全主键读行**（`detail=summary` 不取 `data` 列，§11 第 5 轮 A m1）；无行/过期（`row_ttl`，D3）→ `enqueue` 并返回现状。返回 D4 的 `resolved`（`utoipa` schema → 两份 OpenAPI 重生成，F6.9）。**用 `rev` 不用 hash 绑定**：fe 在不安全上下文里没有 sha256（F6.10），而 `rev` 已在 `ReportBlock` 里且任何 payload 改动都会 bump（同内容不 bump，与 hash 不变一致）。
- fe 查询：`trackReportSeriesQueryOptions(trackId, blockId, rev)`，key `['track-report-series', trackId, blockId, rev]`；**`track.report_edited` 不失效 `['track-report-series', trackId]` 前缀**（§11 第 5 轮 A m6：前缀失效让每个块都重取默认 `full`（≤ 1 MiB/块），即使该块 `rev` 未变；事件不带块 id（F3.6；fe 侧 `plan((event) => …)` 只有 `track_id`，`invalidation-plan.ts:291-294`），plan 无法按块选，所以载体是「不失效前缀 + key 含 `rev`」：改了参数的块 `rev` 变 → 新 key → 自然重取，未改的块复用缓存）；series 查询设 `staleTime`（估值 5 min，S4.3）；响应 `pending` 时 `refetchInterval` 3s（2 分钟后退到 30s），非 pending 停止，`refetchInterval` 不受 `staleTime` 影响；`ApiError.failure.status === 409` 视为「等报告刷新」：不进错误态，渲染同 `pending` 的一行文字，`['track-report']` 失效后新 `rev` 的 query 自然替换（§11 第 4 轮 A m8；v6 不再由 `track.report_edited` 失效 series 前缀，但焦点 refetch 仍可能让旧 `rev` 的 query 先于报告刷新重取）；过期行由服务端刷新后要等下一次 fetch（焦点/失效，且在 `staleTime` 之外）才可见（G12）。**浏览器打开报告 = 一次读 = 一次 enqueue**：这就是 v3 里人这一侧的触发。
- 非 ok 态：`pending`（带 `reason` 时显示 reason）/`unavailable` 渲染 caption + 一行文字（复用 table 的 `LiveTableNotice` 形态，F6.3）；查询 loading 渲染 caption + "Loading …"。
- zod：`as_of` 查 `YYYY-MM-DD` 形状与历法（与内核 S1 一致）；未来日期可在编辑器里**提示**"will pin once the source publishes through this date"，不拒绝（与内核一致，D1）。
- legacy `web/`：不加渲染器（F6.8）；两份 OpenAPI 仍因新路由一起重生成。
- 不引入 Recharts/lightweight-charts。

### D6 安全

- `source` 只接受 `neige://plugin/<id>/<tool>`；内核从中只取 `plugin_id`/`tool` 做 `registry.get` 精确查找，**从不**构造 URL、从不拼名字反解；只调 manifest 内、`kind == None`、`readOnlyHint == true` 的工具；只走本地 `Stdio`/`Cli` 变体，`Http` 拒绝（D2 步骤 2-4；这是对 §11 B1/C1 的回答）。
- `series` 字符串经内核形状检查后作为 tool 参数原样交给插件，插件用 `parse_asset` 再判；内核不解释 venue。
- 解析以内核身份在后台执行；作用域 `plugin_scope_for_track` 与 agent 路由同一规则。
- 读路径：HTTP 路由走既有 Principal；MCP read 走 `resolve_report_for_caller`；两者都不调插件、不写 DB。
- 资源约束（每条指到执行它的机制）：8 条序列（`validate_chart_series`）；每插件串行（lane）；30s 超时（`tokio::time::timeout`）且超时清 responder（`ResponderSlot` guard）；2 MiB 是**接受上限**（步骤 6 校验，不是传输层内存上界——那是 #1634）；区间上界点数、`points.len() ≥ 2`、`ts_ms` 零点（步骤 6）；1 MiB 行、`reason` 256 字符（写入前截断）；in-flight 去重（键 `(track, block)` 不含 hash；检查与插入是锁内一次 `insert`）+ drain 准入 TTL（`row_ttl`：按行状态与 series 结果）；lane 通道 unbounded、排队 job 总数 ≤ in-flight 键数 = 被读过且 job 尚未结束的块数（执行中的 job 也持 guard）、`enqueue` 同步非阻塞；lane 只为路由命中且作用域非 `None` 的插件建（`Only(other)` 也进 lane、落永久行，G17；§11 第 5 轮 A m4），未命中不排队、不 spawn、不落行、零次插件调用；检查-重建-投递在 `lanes` 锁内；插件侧 `deadline_ms` 丢弃。
- 插件 `market.series` 只读，不写 overlay/KV；不新增 `neige.*` 回调，不扩权限模型；不新增 Event kind。

### D7 与 #1612 `layout` 的取舍

保留：`neige://plugin/<id>/<x>` 作为唯一外部引用语法；"配置声明式、无表达式求值"；unit/currency 不做隐式换算；"缺数据显式态、零不是缺"。
放弃：一个 kind 承载布局 + 多 item + 表 + 图；数据来自 overlay 行再做 join/annotations；Recharts 与颜色 `#RRGGBB` 进 payload；模板化的 selector/total/share 计算。
参照材料（`report-layout-contract.md`、`layout.rs`、`portfolio-template-review.md`）**不在基线树**（`find` 零命中，仓内 `layout.rs` 只有 `dedicated_codex/layout.rs`）；它们是 #1612 讨论的附件，本节只取其精神，不引用行号。

## 5. 切片表

| 片 | 内容 | 依赖 | 可独立合入 | 行为变化 | 估算行数 |
|---|---|---|---|---|---|
| S1 契约 | `kinds.rs` `KIND_CHART_SERIES` + `validate_chart_series`（纯函数：形状 + `as_of` 历法 + `1M+month` 组合拒绝）+ `MAX_CHART_SERIES`；`DATA_KINDS` 5 项；`kinds_tests.rs` 正反例；`contracts.rs` kinds_table 项 + F2.11/F2.12 文字；`fe/core/domain/report.ts` zod + `payloadSchemaFor`；`document/public.tsx` `case 'chart.series'` 占位；`mcp_track_report_blocks.rs` 加入口拒绝用例 | 无 | 是 | agent 可写 `chart.series`，read 无 `resolved`，fe 占位，web unsupported | ~580 |
| S2 解析任务 + 存储 + read 水合 | 迁移 `report_series`；`SeriesRequest`/`request_hash`（含 plugin_id, tool）/`summarize`/`yesterday_utc`；`SeriesResolver`（`(track, block)` in-flight 键 + `InflightGuard`、unbounded lane + 锁内检查-重建-投递、预检未命中 / 作用域 `None` 返回 `Miss(reason)`、读事务准入 + 出队时派生请求、`resolve`、请求带 `mode`、校验清单含窗口下界与按 `(mode, period)` 分支的「结束日 < `complete_through`」、写行、`row_ttl`（按 series 结果选 TTL）、准入与读端按全主键选行、默认摘要读不取 `data`、`sqlite_pool()` `None` 分支、锁内原子 `insert`、严格 `>` 钉住、可注入超时、failpoints）；`plugin_tool_entry` + 与 `plugin_tool_route` 的元测试；**`McpClient::call` 的 `ResponderSlot` guard + `pending_responders()`**；`calm.report.read` `resolve` 入参 + `resolved`（chart.series 与 live table，`Pending { reason }`）+ enqueue；fork 复制全部行；集成测试用假插件（`boot_plugin_host`）覆盖 seq 3/3a/3a′/3b/3c/3d′/3e/3f/3h/4/4a/4b/6/7a/7b/7c/7d/8/9/12 | S1 | 是 | Planner/Assistant read 到摘要；插件缺席时 `pending`→`unavailable`；所有内核→插件调用超时后不再留 responder | ~1040 |
| S3 market 插件 | `market.series` tool（manifest、`tools_call_reply` 分支、腾讯 ifzq + Binance klines 源、新浪兜底、内存缓存、按 `[start, as_of]` 窗口取数 + 按日期分页拼接、`complete_through` 探测先于取数（n=3）、缓存页带探测值且键含 UTC 日期、同源探测/取窗（切兜底重新探测）、周/月从日线聚合、纳入规则按 `(mode, period, venue)` 分支（live 股票日线 `≤ as_of`；CRYPTO / 周月 / frozen 严格 `< complete_through`）、U8/U9 spike、深度不足 / 近端缺口 `unavailable`、`deadline_ms` 出队丢弃、`unknown_asset`、<2 点报 `unavailable`、`ts_ms` 折到 UTC 零点）、README、fixture server 测试 | S1（只共享 wire 形状；与 S2 并行） | 是 | `plugin.dev-neige-market_market.series` 对 agent 可用；S2 合入后图有数据 | ~1230 |
| S4 路由 + fe 渲染 | `routes/track_report_series.rs`（rev 绑定、409、enqueue）、两份 OpenAPI 重生成、`queries.ts` 查询（`staleTime`、不随 `track.report_edited` 前缀失效）+ pending 轮询 + 409 等待态、`features/report/series/`、`CandlesFigure` 抽取、未钉住标记、browser 测试 | S2 | 是 | 人看到图；S3 未合时看到 `unavailable` 文案 | ~800 |

顺序：S1 → (S2 ∥ S3) → S4。总规模 ≈ 3650（v5 ≈ 3530、v4 ≈ 3450、v3 ≈ 3250、v2 ≈ 3300、v1 ≈ 3900）：S2 +~40（`row_ttl`、全主键选行、原子 `insert`、`mode`、`sqlite_pool()` 分支）；S3 +~80（`mode` 分支、venue 判断、缓存日期键、同源重探测、live 周/月负例）；S4 ±0。S2 与 S3 的接缝是 D2 的请求/回复 JSON，两边共用 `crates/calm-server/tests/fixtures/market_series_reply.json`。**S2 先于 S3 合入时**：已装的 market 插件 manifest 只暴露 `market.quote` / `market.holdings.set` / `market.holdings.list`（`plugins/market/manifest.json:10-62`），`plugin_tool_entry` 回 `NotExposed` → 读端 `pending, reason: "plugin dev-neige-market does not expose market.series"`，不落行、**零次** tools/call（S3 合入并重启插件后的下一次读就排队；v4 写成 6h 的 `unavailable` 行，§11 第 4 轮 A M2）——到不了插件的 `unknown tool` 分支（`main.rs:2572`，F4.13；v3 写成 `unknown tool` 是错的，§11 R3-MINOR-3）。S2 合入后 #1634 只剩传输层字节上界一项（F4.17）。

### 5.1 实现约束（编号清单，实现简报直接引用）

每条约束都指到它的落点（D / § / seq / A 编号）；简报按「S<n>.<k>」引用。估值类常量在各片清单里标「估值、评审可调；改数不改结构」。

**S1 契约**

- S1.1 `validate_chart_series` 三段都是纯函数（无时钟，F1.12）：形状；`as_of` 历法（闰年 + 每月天数）；`(range, period)` 组合。（D1、seq 2n/7、A2）
- S1.2 组合的结构下限（窗口 = `RANGE_DAYS + 1` 个历日；周 = `⌊(天数 − 6) / 7⌋`；月 = 最坏对齐下的完整自然月数；数据依赖的「最后一个周期要等更晚 bar」不算在内；周/月两列由修订者对 2024-01-01 起四年内每个 `as_of` 穷举得到）：

  | range（历日） | day | week | month |
  |---|---|---|---|
  | 1M（32） | ≥ 22 个工作日减假期 | 3 | **0**（拒绝） |
  | 3M（93） | 远大于 2 | 12 | 2 |
  | 6M（184） | 远大于 2 | 25 | 5 |
  | 1Y（367） | 远大于 2 | 51 | 11 |
  | 2Y（732） | 远大于 2 | 103 | 23 |
  | 5Y（1828） | 远大于 2 | 260 | 59 |

  只有 `1M + month` 结构上装不下 2 点 → S1 拒绝（D1）；其它组合的「< 2 点」是数据问题（停牌、新上市），归 S3 的 `unavailable, reason:"no data in range"`。（A2）
- S1.3 `DATA_KINDS` 5 项、`KIND_CHART_SERIES`、`MAX_CHART_SERIES = 8`；`contracts.rs::kinds_table` 一项 + F2.11 / F2.12 两段手写文字；`document/public.tsx` 占位 case；web/ 不加。（D1、A1、A3）
- S1.4 fe zod `chartSeriesPayloadSchema` 与内核同一历法规则；未来日期提示不拒绝。（D5）

**S2 解析任务 + 存储 + read 水合**

- S2.1 in-flight：`Key = (TrackId, BlockId)`，不含 hash；检查与插入是锁内一次 `HashSet::insert`（原子），只有插入成功者构造 `InflightGuard`；预检未命中 → guard drop 释放键、返回 `Miss(reason)`。`inflight.len()` = 排队 job 数 + 执行中 job 数（每 lane ≤ 1）。（D2 enqueue 1/3、A9、A9g、A9i）
- S2.2 `inflight` 与 `lanes` 两把 `std::sync::Mutex` 一律 `lock().unwrap_or_else(PoisonError::into_inner)`；检查-重建-投递在 `lanes` 锁内、无 `.await`；`rebuild_lane(&mut MutexGuard<'_, HashMap<…>>)`；lane 通道 `mpsc::unbounded_channel`。（D2 enqueue 3、A9d、A9f）
- S2.3 准入用读事务：`repo.sqlite_pool()` 为 `None` → 丢弃 + warn、不落行；`Some(pool)` → `pool.begin()`（DEFERRED），`report_blocks_snapshot_tx` 重读块，判定后 rollback、不跨插件调用持有；只有步骤 7 写行用 `write_in_tx_typed`。（D2 步骤 1/7、F7.8）
- S2.4 出队时由**当前** payload 派生 `SeriesRequest`（含 `mode`）与 hash；`(plugin_id, tool)` 不属本 lane → 丢弃；`start = as_of − RANGE_DAYS` 在此算，`from_ymd_opt` 失败 → 丢弃 + warn。（D2 步骤 1、seq 3a′、A9g）
- S2.5 行查询一律按全主键 `(track_id, block_id, request_hash)`：准入先派生 hash 再精确选行，**不按 `(track, block)` 任取**；读端（`calm.report.read` 与 HTTP 路由）同样；用例「h1 钉住行保留 + h2 无行 → 解析 h2」。（D2 步骤 1、D4、D5、A9h）
- S2.6 默认摘要读只 SELECT `status, reason, as_of, resolved_at, pinned, summary`，不取 `data` 列；`full` 才取 `data`。（D4、D5）
- S2.7 `row_ttl(row)` 一个纯函数、读端与准入共用：`status = ok` ∧ `summary.series` 全部 `ok` → `SERIES_TTL`；否则（`unavailable` 行、任一 series 非 `ok` 的 `ok` 行）→ `SERIES_UNAVAILABLE_TTL`；`pinned` 行不过期。（D3、seq 3d′、A9b）
- S2.8 请求 `{series, fields, period, mode, start, as_of, deadline_ms}`：`mode` 由 `as_of` 的有无派生（存在 = `frozen`、缺席 = `live`）、不入指纹；live `as_of = yesterday_utc(now)`；`request_hash = sha256(canonical_json({plugin_id, tool, series, fields, range, period, as_of}))`。（D1、D2 请求窗口/形状、A8）
- S2.9 校验清单按 `(mode, period)` 分支：`(frozen ∨ period ≠ day)` → 每点周期结束日 < `complete_through`；`live ∧ day` → 不查这一条。两分支都查：每点日期 ∈ `[start, as_of]`、周期结束日 ≤ `as_of`、week 周一 / month 1 日、`ts_ms` 零点、严格升序、有限数、每点长度、`≥ 2` 点、`complete_through` 存在且 `YYYY-MM-DD`、点数 ≤ `max_points`、一一对应、2 MiB 接受上限、1 MiB 行。（D2 步骤 6、seq 3e、A12）
- S2.10 钉住判据只对 frozen：全部 `ok` ∧ 末点 ≤ `as_of` ∧ 每条 `complete_through > as_of`（严格）；写入 `INSERT … ON CONFLICT DO UPDATE … WHERE pinned = 0`；live 行永远 `pinned = 0`。（D3、seq 7a/7b/7d、A7/A7b）
- S2.11 否定态两类：预检未命中（`NotInstalled / NotRunning / NotExposed`）/ 作用域 `None` / `connector_client` `None` / 出队时再遇预检未命中 → **不落行**、读端 `pending, reason`；`Only(other)` / `ForgeAction` / 非只读 / `Http` → **永久 `unavailable` 行**、零调用。`Resolved::Pending { reason: Option<String> }`。（D2 enqueue 2、resolve 2-4、seq 3b/3f、A5、A9e、A11）
- S2.12 `McpClient::call` 的 `ResponderSlot` guard + `#[cfg(test)] pending_responders()`；`SERIES_RESOLVE_TIMEOUT` 是字段可注入；`now: fn() -> i64` 注入；failpoints `fail_write_once / panic_drain_once / hold_in_rebuild / hold_in_precheck`；`#[cfg(test)] lanes_try_lock()`；`new_unstarted()` 记录器。（D2 步骤 5 与 seam、A5/A5b/A9c/A9d/A9f/A9i/A10/A10b）
- S2.13 `reason` 写入前截到 256 字符；`summary` 写入前由 `summarize` 算好；`unavailable` 行 `summary` NULL；fork 事务里 `INSERT … SELECT` 复制全部行；FK `ON DELETE CASCADE`。（D3、seq 9/12、A13、A20）
- S2.14 `plugin_tool_entry(registry, running_ids, plugin_id, tool) -> NotInstalled | NotRunning | NotExposed | Found(ExposedTool)`，`registry.get` 精确查找、不拼名字反解；与 `plugin_tool_route` 的集合相等元测试。（D2 步骤 2、A11、A19）
- S2.15 常量估值（评审可调；改数不改结构）：`SERIES_RESOLVE_TIMEOUT = 30s`、`SERIES_TTL = 6h`、`SERIES_UNAVAILABLE_TTL = 2 min`、`MAX_SERIES_REPLY_BYTES = 2 MiB`、`MAX_SERIES_ROW_BYTES = 1 MiB`、`reason` 256 字符。（原 §9 U2）
- S2.16 迁移号最后定（F7.1）；`SYNC_EVENT_VERSION` / `REST_API_VERSION` / `WEB_COMPAT_VERSION` / `SCHEMA_VERSION` 都不 bump（F7.2-F7.4）；两份 OpenAPI 在 S4 重生成。

**S3 market 插件**

- S3.1 实现前先做两个 spike 并把结果写回本文档 §9：U8「ifzq 带 start/end 且窗口超约 640 根时截哪一端」（fixture 照实模拟）；U9「ifzq / Sina 美股日线是否吸收盘后成交」——**若吸收，US venue 回退严格规则**（插件侧一行，与 CRYPTO 同臂）。（§9、§2.5 收盘时刻表、A14）
- S3.2 每条 series 的执行顺序：1 探测 → 2 取窗口（含全部分页）→ 4 深度 / 近端检查 → 3 聚合与纳入；探测最先、n=3、至少一根非基准行否则 `unavailable, reason:"probe returned no recent bar"`；`complete_through` = 探测时刻所见最新**日线**日期，与 `period` 无关；**任何 mode 都探测并回传**。（§2.5 约束 1、seq 3i/3k、A14 `probe_precedes_window_fetch`）
- S3.3 同一条 series 的探测与取窗同源；切兜底源（ifzq → Sina）必须对新源重新探测再取窗；缓存页的 `observed_complete_through` 按源记。（§2.5 约束 2、A14 `fallback_source_reprobes`）
- S3.4 内存缓存页键含取数时刻的 UTC 日期，只命中 `fetched_on == 今天 UTC` 的页（永不跨 UTC 午夜复用，对全部 venue 与 mode）；用到缓存页时回复 `complete_through` = min(本次探测, 各页 `observed_complete_through`)。（§2.5 约束 2、A14 `cache_page_never_crosses_utc_midnight`）
- S3.5 纳入规则按 `(mode, period, venue)`：`live ∧ day ∧ venue ∉ {CRYPTO}` → `start ≤ 日期 ≤ as_of`；其它（frozen、周/月、CRYPTO 任何 mode）→ `period_start ≥ start ∧ period_end ≤ as_of ∧ period_end < complete_through`；venue 由 `parse_asset` 判（F5.1）；`CN:` 与未知 venue 落 `unknown_asset`。（§2.5 约束 3、seq 3j/3j′/3l、A14）
- S3.6 period_end 语义：截止比较用周期结束日（day = bar 日期、week = ISO 周日、month = 月末），不是 `ts_ms`；live 周/月负例（周中请求当前周不出；周一 00:30 缺周五 bar 时上周不出）必须有 fixture。（§2.5 约束 3、seq 3j′、A14）
- S3.7 深度 / 近端：最早 bar > `start + SERIES_FETCH_MARGIN_DAYS` → `unavailable, reason:"lookback exceeds source depth"`；窗口内最晚日线 bar < `as_of − SERIES_FETCH_MARGIN_DAYS` → `unavailable, reason:"no data near cutoff"`；两者在聚合与纳入之前。`SERIES_FETCH_MARGIN_DAYS = 14`（估值，评审可调）。（§2.5 约束 4、seq 3i′、A14）
- S3.8 wire：请求 `{series, fields, period, mode, start, as_of, deadline_ms}`；出队时 `deadline_ms` 已过 → 不打网络、`tool_error("deadline exceeded")`；回复 `structuredContent.series[]` 每条 `{asset, currency?, status, complete_through?, points?, reason?}`；`asset` 逐项回显请求字符串；<2 点自己报 `unavailable, reason:"no data in range"`；`ts_ms` 折到 UTC 零点；周/月 `ts_ms` = 周期起始日；ifzq `qfqday` / `day` 两键、列序 o,c,h,l,v 重排、按日期过滤（U6 基准行）；ifzq 必须 start+end 都给，Binance `startTime` / `endTime`；单次约 600 根分页、按日期去重。（D2 请求/回复形状、§2.5 约束 2、seq 3d/3g、A14/A14b）
- S3.9 manifest `exposes_tools` 加 `market.series`（`readOnlyHint: true, openWorldHint: true`）；只读、不写 overlay / KV、不新增 `neige.*` 回调；README 与 fixture server 测试。（D2、D6、F5.6）

**S4 路由 + fe 渲染**

- S4.1 路由 `GET /api/tracks/{id}/report/series/{block_id}?rev=<n>&detail=full|summary`（默认 `full`）：`rev` 不等 → 409 `{current_rev}`；无块 → 404；算 hash → 按全主键读行；`detail=summary` 不取 `data` 列；无行 / 过期（`row_ttl`）→ `enqueue` 并返回现状；Principal 鉴权同其它 track 路由；`utoipa` schema → 两份 OpenAPI 重生成。（D5、seq 5、A16、A17）
- S4.2 fe：`ApiError.failure.status === 409` 渲染等待态、不进错误态、不重试，等 `['track-report']` 失效后新 `rev` 的 query 自然替换。（D5、A16b）
- S4.3 fe 查询：key `['track-report-series', trackId, blockId, rev]`；**`track.report_edited` 不失效 `['track-report-series', trackId]` 前缀**（事件无块 id，F3.6）；series 查询 `staleTime`（估值 5 min）；`pending` 时 `refetchInterval` 3s、2 分钟后 30s（估值，原 §9 U5），非 pending 停止。（D5、A21、G12）
- S4.4 fe 渲染：`pending` 带 `reason` 时显示；`unavailable` 显示 reason；未钉住 frozen 标 "not pinned — source data through <min complete_through>"；`normalized` 首点 ≤ 0 标 "cannot normalize (first value ≤ 0)"；token 上色、不含字面颜色；`LiveTableNotice` 形态复用。（D5、seq 5a、A15）
- S4.5 `CandlesFigure` 从 `candles/public.tsx:89-190` 抽出共用；不引入 Recharts / lightweight-charts；web/ 不加渲染器。（D5、F6.8）

## 6. 验收场景与 must-red 变异

每条变异**单跑**必转红。

| # | 场景 | 断言 | must-red 变异（改哪一行 → 哪条测试转红） |
|---|---|---|---|
| A1 | `upsert{kind:"chart.series"}` 合法 payload 落盘为 canonical fence | read `text` 含 fence，`parse_fence` 回同 payload | `validate_chart_series` 把 `series` 必填改成可选 → `kinds_tests::chart_series_payload_valid_and_invalid` 的 "series: required" 断言红 |
| A2 | 经 `calm.report.commit` 写无 venue 的 `series:["NVDA"]` / `as_of:"2026/09/10"`（形状错）/ `as_of:"2026-02-30"`（历法错）/ `{range:"1M", period:"month"}`（结构性不足 2 点）；**`as_of:"2099-01-01"` 被接受** | 前四者 `-32602`，docRev 不变，事件零；后者 200 落盘 | **直接测入口**：`validate_chart_series` 删掉 venue 正则 → `mcp_track_report_blocks::commit_rejects_chart_series_without_venue`（S1 新增，走真实 MCP 入口）红；删掉历法检查 → `commit_rejects_non_calendar_as_of` 红（`2026-02-30` 落盘）；删掉组合检查 → `commit_rejects_month_period_in_one_month_range` 红；给 `validate_chart_series` 加一条硬编码的 `year > 2026 → Err`（模拟任何"与今天比"的检查）→ `commit_accepts_future_as_of` 红。不再用"跳过 `render_data_block`"做变异：F2.2 的 op 层复核会让那种变异保持绿 |
| A3 | kinds 表、upsert enum、commit enum 三者含 `chart.series` 且相等 | `contracts.rs:695-711` | `block_kind_enum()` 硬编码四项 → 既有测试红 |
| A4 | 行存在时 read 默认给 summary，`n/first/last/change_pct/high/low/as_of/complete_through` 与 fixture 一致 | 集成测试比对 fixture 期望 | `summarize` 里 `change_pct` 用 `(last-first)/last` → `read_hydrates_chart_series_summary_from_row` 红 |
| A5 | 超时 / `isError` / 回复非 object → 行 `unavailable`（`resolved_at` 距今 < 2 min）、reason 各不同；插件未安装 / 未运行 / 未暴露 → read 返回 `pending` 带对应 reason、`report_series` 零行、`new_unstarted()` 记录器零 job；read 仍 200 | 前三条直接调 `resolve`，后三条走真实 read；超时用例把 `SeriesResolver` 的 `resolve_timeout` 注入为 50ms（不是常量 30s，§11 A m5） | `resolve` 删掉 `tokio::time::timeout` 包裹 → `resolve_marks_a_hung_plugin_unavailable`（永不回复的假插件；测试自身 5s 上限）红（挂死） |
| A5b | **超时不泄漏 responder**：永不回复的假插件，`timeout(1s, client.call(…))` 两次 | 两次都 `Err(Elapsed)`；`client.pending_responders() == 0` | `ResponderSlot` 的 `Drop` 改成空实现 → `timed_out_calls_leave_no_responder`（`plugin_host/mcp.rs` 单元测试）红（读到 2） |
| A6 | `resolve:{b:"full"}` 才有 `points`；默认无 | JSON 断言 | read 无条件塞 `points` → `read_full_is_opt_in_per_block` 红 |
| A7 | frozen：直接并发调两次 `resolve`（假插件两次回不同数据、都满足钉住条件），再改 fixture 并第三次调 | 行 = 第一次完成者的数据；`pinned=true`；第三次后行不变 | 写入去掉 `WHERE report_series.pinned = 0` → `frozen_row_is_pinned_by_first_complete_resolution_and_never_overwritten` 红 |
| A7b | frozen 回复含 `unknown_asset` / 某条 `complete_through` **不晚于**请求 `as_of`（fixture 两例：`as_of` 周日、`complete_through` 周五、points 止于周四；`as_of` 周五、`complete_through` 周五、points 止于周四——周五的 bar 没有更晚 bar 证明收盘，按纳入规则不在回复里） | 行 `ok, pinned=false`；TTL 过期后 enqueue 非 no-op；fixture 把 `complete_through` 推到周一并补上周五的 bar 后再 resolve → `pinned=true` 且数据止于周五 | 钉住条件 `complete_through > as_of` 改成 `>=` → `frozen_reply_at_cutoff_is_not_pinned`（周五/周五例）红（钉住了缺周五 bar 的图）；整项删掉 → `frozen_reply_behind_cutoff_is_not_pinned`（周日/周五例）红 |
| A8 | live：行过期后 read 返回旧行并 enqueue；注入 `now` 前进一天、fixture 多给一根 bar（`complete_through` 不必推进：`live ∧ day` 只查 ≤ `as_of`）后 drain，`summary.last` 日期前进、行 `as_of` 前进到新的 `yesterday_utc(now)`；docRev 不变；请求里 `as_of == yesterday_utc(now)`、`start == as_of − 366d` | 用注入的 `now` 把 `resolved_at` 设成过期；断言假插件收到的请求 `start`/`as_of` 与新行 `as_of` | read 对过期行不 enqueue → `stale_live_row_is_served_and_refreshed` 红；内核对 live 请求不填 `as_of` → `live_request_carries_yesterday_utc_cutoff` 红；写行时 `as_of` 列不更新（`DO UPDATE` 漏掉 `as_of`）→ `refreshed_live_row_advances_as_of` 红；内核对 `live ∧ day` 也执行「结束日 < `complete_through`」→ `live_daily_row_includes_yesterday` 红（fixture `complete_through` = 昨天时新 bar 被判 malformed、行 `unavailable`） |
| A9 | in-flight 去重：同一块连续 enqueue 十次，假插件只收到一次 tools/call | 计数 | `enqueue` 不查 `inflight` → `refresh_is_deduplicated_per_key` 红 |
| A9g | **改写合并**：lane 被永不回复的假插件占住（注入超时 5s）；同一块改写 5 次（`series` 各不同）、每次改写后 read 一次 | `inflight.len() == 1`；lane 排队 job 数 == 1；超时后出队的 job 让假插件收到第 5 版 `series`；行的 `request_hash` = 第 5 版 hash | 键改回 `(track, block, hash)` → `rewritten_block_keeps_one_queued_job` 红（排队 5 个）；出队时用入队时的 payload 派生请求 → `dequeued_job_resolves_current_payload` 红（收到第 1 版） |
| A9h | **准入与读端按当前 hash 选行**：h1 行已钉住；块改成 h2（`series` 变）；read → `Queued`；执行 job；再 read | 假插件收到一次 tools/call（h2 的 `series`）；表里 h1 行不变 + 新增 h2 行；第二次 read 返回 h2 行 | 准入按 `(track, block)` 任取一行 → `admission_selects_current_hash_row` 红（读到 h1 钉住行 → 丢弃 → 计数 0、read 仍 `pending`）；读端按 `(track, block)` 任取 → 同一测试的「返回 h2 行」断言红（返回 h1） |
| A9i | **in-flight 插入原子**：`failpoints.hold_in_precheck` 把读者 A 按在预检里（`insert` 已成功）；读者 B 对同键 `enqueue` | B 得 `InFlight`；释放 A → A 得 `Queued`；`inflight.len() == 1`；lane 排队 job == 1 | 改回「`contains` → 预检 → `insert`」三步 → `concurrent_enqueue_admits_exactly_one` 红（B 也 `Queued`，排队 2）——failpoint 把 A 按在空隙里，确定性 |
| A9b | **drain 准入执行 TTL（两种）**：(i) 先让一次 resolve 写下新鲜 `ok` 行，再对同一块 enqueue（模拟迟到读者的旧观察）；(ii) 注入 `now` 写下 1 min 前的 `unavailable` 行 → enqueue；再把它改成 3 min 前 → enqueue；(iii) 注入 `now` 写下 3 min 前的 `ok` 行、其中一条 series `unavailable`（瞬时）→ enqueue；同样 3 min 前但全部 series `ok` 的 `ok` 行 → enqueue | (i) 假插件 tools/call 计数仍为 1；(ii) 前者丢弃（计数不变）、后者重投（计数 +1）；(iii) 前者重投（计数 +1）且新行保留成功那条的数据、后者丢弃 | `resolve` 步骤 1 删掉 `resolved_at ≥ now − TTL → 丢弃` → `fresh_row_is_not_re_resolved_by_late_reader` 红（计数 2）；`unavailable` 行也用 `SERIES_TTL` → `unavailable_row_is_retried_after_two_minutes` 红（3 min 例不重投）；`row_ttl` 只看行 `status` 不看 `summary.series` → `partial_ok_row_is_retried_after_two_minutes` 红（(iii) 前者不重投） |
| A9c | **失败路径不 fail-locked**：`failpoints.fail_write_once` 使步骤 7 返回 Err；再对同键 enqueue | 假插件 tools/call 计数 == 2 | `InflightGuard` 改成只在步骤 7 成功后显式 `remove` → `failed_resolve_releases_inflight_key` 红（计数 1） |
| A9d | **lane 自愈**：`failpoints.panic_drain_once` 让 drain 任务 panic；再 enqueue | 假插件收到 tools/call | `enqueue` 删掉 `is_finished()/send 失败 → 重建 lane` 分支 → `panicked_lane_is_rebuilt` 红 |
| A9e | **预检未命中不落行、不冻结**：插件已安装但停止；read 该块 | `pending`，`reason` 含 `is not running`；`report_series` 零行；`new_unstarted()` 记录器记到一次 `Miss`、零个 job；**启动插件**；再 read → 记录器记到 `Queued`；手动执行 job → 行 `ok`，假插件 tools/call 计数 1 | 预检未命中改回写 `unavailable` 行 → `precheck_miss_never_lands_a_row` 红（第二次 read 读到 2 min 内的新鲜 `unavailable` 行、不排队，计数 0） |
| A9f | **重建在锁内**：`failpoints.hold_in_rebuild`（`rebuild_lane` 在 spawn 之前 `rebuild_entered += 1` 并阻塞等待测试释放）；`panic_drain_once` 让 lane 死掉；任务 A（多线程 runtime 上 `tokio::spawn`，等待是阻塞的、占一个 worker）`enqueue`；测试等到 `rebuild_entered == 1`（必然发生的事件，不是计时）后调用 `#[cfg(test)] lanes_try_lock()` | `try_lock()` 返回 `WouldBlock`（A 在重建期间持有 `lanes` 锁）；释放后 join A，`drain_spawned == 1`；再用 `tokio::sync::Barrier(8)` 让 8 路 `enqueue` 同插件不同块同时起跑，断言 `drain_spawned` 仍为 1 且假插件按序收到 8 次调用 | 把重建移到锁外（`lock()` 判断后释放锁再 spawn+insert）→ `rebuild_holds_the_lanes_lock` 红：`try_lock()` 成功（确定性，不靠概率与轮数；v4 的「200 轮内必现」是过度声称，§11 第 4 轮 codex R4-MINOR）。`&mut MutexGuard` 签名只防「拿不到锁就调用」，不证明持的是 `lanes` 这把锁——证明就是这条 `try_lock` 断言 |
| A10 | **读路径零插件调用**：`SeriesResolver::new_unstarted()` 下对无行的块 read → `pending`，假插件 tools/call 计数为 0 | 计数 | read 里改成内联调用 resolver → `read_never_calls_the_plugin` 红 |
| A10b | **写路径不触发**：`SeriesResolver::new_unstarted()` 记录器下 commit 一个 `chart.series` 块，**不 read** | 记录器 `enqueue` 调用数 == 0；`report_series` 无行；假插件计数 0（不是「等 2s」的计时型负测试，§11 A m5） | 在 `write.rs` 提交后加 `enqueue` → `write_does_not_trigger_resolution` 红（调用数 1） |
| A11 | 3f：`source` 指向 `market.holdings.set`（`readOnlyHint:false`）/ 不在 manifest 的名字 / ForgeAction 工具 / `neige://plugin/aa_b/c`（fixture 注册插件 `aa` 暴露工具 `b_c`；`a` 不是合法 id，`manifest.rs:2305` `len < 2` 拒绝，§11 R3-MINOR-2） | 前两例（`readOnlyHint:false` / ForgeAction）：行 `unavailable`，假插件计数 0；「不在 manifest 的名字」= `NotExposed` 与 `aa_b` = `NotInstalled`：read 返回 `pending`、零行，最后一例 reason 是 `plugin aa_b is not installed`（原字符串），且插件 `aa` 收到零次 tools/call | `resolve` 删掉 `readOnlyHint` 检查 → `resolve_refuses_non_read_only_tools` 红；`plugin_tool_entry` 改成拼 `plugin.{id}_{tool}` 再 `plugin_tool_route` → `underscore_plugin_id_never_routes` 红（`plugin.aa_b_c` 反解命中 `aa`/`b_c`） |
| A12 | 校验清单：回复 9 条 / 时间戳降序 / `ts_ms` 非零点 / bar > as_of / **bar < start** / **week `ts_ms` 非周一** / **week 周期结束日 > as_of** / **month 非 1 日** / **frozen 日线末点日期 == `complete_through`** / **week 周期结束日 ≤ as_of 但 ≥ `complete_through`** / **`live ∧ day` 末点日期 == `complete_through`（正例，接受）** / **`live ∧ week` 周期结束日 == `complete_through`（拒绝）** / NaN / 点数超上界 / `ok` 配 1 点 / `ok` 缺 `complete_through` 各一条 | 行 `unavailable, reason` 各不同 | 删掉"`points.len() >= 2`"检查 → `ok_series_with_one_point_is_malformed` 红；删掉"严格升序"检查 → `reply_with_descending_timestamps_is_unavailable` 红；删掉"周期结束日 ≤ as_of" → `partial_week_is_rejected_at_the_boundary` 红；把"结束日 < `complete_through`"改回 v4 的"`complete_through` ≥ 末点" → `bar_at_complete_through_is_rejected` 红；对 `live ∧ day` 也执行「< `complete_through`」→ `live_daily_bar_at_complete_through_is_accepted` 红；对 `live ∧ week` 不执行 → `live_week_at_complete_through_is_rejected` 红 |
| A13 | 删 track 后迟到的 `resolve` 写入 | 无行；任务不 panic；in-flight 键已释放 | 去掉 FK（改成无约束表）→ `late_resolution_after_track_delete_leaves_no_orphan` 红 |
| A14 | 插件 `market.series` 对 fixture 源返回升序、`ts_ms` 为 UTC 零点、窗口 `[start, as_of]`（fixture 探测回到更晚的 bar 时含 `as_of` 当日 bar）、**旧 `as_of`（两年前）窗口非空**、**> 640 根按日期分页拼接无重复**、`complete_through` = **先于**窗口请求的探测所见最新日线日期（与 `period` 无关；fixture 记录请求顺序）、**fixture 在第一次请求后推进一天（seq 3k）时，回复要么 `complete_through` = 09-13 且不含 09-13，要么 `complete_through` = 09-14 且 09-13 = `v_close`，永远不是 `v_partial`**、**探测 n=3 只回基准行 → 该条 `unavailable`**、**week / month 只含结束日 ≤ `as_of` ∧ < `complete_through` 的周期（请求日在周中时当前周不输出，fixture 推进到下周一后输出）**、**最早 bar 晚于 `start + 14d` → `lookback exceeds source depth`**、**窗口内最晚 bar 早于 `as_of − 14d` → `no data near cutoff`**、**`live ∧ day`：股票三 venue 的回复含昨天的 bar（fixture 无今天 bar），`CRYPTO` 同 fixture 不含（seq 3l）；同 fixture 下 `frozen` 四条都不含**、**`live ∧ week` 周一 00:30 缺周五 bar → 上周不出（seq 3j′）**、**缓存页跨 UTC 午夜不复用（fixture 时钟推过零点后同窗口再次打源）**、**ifzq 取窗失败切 Sina → 源请求序列里 Sina 探测先于 Sina 取窗**、未知标的 `unknown_asset`、区间内 <2 点报 `unavailable`、腾讯 `qfqday`/`day` 两键、列序重排 | S3 进程级测试 | 截断条件 `<= as_of` 改 `<` → `series_as_of_includes_that_days_bar` 红；窗口取数改回「最新 N 根再过滤」→ `old_as_of_window_is_non_empty` 红（0 点）；`complete_through` 改成过滤后的末点 → `complete_through_is_unfiltered_latest` 红；聚合时不剔除未完成周 → `aggregated_week_never_partial` 红；探测移到取数之后 → `probe_precedes_window_fetch` 红（回复 `complete_through` = 09-14 而 09-13 = `v_partial`）；聚合只按历法判完整 → `period_needs_a_later_daily_bar` 红；删掉近端检查 → `truncated_near_end_is_unavailable` 红；插件对 `CRYPTO` 也放宽 → `crypto_live_daily_stays_strict` 红；对 `live ∧ week` 放宽 → `live_week_needs_a_later_daily_bar` 红；对 `frozen` 也放宽 → `frozen_daily_needs_a_later_bar` 红（3k 的构造钉住 `v_partial`）；周线截止比较改用 `ts_ms` → `period_end_not_period_start_is_compared` 红（周三 live 周线放进半根周 K）；缓存键去掉日期 → `cache_page_never_crosses_utc_midnight` 红；切源不重探测 → `fallback_source_reprobes` 红 |
| A14b | 插件 `deadline_ms` 已过的请求 | 回 `isError`，fixture 源 HTTP 计数 0 | 删掉出队 deadline 检查 → `expired_request_does_not_hit_network` 红（计数 1） |
| A15 | fe：`ok` 画出与序列数相同的 `<polyline>`；`normalized` 首点=100、首点 ≤ 0 的序列标不可归一化；`pending`/`unavailable` 文案；未钉住 frozen 标 `complete_through`；不含字面颜色 | `series/public.test.tsx` | normalized 除以 `last` 而非 `first` → `rebases every series to 100 at its first point` 红 |
| A16 | HTTP：`?rev=` 不等于当前块 rev → 409 | 路由测试 | 路由忽略 `rev` → `series_route_rejects_stale_rev` 红 |
| A16b | fe：series 查询收到 409 | 渲染等待态文案、无错误态、不重试；`['track-report']` 失效后随新 `rev` 重取 | 把 409 当错误渲染 → `stale rev 409 is a wait, not an error` 红 |
| A17 | 两份 OpenAPI 与生成器零漂移 | CI 既有 drift 门禁 | 只重生成一份 → 另一条 CI 红 |
| A18 | 术语棘轮：本文档与新代码不引入退役词 | `gate-1316-terminology-ratchet.sh` | — |
| A19 | `plugin_tool_entry` 与 `plugin_tool_route` 集合相等：fixture registry 的每个 `(id, tool)` × running/not running，加不存在的 `(id, tool)` | `entry is Found(e)` ⇔ `route == Ok(Some((id, tool, e.kind)))` | `plugin_tool_entry` 不查 `running_ids` → `tool_entry_matches_tool_route` 红 |
| A20 | `reason` 截断：假插件回 `isError` 文本 10k 字符 | 行 `reason.chars().count() == 256` | 写入前不截断 → `reason_is_capped` 红 |
| A21 | fe：`track.report_edited` 事件后未改 `rev` 的块不重取 | `invalidation-plan.contract.test.ts`：`track.report_edited` 的 plan 不含 `['track-report-series']` 前缀；series 查询 `staleTime > 0` | plan 追加 `['track-report-series', trackId]` → `report edit does not refetch unchanged series blocks` 红 |

## 7. KNOWN GAPS（登记，不加固）

- G1 价格是否复权由插件源决定（腾讯 `qfq` = 前复权），本设计不声明；`resolved` 不带 `adjusted` 字段。钉住的行冻结的是钉住时刻的复权基准：源之后因分红/拆股重算历史价，钉住行不跟随（这正是钉住的含义，D3 前提假设）。
- G2 `line` 视图多序列跨币种按原值画、只标币种，不换算、不双轴。
- G3 `pinned=false` 的 frozen 行（部分 `unknown_asset`、某条 `unavailable`、或源尚未发布出晚于 `as_of` 的日线 bar：`as_of` 在未来 / `as_of` = 最近一个交易日 / 退市 / 停牌）会按 TTL 反复重投直到满足，可能永远满足不了：**退市/停牌标的永不出现更晚的 bar → 永不钉住**（`as_of` 距最后一根 bar ≤ 14 天时数据可画、图上标"未钉住"；超过 14 天落 S3 约束 4 的 `no data near cutoff`，整条 `unavailable`）；`as_of` = 最近一个交易日的钉住推迟到下一根日线 bar 出现（周末 3-4 天）。
- G4 **新鲜度无界**：解析只由读触发；从未被读的块不解析；过期行被读到时先返回旧数据，刷新后要等下一次读。
- G5 legacy `web/` 只显示 unsupported 一行；手机端同 fe。
- G6 `market.series` 对 agent 可见（无隐藏机制）。
- ~~G7~~ 已关闭：`plugin_tool_entry` 用 `registry.get` 精确查找，未安装 / 未运行 / 未暴露三态可分（D2 步骤 2）。
- G8 行不主动 GC（只随 track 删除级联）；改参数留下旧行。读端与准入按全主键选行、默认摘要读不取 `data`，旧行不落在 read 路径上（§11 第 5 轮 A m1 / codex MINOR）。
- G9 港股历史无兜底源（新浪港股日线不可用）；腾讯 ifzq 挂掉时 HK 序列 `unavailable`。
- ~~G10~~ 已关闭：周/月线由插件从日线聚合，只输出周期结束日 ≤ 截止日的完整周期，`ts_ms` = 周期起始日；内核边界也校验（D2 步骤 6、§2.5 S3 约束 3）。
- G11 **传输层**：`McpClient` 读循环 `read_line` 无字节上限（`mcp.rs:783`），2 MiB 接受上限在整行入内存之后才生效——归 #1634，本设计不加固（第 3 轮 codex R3-6 再次提出，编排者裁决：deferred by scope decision）。**插件侧**：内核超时不通知插件取消；请求留在插件单工作线程队列（`main.rs:2602`）按序处理，缓解只有 `deadline_ms` 出队丢弃（S3），排在前面的慢请求仍会被执行。responder 泄漏本身由 S2 的 `ResponderSlot` guard 关闭，不再是 GAP。
- G12 行刷新不发事件：浏览器在 `pending` 时轮询，过期行刷新后的新数据要等下一次 fetch（且在 `staleTime` 之外，S4.3）；浏览器停轮询后过期行可见到下一次 fetch。
- G13 内核对 `series` 只做字面去重；`HK:9988`/`HK:09988` 会得到两条同资产序列。
- G14 live 截止日按 UTC 日期取昨天：CN/HK 市场当日收盘（UTC 07-08h）后到 UTC 零点之间，live 行仍止于前一交易日，比按本地日历多滞后一天。
- G15 Assistant 可经 `resolved.reason` 读到只读工具的错误文本（≤ 256 字符）；D4 对"Assistant 不得拿 `resolved`"的驳回仍成立，这是它的间接通道。
- G16 未钉住行在 fork 后两个 track 各自刷新、可能分叉；只有钉住行有 fork 不变保证。
- G17 插件模板创建的 Owned track（`plugin_scope = <owner>`）上，非 owner 插件的 `chart.series` 永久 `unavailable, reason:"plugin <id> is outside this track's plugin scope"`（D2 步骤 3 与 agent 路由同一作用域，保守）。今天只有 `git-forge` 认领模板（`issue-development`），投研模板无 owner，不受影响。
- G18 `lookback exceeds source depth`：插件分不清「源历史没那么深」与「标的上市/复牌晚于窗口起点 + 14 天」，两者同落 `unavailable`；新上市标的要用更短的 `range`。对称的近端检查 `no data near cutoff` 同样分不清「退市 / 长期停牌」与「源近端截断」。
- G19 预检未命中 / 作用域 `None` 不落行：读端每次读都重新预检，插件启动后的下一次读就排队（浏览器 `pending` 轮询 3s → 30s，Planner 下一轮 read）；代价是这类块在 fe 上持续轮询直到条件变化（S2 先于 S3 合入的窗口期里 `NotExposed` 是常态，每次轮询 = 一次 DB 读 + 预检，零插件调用）。v4 的「预检时刻的事实冻结 6h」由第 4 轮 A M2 关闭。
- G20 lane 按插件、跨 track 全局串行：一篇 200 块的报告占住 market lane 至多 200 × 30s，其它 track 的图排在后面（与 §2.8 的延迟公式一致；§11 第 4 轮 A m9）。
- G21 纳入规则「结束日 < `complete_through`」的滞后（v6 收窄）：**live 股票日线不滞后**——US/HK/SH/SZ 按「日期 ≤ 昨天 UTC」纳入（§2.5 收盘时刻表；前提「源日线 bar 在 D+1 00:00 UTC 前定稿（常规时段）」对 US 由 U9 spike 补，若源吸收盘后成交则 US 回退严格；错误存活 ≤ 6h 且 live 行永不钉住）；**live 周/月线**与**加密日线**的最后一根依赖更晚的日线 bar 出现（周/月：最多一个周期加一个交易日；加密：Binance 全天候出 K，实际不滞后，只在时钟偏斜的秒级窗口内保守）；**frozen** 图在钉住前少最后一根（`as_of` = 今天 / 昨天）——**冻结不连续**：live 显示到昨天，用户写 `as_of` = 昨天冻结后，新行少最后一根直到明天的 bar 出现；这不是缺陷，是 frozen「存在更晚 bar 才证明已收盘」的证明成本（§11 第 4 轮 codex R4-2、第 5 轮裁决 2）。
- G22 `row_ttl` 按 series 结果选：`unknown_asset`（打错代码、`CN:` 前缀）的块每次被读到且距上次 ≥ 2 min 就重投一次（串行 lane、只在读时、每块每 2 min ≤ 1 次调用，不成风暴）直到块被改；这是采纳 R5-1「任一 series 非 `ok` → 2 min」的代价，不为它区分永久 / 瞬时——插件分不清「源 404」与「代码不存在」，区分只会造出下一个 G18 形状的歧义。

## 8. 与 issue 的出入

1. issue 方向 1 "`chart.candles` 收编为它的一个 view" → 保留 `chart.candles` 为独立内联 kind（D1）。
2. issue 方向 3 "内核在写入时钉住快照" → 写入事务不调插件、不触发任何事；后台任务在**第一次读之后**首次完整解析时钉住（D2/D3）。理由：F2 写路径全在 persist 事务内；写后事件拿不到块 id（F1.11/F3.6）。
3. issue 方向 2 "overlay 是推送不是查询 … 内核契约改动" → 请求-响应通道已存在（F4.6-F4.10），改动缩小为"内核后台任务作为调用者 + 插件声明一个只读 tool + 一张结果表 + `McpClient` 超时清 responder"。
4. issue 方向 4 "不新增工具" → MCP 面不新增；浏览器需要一条 NEW HTTP 路由（D5）。
5. issue "`as_of` / `frozen`" 两个名字 → 只用 `as_of`，语义是**截止日**。
6. **v2 新增**：read 不即时；**v3 新增**：不承诺有界新鲜度，从未被读的块不解析（§2.8）；**v4 新增**：不在截止日当天钉住——`as_of` = 最近交易日的 frozen 块要等下一根日线 bar 出现（D3）；**v5 新增**：任何 bar 只在源发布出更晚的日线 bar 后才纳入，live 日线的最后一根滞后一个交易日（G21）；否定结果不落行，`pending` 可带 `reason`（D2/D4）；**v6 新增**：live 股票日线改按「日期 ≤ 昨天 UTC」纳入、不再等更晚 bar（§2.5 收盘时刻表），CRYPTO / 周月 / frozen 维持严格，请求带 `mode`（D2）；行 TTL 按 series 结果选（D3）。

## 9. 不确定点（只留必须由 S3 spike 才能定的项）

- U6 腾讯 ifzq 的复权基准行（首行 2011 年）是否对所有 us 代码出现：S3 实现按日期过滤规避，测试 fixture 要包含这一行；spike 顺带核对 n=2 / n=3 的行为（S3.2）。
- U8 腾讯 ifzq 带 start/end 且窗口超约 640 根时截哪一端（保留最早还是最新）未实测（第 3 轮 spike 第 3 行不带 start/end，回的是最新 640 根）。S3 实现前先 spike，fixture 照实模拟；无论哪一端，分页 + 对称近端检查都覆盖（S3.7）。
- U9 ifzq / Sina 美股日线是否吸收盘后成交（盘后 16:00–20:00 ET = 冬令时 21:00–01:00 UTC 跨过零点，§2.5 收盘时刻表）：S3 实现前 spike——冬令时段 00:00–01:00 UTC 对同一只美股连取两次，比对日期 = 前一 ET 交易日的 bar 的 close / volume 是否仍在变；**若吸收 → US venue 回退严格规则**（插件侧一行，S3.1）。HK / CN 余量 ≥ 15h，不需要 spike。

已关闭：U1（§2.5 实测）、U3（D4 裁决）、U4（F7.6）、U7（第 3 轮：严格 `>` 使 ifzq 盘中是否列当日 bar 只影响钉住时点）。U2（常量估值）与 U5（轮询节奏）不是 spike 项，移到 §5.1 S2.15 / S4.3 标「估值、评审可调」。

## 10. 参考

- 本文 §2 所有 file:line 基于 `c534bf6b`。
- 第 1 轮评审原文：`docs/_1628-design-review-codex-v1.md`、`docs/_1628-design-review-subagent-v1.md`；第 2 轮：`docs/_1628-design-review-codex-v2.md`、`docs/_1628-design-review-subagent-v2.md`；第 3 轮：`docs/_1628-design-review-codex-v3.md`、`docs/_1628-design-review-subagent-v3.md`（A 通道原文第 49 行有一个 #1316 退役词，修订者替换为「OpenAPI 文件」并在文内加注，其余原文未动）；第 4 轮：`docs/_1628-design-review-codex-v4.md`、`docs/_1628-design-review-subagent-v4.md`（原文未动）；第 5 轮：`docs/_1628-design-review-codex-v5.md`、`docs/_1628-design-review-subagent-v5.md`（原文未动）。
- U1 spike：编排者 2026-09-12 实测，表已复制进 §2.5。窗口 spike：编排者 2026-09-13 实测，表已复制进 §2.5；Binance 未收盘 K 线一行由修订者同日补测。收盘时刻表（§2.5）：第 5 轮两通道引用交易所官方页面（NYSE / HKEX / SSE 交易时间、Binance klines 契约），本仓未实测。
- 相关：#1556 S1（venue-qualified identity）、#1623（`calm.report.commit`）、#960 PR3（kinds 词汇）、#1612（layout 讨论，材料不在基线）、#1634（MCP 传输层字节上限，本设计的 G11 归它）。
- origin/main 在 `c534bf6b` 之后 3 提交（`7754fd32` #1626 投研模板、`60f33140` #1631 worker grants、`bd033633`）：#1631 在 `transport.rs` 的 agent 路径加 `worker_grants::require`，本设计的内核发起调用不经 `dispatch_plugin_tools_call`，不受影响，但 F4.9 行号变基后偏移 +3…+7，S2 实现时重核。

## 11. 处置历史

通道：codex = `_1628-design-review-codex-v<n>.md`；A = `_1628-design-review-subagent-v<n>.md`（通道 A）；编排者 = 裁决。处置词：采纳 / 部分采纳 / 驳回 / 消失（因重写而不再适用）。

### 第 1 轮（v1 → v2）

| 轮次 | 通道 | 发现 | 处置 | 落点 |
|---|---|---|---|---|
| 1 | 编排者 | 总裁决：解析改后台任务，read 与浏览器只读库，永不在读路径调插件 | 采纳。D2/D3/D4/D5 整段重写 | §0 引言、D2-D5、§2.8 |
| 1 | 编排者 | 存储表 vs overlay 二选一并给理由 | 采纳：选表。overlay 被侧栏全量拉取（`routes/overlays.rs:106-112`）+ `OverlaySet` 事件带整 payload（`event.rs:600`）是硬伤 | D3 |
| 1 | 编排者 | FK CASCADE vs 事务内存在性校验二选一 | 采纳：FK。`foreign_keys=ON`（`sqlite/mod.rs:264`）、先例 0104/0106 | D3、F4.15 |
| 1 | 编排者 | U1 spike 表复制进 §2.5 | 采纳 | §2.5 |
| 1 | 编排者 | must-red：A2 直接测入口、钉住用并发竞争者、新增读路径零插件调用变异 | 采纳 | §6 A2/A7/A10 |
| 1 | 编排者 | oracle 每行一个切片、`unknown_asset` 只写一处、状态词只用 `resolved.status`；切片表重排 | 采纳。v1 S5 并入 S2，4 片 ≈ 3300 行 | §3、§5 |
| 1 | 编排者 | fork：复制快照行或登记 GAP | 采纳：复制 `pinned=1` 行（fork 在事务内，`tracks.rs:2324`，id 保留 `:2717`）。**第 2 轮改为复制全部行（R2-8）** | D3、seq 12 |
| 1 | 编排者 | 前端零新路由优先；若选表则一条 GET 带 hash 比对 409 | 部分采纳：一条 GET；绑定载体用 `rev` 不用 hash——fe 在明文 http 下无 `crypto.subtle`（F6.10） | D5 |
| 1 | codex | BLOCKER 读报告可触发任意插件工具（`{source:"neige://plugin/p/reset"}`） | 采纳。任务经 manifest 路由 + `kind == None` + `readOnlyHint == true` + 本地变体；否则 `unavailable` 且零调用。**第 2 轮把路由函数换成精确查找 `plugin_tool_entry`（A m3）** | D2 步骤 2-4、D6、seq 3f、A11 |
| 1 | codex | MAJOR 回复上限不能保证有界内存（`read_line` 先整行入内存，`mcp.rs:783`） | **[第 2 轮证伪，R2-2]** v2 写成"部分采纳…登记 G11"，但 D2/D6/G11 仍把 2 MiB 说成"资源上界"。v3：2 MiB 只是接受上限；传输层字节上界归 #1634 | D2 步骤 6、D6、G11 |
| 1 | codex | MAJOR 超时取消泄漏 responder（`mcp.rs:667/:676`，只在 `:804` 或关闭时清） | **[第 2 轮证伪，R2-1]** v2 写成"串行使每插件未决 ≤ 1，泄漏上界为 1"——假：每次超时留一个槽位，跨块/跨 TTL 无界累积。v3：S2 加 `ResponderSlot` guard | D2 步骤 5、A5b |
| 1 | codex | MAJOR 每块 8s 不等于每次 read 8s；需并发预算 | 消失：read 不再等任何插件调用（D4 纯 DB 读）；任务按插件串行，延迟只落在队列 | D2、D4 |
| 1 | codex | MAJOR 插件停机与作用域策略矛盾：绑定 track 的 owner 停了 → `TrackPluginScope::None`（`tool_visibility.rs:142`）→ 先到 `unavailable` | 采纳。核实为真（F4.11）。v2 定义：`pending` 只表示"无行"；任务遇到 `None`/未运行都写 `unavailable`；seq 3b 用绑定 track 测。**第 2 轮修正 reason 文字（A m8）** | D2 步骤 2-3、seq 3b |
| 1 | codex | MAJOR 成功信封会把缺失数据永久冻结 | 采纳。钉住条件 = 每条 `ok` ∧ 回复 `as_of == 请求 as_of`。**第 2 轮证明回显判据空洞，改为 `complete_through`（A M1）** | D3、seq 7d、A7b |
| 1 | codex | MAJOR 快照插入与 track 删除竞态（清理在 `tracks.rs:4032`，`track_delete_tx :4037`） | 采纳。FK `ON DELETE CASCADE`，迟到写入在约束处失败 | D3、seq 9、A13 |
| 1 | codex | MAJOR 查询 key 里的 payload hash 不绑定 HTTP 响应 | 采纳（载体改 `rev`）。`?rev=` 不等 → 409 `{current_rev}` | D5、seq 5、A16 |
| 1 | codex | MAJOR 共享 resolver 不能建立人/AI 数据相等 | 采纳。两个读者读同一行的同一 `summary`/`data` 字节；`resolved_at` 一并返回。**第 2 轮证明对 live 行仍只是措辞，收窄为钉住行（R2-7）** | D3、D4、seq 10 |
| 1 | codex | MAJOR 回复校验没有执行声明的数据契约 | 采纳。校验清单：一一对应、数量、元组长度、有限数、严格升序、日期 ≤ as_of、点数 ≤ 区间上界、行字节。**第 2 轮补 `points.len() >= 2`、`ts_ms` 零点（R2-9、A m11）** | D2 步骤 6、seq 3e、A12 |
| 1 | codex | MAJOR 首点为 0 的合法序列让 normalized 未定义 | 采纳。显式"不可归一化"态（首点 ≤ 0） | D5、A15 |
| 1 | codex | MAJOR 两条 must-red 变异不可靠：A2 被 `track_report.rs:520/:539` 复核兜住；A7 的 `INSERT OR REPLACE` 顺序重读仍绿 | 采纳。核实为真（F2.2）。A2 变异改在 `validate_chart_series` 本身；A7 用并发竞争者 + `WHERE pinned=0` 变异 | §6 A2/A7 |
| 1 | codex | MAJOR oracle 归属与负态不闭合；"三个非 ok 态"只有两个 | 采纳。每行一片；`unknown_asset` 只在 3d/S3；非 ok 态明确为 `pending`/`unavailable` 两个 + 查询 loading | §3、D5 |
| 1 | codex | MINOR F4.6 漏内核 overlay 写者（`card_fsm.rs:558`） | 采纳。F4.5 改写为"外部写方 / 内核内部写者"两列 | F4.5 |
| 1 | codex | MINOR F4.9 "唯一生产调用者"为假（`routes/cards.rs:588`） | 采纳。F4.10 新增；D4 用它论证内核调用不经角色门 | F4.10、D4 |
| 1 | codex | MINOR S4 pre-S3 状态写错：已装插件回 `unknown tool`（`main.rs:2572`）→ `unavailable` 不是 `pending` | 采纳。F4.13 记录；§5 S4 行为改为 `unavailable`；G9 改写 | F4.13、§5 |
| 1 | codex | MINOR oracle 行 8 过度声称（周末无新 bar；滚动窗口 n 不变） | 采纳。用受控 fixture 多给一根 bar 断言前进，不断言 `n` | seq 8、A8 |
| 1 | A | B1 内核代读者调插件是无守卫 RPC 代理（不查 manifest/kind/readOnlyHint、允许 Http、Assistant 绕过 `PLUGIN_TOOL_ROLES`） | 部分采纳。构造 1-3 全部采纳（同 codex BLOCKER）；"Assistant 不得拿 `resolved`" 驳回：`PLUGIN_TOOL_ROLES`（`transport.rs:88`）在 `:710` 门的是 agent 以自身身份发起的 tools/call；内核发起的调用已有不经它的先例（`cards.rs:588`）；结果是报告内容，Assistant 已能写 `source`（`track_report_blocks.rs:121-296`）且能读 `text` | D2、D4、D6 |
| 1 | A | M1 `payload_hash` 含 caption/view/overlays，改说明文字就解冻 | 采纳。`request_hash` 只盖 `(series, fields, range, period, as_of)`。**第 2 轮补 `plugin_id, tool`（R2-3），并收窄"改 view 不换行"（A m1）** | D1、seq 7c |
| 1 | A | M2 首次块级 ok 钉住把部分失败永久化 | 采纳（同 codex）。 | D3、A7b |
| 1 | A | M3 `as_of` 无上界 | 采纳：`as_of < 今天 UTC`。**第 2 轮证明该检查无载体（calm-types 无时钟，A M2），v3 整个删掉写端时钟检查，改由 `complete_through` 判据消解同一风险** | D1、seq 2n/7 |
| 1 | A | M4 默认水合挂在 CAS 路径且串行；单线程插件排队 | 消失：read 纯 DB 读；串行化改在内核任务侧与插件单线程同构 | D2、D4 |
| 1 | A | M5 F4.6 "写方只有…" 与代码自述矛盾 | 采纳（同 codex MINOR） | F4.5 |
| 1 | A | M6 F2 穷举漏项（`track_recipes.rs:275`、`tracks.rs:1006/:2795`、`track_report.rs:643/:660`）、F2.3 载体指错 | 采纳。重跑穷举命令，9 处生产入口逐条列出，F2.2/F2.3 载体改正 | §2.2 |
| 1 | A | m1 `compatibility.rs` 在 calm-types；`WEB_COMPAT_VERSION=27` | 采纳 | F7.3 |
| 1 | A | m2 `track_report_doc.rs:235` 就是 SHA256 观察 hash | 采纳。F7.6 新增，U4 关闭 | F7.6、§9 |
| 1 | A | m3 D7 引用的 #1612 文件不在基线 | 采纳。标"不在基线树"，去掉行号 | D7 |
| 1 | A | m4 oracle 3a 的 `tools_call` 签名对 Http/Cli 是双参 | 采纳。D2 步骤 4 按变体分派 | D2 |
| 1 | A | m5 事件 kind 权威源是 `calm-types/src/event.rs` | 采纳（v1 已如此，无改动） | §3 |
| 1 | A | m6 `HK:9988`/`HK:09988` 内核去重不到 | 采纳。声明内核只做字面去重，登记 G13 | D1、G13 |
| 1 | A | m7 fork 后无快照行 | 采纳。fork 事务里复制 `pinned=1` 行。**第 2 轮改为全部行（R2-8）** | D3、seq 12 |
| 1 | A | m8 `resolved` 用枚举而非 `Option`+条件字段 | 采纳 | D4 |
| 1 | A | m9 切片可合入性核对；S2 先于 S3 时 `-32601`/`isError` 归 `unavailable` | 采纳（与 codex MINOR 合并写进 §5） | §5 |
| 1 | A | m10 纪律核对全 ✓ | 无改动 | — |
| 1 | 编排者 | 全称量词附穷举命令；不发明机制（核对 `plugin_tool_route`/`trusted_forge_plugin`/`ConnectorClient`） | 采纳。§2.2 命令与结果；`plugin_tool_route` `transport.rs:771-820` 私有 fn；`trusted_forge_plugin` 在 `forge_trust.rs:20`（本设计不走该臂）；`ConnectorClient` `connector.rs:44-55`。**v3 不再改 `plugin_tool_route` 可见性，改加 `plugin_tool_entry`** | §2.2、F4.8/F4.9、D2 |

### 第 2 轮（v2 → v3）

每条先到代码核实再处置；驳回附 file:line。

| 轮次 | 通道 | 发现 | 处置 | 落点 |
|---|---|---|---|---|
| 2 | 编排者 | **核心简化**：删 bus 订阅者，解析的唯一触发是读；写路径什么都不做；保证语句改为"读过之后 N 秒内解析（N = 队列延迟 + 30s）；从未被读的块不解析"；不承诺有界新鲜度 | 采纳。核实：`TrackReportEdited` 只带平铺 `body_after`（`event.rs:579-597`），`BlockSlice { raw }`（`mod.rs:36-38`）、`NonProseFence { kind, payload }`（`fence.rs:28-31`）都无块 id；`subscribe()` 只收之后的信封（`event_bus.rs:185-189`）；且该事件只有 `write.rs:1098` 一个构造点（F3.9 穷举），模板/fork/recipe 写入本就不发。D2 备选加 (b′)/(b″) 并裁决 (b″)；oracle 删 2e；§2.8 加"写后第一次读触发；写完立刻读到的是 pending"与保证语句；G4 改为"新鲜度无界"；A10b 新增"写路径不触发"变异 | §0、§2.8、F1.11/F3.6/F3.9、D2、seq 2/4、A10b、G4 |
| 2 | codex | R2-1 MAJOR 第 1 轮超时处置为假：串行不封顶 responder，永不回复的插件每次超时留一槽，跨块/TTL 无界累积；插件侧队列也累积 | 采纳（第 1 轮"部分采纳"作废，整段重写）。核实：`call` 在 `:667` insert、`:676` await，取消路径无清理，只有 `:672`（writer 死）、`:804`（回复）、`:867` `flush_responders_with_error`（读失败/EOF）移除。v3：S2 在 `:667`-`:676` 之间加 `ResponderSlot` RAII guard（惠及 agent 路由 `transport.rs:722-738` 与 `cards.rs:588`）；must-red A5b（假插件永不回复，两次超时后 `pending_responders() == 0`）。插件侧：登记 G11（内核超时不通知插件），S3 用 `deadline_ms` 出队丢弃（seq 3g、A14b）；请求形状加 `deadline_ms` | D2 步骤 5、请求形状、seq 3c/3g、A5b/A14b、G11、§5 S2/S3、F4.6/F4.13 |
| 2 | codex | R2-2 MAJOR 第 1 轮内存上界处置只是推迟：`read_line` 整行入内存（`:783`），2 MiB 校验在其后；G11 承认但 D2/D6 仍写"资源上界" | 部分采纳。核实为真（F4.6 `:783-810`）。**不在本设计范围**：#1634（OPEN，F4.17）单独处理传输层字节上界。本文档删掉一切把 2 MiB 说成"资源上界/内存上界"的措辞：D2 步骤 6 改为"内核接受并存储的回复上限（校验）"，D6 逐条指向执行机制，G11 改写为指向 #1634 | D2 步骤 6、D6、G11、F4.17、§1 非目标 |
| 2 | codex | R2-3 MAJOR `request_hash` 缺源：换 `source` 同参数 → 同 hash → P 的结果钉在 Q 的声明下 | 采纳。指纹元组加 `plugin_id, tool`；seq 7c 把改 `source` 列入换行 | D1、seq 7c、D3 身份 |
| 2 | codex | R2-4 MAJOR 订阅者拿不到块 id | 采纳，按核心简化删订阅者（见上） | 同上 |
| 2 | codex | R2-5 MAJOR in-flight 去重不执行 TTL：A 完成后 B 用旧观察再 enqueue → 立即再调 | 采纳。`resolve` 步骤 1 加 drain 准入：重读行，`pinned` 或 `resolved_at ≥ now − TTL`（含 `unavailable` 行）→ 丢弃；与"重读块比 hash"并列。seq 3a、must-red A9b | D2 步骤 1、§2.8、seq 3a、A9b、D3 live |
| 2 | codex | R2-6 MAJOR 有损投递与重启：commit 后 enqueue 前崩溃 / `Lagged` 丢事件 → 无读者就永不解析；浏览器停轮询后过期行可见到下次 fetch | 部分采纳：一半因核心简化消失（没有订阅者就没有丢事件问题）；另一半（从未被读的块不解析、停轮询后过期行可见）改为**明确声明的保证边界**写进 §2.8/G4/G12，不加启动扫描、不加定时扫描。核实：`events.rs:787-789` commit 后 emit、`event_bus.rs:185-189` 无重放——作为 v2 订阅者的反证登记在 F3.6/F3.7 | §2.8、G4、G12、D2 触发 3 |
| 2 | codex | R2-7 MAJOR 人/AI 同源只是措辞：live 行被后台刷新替换，浏览器拿 100、agent 拿 110 | 采纳。不变式收窄：**钉住行**任何两个读者任何时刻字节相等（DB 层 `WHERE pinned=0` 不可覆盖）；**未钉住行**同一次读同一行、两次读之间可被替换，`resolved_at` 暴露；seq 10 改为"同 `resolved_at` ⇒ 同字节"（与 A m10 合并）。§1 目标句改"钉住的行对人与 AI 字节相等" | §1、§2.8、D3、seq 10 |
| 2 | codex | R2-8 MAJOR 非交易日冻结未定义（周日 `as_of` 永不满足回显相等）；fork 只复制 pinned 行使子 track 先空后解析出修订价，违反冻结不变式 | 采纳。`as_of` 改为**截止日**语义：插件返回日期 ≤ `as_of` 的点，每条另带 `complete_through`（钉住判据见 A M1）；fork 复制**全部** `report_series` 行（核实 `tracks.rs:2322-2329` fork 在事务里拿快照、`:2717` 保留 id）；保证边界"钉住行不变；未钉住行各自刷新可能分叉"登记 G16 | D1、D3 frozen/fork、seq 7/7d/12、G16 |
| 2 | codex | R2-9 MAJOR 完整性校验允许 `ok` + 空 `points` 被永久钉住 | 采纳（与 A m7 合并）。校验清单加 `ok` 项 `points.len() >= 2`，否则整个回复 malformed → `unavailable`；插件对 <2 点自己报 `unavailable, reason:"no data in range"`；A12 加变异 | D2 步骤 6、回复形状、seq 3e、A12/A14 |
| 2 | codex | R2-MINOR "改 view 不换行"与 `fields` 由 `view` 派生矛盾 | 采纳（与 A m1 合并）。不变式收窄为"不改变派生 `fields` 的 view 切换复用行"：`line↔normalized↔bar` 复用，`candles` 与其它互换换行 | D1、D3 身份、seq 7c |
| 2 | A | M1 MAJOR 钉住条件"回复 `as_of == 请求 as_of`"要么空洞（回显）要么对非交易日永不满足 | 采纳 A 的方案。插件回复每条 series 带 `complete_through` = 源**未过滤**的最新 bar 日期；钉住 = 每条 `ok` ∧ 每条末点 ≤ `as_of` ∧ 每条 `complete_through >= as_of`。**[第 3 轮修正：v3 用「插件总是拉最新 N 根再按 `as_of` 过滤」实现 `complete_through`，被 codex R3-2 / A M1 证明是缺陷本身（旧 `as_of` 永久截断或 0 点）；`>=` 被 codex R3-1 / A M2 证明在相等时钉住未收盘 bar。v4 改窗口取数 + 单独探测 + 严格 `>`；第 4 轮再加「探测先于取数」（codex R4-1）与「结束日 < `complete_through`」（codex R4-2）]**；周日 `as_of` → 源最新周五 → 未钉住 → 周一出 bar 后 TTL 重投钉住，"最多延迟到下一个交易日"；回复顶层 `as_of` 删除；A7b 改用新判据 | D2 步骤 6、回复形状、D3 frozen、seq 7a/7d、A7b、G3 |
| 2 | A | M2 MAJOR `as_of < 今天(UTC)` 落在无时钟的 calm-types 纯函数 | 采纳，且**整个删掉这条写端校验**。核实：`calm-types/Cargo.toml` 无 chrono/time/jiff，`src` 无 `Utc::now`/`SystemTime::now`（F1.12）。有了 `complete_through` 判据，未来 `as_of` 自然是"未钉住、到期后自动钉住"；`validate_chart_series` 只查 `YYYY-MM-DD` 形状；A2/2n 的"`as_of` ≥ 今天"用例删掉，A2 加"接受未来日期"正例与"加时钟检查必红"变异；fe zod 可提示不拒绝。第 1 轮 A M3 的处置作废 | D1、F1.12、seq 2n/7、A2、D5 zod、§5 S1 |
| 2 | A | M3 MAJOR live 吞盘中半根 bar（`≤ 今天`） | 采纳。live 请求截止日 = **昨天 UTC**，内核在请求里显式填 `as_of = yesterday_utc(now_ms)`（calm-server，有 `chrono`，F7.7），插件对两态走同一条过滤路径；校验清单"每点日期 ≤ 请求 `as_of`"两态统一；登记 G14（CN/HK 按 UTC 多滞后一天）；`report_series.as_of` 改 `NOT NULL`（两态都有截止日）；A8 加"live 请求带昨天 UTC"断言 | D2 请求截止日、请求形状、D3 表/live、F7.7、A8、G14、§1 非目标 |
| 2 | A | M4 MAJOR resolver 失败路径 fail-locked：步骤 7 Err → 键留在 `inflight` → 永久 `pending`；drain panic → receiver 丢 → `send` 静默失败 | 采纳。`InflightGuard` RAII（任何退出路径释放键）；lane 用 `JoinHandle` 监督，`enqueue` 时 `is_finished()` 或 `send` 失败 → 重建；`#[cfg(test)] failpoints`；seq 3h；A9c/A9d 两条 must-red；D2 步骤 8 显式清理删除 | D2 enqueue 1/3、步骤 7-8、seq 3h/9、A9c/A9d |
| 2 | A | m1 MINOR "改 view hash 不变"过宽 | 采纳（与 codex R2-MINOR 合并，见上） | D1、seq 7c |
| 2 | A | m2 MINOR G11 "泄漏上界为 1"为假 | 采纳（与 codex R2-1 合并，见上） | D2 步骤 5、G11 |
| 2 | A | m3 MINOR D2 步骤 2 用 `format!("plugin.{id}_{tool}")` 反解：`source` 段含 `_`（`kinds.rs:162-175`），`neige://plugin/a_b/c` 会路由到插件 `a` 工具 `b_c`；且路由不返回 annotations | 采纳。核实：manifest id 规则 `^[a-z0-9][a-z0-9.-]{1,63}$` 不含 `_`（`manifest.rs:2303-2314`，测试 `:3172-3177`）；`PluginRegistry::get(id)` 存在（`registry.rs:292`）；`plugin_tool_route` 返回 `(id, tool, kind)` 无 `ExposedTool`（`transport.rs:771-781`）。v3：S2 新增 `plugin_tool_entry(registry, running_ids, plugin_id, tool) -> NotInstalled | NotRunning | NotExposed | Found(ExposedTool)`，用 `registry.get` 精确查找，不拼名字；`plugin_tool_route` 不再改 `pub(crate)`；A19 元测试锁两者集合相等；A11 加 `a_b` 用例；**G7 由此关闭** | F4.16、D2 步骤 2、D6、seq 3f、A11/A19、G7 |
| 2 | A | m4 MINOR `unavailable.reason` 回显 `isError` 文本无长度上界；Assistant 由此读任意只读工具错误文本 | 采纳。`reason` 写入前截到 256 字符（A20）；登记 G15 | D2 步骤 6、D3 表、D4、A20、G15 |
| 2 | A | m5 MINOR 模板/fork/recipe 写入不发 `TrackReportEdited`（只有 `write.rs:1098` 发） | 消失（删订阅者后无此差异）。事实保留为 F3.9，作为删订阅者的又一理由 | F3.9 |
| 2 | A | m6 MINOR 按文档 `source` 的 plugin_id 建队列 → 1000 个不存在 id = 1000 条队列 | 采纳。`enqueue` 先做路由预检：命中才建 lane；未命中 `tokio::spawn` 一次性 `resolve`（同一代码路径写 `unavailable`） | D2 enqueue 2 |
| 2 | A | m7 MINOR `ok` 配 `points=[]` 未定义 | 采纳（与 codex R2-9 合并：门槛取 2 点而非 1 点，摘要 `change_pct` 与图都需要两点） | D2 步骤 6 |
| 2 | A | m8 MINOR 3b 在 `TrackPluginScope::None` 时 reason "plugin X is not running" 是假话 | 采纳。改为 `"track owner plugin unavailable"`；`Only(other)` 另给 reason | D2 步骤 3、seq 3b |
| 2 | A | m9 MINOR 3d 标 S3 但"块级 ok、`pinned=false`"是 S2 行为 | 采纳。拆为 3d（S3 回复形状）/ 3d′（S2 处理） | seq 3d/3d′ |
| 2 | A | m10 MINOR seq 10 "字节相等"只在无 TTL 刷新时成立 | 采纳（与 codex R2-7 合并，见上） | seq 10 |
| 2 | A | m11 MINOR `ts_ms` → 日期的 UTC 折算约定未写 | 采纳。规定 `ts_ms` = 交易日 UTC 零点，写进回复形状；校验清单加 `ts_ms % 86_400_000 == 0`；日期比较用整数除法 | D2 步骤 6、回复形状、seq 3e、A12/A14 |
| 2 | A | §2 F3.7 `event_bus.rs` 在 calm-truth，文档未标 crate | 采纳 | F3.7 |
| 2 | A | 切片与纪律核对（S1→(S2∥S3)→S4 各自可合入；迁移号推迟；不 bump 三常量；`Resolved` 用枚举）✓ | 无改动 | — |
| 2 | 编排者 | 切片影响：删订阅者后 S2 变小，`McpClient` guard 计入 S2，重算行数 | 采纳。S1 ~550 / S2 ~950 / S3 ~950 / S4 ~800，总 ≈ 3250（v2 3300） | §5 |


### 第 3 轮（v3 → v4）

每条先到代码核实再处置；驳回附 file:line。两通道各自独立命中同两处缺陷（窗口截断、相等判据），编排者裁决与评审并列。

| 轮次 | 通道 | 发现 | 处置 | 落点 |
|---|---|---|---|---|
| 3 | 编排者 | **交叉命中 1：窗口截断**（codex R3-2 / A M1）——「拉最新 N 根再按 `as_of` 过滤」让 `range=1Y`、`as_of` 六个月前的块钉在半年数据上，两年前的块 0 点永久 `unavailable`；那句话就是缺陷本身 | 采纳，整段重写。窗口定义进 D1：`[as_of − RANGE_DAYS[range], as_of]`（历日表与 `max_points` 共用），live 即 `[昨天 − RANGE_DAYS, 昨天]`；内核在请求里填 `start`、删 `range`（一个算法一个实现）；校验清单加 `≥ start`；S3 按 spike（编排者 U7 spike 行）显式 start/end 取数、约 640 根按日期分页、`complete_through` 单独探测；超出源深度 → `unavailable, reason:"lookback exceeds source depth"`（判据：最早 bar > `start + 14d`）登记 G18；A14 加旧 `as_of` 窗口非空 + 分页 + 深度不足用例；§2.5 与 D2 回复注释里的「总是拉最新 N 根」删除；seq 3i/3i′ 新增。**[第 4 轮修正，codex R4-1：v4 没有规定探测与取数的顺序，v5 规定先探测再取窗口、缓存页带探测值]** | D1、§2.5、D2 请求窗口/形状/步骤 6、seq 3/3e/3i/3i′、A8/A12/A14、G18、§11 第 2 轮 A M1 行标注 |
| 3 | 编排者 | **交叉命中 2：严格 `>`**（codex R3-1 / A M2）——`complete_through == as_of` 时钉住的是内核无法证明已收盘的那根 bar | 采纳。核实 A 的断言：Binance `klines?interval=1d&limit=2` 不带 end 的末根 `closeTime` 在未来 [实测 2026-09-13 02:39 UTC]，即不带 end 的探测**必然**含当前未收盘日 K，`>=` 下 `as_of = 今天` 的 crypto frozen 块会钉在半根 bar 上。判据改 `complete_through > as_of`；`complete_through` 定义为源未过滤的最新**日线** bar 日期、与 `period` 无关（A：周/月聚合 bar 的日期会骗过判据）；D3 加前提假设（bar 日期正确、按时间顺序发布）与逐场景表；U7 关闭；G3 登记退市/停牌永不钉住、最近交易日的钉住推迟到下一根 bar；§8 加出入 7；附带：live 行永远 `pinned=0`（探测让 live 也能满足 `>`，修订者补写进 D3） | D1、D3 frozen/live、seq 7a/7d、A7b、G3、U7、§8 |
| 3 | codex | R3-1 MAJOR 相等不能证明截止 bar 已收盘；U7 仍开；记录 U7 不等于解决已采纳的 A M1/M2 | 采纳（同交叉 2）。codex 自己给的边界（延迟到更晚 bar 出现、退市序列永不钉、不证明历史完整、不防事后修正）全部写进 D3/G3/G1 | 同上 |
| 3 | codex | R3-2 MAJOR 「最新 N 再过滤」永久钉住被截断的历史窗口 | 采纳（同交叉 1） | 同上 |
| 3 | codex | R3-3 MAJOR 周/月聚合重新引入未完成 bar：周四冻结到周三，周一-周三聚成半根周 K，周四的日线日期满足严格 `>`；live 聚合把当前半周标成最新日期也能过昨天截止 | 采纳。核实 spike：ifzq `week`/`month` 端点只回当前一根且日期是最新日线日期（不可用），周/月必须插件自聚合。规则写进 §2.5 S3 约束 3：ISO 周 / 自然月、UTC 日期、只输出 `period_start ≥ start ∧ period_end ≤ as_of` 的完整周期、`ts_ms` = 周期起始日零点、聚合值定义；内核边界也校验（周一 / 1 日、周期结束日 ≤ `as_of`）；§2.8 写 live 周线最多滞后一周；G10 关闭；seq 3j、A12 三条、A14 两条。**[第 4 轮修正，codex R4-2：「结束日 ≤ 截止日」只是历法完整，未来截止日下当前半周照样输出；v5 改「结束日 ≤ `as_of` ∧ < `complete_through`」，日线同样适用]** | §2.5、§2.8、D1 period、D2 步骤 6/回复形状、seq 3e/3j、A12/A14、G10 |
| 3 | codex | R3-4 MAJOR 路由预检未命中绕过 lane：插件停止时 enqueue 多个块 → 各自一次性任务；插件在它们二次查找前启动 → 全部在 lane 外并发调用 | 采纳。核实：`running_plugin_ids()`（`plugin_host/mod.rs:3260`）与 `connector_client()`（`:3312`）各自 `lock_table()` 一次，状态在两次采样之间可变，v3 的「步骤 2 会再次未命中」不成立。v4：job 携带预检结果，一次性任务只做准入 + 写 `unavailable`，不二次查找、不取 `connector_client`、不调插件；lane 里的 job 才走 resolve 步骤 2；A9e must-red（`new_unstarted()` 记录器截住一次性 job，启动插件后再执行，断言零调用）；G19 登记「预检时刻的事实要等 TTL + 下次读」。**[第 4 轮修正，A M2：v4 的「保留否定结果」把每次进程重启后最先几秒的读冻结 6h；v5 删除一次性任务，预检未命中 / 作用域 `None` 不落行、读端 `pending, reason`，真正调用失败的 `unavailable` 行用 2 min TTL]** | D2 enqueue 2 / resolve 2、D6、seq 3b、A9e、G19、§2.8 例外 (d) |
| 3 | codex | R3-5 MAJOR 有界 mpsc `send().await` 在挂住 30s 的调用后面让读者等通道容量，与「enqueue 不等插件」矛盾 | 采纳（与 A m1 合并）。lane 通道改 `mpsc::unbounded_channel`，`enqueue` 用同步非阻塞 `UnboundedSender::send`；上界机制是 in-flight 集合（每键至多一个 job ⇒ 排队总数 ≤ `inflight.len()`），不是通道容量；D6 资源约束逐条指机制。**[第 4 轮修正，codex R4-3 / A m1：键含 hash 时同一块反复改写可挂上千个 job，v4 的「≤ 工作区块数」为假；v5 键改 `(track, block)`，上界 = 被读过且未出队的块数]** | D2 enqueue 3、D6、§2.8 |
| 3 | codex | R3-6 MAJOR R2-2 仍是措辞/范围处置：插件 stdout 无换行的任意长行在 `mcp.rs:783` 整行入内存后才到 `:802` 解析，guard/超时/reason 上限/接受上限都管不到 | **维持转 #1634，deferred by scope decision**（编排者第 3 轮裁决，范围裁决不是技术驳回）。核实 `mcp.rs:783` 未变；G11 加注 | G11、F4.17 |
| 3 | codex | R3-MINOR-1 保证语句与失败规则矛盾：D:148「30s 内得到一行」但超时只盖插件调用，D:250 允许写失败无行，D:240 允许 lane panic 丢 job；D:183/301「下一个交易日钉住」却要求 TTL 后再读；D:300「过夜必刷新」但 23:59 写入的行 00:01 仍新鲜 | 采纳。§2.8 保证语句改写为「30s 内返回或超时 + 一次写行」并附例外清单 (a) 写失败无行 (b) lane panic 丢 job (c) 被替换 lane 丢 job (d) 预检 miss 行是预检时刻的事实，全部以「下次读重投」收口；「下一个交易日钉住」改为「更晚 bar 出现 ∧ 之后一次读 ∧ TTL 已过」；D3 live 与 §2.8 写明 TTL 是时长不是日历 | §2.8、D3 live/frozen、seq 7d、G4 |
| 3 | codex | R3-MINOR-2 A11 fixture 插件 `a` 过不了 manifest 校验 | 采纳。核实 `manifest.rs:2303-2314` `is_valid_plugin_id`：`bytes.len() < 2 → false`（`:2305`）。fixture 改 `aa` / `b_c` / `neige://plugin/aa_b/c`；断言改为 reason 是原字符串 `aa_b` 且插件 `aa` 零调用（「不含 `a`」对 `aa_b` 是空洞断言） | A11 |
| 3 | codex | R3-MINOR-3 S2 先于 S3 时的失败路径写成插件 `unknown tool`，但 manifest 未暴露 `market.series`，D2 步骤 2 在调用前就回 `NotExposed` | 采纳。核实 `plugins/market/manifest.json:10-62` `exposes_tools` 只有 `market.quote` / `market.holdings.set` / `market.holdings.list`。§5 与 seq 3c 改为 `NotExposed` → `unavailable, reason:"plugin dev-neige-market does not expose market.series"`、零调用；`main.rs:2572` 的分支对本构造不可达 | §5、seq 3c |
| 3 | A | M1 MAJOR `range` 锚点未定义，旧 `as_of` 永久 `unavailable`（`{range:"1Y", as_of:"2024-12-31"}` 拉最新 1Y 全部 > `as_of` → 0 点 → 每 6h 重投永远如此） | 采纳（同交叉 1）。A 的「反推拉取深度」被 spike 替换为显式 start/end 窗口 + 分页 | 同交叉 1 |
| 3 | A | M2 MAJOR 严格 `>` + `complete_through` 必须是日线定义 | 采纳（同交叉 2）。A 的逐场景表核对后写进 D3；「U7 提议的前一天保守值」随 U7 关闭作废 | 同交叉 2 |
| 3 | A | m1 MINOR lane 重建竞态：两个并发 `enqueue` 都见 `is_finished()` → 各建一条，短暂两条 drain 同跑一个插件；被替换 lane 队列里的 job 随 receiver drop 丢失 | 采纳（与 codex R3-5 合并）。检查-重建-投递在 `std::sync::Mutex` 锁内、无 `.await`（spawn 与 unbounded send 都同步）；`rebuild_lane` 以 `&mut HashMap` 为参数（拿不到锁调不了）；核实 tokio 1.52.3（`Cargo.lock:3644`）`Rx::drop` 先 `close()` 再 `drain()` 排空缓冲区（`sync/mpsc/chan.rs:487-508`）→ 被替换 lane 的 job 被 drop、guard 释放键；写进 §2.8 例外 (b)(c)；A9f（签名 + 200 轮循环复现，循环部分是概率性的，已如实标注）。**[第 4 轮修正，codex R4-MINOR：签名不证明持锁、200 轮不保证复现；v5 改 `hold_in_rebuild` failpoint + `try_lock` 断言，确定性]** | D2 enqueue 3、seq 3h、A9f、§2.8 |
| 3 | A | m2 MINOR Owned track 只能解析 owner 的工具（`tool_visibility.rs:141` `Owned → Only(plugin.id)`）：绑定了非 market owner 的 track 上 market 图永久 `unavailable`；oracle/G 表未登记 | 采纳登记，规则维持（编排者：与 agent 路由同一作用域，保守）。核实影响面：owner 只在创建时由「运行中可信插件的 manifest `templates` 认领了该模板 key」得来（`routes/tracks.rs:1683-1688`、`:1691-1695`、`:1750-1752`）；`grep -n '"templates"' plugins/*/manifest.json` 只有 `git-forge` 认领 `issue-development`（`plugins/git-forge/manifest.json:302-306`）；投研模板 `investment-research`（#1626，origin/main `7754fd32`，不在基线）无认领 → `plugin_scope = NULL` → `All`。seq 3b 加 (iii)，G17 登记 | D2 步骤 3、seq 3b、G17 |
| 3 | A | m3 MINOR `summary TEXT NOT NULL` 与 `unavailable` 行矛盾 | 采纳，改 nullable（`unavailable` 行 NULL；D4 的 `Resolved::Unavailable` 本就无 summary） | D3 表 |
| 3 | A | m4 MINOR drain 准入用 `load_report_read_snapshot` 太重 | 采纳。核实 `track_report_read.rs:43-52` 先 `load_settings` 再算 `task_diagnostics`；`report_blocks_snapshot_tx(tx, track_id) -> (String, Vec<ReportBlock>)`（`track_report.rs:73`）在事务内给带 payload 的块，先例 `task_recovery/admission.rs:158`、`file_delivery/repair.rs:101`。准入改为一次短 `write_in_tx_typed` 事务，判定后即提交、不跨插件调用持有 | D2 步骤 1 |
| 3 | A | m5 MINOR A10b「等 2s 不 read」是计时型负测试；A5「测试超时 40s」 | 采纳。A10b 改 `new_unstarted()` 记录器断言 `enqueue` 调用数 0 + 无行；`SERIES_RESOLVE_TIMEOUT` 改为 `SeriesResolver` 字段，A5 注入 50ms | A5、A10b、D2 seam |
| 3 | A | m6 MINOR F3.9「共 7 处」错，实跑 26 | 采纳。`grep -rn "TrackReportEdited" crates/calm-server/src --include='*.rs' \| wc -l` = 26；`grep -rn "Event::TrackReportEdited {" … \| grep -v tests.rs` = 5（1 构造 `write.rs:1098` + 4 match）；结论「生产构造点只有一处」不变 [实测 2026-09-13] | F3.9 |
| 3 | A | m7 MINOR seq 8 缺 live 行 `as_of` 前进断言 | 采纳。seq 8 与 A8 加「行 `as_of` = 新的 `yesterday_utc(now)`」，变异「`DO UPDATE` 漏 `as_of` 列」 | seq 8、A8 |
| 3 | A | 第 2 轮处置复核全部落地为机制；残留「上界」措辞全是引述或否定 ✓ | 无改动 | — |
| 3 | 编排者 | U7 spike 由编排者完成（`spike-u7-window-fetch.md`）：ifzq 只给 end 不可用、start+end 可用、单次约 640 根、周/月端点不可用；Binance `endTime` 可用 | 采纳。表复制进 §2.5 标 [实测 2026-09-13 编排者]；修订者补测 Binance 不带 end 的探测含未收盘 K（一行，标修订者） | §2.5、§10 |
| 3 | 修订者 | 附带：请求形状加 `start`、删 `range`（内核算一次窗口，插件不再复算）；A12/A14 相应加窗口下界用例 | **已裁决（第 4 轮，A m6 前半 / codex 第 4 轮复核）**：通道 A 与 codex 都核对为一致，编排者采纳 | D2 请求形状、§2.5 S3 约束 2 |

### 第 4 轮（v4 → v5）

每条先到代码核实再处置；驳回附 file:line。本轮无驳回。计数：codex 3 MAJOR + 1 MINOR；A 2 MAJOR + 9 MINOR；另 1 行修订者附带。

| 轮次 | 通道 | 发现 | 处置 | 落点 |
|---|---|---|---|---|
| 4 | codex | R4-1 MAJOR 探测晚于取数会认证未收盘窗口：23:59:59 取到当日仍在变的 bar，00:00:01 探测到次日 bar，严格 `>` 钉住盘中值；日期正确、按序发布都成立，分页拉长窗口；缓存页也要服从顺序 | 采纳（编排者裁决 1）。核实：v4 §2.5 约束 1/2 只说「单独一次探测」没有顺序。v5：S3 约束改为每条 series 严格 1 探测 → 2 取窗口（含全部分页）→ 3 纳入 → 4 深度/近端；`complete_through` 记录探测时刻的观察；理由写进约束 1（探测时 `complete_through > as_of` ⇒ 每个 ≤ `as_of` 的 bar 已收盘 ⇒ 之后取到的是终态，修正除外 G1）；缓存页记录 `observed_complete_through`、回复取 min；seq 3k（fixture 在第一次请求后推进一天，钉住值必是收盘值）；A14 加顺序断言与变异 | §2.5 S3 约束 1/2、D2 回复注释、D3 frozen、seq 3i/3k、A14、§5.1 S3 |
| 4 | codex | R4-2 MAJOR 未来截止日仍出未完成周期：周三请求 `as_of` = 周日，周一至周三聚成的 09-07 周 K 结束日 ≤ 截止日、`ts_ms` 周一、`complete_through` = 周三 ≥ `ts_ms`，每条检查都过；月末截止日同形 | 采纳（编排者裁决 2）。核实：v4 约束 3「完整按历法」与步骤 6「周期结束日 ≤ as_of」「`complete_through` ≥ 末点」对该构造全部为真。v5：纳入规则统一为**结束日 ≤ `as_of` ∧ 结束日 < `complete_through`**，日线同样适用（今天的 bar 在明天的 bar 出现前不纳入；live 的昨天 bar 在今天的 bar 出现前不纳入），插件聚合与内核校验清单两处执行；步骤 6 的「`complete_through` ≥ 末点」改为「每点结束日 < `complete_through`」（末点 == `complete_through` 也 malformed）；§2.8 首条改写；A7b fixture 随之改（points 止于周四）；seq 3j 改为周中请求的构造；A12/A14 加用例与变异；代价登记 G21（live 股票日线滞后一个交易日、美股在亚洲白天少最后一根）；§1 非目标、§8 出入 7 | §2.5 S3 约束 3、D2 步骤 6、§2.8、seq 3e/3j/7a/7d/8、A7b/A8/A12/A14、G21、§1、§8 |
| 4 | codex | R4-3 MAJOR in-flight 键含 hash ⇒ 队列不以块数为界：lane 被慢调用占住时反复改写并读取一个块，每个 `(track,block,hash)` 都是新键，一个块可挂上千个 job；陈旧 hash 出队后才丢弃，删块也留着 | 采纳（编排者裁决 3）。核实：v4 enqueue 步骤 1 键含 hash、步骤 3 上界句以「每块一个当前 hash」为前提，构造成立。v5：键与 job 改 `(track_id, block_id)`；出队时读当前块、派生请求与 hash、按 TTL/pinned 准入；`(plugin_id, tool)` 不属本 lane → 丢弃重投；上界 = 被读过且未出队的块数；seq 3/3a′、A9/A9g、D6、§2.8 同步改 | D2 enqueue 1/3、resolve 1、D3 身份、D6、§2.8、seq 3/3a′/6/7c、A9/A9g |
| 4 | codex | R4-MINOR A9f 过度声称：`&mut HashMap` 可来自普通局部 map，签名不证明持锁；200 轮概率复现不保证出现 | 采纳（编排者裁决 4）。v5：`rebuild_lane` 改收 `&mut MutexGuard`（防误用，仍不是证明）；A9f 改 `hold_in_rebuild` failpoint 把重建者按在临界区里，测试线程 `lanes_try_lock()` 必须 `WouldBlock`——锁外重建的变异下 `try_lock` 成功，确定性；8 路并发用 `tokio::sync::Barrier` 起跑只做功能断言；删「必现」 | D2 enqueue 3、A9f |
| 4 | A | M1 MAJOR `as_of` 只查形状不查历法：`2026-02-31` 落盘后 `NaiveDate::parse` 在 read 或 drain 里 `Err`，要么 seq 4「read 永远 200」被证伪，要么落成每 TTL 重投的永久 `unavailable`；「无时钟」推不出「不能查历法」 | 采纳（编排者裁决 5）。核实：v4 D1 明写「不查历法」，D2 在 calm-server 用 `chrono` 算 `start`，无失败出口。v5：S1 `validate_chart_series` 加历法（闰年 + 每月天数，纯函数）；D1 措辞改「查历法、不与今天比较」；seq 2n/7 加 `2026-02-30`；A2 加变异；D2 步骤 1 写明 `start` 在出队时算、对绕过校验的数据丢弃不落行 | D1、§2.2 结论、D2 步骤 1、seq 2n/7、A2、§5 S1、§5.1 |
| 4 | A | M2 MAJOR 每次进程/插件重启后最先几秒被读到的图表 `unavailable` 六小时：`running_plugin_ids` 不含 `Spawning`，预检未命中的一次性任务写 6h 行，浏览器 WS 重连 refetch / Planner 首轮 read 全中，用户刷新无效；ifzq 一次瞬时 5xx 同形 | 采纳（编排者裁决 6）。核实 `plugin_host/mod.rs:3256-3262` 注释与 `Running` 过滤属实；D5 重连 refetch、F3.3 每轮 read 属实。v5 两处改：(a) 预检未命中（`NotInstalled/NotRunning/NotExposed`）与作用域 `None` **不落行**，`enqueue` 返回 `Miss(reason)`，读端 `pending, reason`，不排队、不 spawn（一次性任务整个删除；出队时再遇到这些否定态也丢弃不落行）；(b) 真正调用失败写的 `unavailable` 行用 `SERIES_UNAVAILABLE_TTL = 2 min`，drain 准入按行状态选 TTL。改 D2 enqueue 2 / resolve 1-4、D3 TTL、D4 `Pending { reason }`、§2.8、seq 3b/3c/3f/4、A5/A9b/A9e、G19；§5 的 S2-先于-S3 段随之改。第 3 轮 R3-4 的「保留否定结果」由此变为「否定结果不落行」，已在第 3 轮该行标注 | D2、D3 live、D4、§2.8、seq 3b/3c/3f/4、A5/A9b/A9e、G19、§5 |
| 4 | A | m1 MINOR D2 上界「≤ 工作区块数（每块一个当前 hash）」过宽：写 h2 时 h1 的 job 仍在飞 | 采纳（随 codex R4-3 重写，编排者裁决 7）。键不含 hash 后上界改为「≤ 被读过且未出队的块数」 | D2 enqueue 3、D6 |
| 4 | A | m2 MINOR §2.8 例外清单漏「进程重启丢排队 job」 | 采纳。例外 (e) | §2.8 |
| 4 | A | m3 MINOR `{range:"1M", period:"month"}` 结构上永远 < 2 点 → 永久 `unavailable` 每 TTL 重投 | 采纳（进 S1）。核实：32 个历日至多含 1 个完整自然月；其它 17 个组合结构下限 ≥ 2（§5.1 表）。S1 拒绝该组合；A2 加用例与变异 | D1、§5.1 S1、A2 |
| 4 | A | m4 MINOR ifzq > 640 根截哪一端未测；若保留最早 N 根，分页第一页就停、近端静默缺失，内核清单抓不到 | 采纳（进 S3）。S3 先 spike（U8）、fixture 照实模拟；加对称检查「窗口内最晚日线 bar ≥ `as_of − 14d`，否则 `unavailable, reason:"no data near cutoff"`」；G3/G18 相应改（退市超 14 天落 `unavailable` 而非「可画但未钉住」）；A14 加用例与变异 | §2.5 S3 约束 4、§5.1 S3、A14、G3/G18、U8 |
| 4 | A | m5 MINOR 探测「最新 2 根」：us 实测 n=3 回 2 行含基准行，n=2 未测；只回基准行则 `complete_through = 2011` 永不钉住 | 采纳（进 S3）。探测 n=3 且要求至少一根非基准行，否则该条 `unavailable`；A14 加用例 | §2.5 S3 约束 1、A14 |
| 4 | A | m6 MINOR §11 末行「待编排者确认」应改已裁决；准入用 `write_in_tx_typed`（`BEGIN IMMEDIATE`）会与报告写入争单写者 | 采纳。核实 `write_in_tx` 用 `begin_immediate_tx`（`events.rs:872-874`、`infra.rs:15`）；只读先例 `sqlite_pool()` + pool 读（`settled.rs:148`）；WAL（`sqlite/mod.rs:279`）。F7.8 新增；D2 步骤 1 改读事务（`pool.begin()` DEFERRED）、步骤 7 才写事务；§11 第 3 轮末行改已裁决 | F7.8、D2 步骤 1/7、§5.1 S2、§11 第 3 轮末行 |
| 4 | A | m7 MINOR `std::sync::Mutex` poison：`InflightGuard::drop` 在 unwind 中 `lock().unwrap()` 二次 panic → abort；`lanes` poison 后读路径每次 panic | 采纳（进 S2）。两把锁一律 `unwrap_or_else(PoisonError::into_inner)`；A9d 顺带覆盖 | D2 enqueue 3、§5.1 S2 |
| 4 | A | m8 MINOR `track.report_edited` 同时失效报告与 series 查询，series 可能先带旧 `rev` 重取 → 409 闪错误态 | 采纳（进 S4）。核实 fe 的 `ApiError.failure.status`（`queries.ts:844`）可分辨 409。D5：409 = 等待态；A16b | D5、§5.1 S4、A16b |
| 4 | A | m9 MINOR lane 按插件跨 track 全局串行未登记 | 采纳。G20 | G20 |
| 4 | 修订者 | 附带（编排者可否决）：(1) `Only(other)`（G17，对该 track 永久）不在 A M2 的「不落行」清单里，v5 让它进 lane、由 resolve 步骤 3 写 `unavailable` 行——否则 fe 对永久条件轮询不止；同理路由命中后的 `ForgeAction` / 非只读 / `Http` 也落行。(2) 出队时再遇预检 / 作用域 / `connector_client` 的否定态一律丢弃不落行，与读端同一规则。(3) `Resolved::Pending { reason: Option<String> }`，fe 显示 reason。(4) R4-2 的代价 G21（live 股票日线滞后一个交易日、美股在亚洲白天少最后一根）是裁决 2 的直接后果，修订者如实登记而未削弱规则 | 已裁决（第 5 轮：通道 A §0 逐条复核四条附带与 G19 自洽 ✓、codex 复核 A M2「部分解决」的同胞分支在第 5 轮作为 R5-1 采纳；(4) 的 G21 由第 5 轮裁决 2 收窄为 live 周/月与加密日线） | D2 enqueue 2 / resolve 2-4、seq 3b/3f、D4、G21 |

### 第 5 轮（v5 → v6，计划中的最后一轮修订）

每条先到代码核实再处置；本轮无驳回。计数：codex 3 MAJOR + 3 MINOR + 1 venue 表（判 REVISE）；A 0 MAJOR + 7 MINOR（判 APPROVE）；两通道对编排者的 live 放宽提议各给逐市场判断，编排者取两家更保守者的并集为最终规则（裁决 2）。

| 轮次 | 通道 | 发现 | 处置 | 落点 |
|---|---|---|---|---|
| 5 | codex | R5-1 MAJOR 单条 series 瞬时失败被块级 6h TTL 锁住：两资产回复一条 `ok` 一条瞬时 `unavailable` → 通过校验 → 块级 `ok, pinned=false` → 6h；源立即恢复也不重试那条；A M2 的同胞分支 | 采纳（裁决 1）。核实 v5 D3「`ok` 行仍 6h」、D2 步骤 1 `TTL(row.status)` 只看行状态，构造成立。v6：`row_ttl(row)` 一个纯函数、读端与准入共用；任一 series 非 `ok` → `SERIES_UNAVAILABLE_TTL`，全 `ok` → `SERIES_TTL`；成功的 series 数据保留在行里；代价登记 G22 | D3 live、D2 触发 1 / 步骤 1、§2.8、seq 3d′、A9b (iii)、S2.7、G22 |
| 5 | codex | R5-2 MAJOR 编排者提议的 live 放宽经缓存复用重开 R4-1：23:59 frozen / 直读缓存了含 Binance 半根 D bar 的页、`observed_complete_through = D`；午夜后 live 截止日 = D 复用该页 → 「日期 ≤ 截止日」接受缓存里的盘中值；之后的时间不能让之前取到的字节定稿 | 采纳（裁决 2）。核实 v5 约束 2 缓存页只带 `observed_complete_through`、无日期键，构造成立。v6：缓存页键含取数 UTC 日期、永不跨午夜复用（对全部 venue 与 mode）；CRYPTO 任何 mode 严格（这条构造的 venue 本身被排除，日期键仍对股票 venue 关闭同形窗口）；frozen 保持探测顺序与严格纳入 | §2.5 约束 2/3、S3.4、A14 |
| 5 | codex | R5-3 MAJOR live / frozen 无 wire 判别：两态请求同形，frozen 截止日恰为昨天时与 live 不可分，而提议要求插件按 mode 选规则；从截止日反推 mode 会削弱 frozen | 采纳（裁决 2）。请求加 `mode: "live" \| "frozen"`（内核按 payload 是否有 `as_of` 填，不入指纹）；内核校验按 `(mode, period)` 分支；缓存行为随 S3.4 | D1 派生、D2 请求窗口 / 形状 / 步骤 6、seq 3、S2.8 / S2.9、S3.5 / S3.8 |
| 5 | codex | MINOR period_end 语义要在放宽下存活：若「日期」指存储的周一 / 1 日 `ts_ms`，周三的 live 周线请求会把周一至周二当完整周 | 采纳（裁决 3，进 S3）。截止比较用周期结束日、两层都查 `period_start ≥ start`；live 周/月负例进 A14、seq 3j′ | §2.5 约束 3、S3.6、seq 3j′、A12 / A14 |
| 5 | codex | MINOR 准入必须选当前 hash 的行：旧行故意保留（G8），h1 钉住后块改成 h2，按 `(track, block)` 查到 h1 → 永远压住 h2 | 采纳（裁决 3，进 S2）。核实 v5 步骤 1「读该 `(track_id, block_id)` 的行」、D4「按 `(track_id, block_id)` 的查询」都过宽。v6：先派生 h2 再按全主键精确选行，读端与 HTTP 路由同；A9h 用例「h1 钉住行保留 + h2 无行」 | D2 步骤 1、D4、D5、S2.5、A9h |
| 5 | codex | MINOR 预检后再插入 in-flight 不原子：两个读者都能穿过检查-插入空隙；`inflight.len() ≤ 未出队块数` 措辞错（执行中的 job 也持 guard） | 采纳（裁决 3，进 S2）。锁内 `HashSet::insert` 原子判定、只给插入成功者建 guard、预检未命中 guard drop 释放；上界句改「排队 + 执行中（每 lane ≤ 1）」；A9i 用 `hold_in_precheck` failpoint 确定性复现 | D2 enqueue 1/3、D6、S2.1、S2.12、A9i |
| 5 | codex | venue 表：US 常规时段 20:00/21:00 UTC 但盘后到 00:00/01:00 UTC、需证据源日线只含常规时段；HK 08:10、CN 07:00 历法截止成立但源定稿时刻不能只从交易所时钟推；Binance UTC `1d` 在边界后取才 sound | 采纳（裁决 2）。写进 §2.5 收盘时刻表并标「本仓未实测、交易所收盘 ≠ 源定稿」；US 登记 U9 spike、若吸收盘后回退严格；CRYPTO 严格 | §2.5 表、U9、S3.1 |
| 5 | A | §0 第 4 轮处置复核：全部由机制落地、修订者四条附带自洽 ✓ | 无改动；§11 第 4 轮末行改「已裁决」（m5） | §11 第 4 轮 |
| 5 | A | §1 live 放宽逐市场判断：US/HK/SH/SZ 日线常规时段成立且值得采纳（G21「周日看周四收盘」是真实产品缺陷）；CRYPTO 余量 0、放宽零收益只添偏斜窗口；live 周/月线会以新形态重开（周一 00:30 缺周五 bar）；需要 `mode` 判别；冻结不连续要写进 G21 | 采纳（裁决 2，与 codex R5-2 / R5-3 合并为最终规则：只有 `live ∧ day ∧ venue ∉ {CRYPTO}` 放宽）。核实 `Venue` 闭集含 `Crypto`（`plugins/market/main.rs:175-225`）、venue 由插件 `parse_asset` 判（F5.1）。落点按 A 列的清单：D2 请求形状加 `mode`、步骤 6 分支、§2.5 约束 3、§2.8 首条、seq 8 fixture 不再需要 `complete_through` 推进、A8 / A12 / A14 变异改为只对 frozen / 周月 / CRYPTO、G21 改写、U9 | §1、§2.5、§2.8、D2、seq 3l / 3j′ / 8、A8 / A12 / A14、G21、U9 |
| 5 | A | §2 攻击 v5（改源换 lane、出队时块已删、check-precheck-insert 交错、源挂死 + 200 块、`deadline_ms` 时钟、极端 `as_of`、跨源 fallback、缓存 min、`Only(other)` 2 min TTL）：未找到证伪构造 | 无改动。A 读 v5 的 in-flight 为「`insert` 在锁内原子」、codex 读为「先查后插」——v5 措辞两可，v6 按 codex 的构造写成显式原子 `insert`；跨源 fallback 的「同一 series 内不交叉」落为 m3 | D2 enqueue 1、S3.3 |
| 5 | A | m1 MINOR 行查询按全主键、默认摘要读不 SELECT `data`（G8 不 GC ⇒ 50 行 × ≤ 1 MiB 落在 CAS 必经 read 路径） | 采纳（裁决 4，进 S2 / S4）。与 codex 准入 MINOR 同源，一并改 D2 步骤 1 / D4 / D5 | D4、D5、S2.5 / S2.6、S4.1、G8 |
| 5 | A | m2 MINOR `repo.sqlite_pool()` 是 `Option`（trait 默认 `None`，`calm-truth/src/db/mod.rs:1236-1238`），D2 步骤 1 未定义 `None` 分支 | 采纳（进 S2）。核实属实，`state.rs:686` 透传。`None` → 丢弃 + warn、不落行 | D2 步骤 1、S2.3 |
| 5 | A | m3 MINOR 同一 series 的探测与取窗必须同源；切 Sina 兜底要重新探测；缓存页 `observed_complete_through` 按源记 | 采纳（进 S3） | §2.5 约束 2、S3.3、A14 |
| 5 | A | m4 MINOR D6「作用域允许」与 enqueue 步骤 2「`All \| Only(_)` 都进 lane」不一致 | 采纳（文档一句）：改「作用域非 `None`」 | D6 |
| 5 | A | m5 MINOR §11 第 4 轮末行「待编排者确认」 | 采纳：改「已裁决」 | §11 第 4 轮 |
| 5 | A | m6 MINOR `track.report_edited` 失效 series 前缀让每个块重取默认 `full`（≤ 1 MiB/块）即使 `rev` 未变 | 采纳（裁决 4，进 S4）。核实 `invalidation-plan.ts:291-294` 的 plan 是事件的纯函数、事件 data 只有 `track_id`（无块 id，F3.6）→ 「只失效 `rev` 变化的块」在 plan 层无载体；v6 选「不失效前缀 + key 含 `rev` + `staleTime`」；A21 | D5、S4.3、A21、G12 |
| 5 | A | m7 若采纳 §1 提议按其清单落点 | 随裁决 2 处理 | — |
| 5 | 编排者 | 收尾：§5.1 整理为编号清单；§9 只留 S3 spike 项 | 采纳。U2 / U5 移入 S2.15 / S4.3；U9 新增；U1 / U3 / U4 / U7 归入「已关闭」一行 | §5.1、§9 |
