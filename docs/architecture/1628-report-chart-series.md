# 声明式图表 `chart.series`（#1628）— 设计 v3

基线：`origin/main` = `c534bf6b`（工作树 `feat/report-chart-series`）。所有 file:line 都在该基线上实测（[实测]），未核实的写 "未核实"。v1 → v2 → v3 的每条改动登记在 §11。

**v2 的模型变化（用户 2026-09-12 拍板）**：解析是**后台任务**，`calm.report.read` 与浏览器只读**已存储的行**，读路径上永不调用插件。需要即时数据时 Planner 自己调 `market.series`。

**v3 的模型变化（编排者 2026-09-13 裁决）**：**解析的唯一触发是读**。v2 的 bus 订阅者被第 2 轮证明拿不到块 id（`TrackReportEdited` 只带平铺 `body_after`，`BlockSlice` 只有 `raw`，F3.6/F1.11），且 bus 有损、无重放、无启动扫描；与其让订阅者再去加载权威块，不如删掉它。写路径什么都不做；`calm.report.read` 与 HTTP GET 读到无行 / 过期行时 `enqueue`。理由：读者兜底本来就是保证，订阅者只是优化；Planner 每轮都 read（CAS 必经，F3.3），人打开报告时浏览器 `pending` 轮询 3s，两个读者都会在几秒内触发。同时 v3 修正了第 1 轮两条被证伪的处置（超时泄漏"上界为 1"、2 MiB "内存上界"），把 `as_of` 改为**截止日**语义并用 `complete_through` 做钉住判据，删掉写端的时钟检查。

## 1. 目标与非目标

**目标。** 让投研报告里的图表只*命名*数据与视图：Planner / 人写一个 `chart.series` 块（`{series, field, range, view, as_of?}`），内核在块被**读到**且无结果时投一个后台任务向 market 插件解析"资产 × 字段 × 区间 → 序列"并把结果存成一行；内核对每个这样的块提供两态（`as_of` 缺席 = live，截止日 = 解析时的昨天 UTC，按 TTL 随源流动；`as_of` 存在 = frozen，截止日 = `as_of`，源发布到截止日后钉住）；`calm.report.read` 对数据块返回存储行的 `resolved` 摘要（默认）或原始序列（按块显式要），read 是纯 DB 读、永不因插件失败而失败；前端 fe/ 用现有 SVG 路线画 line / normalized / bar / candles；钉住的行对人与 AI 字节相等。

**非目标。** 不在正文里发明宏语法；不给内核加行情源（数据仍由插件解析）；不把 `chart.candles` 的已存文档做数据迁移；不给 legacy `web/` 加新渲染器（它按现有规则显示 `unsupported block kind chart.series`，见 §4 D5）；不做缩放/联动/图表库懒加载；不做盘中序列（live 截止日 = 昨天 UTC，D2）；不做跨币种换算（每条序列自带 `currency`，normalized 视图天然可比，line 视图按原值画并标币种）；**不承诺 read 即时、不承诺有界新鲜度**（§2.8）；不在写时触发解析；不加固 MCP 传输层字节上界（#1634）。

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

结论：新 kind 只需进 `DATA_KINDS` + 一个 `validate_chart_series` + `kinds_table` 一项，上述 9 处入口全部经 `validate_payload` 自动覆盖。**`validate_chart_series` 只做形状校验**（F1.12：该 crate 无时钟，任何"与今天比"的检查都放不进这里）。

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
| F3.9 | `Event::TrackReportEdited { .. }` 的**生产构造点只有一处** `track_report/write.rs:1098`（`grep -rn "TrackReportEdited" crates/calm-server/src --include='*.rs'` 共 7 处：`write.rs:1098` 构造；`dispatcher/mod.rs:117/1142/1646` 与 `decision_sink.rs:1220` 是 match 消费；`track_report.rs:322`、`contracts.rs:506` 是描述文字）。模板（F2.5）、fork（F2.6）、recipe（F2.7）写入不经 `write.rs` 的这条路径——v2 订阅者对它们本就只能靠读者兜底 | [实测] |

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

S3 约束：腾讯 ifzq 为股票三市场主源（单端点、显式条数、复权；解析时 `qfqday`/`day` 两个键都要认、列序按 o,c,h,l,v 重排、按日期过滤）；Binance klines 为 crypto；新浪只作 A 股/美股兜底，**港股无兜底**（登记 G9，与 #1556 D3″ 同形）。插件**总是拉最新 N 根再按 `as_of` 过滤**，所以它知道源未过滤的最新 bar 日期——这就是 `complete_through`（D2）。

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

### 2.8 简化假设（用户："水合只需要给一个够用的简化就行"）

- 只做日线及以上（`period ∈ day/week/month`），不做盘中：**live 的截止日是解析时的昨天 UTC**，当天的 bar 永远不进 live 行（D2）。
- **解析在写后的第一次读触发，不在写时触发；写完立刻读到的是 `pending`。** 触发点只有两个读者（`calm.report.read`、HTTP GET），写路径不做任何事。
- **保证语句**：一个块在被读过之后，其 job 在该插件队列里排到时至多 30s（`tokio::time::timeout(SERIES_RESOLVE_TIMEOUT)`，D2 步骤 5）得到一行（`ok` 或 `unavailable`）；队列延迟 = 排在前面的 job 数 × 各自 ≤ 30s。**从未被读的块不解析。不承诺有界新鲜度**：过期行要等下一次读才重投，浏览器停轮询后过期行可见到下一次 fetch。
- 解析失败不重试风暴：失败行也按同一 TTL 才再投；同一键同一时刻至多一个任务（in-flight 去重）；job 出队时行已新鲜或已钉住 → 丢弃（drain 准入，D2 步骤 1）。
- 人与 AI 读同一行：摘要在写入行时由同一个 Rust 函数算好存下，两个读者只做序列化。**钉住的行**任何两个读者任何时刻字节相等；**未钉住的行**同一次读拿到同一行、两次读之间可被后台刷新替换，`resolved_at` 暴露这一点（D3）。
- 行写入不发事件：浏览器在 `pending` 时短轮询，其它时候靠既有失效与刷新（D5）。
- 内核不理解 venue、不理解交易日历、不做复权声明；这些归插件源。内核只比较日期字符串与 `ts_ms` 整数。

## 3. Oracle trace

Planner 写 `chart.series` → 内核校验落盘（不触发任何事）→ 某个读者读到无行 → `enqueue` → 任务调插件、写一行 → 前端/Planner 读那一行。事件 kind 均为真实 `Event` 变体。状态：✅ 今天已成立 / ⚠️ 本设计新增或改动 / ❌ 今天为假、本设计修正。**每个 ⚠️/❌ 行恰好一个切片**（"片"列）。状态词：块级只用 `resolved.status ∈ {ok, pending, unavailable}`；`unknown_asset` 是插件回复里**每条 series** 的状态词，只在 3d 出现、归 S3。

