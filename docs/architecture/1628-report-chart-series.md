# 声明式图表 `chart.series`（#1628）— 设计 v2

基线：`origin/main` = `c534bf6b`（工作树 `feat/report-chart-series`）。所有 file:line 都在该基线上实测（[实测]），未核实的写 "未核实"。v1 → v2 的每条改动登记在 §11。

**v2 的模型变化（用户 2026-09-12 拍板）**：解析是**后台任务**，`calm.report.read` 与浏览器只读**已存储的行**，读路径上永不调用插件。v1 第 1 轮 17 条发现里超时泄漏、并发预算、CAS 路径变慢、内核缓存、插件排队、人/AI 数字不一致、删除竞态全部源自「read 里同步调插件」一个选择；需要即时数据时 Planner 自己调 `market.series`。D2、D3、D4、D5 整段重写。

## 1. 目标与非目标

**目标。** 让投研报告里的图表只*命名*数据与视图：Planner / 人写一个 `chart.series` 块（`{series, field, range, view, as_of?}`），内核在块落盘后投一个后台任务向 market 插件解析"资产 × 字段 × 区间 → 序列"并把结果存成一行；内核对每个这样的块提供两态（`as_of` 缺席 = live，按 TTL 随源流动；`as_of` 存在 = frozen，钉在时点）；`calm.report.read` 对数据块返回存储行的 `resolved` 摘要（默认）或原始序列（按块显式要），read 是纯 DB 读、永不因插件失败而失败；前端 fe/ 用现有 SVG 路线画 line / normalized / bar / candles；人与 AI 读同一行、同一份字节。

**非目标。** 不在正文里发明宏语法；不给内核加行情源（数据仍由插件解析）；不把 `chart.candles` 的已存文档做数据迁移；不给 legacy `web/` 加新渲染器（它按现有规则显示 `unsupported block kind chart.series`，见 §4 D5）；不做缩放/联动/图表库懒加载；不做盘中序列；不做跨币种换算（每条序列自带 `currency`，normalized 视图天然可比，line 视图按原值画并标币种）；**不承诺 read 即时**（新鲜度以 TTL 为界，§2.8）。

## 2. 事实表

### 2.1 block kind 词汇与校验（`crates/calm-types/src/report_blocks/`）

| # | 事实 | 位置 |
|---|---|---|
| F1.1 | 数据 kind 闭集 `DATA_KINDS = [chart.candles, table, app, task]`；`is_data_kind` 只认这四个 | [实测] `kinds.rs:58-62` |
| F1.2 | `validate_payload(kind, payload)` 按 kind 分派；未知 kind 本身是错误（列出 `DATA_KINDS`）；形状合法后再量一次 `canonical_json` 的字节数，上限 `MAX_CANONICAL_BYTES = 256KB` | [实测] `kinds.rs:89-134`，常量 `:46-55` |
| F1.3 | caps：`MAX_CHART_CANDLES=5000`、`MAX_TABLE_COLUMNS=32`、`MAX_TABLE_ROWS=500`、`MAX_STRING_CHARS=2048` | [实测] `kinds.rs:46-55` |
| F1.4 | live `source` 两段语法：`neige://plugin/<plugin_id>/<overlay_kind>`，前缀常量 `LIVE_SOURCE_PREFIX`，每段字符集 `[A-Za-z0-9._-]`，**只查形状不查存在**（"插件尚未安装是正常状态"） | [实测] `kinds.rs:137-177` |
| F1.5 | `validate_chart`：allow-list `symbol, period, candles, overlays, caption`；`period ∈ day/week/month`；candles 行 5 或 6 个数；`overlays ∈ ma20/ma60` | [实测] `kinds.rs:483-539` |
| F1.6 | `validate_table` 用 `source` 的*存在*选 live 形态，live 形态只允许 `source, caption`（两种形态互斥） | [实测] `kinds.rs:547-570` |
| F1.7 | fence 形状：```` ```neige-block <kind> ```` 开头、JSON **object** 内容、裸 ```` ``` ```` 收尾；`render_fence ∘ parse_fence` 幂等；`canonical_json` 键排序、2 空格、纯标量数组单行 | [实测] `fence.rs:33-77`、`:84-136` |
| F1.8 | `flatten(split_body(body)) == body` 字节级不变式；格式错误的 neige fence 读成 prose，写端必须用 `invalid_neige_fences` 拒绝 | [实测] `mod.rs:3-7`、`:43-63`、proptest `:387-388` |
| F1.9 | "一个块能装什么"的唯一定义：`check_prose_markdown` / `render_data_block`（先 `validate_payload` 再 `render_fence`）/ `unknown_kind_message` | [实测] `mod.rs:196-233` |
| F1.10 | 现有 chart 校验测试 `chart_payload_valid_and_invalid`；live table 三条测试 | [实测] `kinds_tests.rs:5`、`:385/:406/:425` |
| F1.11 | 解析 fence 的公共函数：`split_body(body) -> Vec<BlockSlice>`、`parse_fence(raw) -> Option<NonProseFence>` — 后台任务用它们从事件里的 `body_after` 找 `chart.series` 块 | [实测] `mod.rs:48`、`fence.rs:52` |

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

结论：新 kind 只需进 `DATA_KINDS` + 一个 `validate_chart_series` + `kinds_table` 一项，上述 9 处入口全部经 `validate_payload` 自动覆盖。

### 2.3 读端与写后钩子

| # | 事实 | 位置 |
|---|---|---|
| F3.1 | `ReportReadSnapshot { updated_at, schema_version, doc_rev, summary, body, blocks: Vec<ReportBlock>, task_diagnostics }`，一次 `card_get_with_body_crdt` 单行读 | [实测] `track_report_read.rs:12-20`、`:43-60` |
| F3.2 | `ReportBlock { id, kind, rev, payload }` — 快照里**有** payload | [实测] `calm-types/src/track_report.rs:16-22` |
| F3.3 | `calm.report.read` 响应 `{ text, body(alias), summary, schemaVersion, docRev, updated_at, blocks: [{id, kind, rev}], taskDiagnostics?(Planner only) }`；入参只有 `with_markers`；Planner 与 Assistant 都能 read；注释明说 read 是 `docRev`/`rev` 的**唯一来源**、每次写前必 read（所以 read 路径不能变慢） | [实测] `mcp_server/tools/track_report.rs:142-222`，角色 `:148-153`，index `:190-194` |
| F3.4 | **live `source` 在读端不解析**：handler 只把 `flat_text` 拼进 `text`，块索引不带 payload | [实测] `track_report.rs:172-194` |
| F3.5 | **没有面向 agent 的 overlay 读取工具**：`grep -rln overlay crates/calm-server/src/mcp_server/` 只命中 `contracts.rs`（描述文字） | [实测] |
| F3.6 | **写后钩子的落点**：报告写入走 `write_with_actor_events_typed`，calm-truth 在 `tx.commit().await?` **之后**逐条 `bus.emit_envelope`；`Event::TrackReportEdited` 带 `track_id, card_id, body_before, body_after` | [实测] `track_report/write.rs:832-880`；`calm-truth/src/db/sqlite/events.rs:787-789`；`calm-types/src/event.rs:579-597` |
| F3.7 | **事务外后台反应的既有形状**：`bus.subscribe()` + 自己的 `tokio::spawn` 循环，`RecvError::Lagged` 只 warn 继续（bus 是 lossy 的 `broadcast`，订阅者各自定 catch-up 策略） | [实测] `card_fsm.rs:350-364`；`dispatcher/mod.rs:1118-1127`（poke scheduler）；`event_bus.rs:109-131`、`:187` |
| F3.8 | 报告块快照在事务内可读：`report_blocks_snapshot_tx(tx, track_id)` | [实测] `track_report.rs:73` |

### 2.4 overlay 机制与内核↔插件调用面