| seq | phase | actor | trigger / MCP tool | 效果 | 可观察事件 | 不变式断言 | 状态 | 片 |
|---|---|---|---|---|---|---|---|---|
| 1 | discover | Planner | `calm.report.blocks.kinds` | 返回含 `chart.series` 的 kinds 表 | 无 | `upsert.kind.enum == commit.ops.kind.enum == kinds_table.kinds`（`contracts.rs:695-711`） | ⚠️ | S1 |
| 2 | write | Planner | `calm.report.commit{ops:[{op:"upsert", kind:"chart.series", payload:{source:"neige://plugin/dev-neige-market/market.series", series:["US:NVDA","HK:9988"], view:"normalized", range:"1Y"}}], if_doc_rev, message}` | `validate_payload` 通过 → canonical fence 落盘，docRev+1；**事务里不调插件、不写 `report_series`、不 enqueue** | `CardUpdated` + `TrackReportEdited` 恰好各一 | `flatten(split_body(body))==body`；`parse_fence` 回同 payload；假插件 tools/call 计数 0 | ⚠️ | S1 |
| 2n | write-neg | Planner | 同上但 `series:["NVDA"]`（无 venue）/ 9 条 / `view:"candles"` 配 2 条 / `source:"https://…"` / `as_of:"2026/09/10"`（形状错） | `-32602` 字段级错误；不写不发事件 | 无 | 拒绝发生在 `validate_chart_series`（纯形状，无时钟），F2 的 9 个入口都经它 | ⚠️ | S1 |
| 3 | resolve | 内核任务 | drain 任务取出 job：准入（重读块、hash 仍等于当前 payload、行不存在或已过期且未钉住）→ `plugin_tool_entry` 命中 → `connector_client` 为本地变体 → `timeout(30s, tools_call("market.series", args{…, as_of, deadline_ms}, Some(track_id)))` | 回复通过校验清单 → 事务内 `INSERT … ON CONFLICT DO UPDATE … WHERE pinned=0` 一行 `status=ok`，`summary` 算好存下 | 无 | 同一 `(track,block,hash)` 同一时刻至多一个任务；写入受 FK 约束 | ⚠️ | S2 |
| 3a | resolve-admit | 内核任务 | 两个读者各对同一过期行 `enqueue`；第一个 job 写了新行后第二个 job 出队 | 第二个 job 重读行：`resolved_at` 在 TTL 内 → **丢弃，零次 tools/call** | 无 | in-flight 去重之外还有出队准入；TTL 在 drain 侧执行 | ⚠️ | S2 |
| 3b | resolve-neg | 内核任务 | (i) `source` 的插件未安装 / 未运行；(ii) 绑定 track 的 owner 不可用（`TrackPluginScope::None`，source 插件可能在跑） | (i) 写 `unavailable, reason="plugin dev-neige-market is not installed"` / `"… is not running"`；(ii) 写 `unavailable, reason="track owner plugin unavailable"` | 无 | 不发插件调用；TTL 后再投；reason 说的是真话（m8） | ⚠️ | S2 |
| 3c | resolve-neg | 内核任务 | 超时 / `isError`（含 S3 前的 `unknown tool`）/ 回复非 object | 写一行 `unavailable, reason`（≤256 字符） | 无 | 超时只影响该插件队列，读者不等；**超时后 `McpClient.responders` 里该 id 已被 guard 移除**（假插件永不回复，两次超时后 `pending_responders() == 0`） | ⚠️ | S2 |
| 3d | resolve-partial | 插件 | 请求 `series` 里有 `US:NOPE` | 回复 `series[j].status="unknown_asset", reason`，其它条 `ok`（各带 `complete_through`） | 无 | 插件不因一条未知资产拒绝整个请求 | ⚠️ | S3 |
| 3d′ | resolve-partial | 内核任务 | 收到 3d 的回复 | 内核块级 `ok`，`pinned=false`，`summary.series[j]` 保留 `unknown_asset` | 无 | 部分失败不拖垮整块，也不钉住 | ⚠️ | S2 |
| 3e | resolve-neg | 内核任务 | 回复超 `MAX_SERIES_REPLY_BYTES` / 资产不一一对应 / `ts_ms` 非严格升序或非 UTC 零点 / 非有限数 / bar 日期 > 请求 `as_of` / 点数 > 区间上界 / `ok` 项 `points.len() < 2` / `ok` 项缺 `complete_through` | 整行 `unavailable, reason` | 无 | 校验清单在内核边界（D2 步骤 6） | ⚠️ | S2 |
| 3f | resolve-neg | 内核任务 | `source` 指向不在 manifest 的工具 / `kind: ForgeAction` / `readOnlyHint != true` / `ConnectorClient::Http` / plugin_id 段含 `_`（`neige://plugin/a_b/c`） | 整行 `unavailable, reason`（reason 里的 plugin id 是 `source` 里的原字符串），**零次** tools/call | 无 | 精确查找 `plugin_tool_entry(registry, running, plugin_id, tool)`，不反解字符串；与 `plugin_tool_route` 的集合相等元测试（A19） | ⚠️ | S2 |
| 3g | resolve-deadline | 插件 | 出队时 `deadline_ms` 已过（内核早已超时） | 不打网络，回 `tool_error("deadline exceeded")`（内核侧 responder 已清，回复被 `mcp.rs:804` 的 else 臂 warn 丢弃） | 无 | 插件按自身时钟丢弃过期请求；fixture 源计数 0 | ⚠️ | S3 |
| 3h | resolve-fail | 内核任务 | 步骤 7 写行失败（测试 failpoint）/ drain 任务 panic（测试 failpoint） | in-flight 键随 RAII guard 释放；lane 在下一次 `enqueue` 时因 `send` 失败或 `JoinHandle::is_finished()` 重建 | 无 | 同键再 `enqueue` 仍会调插件（不 fail-locked 成永久 `pending`） | ⚠️ | S2 |
| 4 | read-pending | Planner | `calm.report.read{}`，该块尚无行 | `resolved.status="pending"`；read 顺手 `enqueue`（内存操作 + spawn，不写 DB） | 无 | read 期间假插件收到零次 tools/call；read 永远 200 | ❌→⚠️ | S2 |
| 4a | read-summary | Planner | 同上，行已存在 | `blocks[i].resolved` = 行里的 `status/as_of/resolved_at/pinned/summary` | 无 | 纯 DB 读，无超时无并发预算 | ⚠️ | S2 |
| 4b | read-full | Planner | `calm.report.read{resolve:{"b_x":"full"}}` | 该块附 `series[j].points`；未点名的仍 summary | 无 | `points` 来自同一行的 `data` | ⚠️ | S2 |
| 5 | render-fetch | 浏览器 | `GET /api/tracks/{id}/report/series/{block_id}?rev=<n>`（NEW 路由） | 返回同一行（默认带 points）；无行 → `pending` 并 enqueue | 无 | `rev` ≠ 当前块 rev → 409 `{current_rev}`，旧数据不会贴到新参数上 | ⚠️ | S4 |
| 5a | render | fe | `ReportSeriesBlock` | SVG line/normalized/bar/candles；`pending`/`unavailable` 各渲染 caption + 一行文字；`pending` 时 3s 轮询直到非 pending；未钉住的 frozen 图标 "source data through <complete_through>" | 无 | 不含字面颜色；normalized 首点 ≤ 0 的序列显式标"不可归一化" | ⚠️ | S4 |
| 6 | live-stale | 任一读者 | 读到 live 行 `resolved_at < now - 6h` | 返回旧行 + `enqueue`（stale-while-revalidate） | 无 | 正在跑的键不再投（in-flight 去重） | ⚠️ | S2 |
| 7 | freeze-write | Planner | `commit` 写 `as_of:"2026-09-06"`（周日） | 存 fence；同 seq 2 | 同 seq 2 | 内核只查 `YYYY-MM-DD` 形状；不与今天比较 | ⚠️ | S1 |
| 7a | freeze-pin | 内核任务 | 回复每条 `ok` ∧ 每条末点日期 ≤ `as_of` ∧ 每条 `complete_through >= as_of` | 写行 `pinned=true` | 无 | 之后 enqueue 对该键是 no-op；`DO UPDATE … WHERE pinned=0` 拒绝覆盖 | ⚠️ | S2 |
| 7b | freeze-hold | 源 | 钉住后假插件换数据、TTL 过期 | 行**不变**，`resolved` 仍是钉住的数据 | 无 | 两个并发首解析竞争者只有一个能写、之后谁都不能覆盖 | ⚠️ | S2 |
| 7c | freeze-rehash | Planner | 改 `range`/`series`/`as_of`/`source`（hash 变）或 `view` 在 `candles` 与其它之间切换（`fields` 变 → hash 变）；改 `caption`/`overlays` 或 `view` 在 `line↔normalized↔bar` 之间切换（hash 不变） | 前者新行、旧行留到 track 删除；后者复用原行 | `CardUpdated`+`TrackReportEdited` | 行身份 = `(track,block,request_hash)`，不是 rev；指纹含 `plugin_id, tool` | ⚠️ | S2 |
| 7d | freeze-incomplete | 内核任务 | frozen 块回复里有 `unknown_asset`，或某条 `complete_through < as_of`（源最新 bar 是周五、`as_of` 是周日） | 写行 `ok, pinned=false`；TTL 后再投；源发布下一个交易日的 bar 后 `complete_through ≥ as_of` → 钉住 | 无 | 不完整快照永不钉住；**最多延迟到下一个交易日** | ⚠️ | S2 |
| 8 | live-drift | 假插件 fixture | live 行过期后 fixture 多给一根（日期 ≤ 昨天 UTC 的）bar | 新行 `summary.last` 日期前进；docRev **不变** | 无 | 用受控 fixture 断言，不用日历时间 | ⚠️ | S2 |
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
  "range":   "1Y",                                   // 可选，1M|3M|6M|1Y|2Y|5Y，默认 1Y
  "period":  "day",                                  // 可选，day|week|month，默认 day
  "view":    "line",                                 // 可选，line|normalized|bar|candles，默认 line；candles 要求 series.len()==1
  "as_of":   "2026-09-10",                           // 可选，截止日。只查 YYYY-MM-DD 形状（四位-两位-两位，月 01-12、日 01-31），
                                                    // 不查历法、不与今天比较；存在 = frozen；缺席 = live
  "overlays": ["ma20"],                              // 可选，ma20|ma60，只对 line/candles 生效
  "caption": "…"                                     // 可选
}
```

- 不允许 inline 数据：数据只由 `source` 解析。要内联的数据继续用 `chart.candles`。
- `series` 去重按字面；`HK:9988` 与 `HK:09988` 在插件里是同一身份（F5.1 `canonical_symbol`），**内核不折叠**（不复制插件的 venue 规则），插件回复里两条同资产序列由校验清单的"资产一一对应"规则接受（回复 `asset` 必须逐项等于请求字符串）。
- **`as_of` 是截止日（cutoff），不是"最后一根 bar 的日期"**：插件返回所有 bar 日期 ≤ `as_of` 的点；每条序列的末点日期可以早于 `as_of`（周末、假日、停牌）。**没有 `as_of` 上界**：写端不与今天比较（F1.12 该 crate 无时钟，§11 A M2）；未来日期的 `as_of` 只是"源尚未发布到截止日"的 frozen 块——`complete_through < as_of` → 未钉住，按 TTL 重投，到期后自动钉住（D3）。v2 担心的"`as_of:"2099-01-01"` 让钉住退化成钉在第一个任务跑的时刻"由 `complete_through` 判据消解，不需要时钟。fe zod 对未来日期可**提示**，不拒绝。
- `chart.candles` **不收编、不迁移**：已存文档、两个前端各有渲染器（F6.1、F6.8）、有校验与集成测试。折衷：`kinds_table` 里 `chart.candles` 的 usage 改为"内联数据的逃生口；行情能由插件解析的标的请用 `chart.series`"（F2.12 那句删掉）。**这是与 issue 方向 1 的出入**（§8）。
- caps：`MAX_CHART_SERIES = 8`（新常量，放 `kinds.rs:46-55` 旁）。点数上界不再复用 `MAX_CHART_CANDLES`，改为按区间自然大小（D2）。
- 校验落点：`kinds.rs` 新 `validate_chart_series`（挨着 `validate_chart :483`，**纯形状函数，无时钟**），`DATA_KINDS` 变 5 项，`KIND_CHART_SERIES` 常量；`contracts.rs::kinds_table` 加一项并改 F2.11 两段手写文字；`fe/core/domain/report.ts` 加 `chartSeriesPayloadSchema` + `payloadSchemaFor` 分支；`web/` 不加（落 opaque，F6.8）。
- Rust 侧派生（calm-server）：`SeriesRequest::from_payload(&Value) -> (SeriesRequest, request_hash)`，`fields` 由 `view` 派生（candles → `[open,high,low,close,volume]`，否则 `[field]`）；**`request_hash = hex(sha256(canonical_json({plugin_id, tool, series, fields, range, period, as_of})))`**——`plugin_id, tool` 入指纹（§11 R2-3：换源必须换行，否则 P 的结果会钉在 Q 的声明下）；`as_of` 是 payload 里的值（live 缺席，**live 的运行时截止日不入指纹**，否则每天一行）。`caption/overlays` **不入指纹**（表现层）；`view` 只通过 `fields` 间接入指纹：`line↔normalized↔bar` 互换 `fields` 不变 → 复用行，`candles` 与其它互换 → 换行（§11 R2-MINOR / A m1）。

**依据。** F1.1-F1.6、F1.4、F1.12、F5.1、F6.5/F6.8。

### D2 解析任务：内核如何、何时向插件要序列

**问题。** v1 在 read 里同步调插件；v2 用 bus 订阅者在写后触发，但订阅者从 `body_after` 拿不到块 id（F1.11、F3.6），且 bus 有损无重放（F3.7），模板/fork/recipe 写入根本不发该事件（F3.9）。

**备选。** (a) 保持 read 内同步解析。 (b) 写后 bus 订阅者 + 读者兜底。 (b′) 订阅者改为收到事件后加载权威块快照再 enqueue。 (b″) **只由读触发**。 (c) 插件预推全部历史成 overlay。

**裁决：(b″)。** (b′) 被否：它让订阅者做读者已经在做的事（加载快照、算 hash、比行），而 bus 的丢失/重启/无启动扫描三个问题一个都没解决，最终仍靠读者兜底；(b″) 删掉这层，保证语句反而更诚实（§2.8）。机制：

- **触发**（两处，都只 `enqueue`，不调插件、不写 DB）：
  1. `calm.report.read`（D4）：对每个 `chart.series` 块，读行；无行 / 过期 live 行 / 过期 `pinned=false` 行 → `enqueue` 并返回现状。
  2. HTTP GET（D5）：同上。
  3. 写路径不做任何事；不做启动扫描、不做定时扫描。
- **`enqueue(track_id, block_id, request)`**（NEW `SeriesResolver`，挂在 `AppContext`）：
  1. `inflight: Mutex<HashSet<Key>>`，`Key = (TrackId, BlockId, RequestHash)`；已在集合 → no-op。否则插入并构造 `InflightGuard { set, key }`（`Drop` 时 `remove`）随 job 走——job 无论正常完成、提前 `return Err`、panic 展开、还是随 lane receiver 一起被 drop，键都释放（§11 A M4 构造 1）。
  2. 路由预检 `plugin_tool_entry(registry, &running_ids, plugin_id, tool)`（见步骤 2）：**命中** → 该插件的 lane；**未命中** → 不建 lane，`tokio::spawn(resolve(job))` 一次性任务（`resolve` 步骤 2 会再次未命中并写 `unavailable`）。所以 lane 只为路由命中的插件存在，一篇写了 1000 个不存在 plugin_id 的报告只产生 1000 个短命写行任务、零条 lane（§11 A m6）。
  3. lane：`lanes: Mutex<HashMap<PluginId, Lane { tx: mpsc::Sender<Job>, drain: JoinHandle<()> }>>`，首次命中时 `tokio::spawn` drain 循环；`enqueue` 时若 `drain.is_finished()` 或 `tx.send()` 失败（receiver 已随 panic 掉的 drain 一起 drop）→ 重建 lane 再 send（§11 A M4 构造 2）。**按插件串行**——与 market 插件自身的单工作线程（F4.13）同构。测试 seam：`SeriesResolver::new_unstarted()` 只记录不 drain（A10 用）；`#[cfg(test)] failpoints { fail_write_once, panic_drain_once }`（A9c/A9d 用）；`now: fn() -> i64` 注入（默认 `calm_truth::model::now_ms`，A8 用）。