| # | 事实 | 位置 |
|---|---|---|
| F4.1 | `neige.overlay.set` 回调：`entity_kind` 必须 externally_writable（`card`/`track`），manifest `overlays_write` 授权，`validate_overlay_payload` 只校验内核自有 kind（插件 kind 不透明、**无字节上限**），`plugin_id` 由内核注入 | [实测] `plugin_host/callbacks.rs:268-320`；`calm-truth/src/validation.rs:417-445`、`:575-577` |
| F4.2 | 存储：`overlays` 表 `UNIQUE(plugin_id, entity_kind, entity_id, kind)`，**无外键**；upsert 为 `ON CONFLICT ... DO UPDATE` | [实测] `calm-truth/migrations/0001_init.sql:42-52`；`db/sqlite/overlay.rs:7-16` |
| F4.3 | 读：`overlays_for(entity_kind, entity_id)` / `overlays_by_kind(entity_kind)`；`GET /api/overlays?entity_kind=track`（不带 id）**返回全工作区该 kind 的所有 overlay，侧栏用这个形态**；`GET /api/tracks/{id}` 详情也带该 track 全部 overlays | [实测] `db/mod.rs:83-84`；`routes/overlays.rs:106-112`、`:125-134`；`routes/tracks.rs:1102-1121` |
| F4.4 | 事件 `Event::OverlaySet(Overlay)`（带整个 payload）/ `OverlayDeleted`，wire 名 `overlay.set` / `overlay.deleted` | [实测] `calm-types/src/event.rs:600-608` |
| F4.5 | overlay 写方：外部只有插件回调（`callbacks.rs:310`）与手测路由（`routes/overlays.rs:202`）；**内核内部写者**另有 `card_fsm.rs:558/:683`、track structure creation、`child_track_adapter`（路由头注自列） | [实测] `grep -rn overlay_upsert_tx crates/calm-server/src`；`routes/overlays.rs:80-82` |
| F4.6 | 内核→插件请求-响应面：`McpClient::tools_call(name, arguments, track_id)` 把 track 放在 `params._meta`；`McpClient::call` **无超时**（只有 `initialize` 包了 10s，`:477`）；`call` 先把 responder 插进 `responders` 表（`:667`）再 `rx.await`（`:676`），**调用方超时/取消不会移除 responder**，只有对端回复（`:804`）或传输关闭（`flush_responders_with_error`）才清；读循环 `read_line` 先把整行读进内存再 `parse_frame`，**无字节上限** | [实测] `plugin_host/mcp.rs:664-679`、`:778-810`、`:900` |
| F4.7 | 按插件 id 取客户端：`PluginHost::connector_client(id) -> Option<ConnectorClient>`（Running 才有）、`mcp_client(id)`（只给 stdio） | [实测] `plugin_host/mod.rs:3296-3320` |
| F4.8 | `ConnectorClient` 三变体：`Stdio(Arc<McpClient>)`（本地子进程，`tools_call` 三参带 track）、`Http(Arc<HttpMcpClient>)`（远端 mcp-http，`tools_call` 双参、注释明说 "somebody else's service"）、`Cli(Arc<CliQueryRuntime>)`（本地 exec，双参） | [实测] `plugin_host/connector.rs:44-55`；`mcp_server/transport.rs:722-738` |
| F4.9 | agent 路由 `dispatch_plugin_tools_call`：身份先于路由 → `plugin_tool_route(registry, name, running_ids)` 只认 manifest `exposes_tools` 里的工具并带回 `kind` → `plugin_scope_for_track(...).allows(plugin_id)` → `require_role_any(PLUGIN_TOOL_ROLES = [Planner, Worker])` → `kind == None` 走 `connector_client` 三变体分派；`Some(ForgeAction)` 走 `trusted_forge_plugin` + `mcp_client` 专用臂；`plugin_tool_route` 是私有 `fn` | [实测] `transport.rs:88`、`:667-762`、`:771-800`；`forge_trust.rs:20` |
| F4.10 | **内核作为调用者已有先例**：HTTP 建卡路径 `routes/cards.rs:588` 直接 `mcp.tools_call(&via.tool_name, via.arguments, None)`，前置门是插件权限 `cards_create`（`:573`），不经 `PLUGIN_TOOL_ROLES` | [实测] |
| F4.11 | `TrackPluginScope`：无 track / 未绑定 → `All`；绑定且 owner 运行∧可信 → `Only(id)`；绑定但 owner 不可用 → `None`（`allows` 恒 false，fail-closed） | [实测] `tool_visibility.rs:50-70`、`:136-149` |
| F4.12 | `ExposedTool { name, description?, kind?: Option<ToolKind>(只有 ForgeAction), input_schema?, annotations?: Option<Value> }`；`readOnlyHint` 是 `annotations` JSON 里的键；没有"仅内核可调"的标记 | [实测] `plugin_host/manifest.rs:766-780`；`grep -n "hidden\|internal" tool_visibility.rs` 零命中 |
| F4.13 | 插件侧回调 `Rpc::call` 有 15s 超时；插件把 tools/call 送到**单**工作线程串行处理；未知工具回 `tool_error("unknown tool …")`（`isError`） | [实测] `plugins/market/main.rs:52`、`:2572`、`:2590-2603` |
| F4.14 | `neige.kv.set` 配额按整个 keyset 的 JSON 文本长度计；market manifest 设 `kv_quota_bytes: 262144` | [实测] `callbacks.rs:792-835`；`plugins/market/manifest.json` permissions |
| F4.15 | SQLite 连接池 `PRAGMA foreign_keys = ON` 每连接；`REFERENCES tracks(id) ON DELETE CASCADE` 有先例（0104、0106）；track 删除在一个事务里先做 overlay 等显式清理再 `track_delete_tx` | [实测] `calm-truth/src/db/sqlite/mod.rs:4`、`:264`；`migrations/0104_candidate_review.sql:5`、`0106_candidate_repair.sql:4`；`routes/tracks.rs:4032-4037` |

回答：overlay 是推送模型；请求-响应通道已存在且内核作为调用者已有先例（F4.6/F4.10）。新的是"内核在后台任务里作为调用者，经 manifest 路由与只读约束"（§4 D2）。

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

S3 约束：腾讯 ifzq 为股票三市场主源（单端点、显式条数、复权；解析时 `qfqday`/`day` 两个键都要认、列序按 o,c,h,l,v 重排、按日期过滤）；Binance klines 为 crypto；新浪只作 A 股/美股兜底，**港股无兜底**（登记 G9，与 #1556 D3″ 同形）。

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

### 2.7 迁移与版本常量

| # | 事实 | 位置 |
|---|---|---|
| F7.1 | 迁移目录 `crates/calm-truth/migrations/`，最新 `0106_candidate_repair.sql`；**本设计 S2 需要一张新表，号最后才定** | [实测] `ls … \| tail` |
| F7.2 | `SYNC_EVENT_VERSION = 20`；门禁 `gate-sync-event-version-lockstep.sh`。**本设计不新增 Event kind**（行写入不发事件，§4 D3），不 bump | [实测] `calm-types/src/event.rs:258` |
| F7.3 | `REST_API_VERSION = "7"`（`crates/calm-types/src/compatibility.rs:5`）；`WEB_COMPAT_VERSION = 27`（`routes/version.rs:116`），门禁只查三处相等。新增一条 GET 路由是加法，不 bump | [实测]（v1 把 `compatibility.rs` 写在 calm-server，§11 m1） |
| F7.4 | `TrackReportPayload::SCHEMA_VERSION = 4`；新 kind 不改 payload 形状，不 bump | [实测] `calm-types/src/track_report.rs:146-152` |
| F7.5 | #1316 术语棘轮扫 `docs`（`RATCHETED_SCOPES=(crates fe docs e2e)`，`git grep` 只看已跟踪文件）；本文档避免退役词 | [实测] `scripts/gate-1316-terminology-ratchet.sh:459` |
| F7.6 | 仓内已有 `sha2::Sha256` 用法（观察 hash）：`track_report_doc.rs:232-235`；`request_hash` 复用同一 crate，不再引入依赖 | [实测]（v1 U4 关闭，§11 m2） |

### 2.8 简化假设（用户："水合只需要给一个够用的简化就行"）

- 只做日线及以上（`period ∈ day/week/month`），不做盘中。
- 新鲜度以 TTL 为界（6 小时，D3），**不承诺 read 即时**；要即时数据 Planner 自己调 `plugin.dev-neige-market_market.series`。
- 解析失败不重试风暴：失败行也按同一 TTL 才再投，同一键同一时刻至多一个任务。
- 人与 AI 读同一行：摘要在写入行时由同一个 Rust 函数算好存下，两个读者只做序列化。
- 行写入不发事件：浏览器在 `pending` 时短轮询，其它时候靠既有失效与刷新（D5）。
- 内核不理解 venue、不理解交易日历、不做复权声明；这些归插件源。

## 3. Oracle trace

Planner 写 `chart.series` → 内核校验 → 事务提交后 bus 投任务 → 任务调插件、写一行 → 前端/Planner 读那一行。事件 kind 均为真实 `Event` 变体。状态：✅ 今天已成立 / ⚠️ 本设计新增或改动 / ❌ 今天为假、本设计修正。**每个 ⚠️/❌ 行恰好一个切片**（"片"列）。状态词：块级只用 `resolved.status ∈ {ok, pending, unavailable}`；`unknown_asset` 是插件回复里**每条 series** 的状态词，只在 3d 出现、归 S3。

| seq | phase | actor | trigger / MCP tool | 效果 | 可观察事件 | 不变式断言 | 状态 | 片 |
|---|---|---|---|---|---|---|---|---|
| 1 | discover | Planner | `calm.report.blocks.kinds` | 返回含 `chart.series` 的 kinds 表 | 无 | `upsert.kind.enum == commit.ops.kind.enum == kinds_table.kinds`（`contracts.rs:695-711`） | ⚠️ | S1 |
| 2 | write | Planner | `calm.report.commit{ops:[{op:"upsert", kind:"chart.series", payload:{source:"neige://plugin/dev-neige-market/market.series", series:["US:NVDA","HK:9988"], view:"normalized", range:"1Y"}}], if_doc_rev, message}` | `validate_payload` 通过 → canonical fence 落盘，docRev+1；**事务里不调插件、不写 `report_series`** | `CardUpdated` + `TrackReportEdited` 恰好各一 | `flatten(split_body(body))==body`；`parse_fence` 回同 payload | ⚠️ | S1 |
| 2n | write-neg | Planner | 同上但 `series:["NVDA"]`（无 venue）/ 9 条 / `view:"candles"` 配 2 条 / `source:"https://…"` / `as_of` ≥ 今天 UTC | `-32602` 字段级错误；不写不发事件 | 无 | 拒绝发生在 `validate_chart_series`，F2 的 9 个入口都经它 | ⚠️ | S1 |
| 2e | enqueue | 内核 bus 订阅者 | 收到 `track.report_edited` 信封 | 对 `body_after` 里每个 `chart.series` 块算 `request_hash`；无行或过期 → `enqueue(track_id, block_id, hash)` | 无 | 订阅者不调插件、不写 DB；`Lagged` 只 warn（读者是兜底，seq 4） | ⚠️ | S2 |
| 3 | resolve | 内核任务 | drain 任务取出 job：重读块、hash 仍等于当前 payload → `plugin_tool_route` 通过 → `connector_client` 为本地变体 → `timeout(30s, tools_call("market.series", args, Some(track_id)))` | 回复通过校验清单 → 事务内 `INSERT … ON CONFLICT DO UPDATE … WHERE pinned=0` 一行 `status=ok`，`summary` 算好存下 | 无 | 同一 `(track,block,hash)` 同一时刻至多一个任务；写入受 FK 约束 | ⚠️ | S2 |
| 3b | resolve-neg | 内核任务 | 插件未运行 / 绑定 track 的 owner 不可用（`TrackPluginScope::None`） | 写一行 `status=unavailable, reason="plugin dev-neige-market is not running"` | 无 | 不发插件调用；TTL 后再投 | ⚠️ | S2 |
| 3c | resolve-neg | 内核任务 | 超时 / `isError`（含 S3 前的 `unknown tool`）/ 回复非 object | 写一行 `unavailable, reason` | 无 | 超时只影响该插件队列，读者不等 | ⚠️ | S2 |
| 3d | resolve-partial | 插件 | `series` 里有 `US:NOPE` | 回复 `series[j].status="unknown_asset", reason`，其它条 `ok`；内核块级 `ok`，`pinned=false` | 无 | 部分失败不拖垮整块，也不钉住 | ⚠️ | S3 |
| 3e | resolve-neg | 内核任务 | 回复超 `MAX_SERIES_REPLY_BYTES` / 资产不一一对应 / 时间戳非严格升序 / 非有限数 / bar 日期 > as_of / 点数 > 区间上界 | 整行 `unavailable, reason` | 无 | 校验清单在内核边界（D2） | ⚠️ | S2 |
| 3f | resolve-neg | 内核任务 | `source` 指向不在 manifest 的工具 / `kind: ForgeAction` / `readOnlyHint != true` / `ConnectorClient::Http` | 整行 `unavailable, reason`，**零次** tools/call | 无 | 路由与 agent 路径同一函数 `plugin_tool_route` | ⚠️ | S2 |
| 4 | read-pending | Planner | `calm.report.read{}`，该块尚无行 | `resolved.status="pending"`；read 顺手 `enqueue` | 无 | read 期间假插件收到零次 tools/call；read 永远 200 | ❌→⚠️ | S2 |
| 4a | read-summary | Planner | 同上，行已存在 | `blocks[i].resolved` = 行里的 `status/as_of/resolved_at/pinned/summary` | 无 | 纯 DB 读，无超时无并发预算 | ⚠️ | S2 |
| 4b | read-full | Planner | `calm.report.read{resolve:{"b_x":"full"}}` | 该块附 `series[j].points`；未点名的仍 summary | 无 | `points` 来自同一行的 `data` | ⚠️ | S2 |
| 5 | render-fetch | 浏览器 | `GET /api/tracks/{id}/report/series/{block_id}?rev=<n>`（NEW 路由） | 返回同一行（默认带 points）；无行 → `pending` 并 enqueue | 无 | `rev` ≠ 当前块 rev → 409 `{current_rev}`，旧数据不会贴到新参数上 | ⚠️ | S4 |
| 5a | render | fe | `ReportSeriesBlock` | SVG line/normalized/bar/candles；`pending`/`unavailable` 各渲染 caption + 一行文字；`pending` 时 3s 轮询直到非 pending | 无 | 不含字面颜色；normalized 首点 ≤ 0 的序列显式标"不可归一化" | ⚠️ | S4 |
| 6 | live-stale | 任一读者 | 读到 live 行 `resolved_at < now - 6h` | 返回旧行 + `enqueue`（stale-while-revalidate） | 无 | 正在跑的键不再投（in-flight 去重） | ⚠️ | S2 |
| 7 | freeze-write | Planner | `commit` 写 `as_of:"2026-09-10"` | 存 fence；同 seq 2 | 同 seq 2 | 内核校验 `as_of < 今天 UTC` | ⚠️ | S1 |
| 7a | freeze-pin | 内核任务 | 回复每条 `ok` 且 `as_of == 请求 as_of` | 写行 `pinned=true` | 无 | 之后 enqueue 对该键是 no-op；`DO UPDATE … WHERE pinned=0` 拒绝覆盖 | ⚠️ | S2 |
| 7b | freeze-hold | 源 | 钉住后假插件换数据、TTL 过期 | 行**不变**，`resolved` 仍是钉住的数据 | 无 | 两个并发首解析竞争者只有一个能写、之后谁都不能覆盖 | ⚠️ | S2 |
| 7c | freeze-rehash | Planner | 改 `range`/`series`/`as_of`（hash 变）；改 `caption`/`view`（hash 不变） | 前者新行、旧行留到 track 删除；后者复用原行 | `CardUpdated`+`TrackReportEdited` | 行身份 = `(track,block,request_hash)`，不是 rev | ⚠️ | S2 |
| 7d | freeze-incomplete | 内核任务 | frozen 块回复里有 `unknown_asset` 或 `as_of` 早于请求 | 写行 `ok, pinned=false`；TTL 后再投 | 无 | 不完整快照永不钉住 | ⚠️ | S2 |
| 8 | live-drift | 假插件 fixture | live 行过期后 fixture 多给一根 bar | 新行 `as_of` 前进；docRev **不变** | 无 | 用受控 fixture 断言，不用日历时间 | ⚠️ | S2 |
| 9 | delete | 人 | 删 track | `ON DELETE CASCADE` 清掉 `report_series`；迟到的任务写入被 FK 拒绝 | `TrackDeleted` | 无孤儿行 | ⚠️ | S2 |
| 10 | human-read | 人 | 打开报告 | seq 5 的字节 = seq 4a 的 `resolved`（同一行、同一序列化） | — | 人与 AI 同源 | ⚠️ | S4 |
| 11 | legacy | 人（web/） | 打开报告 | `unsupported block kind chart.series` | — | 差异被声明（§7 G5） | ✅ | — |
| 12 | fork | 人 | fork track | fork 事务里 `INSERT … SELECT` 复制源 track `pinned=1` 的行到新 track（block id 不变，F2.6）；live 行不复制 | 现有事件 | 冻结图 fork 后不变 | ⚠️ | S2 |

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
  "as_of":   "2026-09-10",                           // 可选，YYYY-MM-DD 且 < 今天(UTC)；存在 = frozen；缺席 = live
  "overlays": ["ma20"],                              // 可选，ma20|ma60，只对 line/candles 生效
  "caption": "…"                                     // 可选
}
```

- 不允许 inline 数据：数据只由 `source` 解析。要内联的数据继续用 `chart.candles`。
- `series` 去重按字面；`HK:9988` 与 `HK:09988` 在插件里是同一身份（F5.1 `canonical_symbol`），**内核不折叠**（不复制插件的 venue 规则），插件回复里两条同资产序列由校验清单的"资产一一对应"规则接受（回复 `asset` 必须逐项等于请求字符串）。
- `as_of` 上界：`as_of < 今天（UTC 日期）`。没有它，`as_of:"2099-01-01"` 会让"钉在时点"退化成"钉在第一个任务跑的时刻"（§11 M3）。
- `chart.candles` **不收编、不迁移**：已存文档、两个前端各有渲染器（F6.1、F6.8）、有校验与集成测试。折衷：`kinds_table` 里 `chart.candles` 的 usage 改为"内联数据的逃生口；行情能由插件解析的标的请用 `chart.series`"（F2.12 那句删掉）。**这是与 issue 方向 1 的出入**（§8）。
- caps：`MAX_CHART_SERIES = 8`（新常量，放 `kinds.rs:46-55` 旁）。点数上界不再复用 `MAX_CHART_CANDLES`，改为按区间自然大小（D2）。
- 校验落点：`kinds.rs` 新 `validate_chart_series`（挨着 `validate_chart :483`），`DATA_KINDS` 变 5 项，`KIND_CHART_SERIES` 常量；`contracts.rs::kinds_table` 加一项并改 F2.11 两段手写文字；`fe/core/domain/report.ts` 加 `chartSeriesPayloadSchema` + `payloadSchemaFor` 分支；`web/` 不加（落 opaque，F6.8）。
- Rust 侧派生：`SeriesRequest::from_payload(&Value) -> (SeriesRequest, request_hash)`，`fields` 由 `view` 派生（candles → `[open,high,low,close,volume]`，否则 `[field]`）；`request_hash = hex(sha256(canonical_json({series, fields, range, period, as_of})))`。`caption/view/overlays` **不入指纹**（表现层）。

**依据。** F1.1-F1.6、F1.4、F5.1、F6.5/F6.8。

### D2 解析任务：内核如何、何时向插件要序列（整段重写）

**问题。** v1 在 read 里同步调插件；17 条发现里 12 条由此而来。

**备选。** (a) 保持 read 内同步解析，加并发预算/缓存/传输层上限。 (b) 解析改为后台任务，read 只读行。 (c) 插件预推全部历史成 overlay。

**裁决：(b)。** 机制：

- **触发**（三处，都只 `enqueue`，不调插件）：
  1. bus 订阅者（F3.7 形状，NEW `track_report_series::subscriber`）：收到 `track.report_edited`，`split_body(body_after)` → 每个 `chart.series` fence → `SeriesRequest::from_payload` → 若无行或行过期且未 pinned → `enqueue`。`Lagged` 只 warn：读者兜底。
  2. `calm.report.read`（D4）与 HTTP GET（D5）：读到无行 / 过期 live 行 / 过期 `pinned=false` 行 → `enqueue` 并返回现状（stale-while-revalidate）。
  3. 没有第三处：不做启动扫描、不做定时扫描、不重试风暴。
- **去重与串行**：NEW `SeriesResolver`（挂在 `AppContext`）：`inflight: Mutex<HashSet<(TrackId, BlockId, RequestHash)>>`，`enqueue` 时已在集合 → no-op；按 `plugin_id` 各一条 `mpsc` 队列 + 一个 drain 任务（首个 job 时 `tokio::spawn`），**按插件串行**——与 market 插件自身的单工作线程（F4.13）同构，队列不会在插件里堆积。测试 seam：`SeriesResolver::new_unstarted()` 只记录不 drain（A-zero 用）。
- **任务 `resolve(track_id, block_id, request_hash)`**：
  1. 重读块（`report_blocks_snapshot_tx` 或 `load_report_read_snapshot`）；块不存在 / 当前 payload 的 hash ≠ job 的 hash / 行已 `pinned` → 丢弃（stale job）。
  2. 路由：`plugin_tool_route(registry, &format!("plugin.{plugin_id}_{tool}"), &running_ids)`（F4.9，改 `pub(crate)`）。`None` → `unavailable, reason="plugin <id> is not running or does not expose <tool>"`。`Some(kind)` 且 `kind != None` → `unavailable, reason="tool is not an ordinary read-only tool"`。manifest `exposes_tools[tool].annotations["readOnlyHint"] != true` → 同上。
  3. 作用域：`plugin_scope_for_track(ctx, Some(track_id)).allows(plugin_id)`（与 agent 路由同一规则 F4.11；绑定 track 的 owner 不可用 → `None` → `unavailable, reason="plugin <id> is not running"`）。
  4. 客户端：`connector_client(plugin_id)`：`None` → `unavailable, "not running"`；`Http(_)` → `unavailable, reason="remote connectors are not series sources"`（远端服务不该由文档内容驱动被内核请求，§11 B1 构造 2）；`Stdio(c)` → `c.tools_call(tool, args, Some(&track_id))`；`Cli(c)` → `c.tools_call(tool, args)`。
  5. 超时：`tokio::time::timeout(SERIES_RESOLVE_TIMEOUT = 30s, …)`。理由：后台执行、按插件串行，长超时的代价是该插件队列的延迟而不是任何读者的等待；8 条资产逐条打腾讯/Binance 各 ≤3s 的最坏情况在 30s 内。超时后 `McpClient` 的 responder 槽位留到对端回复或传输关闭才清（F4.6）——串行保证同一插件同一时刻至多一个未决请求，泄漏上界为 1（G11）。
  6. **回复校验清单**（内核边界，任一不过 → 整行 `unavailable, reason`）：`isError != true`；`structuredContent` 是 object；序列化整个 `CallToolResult` ≤ `MAX_SERIES_REPLY_BYTES = 2 MiB`；`series` 数组长度 == 请求长度且第 j 项 `asset` == 请求第 j 条（一一对应）；每项 `status ∈ {ok, unknown_asset, unavailable}`；`ok` 项 `points` 每点长度 == 1 + `fields.len()`，时间戳 `ts_ms` 严格升序，数值全部有限（`f64::is_finite`）；每点日期（UTC）≤ `as_of`（frozen）或 ≤ 今天；点数 ≤ `max_points(range, period)`（区间日历天数 / period 天数 + 2：1Y day = 368、5Y day = 1829、5Y week = 263、5Y month = 62）；回复 `as_of` 是 `YYYY-MM-DD` 且 ≥ 每条最后一点日期；存储 `data` 序列化 ≤ `MAX_SERIES_ROW_BYTES = 1 MiB`。
  7. 写行（D3），`summary` 由 `summarize(&Series) -> Summary` 在写入前算出并一起存。
  8. `inflight.remove(key)`。
- **请求形状**（内核由 payload 派生）与**回复形状**（`structuredContent`）：
  ```jsonc
  // 请求
  { "series": ["US:NVDA","HK:9988"], "fields": ["close"], "range": "1Y", "period": "day", "as_of": "2026-09-10" /* 可选 */ }
  // 回复
  { "as_of": "2026-09-11",
    "series": [
      { "asset": "US:NVDA", "currency": "USD", "status": "ok", "points": [[ts_ms, close], …] },
      { "asset": "HK:9988", "status": "unknown_asset", "reason": "…" } ] }
  ```
- 插件 manifest `exposes_tools` 加 `market.series`（`readOnlyHint: true, openWorldHint: true`）。它同时对 agent 可见为 `plugin.dev-neige-market_market.series`（F4.12 无隐藏机制）——这是接受的，也是"要即时数据自己调"的通道（§2.8）。
- 与 overlay 推送的关系：互不替代。表继续用推送；序列用内核拉取。两者共享 `neige://plugin/<id>/<x>` 语法，第二段含义不同（overlay kind vs tool 名）；kinds 表与 schema description 必须写清。