- **任务 `resolve(job)`**：
  1. **准入**：重读块（`load_report_read_snapshot`）与行。块不存在 / 当前 payload 的 hash ≠ job 的 hash → 丢弃（stale job）。行存在且（`pinned = 1` 或 `resolved_at ≥ now − SERIES_TTL`，含 `unavailable` 行）→ 丢弃（§11 R2-5：TTL 在 drain 侧执行，不信任读者的旧观察）。
  2. **路由**：NEW `pub(crate) fn plugin_tool_entry(registry: &PluginRegistry, running_ids: &BTreeSet<String>, plugin_id: &str, tool: &str) -> ToolEntry` 放在 `transport.rs` `plugin_tool_route` 旁；实现 = `registry.get(plugin_id)`（F4.16 精确查找，**不拼 `plugin.{id}_{tool}` 再反解**）→ `manifest.exposes_tools.iter().find(name == tool)` → `running_ids.contains(plugin_id)`。返回枚举 `NotInstalled | NotRunning | NotExposed | Found(ExposedTool)`（一个布尔装不下四种结论）。`NotInstalled` → `unavailable, reason="plugin <id> is not installed"`；`NotRunning` → `"… is not running"`；`NotExposed` → `"… does not expose <tool>"`；`Found(entry)` 且 `entry.kind.is_some()` → `"tool is not an ordinary read-only tool"`；`entry.annotations["readOnlyHint"] != true` → 同上。**G7 由此关闭**（未安装/未运行/未暴露三态可分）。`plugin_tool_route` 不改（不需要 `pub(crate)`）；S2 加一条元测试：对 fixture registry 里每个 `(id, tool)` 与若干不存在的组合，`plugin_tool_entry(...) is Found` ⇔ `plugin_tool_route(registry, "plugin.{id}_{tool}", running) == Ok(Some((id, tool, kind)))`（A19）。
  3. **作用域**：`plugin_scope_for_track(ctx, Some(track_id)).allows(plugin_id)`（与 agent 路由同一规则 F4.11）。`TrackPluginScope::None`（绑定 track 的 owner 不可用）→ `unavailable, reason="track owner plugin unavailable"`（不是"plugin X is not running"——source 插件可能正在跑，§11 A m8）；`Only(other)` → `unavailable, reason="plugin <id> is outside this track's plugin scope"`。
  4. **客户端**：`connector_client(plugin_id)`：`None` → `unavailable, "not running"`；`Http(_)` → `unavailable, reason="remote connectors are not series sources"`（远端服务不该由文档内容驱动被内核请求，§11 B1 构造 2）；`Stdio(c)` → `c.tools_call(tool, args, Some(&track_id))`；`Cli(c)` → `c.tools_call(tool, args)`。
  5. **超时与 responder 清理**：`tokio::time::timeout(SERIES_RESOLVE_TIMEOUT = 30s, …)`。理由：后台执行、按插件串行，长超时的代价是该插件队列的延迟而不是任何读者的等待；8 条资产逐条打腾讯/Binance 各 ≤3s 的最坏情况在 30s 内。**超时不再泄漏 responder**：S2 在 `McpClient::call` 里、`responders.insert`（`mcp.rs:667`）与 `rx.await`（`:676`）之间加一个 `ResponderSlot { map: &ResponderMap, id }` RAII guard，`Drop` 时 `map.lock().remove(&id)`（对端已回复时 `:804` 已 `remove`，再 `remove` 是 no-op）；超时取消 = future 被 drop = guard 被 drop = 槽位移除。这是对**所有**内核→插件调用的修复（agent 路由 `transport.rs:722-738`、`cards.rs:588` 一并受益）；`#[cfg(test)] pub(crate) fn pending_responders(&self) -> usize` 给 A5b 用。v2 的"串行化把泄漏封顶为 1"是假的：永不回复的插件下每次超时留一个槽位，跨块、跨 TTL 周期无界累积（§11 R2-1，第 1 轮处置作废）。**插件侧**：内核超时不通知插件取消（登记 G11），请求仍留在插件单工作线程队列里按序处理（F4.13）；缓解在 S3——请求带 `deadline_ms`（内核 `now_ms() + 30_000`），`market.series` 出队时若自身时钟已过 `deadline_ms` → 不打网络，回 `tool_error("deadline exceeded")`（seq 3g）。
  6. **回复校验清单**（内核边界，任一不过 → 整行 `unavailable, reason`）：`isError != true`；`structuredContent` 是 object；序列化整个 `CallToolResult` ≤ `MAX_SERIES_REPLY_BYTES = 2 MiB`（**这是内核接受并存储的回复上限，是校验，不是内存上界**——传输层 `read_line` 在此之前已把整行读进内存，字节上界见 #1634 / G11）；`series` 数组长度 == 请求长度且第 j 项 `asset` == 请求第 j 条（一一对应）；每项 `status ∈ {ok, unknown_asset, unavailable}`；`unknown_asset`/`unavailable` 项的 `reason` 是字符串（存储时截到 256 字符）；`ok` 项：`complete_through` 是 `YYYY-MM-DD`；**`points.len() >= 2`**（图与摘要都需要两点；空或单点由插件自己报 `unavailable, reason:"no data in range"`，内核收到 `ok` 配 <2 点视为 malformed，§11 R2-9）；每点长度 == 1 + `fields.len()`；`ts_ms` 是整数、**`ts_ms % 86_400_000 == 0`（= 交易日 UTC 零点，§11 A m11）**、严格升序；数值全部有限（`f64::is_finite`）；每点日期（`ts_ms / 86_400_000` 折成 `YYYY-MM-DD`）≤ **请求 `as_of`**（两态统一，因为 live 请求也带 `as_of`）；`complete_through` ≥ 末点日期；点数 ≤ `max_points(range, period)`（区间日历天数 / period 天数 + 2：1Y day = 368、5Y day = 1829、5Y week = 263、5Y month = 62）；存储 `data` 序列化 ≤ `MAX_SERIES_ROW_BYTES = 1 MiB`。
  7. 写行（D3），`summary` 由 `summarize(&Series) -> Summary` 在写入前算出并一起存。写失败（FK、IO）→ warn 并返回；键由 guard 释放。
  8. （无显式清理步骤：`InflightGuard` 在 job 作用域结束时 drop。）