**依据。** F3.6/F3.7（事务外反应的既有形状：commit 后 emit，订阅者 spawn 循环）、F4.9（`plugin_tool_route` 是唯一 manifest 路由函数，直接调用而不是复述）、F4.10（内核作为调用者已有先例）、F4.6（无超时且取消不清 responder → 串行化把泄漏封顶）、F4.13（插件单线程 → 内核端也串行）、F4.14（KV 配额 256KB，插件不能把历史存 KV）。(a) 被否：read 是 CAS 握手的必经路径（F3.3），任何插件延迟都放大到每次写；(c) 被否：撑大 overlay 且插件不知道该推哪些标的。

### D3 存储与 frozen / live（整段重写）

**问题。** 行存哪、身份是什么、谁写、什么时候钉住、删除与 fork。

**备选。** (a) 新表 `report_series`。 (b) 复用 `overlays` 表（`plugin_id = "kernel"` + 新内核 kind）。

**评估 (b)**：省一张迁移，但：① `GET /api/overlays?entity_kind=track` 不带 id 是**全工作区**拉取、侧栏用它（F4.3）；`GET /api/tracks/{id}` 也带全部 overlays——每个图 1Y 日线 ≈ 6KB、candles ≈ 12KB、8 条 5Y candles ≈ 730KB，都会随每次 `overlay.set` 失效被侧栏重新拉下来；② 每次行写入会发 `Event::OverlaySet(Overlay)` 带整个 payload（F4.4）进 WS 与插件 firehose；③ overlay 键 `(plugin_id, entity_kind, entity_id, kind)` 没有 `request_hash` 位，要把 `block_id:hash` 编进 `kind` 字符串；④ overlay 无外键（F4.2），删除竞态靠显式清理。四条里 ①② 是硬伤。

**裁决：(a)。** 迁移（号最后定，F7.1）：

```sql
CREATE TABLE report_series (
    track_id     TEXT    NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    block_id     TEXT    NOT NULL,
    request_hash TEXT    NOT NULL,                 -- hex sha256, D1
    status       TEXT    NOT NULL,                 -- 'ok' | 'unavailable'
    reason       TEXT,                             -- unavailable 时
    as_of        TEXT,                             -- YYYY-MM-DD，ok 时（回复里的 as_of）
    resolved_at  INTEGER NOT NULL,                 -- ms
    pinned       INTEGER NOT NULL DEFAULT 0,
    summary      TEXT    NOT NULL,                 -- JSON，写入时算好（人与 AI 字节相等）
    data         TEXT,                             -- JSON：series[] 含 points，ok 时
    PRIMARY KEY (track_id, block_id, request_hash)
);
```

- **外键 vs 事务内存在性校验**：选外键 `ON DELETE CASCADE`。理由：`foreign_keys = ON` 每连接、同形先例 0104/0106（F4.15）；删除时 `track_delete_tx` 级联清掉行，迟到的任务写入在约束处失败（任务 warn 并丢弃），没有"先查后插"的窗口。
- **写入**：任务在 `write_in_tx_typed` 里 `INSERT … ON CONFLICT(track_id, block_id, request_hash) DO UPDATE SET status, reason, as_of, resolved_at, pinned, summary, data = excluded.* WHERE report_series.pinned = 0`。`pinned` 行在 DB 层不可覆盖（A7 的变异靶点）。不发事件（F7.2 不 bump）。
- **`pending` 不是行的状态**：无行 = `pending`（读时词）。行的 `status` 只有 `ok | unavailable`。
- **live**：`as_of` 缺席。TTL = `SERIES_TTL = 6h`：日线一天一根、收盘后源才更新；6h 让一天内被反复打开的报告最多刷新 4 次，一天开一次的报告刷新一次，且过夜后第一次打开必刷新。失败行同一 TTL。
- **frozen**：`as_of` 存在（且 < 今天，D1）；请求带 `as_of`；插件只返回 ≤ as_of 的 bar。**钉住条件**：每条 series `status == ok` **且**回复 `as_of == 请求 as_of` → `pinned = 1`，此后 `enqueue` 对该键 no-op、写入被 `WHERE pinned = 0` 拒绝。不满足 → `pinned = 0` 的 `ok` 行（图能画，但标"未钉住"），按 TTL 再投直到满足。并发首解析：串行队列 + in-flight 去重使同键同一时刻只有一个任务；即便测试直接并发调用两次 `resolve`，第一个写成 `pinned=1` 后第二个的 `DO UPDATE … WHERE pinned=0` 是 no-op。
- **身份**：`(track_id, block_id, request_hash)`。改 `caption/view/overlays` 不换行（M1）；改 `series/range/period/as_of` 换行，旧行留到 track 删除（G8）。
- **fork**：`routes/tracks.rs:2324` 的 fork 在事务里、block id 保留（F2.6）——同一事务里 `INSERT INTO report_series SELECT <new_track_id>, block_id, request_hash, … FROM report_series WHERE track_id = <source> AND pinned = 1`。live 行不复制（fork 后按 pending 重新解析）。
- **track 删除**：级联，无需改 `routes/tracks.rs:4032-4037` 的显式清理列表。

**依据。** F4.2-F4.4（overlay 的读取与事件体量）、F4.15（FK 先例与 PRAGMA）、F2.6（fork 保留 id）、F7.6（sha256 已在仓内）。

### D4 读端水合（整段重写）

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
    "reason": "…",                     // unavailable
    "resolved_at": "2026-09-12T08:00:00Z",   // ok / unavailable
    "as_of": "2026-09-11", "pinned": false,  // ok
    "view": "normalized", "field": "close", "period": "day", "range": "1Y",
    "series": [
      { "asset": "US:NVDA", "currency": "USD", "status": "ok",
        "n": 251, "first": ["2025-09-11", 118.2], "last": ["2026-09-11", 176.9],
        "change_pct": 49.66, "high": 181.3, "low": 101.4,
        "points": [[ts_ms, v], …] },   // 仅 "full"
      { "asset": "HK:9988", "status": "unknown_asset", "reason": "…" } ] }
  ```
  `status/reason/as_of/resolved_at/pinned/series[].summary` 全部**直接来自那一行**（`summary` 列原样反序列化），read 不重算。
- live `table` 的 `resolved`：`{status, resolved_at, columns: n, rows: n, caption?}`，来源 `repo.overlays_for("track", track_id)` 按 `(plugin_id, kind)` 匹配（与 F6.4 同一规则，DB 读）。
- 摘要定义（`summarize`，写行时执行一次）：`n` = 点数；`first/last` = `[YYYY-MM-DD, value]`（candles 视图 value=close）；`change_pct = (last-first)/first*100` 两位小数，`first == 0` → null；`high/low` = value 极值（candles 用 high/low 列）。HTTP 路由（D5）返回**同一行的同一字节**。
- **读路径零插件调用**：read 只做 `load_report_read_snapshot` + 一次 `report_series` 按 `(track_id, block_id)` 的查询 + 可选 `enqueue`。无超时、无并发预算。`load_report_read_snapshot` 的错误仍是 internal（文档本身读不到，与今天一致）。
- **谁能拿 `resolved`**：Planner 与 Assistant 都拿。裁量按报告内容可见性：解析由内核固定身份在后台执行（先例 F4.10：内核调用不经 `PLUGIN_TOOL_ROLES`），工具被 D2 限定为 manifest 内只读工具，结果是存进 `report_series` 的报告内容，与 `text` 里的 fence 同一可见级。`PLUGIN_TOOL_ROLES = [Planner, Worker]`（F4.9）约束的是 **agent 以自己身份发起** tools/call；这里没有 agent 身份发起的调用。Assistant 能通过写块让内核调只读工具——这与它今天能写 live `table` 的 `source` 同级（写权已在 `[Planner, Assistant]`，`track_report_blocks.rs:121-296`），且它拿到的只是 Planner 也能直接调工具拿到的东西。`taskDiagnostics` 的划线（F3.3）不动。
- token 体量估算不变：summary 每条 ≈ 45 token；full 1Y 日线 ≈ 1.5k token/条、candles ≈ 3.1k、5Y ≈ 7.5k/15.7k——`full` 只按块显式索取。

**依据。** F3.3（read 是 CAS 必经路径 → 纯 DB 读）、F3.5、F4.9/F4.10（角色门的适用边界）。

### D5 前端渲染（整段重写）

**裁决。**
- fe/：NEW `fe/web/src/features/report/series/public.tsx` + `series.module.css`，沿 F6.1 的 SVG 路线：`line`（多序列各一条 `<polyline>`，token 上色）、`normalized`（每条 rebase 到首点=100；**首点 ≤ 0 的序列不画线，图例处标 "cannot normalize (first value ≤ 0)"**，其余序列照画）、`bar`（单序列柱，多序列分组柱，≤8）、`candles`（把 `candles/public.tsx:89-190` 的绘制体抽成 `CandlesFigure({candles, overlays})` 共用）。区间切换不再客户端过滤；图上显示 `range`/`as_of`/币种/`pinned` 标记。
- 数据：NEW 路由 `GET /api/tracks/{id}/report/series/{block_id}?rev=<n>&detail=full|summary`（默认 `full`；`routes/track_report_series.rs`），Principal 鉴权同其它 track 路由。服务端：读快照找块；`rev` 与当前块 `rev` 不等 → **409** `{current_rev}`；无块 → 404；算 hash → 读行；无行/过期 → `enqueue` 并返回现状。返回 D4 的 `resolved`（`utoipa` schema → 两份 OpenAPI 重生成，F6.9）。**用 `rev` 不用 hash 绑定**：fe 在不安全上下文里没有 sha256（F6.10），而 `rev` 已在 `ReportBlock` 里且任何 payload 改动都会 bump（同内容不 bump，与 hash 不变一致）。
- fe 查询：`trackReportSeriesQueryOptions(trackId, blockId, rev)`，key `['track-report-series', trackId, blockId, rev]`；`invalidation-plan.ts` 的 `track.report_edited` 追加 `['track-report-series', trackId]`；响应 `pending` 时 `refetchInterval` 3s（2 分钟后退到 30s），非 pending 停止；过期行由服务端刷新后要等下一次 fetch（焦点/失效）才可见（G12）。
- 非 ok 态：`pending`/`unavailable` 渲染 caption + 一行文字（复用 table 的 `LiveTableNotice` 形态，F6.3）；查询 loading 渲染 caption + "Loading …"。
- legacy `web/`：不加渲染器（F6.8）；两份 OpenAPI 仍因新路由一起重生成。
- 不引入 Recharts/lightweight-charts。

### D6 安全

- `source` 只接受 `neige://plugin/<id>/<tool>`；内核从中只取 `plugin_id`/`tool` 去 `plugin_tool_route`，**从不**构造 URL；只调 manifest 内、`kind == None`、`readOnlyHint == true` 的工具；只走本地 `Stdio`/`Cli` 变体，`Http` 拒绝（D2 步骤 2-4；这是对 §11 B1/C1 的回答）。
- `series` 字符串经内核形状检查后作为 tool 参数原样交给插件，插件用 `parse_asset` 再判；内核不解释 venue。
- 解析以内核身份在后台执行；作用域 `plugin_scope_for_track` 与 agent 路由同一规则。
- 读路径：HTTP 路由走既有 Principal；MCP read 走 `resolve_report_for_caller`；两者都不调插件。
- 资源上界：8 条序列、每插件串行、30s 超时、2 MiB 回复、区间上界点数、1 MiB 行；in-flight 去重；TTL 6h。
- 插件 `market.series` 只读，不写 overlay/KV；不新增 `neige.*` 回调，不扩权限模型；不新增 Event kind。