- **请求截止日**：内核在每个请求里显式填 `as_of`：frozen = payload `as_of`；**live = 昨天 UTC**（`yesterday_utc(now_ms)`，calm-server 用 `chrono` 算，F7.7）。插件对两态走同一条过滤路径，不再有"无 `as_of`"的请求形状；当天的盘中半根 bar 永远不进 live 行（§11 A M3）。代价：CN/HK 市场按 UTC 日期可能多滞后一天（G14）。
- **请求形状**（内核由 payload 派生）与**回复形状**（`structuredContent`）：
  ```jsonc
  // 请求（frozen 与 live 同形；live 的 as_of 由内核填昨天 UTC）
  { "series": ["US:NVDA","HK:9988"], "fields": ["close"], "range": "1Y", "period": "day",
    "as_of": "2026-09-10", "deadline_ms": 1789000000000 }
  // 回复（无顶层 as_of——回显请求值是空洞检查，§11 A M1）
  { "series": [
      { "asset": "US:NVDA", "currency": "USD", "status": "ok",
        "complete_through": "2026-09-11",           // 源未过滤的最新 bar 日期（插件总是拉最新 N 根再过滤）
        "points": [[ts_ms, close], …] },             // ts_ms = 交易日 UTC 零点；全部日期 ≤ 请求 as_of；≥ 2 点
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
    summary      TEXT    NOT NULL,                 -- JSON，写入时算好
    data         TEXT,                             -- JSON：series[] 含 points，ok 时
    PRIMARY KEY (track_id, block_id, request_hash)
);
```