### D7 与 #1612 `layout` 的取舍

保留：`neige://plugin/<id>/<x>` 作为唯一外部引用语法；"配置声明式、无表达式求值"；unit/currency 不做隐式换算；"缺数据显式态、零不是缺"。
放弃：一个 kind 承载布局 + 多 item + 表 + 图；数据来自 overlay 行再做 join/annotations；Recharts 与颜色 `#RRGGBB` 进 payload；模板化的 selector/total/share 计算。
参照材料（`report-layout-contract.md`、`layout.rs`、`portfolio-template-review.md`）**不在基线树**（`find` 零命中，仓内 `layout.rs` 只有 `dedicated_codex/layout.rs`）；它们是 #1612 讨论的附件，本节只取其精神，不引用行号。

## 5. 切片表

| 片 | 内容 | 依赖 | 可独立合入 | 行为变化 | 估算行数 |
|---|---|---|---|---|---|
| S1 契约 | `kinds.rs` `KIND_CHART_SERIES` + `validate_chart_series`（含 `as_of < 今天`）+ `MAX_CHART_SERIES`；`DATA_KINDS` 5 项；`kinds_tests.rs` 正反例；`contracts.rs` kinds_table 项 + F2.11/F2.12 文字；`fe/core/domain/report.ts` zod + `payloadSchemaFor`；`document/public.tsx` `case 'chart.series'` 占位；`mcp_track_report_blocks.rs` 加入口拒绝用例 | 无 | 是 | agent 可写 `chart.series`，read 无 `resolved`，fe 占位，web unsupported | ~600 |
| S2 解析任务 + 存储 + read 水合 | 迁移 `report_series`；`SeriesRequest`/`request_hash`/`summarize`；`SeriesResolver`（队列、去重、drain、`resolve`、校验清单、写行、pinned 规则）；bus 订阅者；`calm.report.read` `resolve` 入参 + `resolved`（chart.series 与 live table）+ enqueue；fork 复制 pinned 行；`plugin_tool_route` 改 `pub(crate)`；集成测试用假插件（`boot_plugin_host`）覆盖 seq 2e/3/3b/3c/3e/3f/4/4a/4b/6/7a/7b/7c/7d/8/9/12 | S1 | 是 | Planner/Assistant read 到摘要；插件缺席时 `pending`→`unavailable` | ~1000 |
| S3 market 插件 | `market.series` tool（manifest、`tools_call_reply` 分支、腾讯 ifzq + Binance klines 源、新浪兜底、内存缓存、`as_of` 截断、`unknown_asset`）、README、fixture server 测试 | S1（只共享 wire 形状；与 S2 并行） | 是 | `plugin.dev-neige-market_market.series` 对 agent 可用；S2 合入后图有数据 | ~900 |
| S4 路由 + fe 渲染 | `routes/track_report_series.rs`（rev 绑定、409）、两份 OpenAPI 重生成、`queries.ts` 查询 + 失效 + pending 轮询、`features/report/series/`、`CandlesFigure` 抽取、browser 测试 | S2 | 是 | 人看到图；S3 未合时看到 `unavailable` 文案 | ~800 |

顺序：S1 → (S2 ∥ S3) → S4。v1 的 S5（frozen 快照）并入 S2：`pinned` 只是行的一列加一条写规则，单独切片没有独立可合入的行为。总规模 ≈ 3300（v1 ≈ 3900）。S2 与 S3 的接缝是 D2 的请求/回复 JSON，两边共用 `crates/calm-server/tests/fixtures/market_series_reply.json`。S2 先于 S3 合入时，已装的 market 插件对 `market.series` 回 `unknown tool`（F4.13）→ `isError` → 行 `unavailable, reason: "plugin error: unknown tool `market.series`"`（§11 C16）。

## 6. 验收场景与 must-red 变异

每条变异**单跑**必转红。

| # | 场景 | 断言 | must-red 变异（改哪一行 → 哪条测试转红） |
|---|---|---|---|
| A1 | `upsert{kind:"chart.series"}` 合法 payload 落盘为 canonical fence | read `text` 含 fence，`parse_fence` 回同 payload | `validate_chart_series` 把 `series` 必填改成可选 → `kinds_tests::chart_series_payload_valid_and_invalid` 的 "series: required" 断言红 |
| A2 | 经 `calm.report.commit` 写无 venue 的 `series:["NVDA"]` / `as_of` = 今天 | `-32602`，docRev 不变，事件零 | **直接测入口**：`validate_chart_series` 删掉 venue 正则（或 `as_of` 上界）→ `mcp_track_report_blocks::commit_rejects_chart_series_without_venue`（S1 新增，走真实 MCP 入口）红。不再用"跳过 `render_data_block`"做变异：F2.2 的 op 层复核会让那种变异保持绿 |
| A3 | kinds 表、upsert enum、commit enum 三者含 `chart.series` 且相等 | `contracts.rs:695-711` | `block_kind_enum()` 硬编码四项 → 既有测试红 |
| A4 | 行存在时 read 默认给 summary，`n/first/last/change_pct/high/low/as_of` 与 fixture 一致 | 集成测试比对 fixture 期望 | `summarize` 里 `change_pct` 用 `(last-first)/last` → `read_hydrates_chart_series_summary_from_row` 红 |
| A5 | 插件未运行 / 超时 / `isError` → 行 `unavailable`；read 仍 200 | 三条用例，直接调 `resolve` | `resolve` 删掉 `tokio::time::timeout` 包裹 → `resolve_marks_a_hung_plugin_unavailable`（永不回复的假插件 + 测试超时 40s）红（挂死） |
| A6 | `resolve:{b:"full"}` 才有 `points`；默认无 | JSON 断言 | read 无条件塞 `points` → `read_full_is_opt_in_per_block` 红 |
| A7 | frozen：直接并发调两次 `resolve`（假插件两次回不同数据、都满足钉住条件），再改 fixture 并第三次调 | 行 = 第一次完成者的数据；`pinned=true`；第三次后行不变 | 写入去掉 `WHERE report_series.pinned = 0` → `frozen_row_is_pinned_by_first_complete_resolution_and_never_overwritten` 红 |
| A7b | frozen 回复含 `unknown_asset` / 回复 `as_of` 早于请求 | 行 `ok, pinned=false`；TTL 过期后 enqueue 非 no-op | 钉住条件改成"块级 ok 即钉" → `incomplete_frozen_reply_is_not_pinned` 红 |
| A8 | live：行过期后 read 返回旧行并 enqueue；fixture 多给一根 bar 后 drain，`as_of` 前进；docRev 不变 | 用时钟 seam 把 `resolved_at` 设成过期 | read 对过期行不 enqueue → `stale_live_row_is_served_and_refreshed` 红 |
| A9 | in-flight 去重：同键连续 enqueue 十次，假插件只收到一次 tools/call | 计数 | `enqueue` 不查 `inflight` → `refresh_is_deduplicated_per_key` 红 |
| A10 | **读路径零插件调用**：`SeriesResolver::new_unstarted()` 下对无行的块 read → `pending`，假插件 tools/call 计数为 0 | 计数 | read 里改成内联调用 resolver → `read_never_calls_the_plugin` 红 |
| A11 | 3f：`source` 指向 `market.holdings.set`（`readOnlyHint:false`）/ 不在 manifest 的名字 / ForgeAction 工具 | 行 `unavailable`，假插件计数 0 | `resolve` 删掉 `readOnlyHint` 检查 → `resolve_refuses_non_read_only_tools` 红 |
| A12 | 校验清单：回复 9 条 / 时间戳降序 / bar > as_of / NaN / 点数超上界 各一条 | 行 `unavailable, reason` 各不同 | 删掉"严格升序"检查 → `reply_with_descending_timestamps_is_unavailable` 红 |
| A13 | 删 track 后迟到的 `resolve` 写入 | 无行；任务不 panic | 去掉 FK（改成无约束表）→ `late_resolution_after_track_delete_leaves_no_orphan` 红 |
| A14 | 插件 `market.series` 对 fixture 源返回升序、`as_of` 截断、未知标的 `unknown_asset`、腾讯 `qfqday`/`day` 两键、列序重排 | S3 进程级测试 | 截断条件 `<= as_of` 改 `<` → `series_as_of_includes_that_days_bar` 红 |
| A15 | fe：`ok` 画出与序列数相同的 `<polyline>`；`normalized` 首点=100、首点 ≤ 0 的序列标不可归一化；`pending`/`unavailable` 文案；不含字面颜色 | `series/public.test.tsx` | normalized 除以 `last` 而非 `first` → `rebases every series to 100 at its first point` 红 |
| A16 | HTTP：`?rev=` 不等于当前块 rev → 409 | 路由测试 | 路由忽略 `rev` → `series_route_rejects_stale_rev` 红 |
| A17 | 两份 OpenAPI 与生成器零漂移 | CI 既有 drift 门禁 | 只重生成一份 → 另一条 CI 红 |
| A18 | 术语棘轮：本文档与新代码不引入退役词 | `gate-1316-terminology-ratchet.sh` | — |

## 7. KNOWN GAPS（登记，不加固）

- G1 价格是否复权由插件源决定（腾讯 `qfq` = 前复权），本设计不声明；`resolved` 不带 `adjusted` 字段。
- G2 `line` 视图多序列跨币种按原值画、只标币种，不换算、不双轴。
- G3 `pinned=false` 的 frozen 行（源缺 as_of 那天的 bar、或部分 `unknown_asset`）会按 TTL 反复重试直到满足，可能永远满足不了；图上标"未钉住"。
- G4 新鲜度以 6h TTL 为界；过期行被读到时先返回旧数据。
- G5 legacy `web/` 只显示 unsupported 一行；手机端同 fe。
- G6 `market.series` 对 agent 可见（无隐藏机制）。
- G7 `unavailable` 的 `reason` 不区分"未安装"与"已装未起"（都来自 `plugin_tool_route` 的 `None`）。
- G8 行不主动 GC（只随 track 删除级联）；改参数留下旧行。
- G9 港股历史无兜底源（新浪港股日线不可用）；腾讯 ifzq 挂掉时 HK 序列 `unavailable`。
- G10 周线/月线由插件从日线聚合（腾讯只用 `day`），`period` 语义"以 bar 收盘日为准"。
- G11 `McpClient` 读循环无字节上限、超时后 responder 槽位留到对端回复或传输关闭（F4.6）。这是所有内核→插件调用的共性（agent 路由、`routes/cards.rs:588`），本设计通过串行化把未决请求封顶为每插件 1 个，不在此加固传输层。
- G12 行刷新不发事件：浏览器在 `pending` 时轮询，过期行刷新后的新数据要等下一次 fetch。
- G13 内核对 `series` 只做字面去重；`HK:9988`/`HK:09988` 会得到两条同资产序列。

## 8. 与 issue 的出入

1. issue 方向 1 "`chart.candles` 收编为它的一个 view" → 保留 `chart.candles` 为独立内联 kind（D1）。
2. issue 方向 3 "内核在写入时钉住快照" → 写入事务不调插件；后台任务首次完整解析时钉住（D3）。理由：F2 写路径全在 persist 事务内。
3. issue 方向 2 "overlay 是推送不是查询 … 内核契约改动" → 请求-响应通道已存在（F4.6-F4.10），改动缩小为"内核后台任务作为调用者 + 插件声明一个只读 tool + 一张结果表"。
4. issue 方向 4 "不新增工具" → MCP 面不新增；浏览器需要一条 NEW HTTP 路由（D5）。
5. issue "`as_of` / `frozen`" 两个名字 → 只用 `as_of`。
6. **v2 新增**：read 不即时（§2.8）；issue 隐含的"读到就是最新"改为 TTL 语义。

## 9. 不确定点

- U2 `SERIES_RESOLVE_TIMEOUT = 30s`、`SERIES_TTL = 6h`、`MAX_SERIES_REPLY_BYTES = 2 MiB`、`MAX_SERIES_ROW_BYTES = 1 MiB` 是估值，S2 评审可调；改数不改结构。
- U5 `pending` 轮询节奏（3s / 2 分钟后 30s）是估值，S4 定。
- U6 腾讯 ifzq 的复权基准行（首行 2011 年）是否对所有 us 代码出现，S3 实现按日期过滤规避，测试 fixture 要包含这一行。

（v1 的 U1 由 §2.5 实测关闭；U3 由 D4 裁决关闭；U4 由 F7.6 关闭。）

## 10. 参考

- 本文 §2 所有 file:line 基于 `c534bf6b`。
- 第 1 轮评审原文：`docs/_1628-design-review-codex-v1.md`、`docs/_1628-design-review-subagent-v1.md`。
- U1 spike：编排者 2026-09-12 实测，表已复制进 §2.5。
- 相关：#1556 S1（venue-qualified identity）、#1623（`calm.report.commit`）、#960 PR3（kinds 词汇）、#1612（layout 讨论，材料不在基线）。

## 11. 处置历史

通道：codex = `_1628-design-review-codex-v1.md`；A = `_1628-design-review-subagent-v1.md`（通道 A）；编排者 = 2026-09-12 裁决。处置词：采纳 / 部分采纳 / 驳回 / 消失（因 D2 重写而不再适用）。