- **外键 vs 事务内存在性校验**：选外键 `ON DELETE CASCADE`。理由：`foreign_keys = ON` 每连接、同形先例 0104/0106（F4.15）；删除时 `track_delete_tx` 级联清掉行，迟到的任务写入在约束处失败（任务 warn 并丢弃，键由 guard 释放），没有"先查后插"的窗口。
- **写入**：任务在 `write_in_tx_typed` 里 `INSERT … ON CONFLICT(track_id, block_id, request_hash) DO UPDATE SET status, reason, as_of, resolved_at, pinned, summary, data = excluded.* WHERE report_series.pinned = 0`。`pinned` 行在 DB 层不可覆盖（A7 的变异靶点）。不发事件（F7.2 不 bump）。
- **`pending` 不是行的状态**：无行 = `pending`（读时词）。行的 `status` 只有 `ok | unavailable`。
- **live**：payload `as_of` 缺席。每次解析的截止日 = 昨天 UTC（D2），存进 `as_of` 列。TTL = `SERIES_TTL = 6h`：日线一天一根、收盘后源才更新；6h 让一天内被反复打开的报告最多刷新 4 次（读触发 + drain 准入共同保证，D2 步骤 1），一天开一次的报告刷新一次，且过夜后第一次打开必刷新。失败行同一 TTL。
- **frozen**：payload `as_of` 存在；请求带它；插件只返回日期 ≤ `as_of` 的 bar。**钉住条件**（内核可验证，不依赖插件回显）：每条 series `status == ok` **∧** 每条末点日期 ≤ `as_of`（校验清单已保证）**∧** 每条 `complete_through >= as_of`（源已发布截止日当日或之后的数据 ⇒ "≤ as_of 的集合"是终态）→ `pinned = 1`，此后 `enqueue` 对该键 no-op、写入被 `WHERE pinned = 0` 拒绝。不满足 → `pinned = 0` 的 `ok` 行（图能画，但标"未钉住，source data through <min complete_through>"），按 TTL 再投直到满足。**非交易日**：`as_of` = 周日、源最新 bar = 周五 → `complete_through`(周五) < 周日 → 未钉住；周一收盘出 bar 后 `complete_through`(周一) ≥ 周日 → 下次 TTL 重投钉住，数据仍是"≤ 周日"即到周五——**钉住最多延迟到下一个交易日**（§11 A M1 / R2-8）。并发首解析：串行队列 + in-flight 去重使同键同一时刻只有一个任务；即便测试直接并发调用两次 `resolve`，第一个写成 `pinned=1` 后第二个的 `DO UPDATE … WHERE pinned=0` 是 no-op。
- **身份**：`(track_id, block_id, request_hash)`。改 `caption/overlays`、`view` 在 `line↔normalized↔bar` 间切换 → 不换行；改 `series/range/period/as_of/source`、`view` 与 `candles` 互换 → 换行，旧行留到 track 删除（G8）。
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
  enum Resolved { Pending, Unavailable { reason, resolved_at }, Ok { as_of, resolved_at, pinned, summary: Summary, points: Option<…> } }
  ```
  序列化摊平：
  ```jsonc
  { "status": "ok" | "pending" | "unavailable",
    "reason": "…",                     // unavailable（≤ 256 字符）
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
- **读路径零插件调用、零 DB 写**：read 只做 `load_report_read_snapshot` + 一次 `report_series` 按 `(track_id, block_id)` 的查询 + 可选 `enqueue`（内存集合 + 路由预检 `registry.get` / `running_plugin_ids().await` + spawn）。无超时、无并发预算。`load_report_read_snapshot` 的错误仍是 internal（文档本身读不到，与今天一致）。
- **谁能拿 `resolved`**：Planner 与 Assistant 都拿。裁量按报告内容可见性：解析由内核固定身份在后台执行（先例 F4.10：内核调用不经 `PLUGIN_TOOL_ROLES`），工具被 D2 限定为 manifest 内只读工具，结果是存进 `report_series` 的报告内容，与 `text` 里的 fence 同一可见级。`PLUGIN_TOOL_ROLES = [Planner, Worker]`（F4.9）约束的是 **agent 以自己身份发起** tools/call；这里没有 agent 身份发起的调用。Assistant 能通过写块让内核调只读工具——这与它今天能写 live `table` 的 `source` 同级（写权已在 `[Planner, Assistant]`，`track_report_blocks.rs:121-296`），且它拿到的只是 Planner 也能直接调工具拿到的东西。副作用登记 G15：Assistant 可经 `unavailable.reason` 读到只读工具的错误文本（≤ 256 字符）。`taskDiagnostics` 的划线（F3.3）不动。
- token 体量估算不变：summary 每条 ≈ 50 token；full 1Y 日线 ≈ 1.5k token/条、candles ≈ 3.1k、5Y ≈ 7.5k/15.7k——`full` 只按块显式索取。

**依据。** F3.3（read 是 CAS 必经路径 → 纯 DB 读）、F3.5、F4.9/F4.10（角色门的适用边界）。

### D5 前端渲染

**裁决。**
- fe/：NEW `fe/web/src/features/report/series/public.tsx` + `series.module.css`，沿 F6.1 的 SVG 路线：`line`（多序列各一条 `<polyline>`，token 上色）、`normalized`（每条 rebase 到首点=100；**首点 ≤ 0 的序列不画线，图例处标 "cannot normalize (first value ≤ 0)"**，其余序列照画）、`bar`（单序列柱，多序列分组柱，≤8）、`candles`（把 `candles/public.tsx:89-190` 的绘制体抽成 `CandlesFigure({candles, overlays})` 共用）。区间切换不再客户端过滤；图上显示 `range`/`as_of`/币种/`pinned` 标记；frozen 且 `pinned=false` 时标 "not pinned — source data through <min complete_through>"。
- 数据：NEW 路由 `GET /api/tracks/{id}/report/series/{block_id}?rev=<n>&detail=full|summary`（默认 `full`；`routes/track_report_series.rs`），Principal 鉴权同其它 track 路由。服务端：读快照找块；`rev` 与当前块 `rev` 不等 → **409** `{current_rev}`；无块 → 404；算 hash → 读行；无行/过期 → `enqueue` 并返回现状。返回 D4 的 `resolved`（`utoipa` schema → 两份 OpenAPI 重生成，F6.9）。**用 `rev` 不用 hash 绑定**：fe 在不安全上下文里没有 sha256（F6.10），而 `rev` 已在 `ReportBlock` 里且任何 payload 改动都会 bump（同内容不 bump，与 hash 不变一致）。
- fe 查询：`trackReportSeriesQueryOptions(trackId, blockId, rev)`，key `['track-report-series', trackId, blockId, rev]`；`invalidation-plan.ts` 的 `track.report_edited` 追加 `['track-report-series', trackId]`；响应 `pending` 时 `refetchInterval` 3s（2 分钟后退到 30s），非 pending 停止；过期行由服务端刷新后要等下一次 fetch（焦点/失效）才可见（G12）。**浏览器打开报告 = 一次读 = 一次 enqueue**：这就是 v3 里人这一侧的触发。
- 非 ok 态：`pending`/`unavailable` 渲染 caption + 一行文字（复用 table 的 `LiveTableNotice` 形态，F6.3）；查询 loading 渲染 caption + "Loading …"。
- zod：`as_of` 只查 `YYYY-MM-DD`；未来日期可在编辑器里**提示**"will pin once the source publishes through this date"，不拒绝（与内核一致，D1）。
- legacy `web/`：不加渲染器（F6.8）；两份 OpenAPI 仍因新路由一起重生成。
- 不引入 Recharts/lightweight-charts。

### D6 安全

- `source` 只接受 `neige://plugin/<id>/<tool>`；内核从中只取 `plugin_id`/`tool` 做 `registry.get` 精确查找，**从不**构造 URL、从不拼名字反解；只调 manifest 内、`kind == None`、`readOnlyHint == true` 的工具；只走本地 `Stdio`/`Cli` 变体，`Http` 拒绝（D2 步骤 2-4；这是对 §11 B1/C1 的回答）。
- `series` 字符串经内核形状检查后作为 tool 参数原样交给插件，插件用 `parse_asset` 再判；内核不解释 venue。
- 解析以内核身份在后台执行；作用域 `plugin_scope_for_track` 与 agent 路由同一规则。
- 读路径：HTTP 路由走既有 Principal；MCP read 走 `resolve_report_for_caller`；两者都不调插件、不写 DB。
- 资源约束（每条指到执行它的机制）：8 条序列（`validate_chart_series`）；每插件串行（lane）；30s 超时（`tokio::time::timeout`）且超时清 responder（`ResponderSlot` guard）；2 MiB 是**接受上限**（步骤 6 校验，不是传输层内存上界——那是 #1634）；区间上界点数、`points.len() ≥ 2`、`ts_ms` 零点（步骤 6）；1 MiB 行、`reason` 256 字符（写入前截断）；in-flight 去重 + drain 准入 TTL；lane 只为路由命中的插件建；插件侧 `deadline_ms` 丢弃。
- 插件 `market.series` 只读，不写 overlay/KV；不新增 `neige.*` 回调，不扩权限模型；不新增 Event kind。

### D7 与 #1612 `layout` 的取舍

保留：`neige://plugin/<id>/<x>` 作为唯一外部引用语法；"配置声明式、无表达式求值"；unit/currency 不做隐式换算；"缺数据显式态、零不是缺"。
放弃：一个 kind 承载布局 + 多 item + 表 + 图；数据来自 overlay 行再做 join/annotations；Recharts 与颜色 `#RRGGBB` 进 payload；模板化的 selector/total/share 计算。
参照材料（`report-layout-contract.md`、`layout.rs`、`portfolio-template-review.md`）**不在基线树**（`find` 零命中，仓内 `layout.rs` 只有 `dedicated_codex/layout.rs`）；它们是 #1612 讨论的附件，本节只取其精神，不引用行号。

## 5. 切片表

| 片 | 内容 | 依赖 | 可独立合入 | 行为变化 | 估算行数 |
|---|---|---|---|---|---|
| S1 契约 | `kinds.rs` `KIND_CHART_SERIES` + `validate_chart_series`（纯形状；`as_of` 只查 `YYYY-MM-DD`）+ `MAX_CHART_SERIES`；`DATA_KINDS` 5 项；`kinds_tests.rs` 正反例；`contracts.rs` kinds_table 项 + F2.11/F2.12 文字；`fe/core/domain/report.ts` zod + `payloadSchemaFor`；`document/public.tsx` `case 'chart.series'` 占位；`mcp_track_report_blocks.rs` 加入口拒绝用例 | 无 | 是 | agent 可写 `chart.series`，read 无 `resolved`，fe 占位，web unsupported | ~550 |
| S2 解析任务 + 存储 + read 水合 | 迁移 `report_series`；`SeriesRequest`/`request_hash`（含 plugin_id, tool）/`summarize`/`yesterday_utc`；`SeriesResolver`（`InflightGuard`、lane + `JoinHandle` 监督、drain 准入、`resolve`、校验清单、写行、pinned 规则、failpoints）；`plugin_tool_entry` + 与 `plugin_tool_route` 的元测试；**`McpClient::call` 的 `ResponderSlot` guard + `pending_responders()`**；`calm.report.read` `resolve` 入参 + `resolved`（chart.series 与 live table）+ enqueue；fork 复制全部行；集成测试用假插件（`boot_plugin_host`）覆盖 seq 3/3a/3b/3c/3d′/3e/3f/3h/4/4a/4b/6/7a/7b/7c/7d/8/9/12 | S1 | 是 | Planner/Assistant read 到摘要；插件缺席时 `pending`→`unavailable`；所有内核→插件调用超时后不再留 responder | ~950 |
| S3 market 插件 | `market.series` tool（manifest、`tools_call_reply` 分支、腾讯 ifzq + Binance klines 源、新浪兜底、内存缓存、统一的 `as_of` 截止日过滤、`complete_through`、`deadline_ms` 出队丢弃、`unknown_asset`、<2 点报 `unavailable`、`ts_ms` 折到 UTC 零点）、README、fixture server 测试 | S1（只共享 wire 形状；与 S2 并行） | 是 | `plugin.dev-neige-market_market.series` 对 agent 可用；S2 合入后图有数据 | ~950 |
| S4 路由 + fe 渲染 | `routes/track_report_series.rs`（rev 绑定、409、enqueue）、两份 OpenAPI 重生成、`queries.ts` 查询 + 失效 + pending 轮询、`features/report/series/`、`CandlesFigure` 抽取、未钉住标记、browser 测试 | S2 | 是 | 人看到图；S3 未合时看到 `unavailable` 文案 | ~800 |

顺序：S1 → (S2 ∥ S3) → S4。总规模 ≈ 3250（v2 ≈ 3300、v1 ≈ 3900）：S2 删订阅者（−~150）、加 responder guard / 准入 / lane 监督 / `plugin_tool_entry`（+~100）；S1 删时钟检查；S3 加 `complete_through`/`deadline_ms`。S2 与 S3 的接缝是 D2 的请求/回复 JSON，两边共用 `crates/calm-server/tests/fixtures/market_series_reply.json`。S2 先于 S3 合入时，已装的 market 插件对 `market.series` 回 `unknown tool`（F4.13）→ `isError` → 行 `unavailable, reason: "plugin error: unknown tool `market.series`"`（§11 C16）。S2 合入后 #1634 只剩传输层字节上界一项（F4.17）。

## 6. 验收场景与 must-red 变异

每条变异**单跑**必转红。

| # | 场景 | 断言 | must-red 变异（改哪一行 → 哪条测试转红） |
|---|---|---|---|
| A1 | `upsert{kind:"chart.series"}` 合法 payload 落盘为 canonical fence | read `text` 含 fence，`parse_fence` 回同 payload | `validate_chart_series` 把 `series` 必填改成可选 → `kinds_tests::chart_series_payload_valid_and_invalid` 的 "series: required" 断言红 |
| A2 | 经 `calm.report.commit` 写无 venue 的 `series:["NVDA"]` / `as_of:"2026/09/10"`（形状错）；**`as_of:"2099-01-01"` 被接受** | 前两者 `-32602`，docRev 不变，事件零；后者 200 落盘 | **直接测入口**：`validate_chart_series` 删掉 venue 正则 → `mcp_track_report_blocks::commit_rejects_chart_series_without_venue`（S1 新增，走真实 MCP 入口）红；给 `validate_chart_series` 加一条硬编码的 `year > 2026 → Err`（模拟任何"与今天比"的检查）→ `commit_accepts_future_as_of` 红。不再用"跳过 `render_data_block`"做变异：F2.2 的 op 层复核会让那种变异保持绿 |
| A3 | kinds 表、upsert enum、commit enum 三者含 `chart.series` 且相等 | `contracts.rs:695-711` | `block_kind_enum()` 硬编码四项 → 既有测试红 |
| A4 | 行存在时 read 默认给 summary，`n/first/last/change_pct/high/low/as_of/complete_through` 与 fixture 一致 | 集成测试比对 fixture 期望 | `summarize` 里 `change_pct` 用 `(last-first)/last` → `read_hydrates_chart_series_summary_from_row` 红 |
| A5 | 插件未安装 / 未运行 / 超时 / `isError` → 行 `unavailable`，reason 各不同；read 仍 200 | 四条用例，直接调 `resolve` | `resolve` 删掉 `tokio::time::timeout` 包裹 → `resolve_marks_a_hung_plugin_unavailable`（永不回复的假插件 + 测试超时 40s）红（挂死） |
| A5b | **超时不泄漏 responder**：永不回复的假插件，`timeout(1s, client.call(…))` 两次 | 两次都 `Err(Elapsed)`；`client.pending_responders() == 0` | `ResponderSlot` 的 `Drop` 改成空实现 → `timed_out_calls_leave_no_responder`（`plugin_host/mcp.rs` 单元测试）红（读到 2） |
| A6 | `resolve:{b:"full"}` 才有 `points`；默认无 | JSON 断言 | read 无条件塞 `points` → `read_full_is_opt_in_per_block` 红 |
| A7 | frozen：直接并发调两次 `resolve`（假插件两次回不同数据、都满足钉住条件），再改 fixture 并第三次调 | 行 = 第一次完成者的数据；`pinned=true`；第三次后行不变 | 写入去掉 `WHERE report_series.pinned = 0` → `frozen_row_is_pinned_by_first_complete_resolution_and_never_overwritten` 红 |
| A7b | frozen 回复含 `unknown_asset` / 某条 `complete_through` 早于请求 `as_of`（fixture：`as_of` 周日、`complete_through` 周五） | 行 `ok, pinned=false`；TTL 过期后 enqueue 非 no-op；fixture 把 `complete_through` 推到周一后再 resolve → `pinned=true` 且数据仍止于周五 | 钉住条件删掉 `complete_through >= as_of` 一项 → `frozen_reply_behind_cutoff_is_not_pinned` 红 |
| A8 | live：行过期后 read 返回旧行并 enqueue；fixture 多给一根 bar 后 drain，`summary.last` 日期前进；docRev 不变；请求里 `as_of == yesterday_utc(now)` | 用注入的 `now` 把 `resolved_at` 设成过期；断言假插件收到的请求 `as_of` | read 对过期行不 enqueue → `stale_live_row_is_served_and_refreshed` 红；内核对 live 请求不填 `as_of` → `live_request_carries_yesterday_utc_cutoff` 红 |
| A9 | in-flight 去重：同键连续 enqueue 十次，假插件只收到一次 tools/call | 计数 | `enqueue` 不查 `inflight` → `refresh_is_deduplicated_per_key` 红 |
| A9b | **drain 准入执行 TTL**：先让一次 resolve 写下新鲜行，再对同键 enqueue（模拟迟到读者的旧观察） | 假插件 tools/call 计数仍为 1 | `resolve` 步骤 1 删掉 `resolved_at ≥ now − TTL → 丢弃` → `fresh_row_is_not_re_resolved_by_late_reader` 红（计数 2） |
| A9c | **失败路径不 fail-locked**：`failpoints.fail_write_once` 使步骤 7 返回 Err；再对同键 enqueue | 假插件 tools/call 计数 == 2 | `InflightGuard` 改成只在步骤 7 成功后显式 `remove` → `failed_resolve_releases_inflight_key` 红（计数 1） |
| A9d | **lane 自愈**：`failpoints.panic_drain_once` 让 drain 任务 panic；再 enqueue | 假插件收到 tools/call | `enqueue` 删掉 `is_finished()/send 失败 → 重建 lane` 分支 → `panicked_lane_is_rebuilt` 红 |
| A10 | **读路径零插件调用**：`SeriesResolver::new_unstarted()` 下对无行的块 read → `pending`，假插件 tools/call 计数为 0 | 计数 | read 里改成内联调用 resolver → `read_never_calls_the_plugin` 红 |
| A10b | **写路径不触发**：commit 一个 `chart.series` 块后等 2s 不 read | 假插件计数 0；`report_series` 无行 | 在 `write.rs` 提交后加 `enqueue` → `write_does_not_trigger_resolution` 红 |
| A11 | 3f：`source` 指向 `market.holdings.set`（`readOnlyHint:false`）/ 不在 manifest 的名字 / ForgeAction 工具 / `neige://plugin/a_b/c`（fixture 注册插件 `a` 暴露工具 `b_c`） | 行 `unavailable`，假插件计数 0；最后一例 reason 含 `a_b` 不含插件 `a` | `resolve` 删掉 `readOnlyHint` 检查 → `resolve_refuses_non_read_only_tools` 红；`plugin_tool_entry` 改成拼 `plugin.{id}_{tool}` 再 `plugin_tool_route` → `underscore_plugin_id_never_routes` 红 |
| A12 | 校验清单：回复 9 条 / 时间戳降序 / `ts_ms` 非零点 / bar > as_of / NaN / 点数超上界 / `ok` 配 1 点 / `ok` 缺 `complete_through` 各一条 | 行 `unavailable, reason` 各不同 | 删掉"`points.len() >= 2`"检查 → `ok_series_with_one_point_is_malformed` 红；删掉"严格升序"检查 → `reply_with_descending_timestamps_is_unavailable` 红 |
| A13 | 删 track 后迟到的 `resolve` 写入 | 无行；任务不 panic；in-flight 键已释放 | 去掉 FK（改成无约束表）→ `late_resolution_after_track_delete_leaves_no_orphan` 红 |
| A14 | 插件 `market.series` 对 fixture 源返回升序、`ts_ms` 为 UTC 零点、按 `as_of` 截止（含当日 bar）、`complete_through` = 未过滤最新 bar 日期、未知标的 `unknown_asset`、区间内 <2 点报 `unavailable`、腾讯 `qfqday`/`day` 两键、列序重排 | S3 进程级测试 | 截断条件 `<= as_of` 改 `<` → `series_as_of_includes_that_days_bar` 红；`complete_through` 改成过滤后的末点 → `complete_through_is_unfiltered_latest` 红 |
| A14b | 插件 `deadline_ms` 已过的请求 | 回 `isError`，fixture 源 HTTP 计数 0 | 删掉出队 deadline 检查 → `expired_request_does_not_hit_network` 红（计数 1） |
| A15 | fe：`ok` 画出与序列数相同的 `<polyline>`；`normalized` 首点=100、首点 ≤ 0 的序列标不可归一化；`pending`/`unavailable` 文案；未钉住 frozen 标 `complete_through`；不含字面颜色 | `series/public.test.tsx` | normalized 除以 `last` 而非 `first` → `rebases every series to 100 at its first point` 红 |
| A16 | HTTP：`?rev=` 不等于当前块 rev → 409 | 路由测试 | 路由忽略 `rev` → `series_route_rejects_stale_rev` 红 |
| A17 | 两份 OpenAPI 与生成器零漂移 | CI 既有 drift 门禁 | 只重生成一份 → 另一条 CI 红 |
| A18 | 术语棘轮：本文档与新代码不引入退役词 | `gate-1316-terminology-ratchet.sh` | — |
| A19 | `plugin_tool_entry` 与 `plugin_tool_route` 集合相等：fixture registry 的每个 `(id, tool)` × running/not running，加不存在的 `(id, tool)` | `entry is Found(e)` ⇔ `route == Ok(Some((id, tool, e.kind)))` | `plugin_tool_entry` 不查 `running_ids` → `tool_entry_matches_tool_route` 红 |
| A20 | `reason` 截断：假插件回 `isError` 文本 10k 字符 | 行 `reason.chars().count() == 256` | 写入前不截断 → `reason_is_capped` 红 |

## 7. KNOWN GAPS（登记，不加固）

- G1 价格是否复权由插件源决定（腾讯 `qfq` = 前复权），本设计不声明；`resolved` 不带 `adjusted` 字段。
- G2 `line` 视图多序列跨币种按原值画、只标币种，不换算、不双轴。
- G3 `pinned=false` 的 frozen 行（部分 `unknown_asset`、某条 `unavailable`、或 `as_of` 在未来 / 源尚未发布到 `as_of`）会按 TTL 反复重投直到满足，可能永远满足不了（停牌、退市、未来日期）；图上标"未钉住"。
- G4 **新鲜度无界**：解析只由读触发；从未被读的块不解析；过期行被读到时先返回旧数据，刷新后要等下一次读。
- G5 legacy `web/` 只显示 unsupported 一行；手机端同 fe。
- G6 `market.series` 对 agent 可见（无隐藏机制）。
- ~~G7~~ 已关闭：`plugin_tool_entry` 用 `registry.get` 精确查找，未安装 / 未运行 / 未暴露三态可分（D2 步骤 2）。
- G8 行不主动 GC（只随 track 删除级联）；改参数留下旧行。
- G9 港股历史无兜底源（新浪港股日线不可用）；腾讯 ifzq 挂掉时 HK 序列 `unavailable`。
- G10 周线/月线由插件从日线聚合（腾讯只用 `day`），`period` 语义"以 bar 收盘日为准"。
- G11 **传输层**：`McpClient` 读循环 `read_line` 无字节上限（`mcp.rs:783`），2 MiB 接受上限在整行入内存之后才生效——归 #1634，本设计不加固。**插件侧**：内核超时不通知插件取消；请求留在插件单工作线程队列（`main.rs:2602`）按序处理，缓解只有 `deadline_ms` 出队丢弃（S3），排在前面的慢请求仍会被执行。responder 泄漏本身由 S2 的 `ResponderSlot` guard 关闭，不再是 GAP。
- G12 行刷新不发事件：浏览器在 `pending` 时轮询，过期行刷新后的新数据要等下一次 fetch；浏览器停轮询后过期行可见到下一次 fetch。
- G13 内核对 `series` 只做字面去重；`HK:9988`/`HK:09988` 会得到两条同资产序列。
- G14 live 截止日按 UTC 日期取昨天：CN/HK 市场当日收盘（UTC 07-08h）后到 UTC 零点之间，live 行仍止于前一交易日，比按本地日历多滞后一天。
- G15 Assistant 可经 `resolved.reason` 读到只读工具的错误文本（≤ 256 字符）；D4 对"Assistant 不得拿 `resolved`"的驳回仍成立，这是它的间接通道。
- G16 未钉住行在 fork 后两个 track 各自刷新、可能分叉；只有钉住行有 fork 不变保证。

## 8. 与 issue 的出入

1. issue 方向 1 "`chart.candles` 收编为它的一个 view" → 保留 `chart.candles` 为独立内联 kind（D1）。
2. issue 方向 3 "内核在写入时钉住快照" → 写入事务不调插件、不触发任何事；后台任务在**第一次读之后**首次完整解析时钉住（D2/D3）。理由：F2 写路径全在 persist 事务内；写后事件拿不到块 id（F1.11/F3.6）。
3. issue 方向 2 "overlay 是推送不是查询 … 内核契约改动" → 请求-响应通道已存在（F4.6-F4.10），改动缩小为"内核后台任务作为调用者 + 插件声明一个只读 tool + 一张结果表 + `McpClient` 超时清 responder"。
4. issue 方向 4 "不新增工具" → MCP 面不新增；浏览器需要一条 NEW HTTP 路由（D5）。
5. issue "`as_of` / `frozen`" 两个名字 → 只用 `as_of`，语义是**截止日**。
6. **v2 新增**：read 不即时；**v3 新增**：不承诺有界新鲜度，从未被读的块不解析（§2.8）。

## 9. 不确定点

- U2 `SERIES_RESOLVE_TIMEOUT = 30s`、`SERIES_TTL = 6h`、`MAX_SERIES_REPLY_BYTES = 2 MiB`、`MAX_SERIES_ROW_BYTES = 1 MiB`、`reason` 256 字符是估值，S2 评审可调；改数不改结构。
- U5 `pending` 轮询节奏（3s / 2 分钟后 30s）是估值，S4 定。
- U6 腾讯 ifzq 的复权基准行（首行 2011 年）是否对所有 us 代码出现，S3 实现按日期过滤规避，测试 fixture 要包含这一行。
- U7 腾讯 ifzq 在盘中是否返回当日未收盘的 bar：无论是否返回，live 的昨天 UTC 截止日都把它过滤掉；但它会影响 `complete_through`（若源在盘中就把当日 bar 列出，`complete_through` 会提前一天到达 `as_of`，frozen 块可能钉在含盘中价的当日 bar 上）。S3 实现要实测并在 fixture 里覆盖；若为真，`complete_through` 取"源最新 bar 日期的前一天"作保守值。

（v1 的 U1 由 §2.5 实测关闭；U3 由 D4 裁决关闭；U4 由 F7.6 关闭。）

## 10. 参考

- 本文 §2 所有 file:line 基于 `c534bf6b`。
- 第 1 轮评审原文：`docs/_1628-design-review-codex-v1.md`、`docs/_1628-design-review-subagent-v1.md`；第 2 轮：`docs/_1628-design-review-codex-v2.md`、`docs/_1628-design-review-subagent-v2.md`。
- U1 spike：编排者 2026-09-12 实测，表已复制进 §2.5。
- 相关：#1556 S1（venue-qualified identity）、#1623（`calm.report.commit`）、#960 PR3（kinds 词汇）、#1612（layout 讨论，材料不在基线）、#1634（MCP 传输层字节上限，本设计的 G11 归它）。

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
| 2 | A | M1 MAJOR 钉住条件"回复 `as_of == 请求 as_of`"要么空洞（回显）要么对非交易日永不满足 | 采纳 A 的方案。插件回复每条 series 带 `complete_through` = 源**未过滤**的最新 bar 日期（插件总是拉最新 N 根再按 `as_of` 过滤，§2.5）；钉住 = 每条 `ok` ∧ 每条末点 ≤ `as_of` ∧ 每条 `complete_through >= as_of`；周日 `as_of` → 源最新周五 → 未钉住 → 周一出 bar 后 TTL 重投钉住，"最多延迟到下一个交易日"；回复顶层 `as_of` 删除；A7b 改用新判据 | D2 步骤 6、回复形状、D3 frozen、seq 7a/7d、A7b、G3 |
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