| 轮次 | 通道 | 发现 | 处置 | 落点 |
|---|---|---|---|---|
| 1 | 编排者 | 总裁决：解析改后台任务，read 与浏览器只读库，永不在读路径调插件 | 采纳。D2/D3/D4/D5 整段重写 | §0 引言、D2-D5、§2.8 |
| 1 | 编排者 | 存储表 vs overlay 二选一并给理由 | 采纳：选表。overlay 被侧栏全量拉取（`routes/overlays.rs:106-112`）+ `OverlaySet` 事件带整 payload（`event.rs:600`）是硬伤 | D3 |
| 1 | 编排者 | FK CASCADE vs 事务内存在性校验二选一 | 采纳：FK。`foreign_keys=ON`（`sqlite/mod.rs:264`）、先例 0104/0106 | D3、F4.15 |
| 1 | 编排者 | U1 spike 表复制进 §2.5 | 采纳 | §2.5 |
| 1 | 编排者 | must-red：A2 直接测入口、钉住用并发竞争者、新增读路径零插件调用变异 | 采纳 | §6 A2/A7/A10 |
| 1 | 编排者 | oracle 每行一个切片、`unknown_asset` 只写一处、状态词只用 `resolved.status`；切片表重排 | 采纳。v1 S5 并入 S2，4 片 ≈ 3300 行 | §3、§5 |
| 1 | 编排者 | fork：复制快照行或登记 GAP | 采纳：复制 `pinned=1` 行（fork 在事务内，`tracks.rs:2324`，id 保留 `:2717`） | D3、seq 12 |
| 1 | 编排者 | 前端零新路由优先；若选表则一条 GET 带 hash 比对 409 | 部分采纳：一条 GET；绑定载体用 `rev` 不用 hash——fe 在明文 http 下无 `crypto.subtle`（F6.10） | D5 |
| 1 | codex | BLOCKER 读报告可触发任意插件工具（`{source:"neige://plugin/p/reset"}`） | 采纳。任务经 `plugin_tool_route`（`transport.rs:771`）+ `kind == None` + `readOnlyHint == true` + 本地变体；否则 `unavailable` 且零调用 | D2 步骤 2-4、D6、seq 3f、A11 |
| 1 | codex | MAJOR 回复上限不能保证有界内存（`read_line` 先整行入内存，`mcp.rs:783`） | 部分采纳。核实为真（F4.6）；任务侧对整个 `CallToolResult` 计字节；传输层上限是 `McpClient` 所有调用者的共性（`cards.rs:588`、`transport.rs:722`），不在本设计加固，登记 G11。不再在读路径上 | D2 步骤 6、G11 |
| 1 | codex | MAJOR 超时取消泄漏 responder（`mcp.rs:667/:676`，只在 `:804` 或关闭时清） | 部分采纳。核实为真；后台串行使每插件未决 ≤ 1，读者不能放大；登记 G11 | D2 步骤 5、G11 |
| 1 | codex | MAJOR 每块 8s 不等于每次 read 8s；需并发预算 | 消失：read 不再等任何插件调用（D4 纯 DB 读）；任务按插件串行，延迟只落在队列 | D2、D4 |
| 1 | codex | MAJOR 插件停机与作用域策略矛盾：绑定 track 的 owner 停了 → `TrackPluginScope::None`（`tool_visibility.rs:142`）→ 先到 `unavailable` | 采纳。核实为真（F4.11）。v2 定义：`pending` 只表示"无行"；任务遇到 `None`/未运行都写 `unavailable, reason="not running"`；seq 3b 用绑定 track 测 | D2 步骤 2-3、seq 3b |
| 1 | codex | MAJOR 成功信封会把缺失数据永久冻结 | 采纳。钉住条件 = 每条 `ok` ∧ 回复 `as_of == 请求 as_of`；否则 `pinned=0` 按 TTL 重试 | D3、seq 7d、A7b |
| 1 | codex | MAJOR 快照插入与 track 删除竞态（清理在 `tracks.rs:4032`，`track_delete_tx :4037`） | 采纳。FK `ON DELETE CASCADE`，迟到写入在约束处失败 | D3、seq 9、A13 |
| 1 | codex | MAJOR 查询 key 里的 payload hash 不绑定 HTTP 响应 | 采纳（载体改 `rev`）。`?rev=` 不等 → 409 `{current_rev}` | D5、seq 5、A16 |
| 1 | codex | MAJOR 共享 resolver 不能建立人/AI 数据相等 | 采纳。两个读者读同一行的同一 `summary`/`data` 字节；`resolved_at` 一并返回 | D3、D4、seq 10 |
| 1 | codex | MAJOR 回复校验没有执行声明的数据契约 | 采纳。校验清单：一一对应、数量、元组长度、有限数、严格升序、日期 ≤ as_of、点数 ≤ 区间上界、行字节 | D2 步骤 6、seq 3e、A12 |
| 1 | codex | MAJOR 首点为 0 的合法序列让 normalized 未定义 | 采纳。显式"不可归一化"态（首点 ≤ 0） | D5、A15 |
| 1 | codex | MAJOR 两条 must-red 变异不可靠：A2 被 `track_report.rs:520/:539` 复核兜住；A7 的 `INSERT OR REPLACE` 顺序重读仍绿 | 采纳。核实为真（F2.2）。A2 变异改在 `validate_chart_series` 本身；A7 用并发竞争者 + `WHERE pinned=0` 变异 | §6 A2/A7 |
| 1 | codex | MAJOR oracle 归属与负态不闭合；"三个非 ok 态"只有两个 | 采纳。每行一片；`unknown_asset` 只在 3d/S3；非 ok 态明确为 `pending`/`unavailable` 两个 + 查询 loading | §3、D5 |
| 1 | codex | MINOR F4.6 漏内核 overlay 写者（`card_fsm.rs:558`） | 采纳。F4.5 改写为"外部写方 / 内核内部写者"两列 | F4.5 |
| 1 | codex | MINOR F4.9 "唯一生产调用者"为假（`routes/cards.rs:588`） | 采纳。F4.10 新增；D4 用它论证内核调用不经角色门 | F4.10、D4 |
| 1 | codex | MINOR S4 pre-S3 状态写错：已装插件回 `unknown tool`（`main.rs:2572`）→ `unavailable` 不是 `pending` | 采纳。F4.13 记录；§5 S4 行为改为 `unavailable`；G9 改写 | F4.13、§5 |
| 1 | codex | MINOR oracle 行 8 过度声称（周末无新 bar；滚动窗口 n 不变） | 采纳。用受控 fixture 多给一根 bar 断言 `as_of` 前进，不断言 `n` | seq 8、A8 |
| 1 | A | B1 内核代读者调插件是无守卫 RPC 代理（不查 manifest/kind/readOnlyHint、允许 Http、Assistant 绕过 `PLUGIN_TOOL_ROLES`） | 部分采纳。构造 1-3 全部采纳（同 codex BLOCKER）；"Assistant 不得拿 `resolved`" 驳回：`PLUGIN_TOOL_ROLES`（`transport.rs:88`）在 `:710` 门的是 agent 以自身身份发起的 tools/call；内核发起的调用已有不经它的先例（`cards.rs:588`）；结果是报告内容，Assistant 已能写 `source`（`track_report_blocks.rs:121-296`）且能读 `text` | D2、D4、D6 |
| 1 | A | M1 `payload_hash` 含 caption/view/overlays，改说明文字就解冻 | 采纳。`request_hash` 只盖 `(series, fields, range, period, as_of)` | D1、seq 7c |
| 1 | A | M2 首次块级 ok 钉住把部分失败永久化 | 采纳（同 codex）。 | D3、A7b |
| 1 | A | M3 `as_of` 无上界 | 采纳。`as_of < 今天 UTC`；钉住还要求回复 `as_of` 相等 | D1、seq 2n/7 |
| 1 | A | M4 默认水合挂在 CAS 路径且串行；单线程插件排队 | 消失：read 纯 DB 读；串行化改在内核任务侧与插件单线程同构 | D2、D4 |
| 1 | A | M5 F4.6 "写方只有…" 与代码自述矛盾 | 采纳（同 codex MINOR） | F4.5 |
| 1 | A | M6 F2 穷举漏项（`track_recipes.rs:275`、`tracks.rs:1006/:2795`、`track_report.rs:643/:660`）、F2.3 载体指错 | 采纳。重跑穷举命令，9 处生产入口逐条列出，F2.2/F2.3 载体改正 | §2.2 |
| 1 | A | m1 `compatibility.rs` 在 calm-types；`WEB_COMPAT_VERSION=27` | 采纳 | F7.3 |
| 1 | A | m2 `track_report_doc.rs:235` 就是 SHA256 观察 hash | 采纳。F7.6 新增，U4 关闭 | F7.6、§9 |
| 1 | A | m3 D7 引用的 #1612 文件不在基线 | 采纳。标"不在基线树"，去掉行号 | D7 |
| 1 | A | m4 oracle 3a 的 `tools_call` 签名对 Http/Cli 是双参 | 采纳。D2 步骤 4 按变体分派 | D2 |
| 1 | A | m5 事件 kind 权威源是 `calm-types/src/event.rs` | 采纳（v1 已如此，无改动） | §3 |
| 1 | A | m6 `HK:9988`/`HK:09988` 内核去重不到 | 采纳。声明内核只做字面去重，登记 G13 | D1、G13 |
| 1 | A | m7 fork 后无快照行 | 采纳。fork 事务里复制 `pinned=1` 行 | D3、seq 12 |
| 1 | A | m8 `resolved` 用枚举而非 `Option`+条件字段 | 采纳 | D4 |
| 1 | A | m9 切片可合入性核对；S2 先于 S3 时 `-32601`/`isError` 归 `unavailable` | 采纳（与 codex MINOR 合并写进 §5） | §5 |
| 1 | A | m10 纪律核对全 ✓ | 无改动 | — |
| 1 | 编排者 | 全称量词附穷举命令；不发明机制（核对 `plugin_tool_route`/`trusted_forge_plugin`/`ConnectorClient`） | 采纳。§2.2 命令与结果；`plugin_tool_route` `transport.rs:771-800` 私有 fn（S2 改 `pub(crate)`）；`trusted_forge_plugin` 在 `forge_trust.rs:20`（本设计不走该臂）；`ConnectorClient` `connector.rs:44-55` | §2.2、F4.8/F4.9、D2 |
