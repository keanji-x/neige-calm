# 声明式图表 `chart.series`（#1628）— 设计 v1

基线：`origin/main` = `c534bf6b`（工作树 `feat/report-chart-series`）。所有 file:line 都在该基线上实测（[实测]），未核实的写 "未核实"。

## 1. 目标与非目标

**目标。** 让投研报告里的图表只*命名*数据与视图：Planner / 人写一个 `chart.series` 块（`{series, field, range, view, as_of?}`），market 插件按需解析"资产 × 字段 × 区间 → 序列"，内核对每个这样的块提供两态（`as_of` 缺席 = live，随源流动；`as_of` 存在 = frozen，钉在时点）；`calm.report.read` 对数据块返回 `resolved` 摘要（默认）或原始序列（按块显式要），read 永不因插件失败而失败；前端 fe/ 用现有 SVG 路线画 line / normalized / bar / candles；人与 AI 看到同一份数据。

**非目标。** 不在正文里发明宏语法；不给内核加行情源（数据仍由插件解析）；不把 `chart.candles` 的已存文档做数据迁移；不给 legacy `web/` 加新渲染器（它按现有规则显示 `unsupported block kind chart.series`，见 §4 D5）；不做缩放/联动/图表库懒加载；不做盘中序列；不做跨币种换算（每条序列自带 `currency`，normalized 视图天然可比，line 视图按原值画并标币种）。

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

### 2.2 写端入口（`crates/calm-server/src/mcp_server/tools/track_report_blocks/`）

穷举命令：`grep -rn "validate_payload\|render_data_block\|check_prose_markdown" crates --include='*.rs'`（在工作树跑，去掉 kinds.rs / kinds_tests.rs / mod.rs 自身）。结果里**生产**写入口共 7 处：

| # | 入口 | payload 校验 | 位置 |
|---|---|---|---|
| F2.1 | `calm.report.blocks.upsert` 与 `calm.report.commit` 的每个 `upsert` op 共用 `resolve_upsert_content`：prose 走 `check_prose_markdown`，数据 kind 走 `render_data_block`，其它 kind 报 `unknown_kind_message` | 是 | [实测] `track_report_blocks.rs:546-617`，commit 描述 `:340-350` |
| F2.2 | `calm.report.write_markdown`：走 `ReportDocOp::WriteMarkdown` → op 层 `track_report_guard::validate_body_fences`（`invalid_neige_fences` + 每个 fence `validate_payload`） | 是 | [实测] `track_report_blocks.rs:291-338`；`track_report_guard.rs:127-145` |
| F2.3 | prose `Replace` shim（`calm.report.write` / `.edit`）：同一 guard | 是 | [实测] `track_report_guard.rs:1-60`（模块头）、`:217` |
| F2.4 | 人用 HTTP：`POST /api/tracks/{id}/report/blocks` 等，`block_content` 同样 `render_data_block` | 是 | [实测] `routes/track_report_blocks.rs:18-29`、`:120-142` |
| F2.5 | 从 recipe 建 track 时的 body 校验 `validate_body_fences`（`routes/tracks.rs`） | 是 | [实测] `routes/tracks.rs:941`、`:2848` |
| F2.6 | 内核自写的 task 块：`track_report/user_start.rs:56`、`track_report/dispatch.rs:212`、`track_report/repair.rs:13` 都过 `render_data_block` | 是 | [实测] 同列 |
| F2.7 | **schema 自描述**只有一处：`contracts.rs::kinds_table()`，`blocks.kinds` 直接返回它；`upsert` 与 `commit` 的 `kind` enum 由 `block_kind_enum()` 从同一张表读出，测试锁死三者相等 | — | [实测] `contracts.rs:41`、`:444-455`、测试 `:695-711` |
| F2.8 | `kinds_descriptor` 的 description 文字**手写**列了四个 kind 名，`upsert_descriptor` 同样手写 "`chart.candles` / `table` / `app` / `task`" | — | [实测] `contracts.rs:20-24`、`:311-313` — 新 kind 要改这两段文字，无测试覆盖 |
| F2.9 | `chart.candles` 的 usage 文字宣称 "The kernel has no market-data source: include every candle" | — | [实测] `contracts.rs:92-100` — 本设计落地后这句要改 |

结论：新 kind 只需进 `DATA_KINDS` + 一个 `validate_chart_series` + `kinds_table` 一项，7 处入口自动覆盖（它们都调 `render_data_block`/`validate_payload`）。

### 2.3 读端

| # | 事实 | 位置 |
|---|---|---|
| F3.1 | `ReportReadSnapshot { updated_at, schema_version, doc_rev, summary, body, blocks: Vec<ReportBlock>, task_diagnostics }`，一次 `card_get_with_body_crdt` 单行读，三种来源（cache / CRDT 投影 / legacy 派生） | [实测] `track_report_read.rs:12-20`、`:43-147` |
| F3.2 | `ReportBlock { id, kind, rev, payload }` — 快照里**有** payload | [实测] `calm-types/src/track_report.rs:16-22` |
| F3.3 | `calm.report.read` 响应 `{ text, body(alias), summary, schemaVersion, docRev, updated_at, blocks: [{id, kind, rev}], taskDiagnostics?(Planner only) }`；入参只有 `with_markers` | [实测] `mcp_server/tools/track_report.rs:107-140`（descriptor）、`:142-222`（handler）、index 构造 `:190-194` |
| F3.4 | **live `source` 在读端不解析**：handler 只把 `flat_text` 拼进 `text`，块索引不带 payload；agent 看到的就是 fence 里的 `{"source": ...}` | [实测] `track_report.rs:172-194` |
| F3.5 | **没有面向 agent 的 overlay 读取工具**：`grep -rln overlay crates/calm-server/src/mcp_server/` 只命中 `contracts.rs`（描述文字），`overlays_for/overlays_by_kind` 在 `mcp_server/` 下零调用 | [实测] 命令与结果同列 |
| F3.6 | Planner 与 Assistant 都能 read；`taskDiagnostics` 仅 Planner | [实测] `track_report.rs:148-154`、`:211-213` |

### 2.4 overlay 机制与内核↔插件调用面

| # | 事实 | 位置 |
|---|---|---|
| F4.1 | `neige.overlay.set` 回调：`entity_kind` 必须 externally_writable（`card`/`track`），manifest `overlays_write` 授权，`validate_overlay_payload` 只校验内核自有 kind（插件 kind 不透明、**无字节上限**），`plugin_id` 由内核注入、忽略参数里的 | [实测] `plugin_host/callbacks.rs:268-320`；`calm-truth/src/validation.rs:417-445`、`:575-577` |
| F4.2 | 存储：`overlays` 表 `UNIQUE(plugin_id, entity_kind, entity_id, kind)`，upsert 为 `ON CONFLICT ... DO UPDATE` | [实测] `calm-truth/migrations/0001_init.sql:42-52`；`calm-truth/src/db/sqlite/overlay.rs:7-16` |
| F4.3 | 读：`overlays_for(entity_kind, entity_id)` / `overlays_by_kind(entity_kind)`；写：`overlay_upsert` / `overlay_delete` / `overlays_clear_by_plugin` | [实测] `db/mod.rs:83-84`、`:622-630`、`:752` |
| F4.4 | 事件 `Event::OverlaySet(Overlay)` / `OverlayDeleted`，wire 名 `overlay.set` / `overlay.deleted`，scope 含 `<entity_kind>:<id>`、`plugin:<id>`、`plugin:*`、`*` | [实测] `calm-types/src/event.rs:600-608`、`:1428-1429`、`:1583-1598` |
| F4.5 | 前端路由：`GET /api/overlays?entity_kind=&entity_id?`（`list_overlays`），track 详情也带 overlays | [实测] `routes/overlays.rs:102`、`:125-134`；`routes/tracks.rs:1097`、`:1111-1119` |
| F4.6 | **overlay 是推送模型**：写方只有插件回调与 M1 手测路由；内核不主动向插件"要"overlay | [实测] `routes/overlays.rs:1-11` |
| F4.7 | **内核→插件请求-响应面已存在**：`McpClient::tools_call(name, arguments, track_id)` 把 track 放在 `params._meta["dev.neige/track"]`；`McpClient::call` 无超时（只有 `initialize` 包了 10s） | [实测] `plugin_host/mcp.rs:617-640`、`:656-679`、超时 `:477` |
| F4.8 | 按插件 id 取客户端：`PluginHost::connector_client(id) -> Option<ConnectorClient>`（stdio/http/cli 三态）、`mcp_client(id)` | [实测] `plugin_host/mod.rs:3296`、`:3312` |
| F4.9 | 该面今天的唯一生产调用者是 agent 经内核 MCP 的 `plugin.<plugin_id>_<tool>` 路由：身份先于路由、`plugin_scope_for_track` 限定模板绑定的 track、`require_role_any(PLUGIN_TOOL_ROLES)`、按 `ConnectorClient` 变体分派；**无超时** | [实测] `mcp_server/transport.rs:667-746`、路由 `:771-801` |
| F4.10 | 插件侧回调 `Rpc::call` 有 15s 超时；插件把 tools/call 送到单工作线程串行处理 | [实测] `plugins/market/main.rs:52`、`:74-140`、`:2580-2700` |
| F4.11 | `neige.kv.set` 配额按整个 keyset 的 JSON 文本长度计；market manifest 设 `kv_quota_bytes: 262144`（默认 1 MiB） | [实测] `callbacks.rs:792-835`；`plugins/market/manifest.json`（permissions）；`perms.rs:37` |
| F4.12 | `ExposedTool { name, description?, kind?: ToolKind(只有 ForgeAction), input_schema?, annotations? }`；没有"仅内核可调、对 agent 隐藏"的标记 | [实测] `plugin_host/manifest.rs:761-780`；`grep -n "hidden\|internal" mcp_server/tool_visibility.rs` 零命中 |

回答：**overlay 是推送模型；但"参数化按需读取"不需要新回调协议** — 内核已经有 kernel→plugin 的 `tools/call` 请求-响应通道（F4.7/F4.8），今天只被 agent 路由使用（F4.9）。新的是"内核自己作为调用者"（§4 D2）。

### 2.5 market 插件

| # | 事实 | 位置 |
|---|---|---|
| F5.1 | 规范身份 `AssetId { venue, symbol }`，`canonical()` = `<VENUE>:<SYMBOL>`；venue 闭集 `CRYPTO/US/HK/SH/SZ`，`CN:` 只为读回旧数据、永不定价；无按形状猜 venue 的规则 | [实测] `main.rs:175-250`、`parse_asset :295`、`canonical_symbol :349`（HK 折成五位） |
| F5.2 | `quote_asset`：CRYPTO → Binance `/api/v3`，其余 → Sina `hq.sinajs.cn/list=<symbol>`（需 `Referer`，GBK）；只取**现价**字段 | [实测] `main.rs:699-702`、`:834-866`、`:1125-1160` |
| F5.3 | 结算：`Currency` 与 `FxLeg`（Sina `fx_s*` 行）、USDT=1 USD 是假设 | [实测] `main.rs:622-668`、`:1244`、`:1281-1312` |
| F5.4 | **历史序列今天没有源**：模块头 "History is forward-only ... the plugin never back-fills from klines"；`grep -n "kline\|daily" main.rs` 只命中这条注释；`history/<track_id>` KV 里存的是**组合总值**逐 tick 点（≤500），`portfolio.history` 是 `table` 形状 | [实测] `main.rs:33-36`、`:55`、`:64-65`、`:1974-2033`、`:2063-2074` |
| F5.5 | 推送：`push_overlay` → `neige.overlay.set{entity_kind:"track", entity_id, kind, payload}` | [实测] `main.rs:2045-2061` |
| F5.6 | 工具：`market.quote`（readOnlyHint）、`market.holdings.set/list`；track 由内核 `_meta` 注入 | [实测] `manifest.json` exposes_tools；`main.rs:494-500` |
| F5.7 | 新浪 / 腾讯是否提供日线历史：README 与代码**都没有记录** — **未核实**。Binance klines：README 只记录 `data-api.binance.vision` 与 `api.binance.com` 同样服务 `/api/v3` 路径（`README.md:291-299`），klines 端点本仓**未核实** | [实测] `grep -n "kline\|history" README.md` 只命中组合历史条目 |
| F5.8 | 已有测试基建：`sina_fixture.rs`、进程级 `tests/cases/market_plugin_process.rs`（`boot_sources`、`call_tool(id, name, args, track)`） | [实测] `market_plugin_process.rs:124`、`:294` |

### 2.6 前端

| # | 事实 | 位置 |
|---|---|---|
| F6.1 | fe 蜡烛图走 **SVG**，头注三条理由：token 可用（`currentColor`/`var(--…)`，canvas 读不到 oklch）、零依赖零懒加载 chunk、是 markup（继承 reduced-motion/focus）；CN 极性红涨绿跌、涨空心跌实心 | [实测] `fe/web/src/features/report/candles/public.tsx:1-22` |
| F6.2 | 蜡烛图区间客户端过滤 `1M/3M/6M/1Y/All`，`viewBox` 740×256，MA20/60 客户端算 | [实测] `candles/public.tsx:27-58`、`:89-125` |
| F6.3 | live table 三态：无 resolver / 未推送 / 推送了但不是 table，都渲染 caption + 一行说明；推送 payload 的 caption 优先 | [实测] `table/public.tsx:19-56` |
| F6.4 | resolver 来源：`router/public.tsx` 用 `liveTableOverlayPayload(track.id, overlays, source)`，按 `(entity_kind='track', entity_id, plugin_id, kind)` 匹配 | [实测] `fe/web/src/app/router/public.tsx:3049-3052`；`fe/core/domain/track.ts:110-131` |
| F6.5 | zod：`chartCandlesPayloadSchema` strictObject；`tableBlockPayloadSchema = union[live, inline]`；`payloadSchemaFor(kind)` switch，未知 kind → `{kind:'unsupported', declaredKind}`；`ReportBlock` 是闭合联合 | [实测] `fe/core/domain/report.ts:52-59`、`:101-118`、`:230-272` |
| F6.6 | 渲染 switch `BlockBody`：`table/chart.candles/task/app/unsupported/prose`，unsupported 渲染 "unsupported block kind X" | [实测] `fe/web/src/features/report/document/public.tsx:326-345` |
| F6.7 | overlay 查询 key `['overlays', entityKind]`，`overlay.set` 事件失效 `['overlays', kind]` 与 `['track', id]`；`track.report_edited` 失效 `['track-report']` 等 | [实测] `fe/web/src/app/providers/queries.ts:173`、`:484-488`；`fe/core/events/invalidation-plan.ts:290-304` |
| F6.8 | legacy `web/`：`typedReportBlockSchema` 只认五个 kind，其它落 `opaqueReportBlockSchema`；渲染 `report-blocks/index.tsx` 未知 kind → `unsupported block kind {kind}`（`role="note"`） | [实测] `web/src/cards/builtins/track-report.tsx:200-226`；`web/src/pages/report-blocks/index.tsx:60-61` |
| F6.9 | 两份签入 OpenAPI：`fe/core/api/generated/openapi.json` 与 `web/src/api/openapi.json`（各 17 处 overlay） | [实测] `grep -c overlay` 两文件 |
| F6.10 | 事件 zod 在 `fe/core/api/schemas.ts:529`（`overlay.set`），生成 wire 在 `fe/core/api/generated/wire.ts:169` | [实测] |

### 2.7 迁移与版本常量

| # | 事实 | 位置 |
|---|---|---|
| F7.1 | 迁移目录 `crates/calm-truth/migrations/`，最新 `0106_candidate_repair.sql`；**本设计只有 S5（frozen 快照表）需要新迁移，号最后才定** | [实测] `ls crates/calm-truth/migrations \| tail` |
| F7.2 | `SYNC_EVENT_VERSION = 20`；门禁 `scripts/gate-sync-event-version-lockstep.sh` 比对迁移里 `event_version = N` 字面量与常量。**本设计不新增 Event kind**（§4 D3/D5），不 bump | [实测] `calm-types/src/event.rs:258`；脚本头 `:1-45` |
| F7.3 | `REST_API_VERSION = "7"`（`compatibility.rs:5`）；`WEB_COMPAT_VERSION` 三处锁步门禁 `gate-web-compat-version-lockstep.sh`。新增一条 GET 路由是加法，不 bump | [实测] |
| F7.4 | `TrackReportPayload::SCHEMA_VERSION = 4`，"Bumping is Tier A breaking"；新 kind 不改 payload 形状（kind 是 wire 上的字符串，未知 kind 前端降级），不 bump | [实测] `calm-types/src/track_report.rs:146-152` |
| F7.5 | #1316 术语棘轮扫 `docs`（`RATCHETED_SCOPES=(crates fe docs e2e)`）；本文档避免退役词 | [实测] `scripts/gate-1316-terminology-ratchet.sh:451-459` |

## 3. Oracle trace

Planner 写 `chart.series` → 内核校验 → 插件解析 → 前端渲染 → Planner `read` 拿摘要 → 人看到同一份。事件 kind 均为真实 `Event` 变体（`calm-types/src/event.rs`），"NEW" 标注新增。状态：✅ 今天已成立 / ⚠️ 本设计新增或改动 / ❌ 今天为假、本设计修正。

| seq | phase | actor | trigger / MCP tool | 效果 | 可观察事件 | 不变式断言 | 状态 |
|---|---|---|---|---|---|---|---|
| 1 | discover | Planner | `calm.report.blocks.kinds` | 返回含 `chart.series` 的 kinds 表（schema+usage） | 无（read-only） | `upsert.kind.enum == commit.ops.kind.enum == kinds_table.kinds`（`contracts.rs:695-711`） | ⚠️ NEW 表项 |
| 2 | write | Planner | `calm.report.commit{ops:[{op:"upsert", kind:"chart.series", payload:{source:"neige://plugin/dev-neige-market/market.series", series:["US:NVDA","HK:9988"], view:"normalized", range:"1Y"}}], if_doc_rev, message}` | `validate_payload` 通过 → `render_fence` 存 canonical fence，docRev+1 | `CardUpdated`（`event.rs:392`）+ `TrackReportEdited`（`:579`）恰好各一 | fence 在 `text` 里可 `parse_fence` 回同一 payload；`flatten(split_body(body))==body` | ⚠️ 校验器 NEW |
| 2n | write-neg | Planner | 同上但 `series: ["NVDA"]`（无 venue）/ 9 条 / `view:"candles"` 配 2 条 / `source:"https://…"` | `-32602`，字段级错误 `series[0]: must be <VENUE>:<SYMBOL>` 等；**不写不发事件** | 无 | 事务前拒绝（`resolve_upsert_content`） | ⚠️ |
| 3 | read | Planner | `calm.report.read{}` | `blocks[i]` 对 `chart.series` 块多出 `resolved`（summary） | 无 | read 永远 200；`resolved.status ∈ ok/pending/unavailable` | ❌→⚠️ 今天无 `resolved` |
| 3a | resolve | 内核 | `track_report_series::resolve(track_id, block)` → `connector_client("dev-neige-market").tools_call("market.series", args, Some(track_id))`，`timeout(8s)` | 插件返回 `{as_of, series:[…]}` | 无内核事件；插件 stderr 日志 | 内核只按 `source` 的 plugin_id 取客户端，绝不发 URL（§4 D6） | ⚠️ NEW |
| 3b | resolve-neg | 内核 | 插件未运行（`connector_client` 为 `None`） | `resolved.status="pending", reason="plugin dev-neige-market is not running"` | 无 | read 仍 200 | ⚠️ |
| 3c | resolve-neg | 内核 | 插件超时 / 回复不是 object / `is_error` | `status="unavailable", reason` | 无 | 同上；超时不阻塞 read 超过 8s | ⚠️ |
| 3d | resolve-neg | 插件 | `series` 里有 `US:NOPE` | 块级 `status="ok"`，该条 `series[j].status="unknown_asset", reason`（其它条正常） | 无 | 部分失败不拖垮整块 | ⚠️ |
| 3e | resolve-neg | 内核 | payload 合法但 8 条 × 5000 点回复超 `MAX_SERIES_REPLY_BYTES` | `status="unavailable", reason="reply too large"` | 无 | 内核内存有上界 | ⚠️ |
| 4 | read-full | Planner | `calm.report.read{resolve:{"b_x":"full"}}` | 该块 `resolved.series[j].points` 带原始序列 | 无 | 只有点名的块给 full；未点名的仍 summary | ⚠️ |
| 5 | render | 浏览器 | `GET /api/tracks/{id}/report/series/{block_id}`（NEW 路由） | 同一个内核 resolver，返回同一 JSON 形状 | 无（HTTP） | fe 摘要字段 = read 摘要字段（同一 Rust 函数产出） | ⚠️ NEW |
| 5a | render | fe | `ReportSeriesBlock` | SVG line/normalized/bar；`candles` 视图复用蜡烛图绘制 | 无 | 三个非 ok 态渲染 caption + 一行文字（同 table） | ⚠️ NEW |
| 6 | re-render | fe | `track.report_edited` 事件 | 失效 `['track-report-series', trackId]`；块 payload 改了就重取 | `TrackReportEdited` | 查询 key 含 payload hash，旧数据不会贴到新参数上 | ⚠️ |
| 7 | freeze-write | Planner | `commit` 写 `as_of:"2026-09-10"` | 存 fence（含 as_of） | `CardUpdated`+`TrackReportEdited` | 写事务里**不调插件**（`commit_report_op` 在 tx 内，`track_report_blocks.rs:20-30`） | ⚠️ |
| 7a | freeze-pin | 内核 | 第一次成功 resolve（read 或 HTTP） | `INSERT OR IGNORE report_series_snapshots(track_id, block_id, payload_hash, as_of, resolved_at, data)`；`resolved.pinned=true` | 无（S5 前 `pinned=false`） | 同一 `(block_id, payload_hash)` 至多一份；后续 resolve 只读该行 | ⚠️ S5 |
| 7b | freeze-neg | 源 | 快照钉住后源数据修正（复权/更正） | frozen 块**不变**，`resolved` 仍是钉住的数据 | 无 | 文档论述下面的图不动 | ⚠️ S5；S5 前 KNOWN GAP G3 |
| 7c | freeze-neg | Planner | 改 `range` 或 `series`（rev bump，payload_hash 变） | 新 hash → 新快照（下一次 resolve 钉住）；旧行留着直到 track 删除 | `CardUpdated`+`TrackReportEdited` | 快照身份 = `(block_id, payload_hash)`，不是 rev | ⚠️ S5 |
| 8 | live-drift | 源 | live 块，过一天 | `resolved.as_of` 从 `2026-09-11` 变 `2026-09-12`，`n+1`；文档 docRev **不变** | 无 | 文档不重写、不重版本 | ⚠️ |
| 9 | caps | Planner | `range:"5Y", period:"day"`（≈1260 点）→ 合法；插件回 >5000 点 | 内核截到最新 `MAX_CHART_CANDLES` 点并 `truncated=true` | 无 | 上界与 `chart.candles` 同一常量 | ⚠️ |
| 10 | human-read | 人 | 打开报告 | seq 5 的数据 = seq 3 的 `resolved`（同一 resolver、同一快照行） | — | 人与 AI 同源 | ⚠️ |
| 11 | legacy | 人（web/） | 打开报告 | `unsupported block kind chart.series` 一行 | — | 差异被声明（§4 D5、§7 G5） | ✅ 现有降级 |
| 12 | delete | 人 | 删 track | overlay 清理旁加 `report_series_snapshots` 删除 | `TrackDeleted` 等现有事件 | 无孤儿快照 | ⚠️ S5 |

## 4. 决策

### D1 契约：`chart.series` payload

**问题。** 精确 JSON 形状；`chart.candles` 是否收编。

**备选。** (a) 新 kind `chart.series`，只命名数据；`chart.candles` 原样保留为内联 kind。 (b) 把 `chart.candles` 扩成双形态（有 `candles` = inline，有 `series` = 声明式），像 `table` 那样按字段存在选形态。 (c) 新 kind 并废弃/迁移 `chart.candles`。

**裁决：(a)。** 形状（strict，未知字段拒绝，字符串 ≤ `MAX_STRING_CHARS`）：

```jsonc
{
  "source":  "neige://plugin/<plugin_id>/<tool>",   // 必填。同 F1.4 两段语法（复用 validate_live_source）；
                                                    // 第二段是插件 *tool 名*（如 market.series），不是 overlay kind
  "series":  ["US:NVDA", "HK:9988"],                 // 必填，1..MAX_CHART_SERIES(=8)，去重；
                                                    // 每条 ^[A-Z]{2,8}:[A-Za-z0-9._-]{1,32}$（venue 必填；内核只查形状，venue 语义归插件 F5.1）
  "field":   "close",                                // 可选，close|open|high|low|volume，默认 close；view=candles 时必须缺席
  "range":   "1Y",                                   // 可选，1M|3M|6M|1Y|2Y|5Y，默认 1Y
  "period":  "day",                                  // 可选，day|week|month（沿用 F1.5 词汇），默认 day
  "view":    "line",                                 // 可选，line|normalized|bar|candles，默认 line；candles 要求 series.len()==1
  "as_of":   "2026-09-10",                           // 可选，YYYY-MM-DD；存在 = frozen（§D3）；缺席 = live
  "overlays": ["ma20"],                              // 可选，ma20|ma60（沿用 F1.5），只对 line/candles 生效
  "caption": "…"                                     // 可选
}
```

- 不允许 inline 数据（无 `candles`/`points` 字段）：数据只由 `source` 解析。要内联的数据（插件不覆盖的标的，如指数、B 股 — 插件明确拒绝，F5.1）继续用 `chart.candles`。
- `chart.candles` **不收编、不迁移**：它已存在于已存文档、两个前端各有渲染器（F6.1、F6.8）、有校验与集成测试（F1.10、`mcp_track_report_blocks.rs:1370/1425`）。收编要么改已存 fence（CRDT 字节级数据迁移，本仓没有先例；已发布迁移不可改），要么让一个 kind 承载两种形态（`table` 的 live/inline 互斥规则已经是"两个答案"问题的边界，再加一层不值）。折衷：`kinds_table` 里 `chart.candles` 的 usage 文字改为"内联数据的逃生口；行情能由插件解析的标的请用 `chart.series`"（F2.9 那句"kernel has no market-data source"删掉，不是修改）。**这是与 issue 方向 1 的出入**（§9）。
- caps：`MAX_CHART_SERIES = 8`（新常量，放 `kinds.rs:46-55` 旁）；点数上界复用 `MAX_CHART_CANDLES`（对回复截断，不是对 payload）；payload 本身很小，`MAX_CANONICAL_BYTES` 自然覆盖。
- 校验落点：`kinds.rs` 新 `validate_chart_series`（挨着 `validate_chart :483`），`DATA_KINDS` 变 5 项（`:58`），`KIND_CHART_SERIES` 常量；`contracts.rs::kinds_table` 加一项并改 F2.8 两段手写文字；`fe/core/domain/report.ts` 加 `chartSeriesPayloadSchema` + `payloadSchemaFor` 分支；`web/` 不加（落 opaque，F6.8）。

**依据。** F1.1-F1.6（词汇闭集与 allow-list 风格）、F1.4（`source` 语法复用，且"不查存在"与插件未装是正常态一致）、F5.1（venue 语义归插件，内核不该复制 venue 表 — 复述 X 的检查必产错）、F6.5/F6.8（前端未知 kind 降级已就位）。

### D2 数据解析面：内核如何向插件要序列

**问题。** overlay 是推送；"资产 × 字段 × 区间 → 序列"需要请求-响应。

**备选。** (a) 新 `neige.*` 回调让插件"按需生成 overlay"（插件收到内核通知后 push 一个 overlay，键含参数 hash）。 (b) 内核直接调用插件 manifest 里声明的 tool（F4.7/F4.8 已有通道）。 (c) 插件把全部历史预推成 overlay。

**裁决：(b)。** 机制：

- 插件 manifest `exposes_tools` 加 `market.series`（`readOnlyHint: true, openWorldHint: true`）。请求形状（内核由 payload 派生）：
  ```jsonc
  { "series": ["US:NVDA","HK:9988"], "fields": ["close"] /* candles 视图 → ["open","high","low","close","volume"] */,
    "range": "1Y", "period": "day", "as_of": "2026-09-10" /* 可选 */ }
  ```
  回复（`CallToolResult.structuredContent`，与 `market.quote` 同风格 `text_result`，`main.rs:2306`）：
  ```jsonc
  { "as_of": "2026-09-11",                       // 全部序列里最新的 bar 日期
    "series": [
      { "asset": "US:NVDA", "currency": "USD", "status": "ok",
        "points": [[ts_ms, close], …] /* candles: [ts_ms,o,h,l,c,v] */ },
      { "asset": "HK:9988", "status": "unknown_asset", "reason": "…" } ] }
  ```
- 内核侧 NEW 模块 `crates/calm-server/src/track_report_series.rs`（挨着 `track_report_read.rs`）：`pub async fn resolve_series_block(ctx, track_id, block) -> ResolvedSeries`。步骤：解析 `source` → `(plugin_id, tool)`；`plugin_scope_for_track(ctx, Some(track_id)).allows(plugin_id)` 否则 `unavailable`（与 F4.9 同一规则）；`plugin_host.connector_client(plugin_id)` 为 `None` → `pending`；`tokio::time::timeout(SERIES_RESOLVE_TIMEOUT = 8s, client.tools_call(tool, args, Some(track_id)))`（`McpClient::call` 无超时 F4.7，必须在此包）；`is_error` / 非 object / 超 `MAX_SERIES_REPLY_BYTES = 2 MiB` → `unavailable`；每条序列点数 > `MAX_CHART_CANDLES` → 保留最新 N 点并 `truncated=true`；产出摘要（§D4）。
- 缓存：**内核不缓存**（读路径无状态；每次 read 对每个 live 块一次调用，8s 上界）。插件侧缓存按 `(asset, period)` 键、TTL 5 分钟（日线一天变一次），落在 `PriceCache`/`PassCache` 旁（`main.rs:744`、`:1325`）。frozen 块在 S5 后不再调插件（读快照行）。
- 与 overlay 推送的关系：**互不替代**。表继续用推送（插件有自己的时钟，谁写谁推）；序列用拉取（参数由文档决定，插件不知道文档里有哪些块）。两者共享 `neige://plugin/<id>/<x>` 语法，但第二段含义不同（overlay kind vs tool 名）；kinds 表与 schema description 必须写清。
- 该 tool 同时对 agent 可见为 `plugin.dev-neige-market_market.series`（F4.9 路由、F4.12 无隐藏机制）— 这是接受的，Planner 直接查序列是正当用途。

**依据。** F4.6-F4.9（通道已存在、只缺内核作为调用者）、F4.7（无超时）、F4.11（KV 配额 256KB，插件不能把历史存 KV — 缓存只在内存）、F4.12（无隐藏标记，因此不假装"内核私有工具"）。(a) 被否：要新增回调协议 + overlay 键里编码参数 + 前端按参数 hash 找 overlay，且 overlay 表无字节上限（F4.1）会被 5000 点 × 块数撑大并随 `overlays_by_kind('track')` 推给侧栏（F6.7）。(c) 被否：同样撑大 overlay 且插件不知道该推哪些标的。

### D3 frozen / live

**问题。** 快照存哪、谁写、什么时候写、revision 语义、`as_of` 格式。

**备选。** (a) 写进 block payload。 (b) 存 overlay（kernel plugin_id）。 (c) 独立表 `report_series_snapshots`，写入时钉住。 (d) 独立表，**首次成功解析时钉住**。

**裁决：(d)。**

- `as_of` 格式：payload 里 `YYYY-MM-DD`（最后一根 bar 的日期，交易日历以插件源为准）；`resolved.as_of` 也是 `YYYY-MM-DD`（实际返回的最后 bar 日期），另给 `resolved.resolved_at`（RFC3339，解析发生的时间）。一个名字一个含义。
- live：`as_of` 缺席；每次 resolve 都问插件；`resolved.as_of` 随源前进；docRev 不变（seq 8）。
- frozen：`as_of` 存在；请求带 `as_of`，插件只返回 ≤ as_of 的 bar。S5 落地 NEW 表 `report_series_snapshots(track_id, block_id, payload_hash, as_of, resolved_at, data TEXT, PRIMARY KEY(track_id, block_id, payload_hash))`，`payload_hash = sha256(canonical_json(payload))`。**谁写**：内核 resolver 在第一次 `status=ok` 时 `INSERT OR IGNORE`（读工具或 HTTP 路由都可能是第一个；并发两个赢家由主键裁决，输家回读该行）。**什么时候**：不在块写入事务里 — `commit_report_op` 在 persist 事务内（`track_report_blocks.rs:20-30`），事务里调插件（8s 网络）不可接受；写入只负责把 `as_of` 存进 fence。**revision 语义**：快照身份是 `(block_id, payload_hash)`，不是 `rev` — 同内容重写不换快照（`upsert_identical_content_keeps_rev` 精神，`mcp_track_report_blocks.rs:1170`），改参数（range/series/as_of）就是新快照。旧行不主动 GC，track 删除时随 overlay 清理一起删（`routes/tracks.rs:4032-4033` 旁）。
- S5 前（S2-S4）frozen 块的行为：每次 resolve 都带 `as_of` 重新问插件，`resolved.pinned=false`。契约不变（`pinned` 字段从 S2 起就在），只是"未钉住"。登记为 KNOWN GAP G3 直到 S5。

**依据。** (a) 否：违背"只命名数据"，`MAX_CANONICAL_BYTES` 256KB 与 5000 点冲突（F1.2/F1.3），canonical fence 进 `text`、进观察 hash，每次 read 都把整份序列送给 agent。(b) 否：overlay 无上限但会被 `overlays_by_kind('track')` 整表拉到侧栏（F4.5/F6.7）；`KERNEL_OVERLAY_PLUGIN_ID` 保留给内核自有 kind 且走 `validate_overlay_payload` 内核 schema（`routes/overlays.rs:83-86`）— 可行但把 5000 点塞进推送面是错的地方。(c) 否：F2.1/F2.2 写路径全在事务内，插件调用不能进事务。迁移号最后定（F7.1）。

### D4 读端水合

**问题。** `calm.report.read` 响应里 `resolved` 的精确形状、`resolve` 入参、fail-open、token 体量。

**裁决。**

- 入参：`{ with_markers?: bool, resolve?: { [block_id]: "summary" | "full" | "none" } }`。默认每个 `chart.series` 块与 live `table` 块给 `summary`；`"full"` 给原始序列 / 全部行；`"none"` 跳过（不调插件）。未知 block_id 忽略（read 永不因入参多余而失败，与 `with_markers` 风格一致）。
- 响应：`blocks[i]` 从 `{id, kind, rev}` 变 `{id, kind, rev, resolved?}`，`resolved` 只在数据被解析的块上出现：
  ```jsonc
  { "status": "ok" | "pending" | "unavailable",
    "reason": "…",                     // 非 ok 时
    "resolved_at": "2026-09-12T08:00:00Z",
    "as_of": "2026-09-11",             // ok 时；frozen 块 = payload.as_of 或更早（源没有那天的 bar）
    "pinned": false,                   // frozen 且已钉住 = true；live 恒 false
    "view": "normalized", "field": "close", "period": "day", "range": "1Y",
    "series": [
      { "asset": "US:NVDA", "currency": "USD", "status": "ok",
        "n": 251, "first": ["2025-09-11", 118.2], "last": ["2026-09-11", 176.9],
        "change_pct": 49.66, "high": 181.3, "low": 101.4,
        "truncated": false,
        "points": [[ts_ms, v], …]      // 仅 "full"
      },
      { "asset": "HK:9988", "status": "unknown_asset", "reason": "…" } ] }
  ```
  live `table` 的 `resolved`：`{status, resolved_at, columns: n, rows: n, caption?}`，`"full"` 加 `payload`（overlay 原样）。来源是 `repo.overlays_for("track", track_id)` 按 `(plugin_id, kind)` 匹配（与 F6.4 同一规则，不调插件）。
- 摘要定义：`n` = 点数；`first/last` = `[YYYY-MM-DD, value]`（candles 视图 value=close）；`change_pct = (last-first)/first*100`，两位小数，first==0 → null；`high/low` = value 的极值（candles 用 high/low 列）。这些由 Rust 函数 `summarize(&Series) -> Summary` 产出，HTTP 路由（D5）返回**同一结构体**序列化，所以前端 tooltip/标题与 agent 读到的是同一份数字。
- fail-open 规则：resolver 的任何 `Err`/超时/插件缺席都落到 `status`，绝不冒泡成 RPC error；`load_report_read_snapshot` 的错误仍是 internal（那是文档本身读不到，与今天一致）。Assistant 也拿 `resolved`（它是报告内容，不是任务运行态，F3.6 的划线不适用）。
- token 体量估算（按 1 token ≈ 4 字符，JSON 紧凑）：summary 每条序列 ≈ 180 字符 ≈ 45 token；8 条 ≈ 360 token；块级字段 ≈ 60 token；一篇 3 个块的报告 ≈ 1.3k token 增量。full：1Y 日线 251 点 × `[1726000000000,176.93]` ≈ 24 字符 ≈ 6 token → ≈ 1.5k token/条（close），candles ≈ 251 × 50 字符 ≈ 3.1k token/条；5Y 日线 ≈ 7.5k / 15.7k token/条；上限 5000 点 ≈ 30k / 62k token/条 — 所以 `full` 只按块显式索取，默认不给。

**依据。** F3.3/F3.4（今天的空壳）、F3.5（无 overlay 读工具，故顺手水合 live table 是最小代价关闭同一抱怨）、F3.2（快照里有 payload，可就地解析）。

### D5 前端渲染

**裁决。**
- fe/：NEW `fe/web/src/features/report/series/public.tsx` + `series.module.css`，沿 F6.1 的 SVG 路线：`line`（多序列各一条 `<polyline>`，`currentColor`/token 上色，右侧 y 轴按原值）、`normalized`（每条 rebase 到首点=100，y 轴百分比）、`bar`（单序列柱，多序列分组柱，≤8）、`candles`（把 `candles/public.tsx:89-190` 的绘制体抽成 `CandlesFigure({candles, overlays})` 供两个块共用；`ReportCandlesBlock` 保持对外 API）。区间切换不再客户端过滤（区间是 payload 参数，改它 = 改文档），图上只显示 `range`/`as_of`/币种/`pinned` 标记。
- 数据：NEW 路由 `GET /api/tracks/{id}/report/series/{block_id}?detail=summary|full`（`routes/track_report_series.rs`，与 `routes/track_report_blocks.rs` 并列），Principal 鉴权同其它 track 路由，返回 D4 的 `resolved` 结构（`utoipa` schema → 两份 OpenAPI 都重生成，F6.9）。fe 查询 `trackReportSeriesQueryOptions(trackId, blockId, payloadHash)`，key `['track-report-series', trackId, blockId, payloadHash]`，staleTime 5 分钟；`invalidation-plan.ts` 的 `track.report_edited` 追加 `['track-report-series', trackId]`。
- 三个非 ok 态渲染 caption + 一行文字（复用 table 的 `LiveTableNotice` 形态，F6.3）；loading 态渲染 caption + "Loading …"。
- legacy `web/`：**不加渲染器**。它已把未知 kind 渲成 `unsupported block kind chart.series`（F6.8），差异在此声明；两份 OpenAPI 仍因新路由一起重生成（否则 CI 两条红）。
- 不引入 Recharts/lightweight-charts（#1612 的 Recharts 路线被关闭；F6.1 的三条理由仍成立）。

### D6 安全

**裁决。**
- `source` 只接受 `neige://plugin/<id>/<tool>`，复用 `validate_live_source`（F1.4）；内核从中只取 `plugin_id` 去 `connector_client(plugin_id)`，**从不**构造 URL、从不把 `source` 当地址；插件访问的外部端点只来自它自己的配置（`sina_endpoint`/`binance_endpoint`，F5.2）。
- `series` 字符串经内核形状检查后作为 tool 参数原样交给插件，插件用 `parse_asset` 再判（F5.1）；内核不解释 venue。
- 同源：HTTP 路由在 `/api/tracks/{id}/…` 下、走既有 Principal；read 工具走 `resolve_report_for_caller`（只读调用者自己的 track）。
- 插件 id 校验：`plugin_scope_for_track` 与 agent 路由同一规则（F4.9）；未运行 → `pending`，不泄露"是否安装"之外的信息。
- 资源上界：8 条序列、8s 超时、2 MiB 回复、5000 点/条截断；read 每块最多一次插件调用；`resolve:{…:"none"}` 可关。
- 插件 `market.series` 为 read-only，不写 overlay/KV；不新增 `neige.*` 回调，不扩权限模型。

### D7 与 #1612 `layout` 的取舍

保留：`neige://plugin/<id>/<x>` 作为唯一外部引用语法（其 `data.source` 也这么做，`ref1612/layout.rs:160-170`）；"配置声明式、无表达式求值"；unit/currency 不做隐式换算（其 `unit.equals` 规则的精神）；"缺数据显式态、零不是缺"。
放弃：一个 kind 承载布局 + 多 item + 表 + 图（12 个 item、列数、间距、surface）— 布局是文档顺序的事，块已经有序；数据来自 overlay 行再做 join/annotations（把表当数据库）；Recharts 与颜色 `#RRGGBB` 进 payload（与 F6.1 token 路线冲突）；模板化的 selector/total/share 计算（投研图不需要，每个都是新校验器）。
为什么：#1612 用户判为错误实践的核心是"把仪表盘模板做成 Report kind"；本设计一个块一个图、参数就是语义参数（series/field/range/view），没有表现层参数。

## 5. 切片表

| 片 | 内容 | 依赖 | 可独立合入 | 行为变化 | 估算行数 |
|---|---|---|---|---|---|
| S1 契约 | `kinds.rs` `KIND_CHART_SERIES` + `validate_chart_series` + `MAX_CHART_SERIES`；`DATA_KINDS` 5 项；`kinds_tests.rs` 正反例；`contracts.rs` kinds_table 项 + F2.8/F2.9 文字；`fe/core/domain/report.ts` zod + `payloadSchemaFor`；`document/public.tsx` `case 'chart.series'` 渲染 caption + "series view lands in a later slice"；`mcp_track_report_blocks.rs` 加 `kinds_returns_all_six_schemas`（改名） | 无 | 是 | agent 可写 `chart.series`，read 无 `resolved`，fe 显示占位，web 显示 unsupported | ~600 |
| S2 内核解析 + read 水合 | `track_report_series.rs`（resolver、summary、fail-open、caps）；`calm.report.read` `resolve` 入参 + `resolved`（chart.series 与 live table）；集成测试用假插件（`mcp_plugin_tools.rs:927 boot_plugin_host` 模式）覆盖 ok/pending/unavailable/timeout/unknown_asset/truncated | S1 | 是 | Planner/Assistant read 到摘要；插件缺席时 `pending` | ~900 |
| S3 market 插件 | `market.series` tool（manifest、`tools_call_reply` 分支、history source、内存缓存、`as_of` 截断）、README、fixture server 测试（`market_plugin_process.rs` 风格）。**前置**：历史源研究（§8 U1），先做最小 spike 核实端点 | S1（只共享 wire 形状；与 S2 并行） | 是 | `plugin.dev-neige-market_market.series` 对 agent 可用 | ~900 |
| S4 路由 + fe 渲染 | `routes/track_report_series.rs`、两份 OpenAPI 重生成、`queries.ts` 查询 + 失效、`features/report/series/`、`CandlesFigure` 抽取、browser 测试 | S2 | 是 | 人看到图；S3 未合时看到 `pending` 文案 | ~1000 |
| S5 frozen 快照 | 迁移（号最后定）`report_series_snapshots`；resolver pin-on-first-ok；`pinned=true`；track 删除清理；测试：钉住后源变化不动 | S2（S3 有更好，测试可用假插件） | 是 | frozen 块真正冻结 | ~500 |

顺序：S1 → (S2 ∥ S3) → S4 → S5。S2 与 S3 的接缝是 D2 的请求/回复 JSON，两边各自用 fixture 锁死同一份样例（把样例 JSON 放 `crates/calm-server/tests/fixtures/market_series_reply.json`，S3 的测试断言插件输出等于它）。

## 6. 验收场景与 must-red 变异

| # | 场景 | 断言 | must-red 变异（改哪一行 → 哪条测试转红） |
|---|---|---|---|
| A1 | `upsert{kind:"chart.series"}` 合法 payload 落盘为 canonical fence | read `text` 含 ```` ```neige-block chart.series ````，`parse_fence` 回同 payload | `kinds.rs::validate_chart_series` 把 `series` 必填改成可选 → `kinds_tests::chart_series_payload_valid_and_invalid`（S1 新增）里 "series: required" 断言红 |
| A2 | 未知字段 / 无 venue / 9 条 / candles+2 条 / `source` 非 `neige://plugin/` 各报字段级错误且不写 | `-32602`，docRev 不变，事件零 | `resolve_upsert_content` 对 `chart.series` 跳过 `render_data_block` 直接 `render_fence` → `mcp_track_report_blocks::upsert_rejects_unknown_kind_and_invalid_payloads`（扩一条 chart.series 用例）红 |
| A3 | kinds 表、upsert enum、commit enum 三者含 `chart.series` 且相等 | `contracts.rs:695-711` 既有测试 | `block_kind_enum()` 硬编码四项 → `task_is_advertised_by_both_block_tool_contracts` 红 |
| A4 | read 默认给 summary，`n/first/last/change_pct/high/low/as_of` 与 fixture 一致 | 集成测试比对 fixture 算出的期望 | `summarize` 里 `change_pct` 用 `(last-first)/last` → `read_hydrates_chart_series_summary`（S2）红 |
| A5 | 插件未运行 → `pending`；超时 → `unavailable`；read 仍 200 | 三条用例 | resolver 删掉 `tokio::time::timeout` 包裹 → `read_marks_a_hung_plugin_unavailable_within_budget`（用一个永不回复的假插件 + 测试超时 10s）红（挂死） |
| A6 | `resolve:{b:"full"}` 才有 `points`；默认无 | JSON 断言 | resolver 无条件塞 `points` → `read_full_is_opt_in_per_block` 红 |
| A7 | frozen 块钉住后，假插件换数据再 read，`resolved` 不变且 `pinned=true` | S5 | `INSERT OR IGNORE` 改 `INSERT OR REPLACE`（或不查表直接调插件）→ `frozen_block_is_pinned_on_first_resolve` 红 |
| A8 | live 块每次 read 都调插件，`as_of` 随假插件推进 | S2 | resolver 对 live 块也走快照缓存 → `live_block_follows_the_source` 红 |
| A9 | 插件 `market.series` 对 fixture 源返回 `[ts_ms, close]` 升序、`as_of` 截断、未知标的 `unknown_asset` | S3 进程级测试 | 截断条件 `<= as_of` 改 `<` → `series_as_of_includes_that_days_bar` 红 |
| A10 | fe：`ok` 画出与序列数相同的 `<polyline>`；`normalized` 首点=100；三个非 ok 态渲染文案；不含字面颜色 | `series/public.test.tsx` | normalized 除以 `last` 而非 `first` → `rebases every series to 100 at its first point` 红 |
| A11 | 两份 OpenAPI 与生成器零漂移 | CI 既有 drift 门禁 | 只重生成一份 → 另一条 CI 红（本地看不见，见 F6.9） |
| A12 | 术语棘轮：本文档与新代码不引入退役词 | `gate-1316-terminology-ratchet.sh` | — |

## 7. KNOWN GAPS（登记，不加固）

- G1 价格是否复权由插件源决定，本设计不声明；`resolved` 不带 `adjusted` 字段。
- G2 `line` 视图多序列跨币种按原值画、只标币种，不换算、不双轴。
- G3 S5 之前 frozen 块每次重新解析，源修正会改图（`pinned=false` 可见）。
- G4 内核不缓存：Planner 高频 read 会对每个 live 块各打一次插件（8s 上界、插件侧 5 分钟缓存兜底）。
- G5 legacy `web/` 只显示 unsupported 一行；手机端同 fe。
- G6 `market.series` 对 agent 可见（无隐藏机制），Planner 可绕过报告直接查。
- G7 `pending` 与 `unavailable` 的区分依赖 `connector_client` 是否为 `None`，不区分"未安装"与"已装未起"。
- G8 快照行不主动 GC（只随 track 删除）；改参数留下旧行。
- G9 历史源（S3）候选未核实（§8 U1）；S3 前 `market.series` 对所有 venue 回 `unavailable, reason:"no history source for <venue>"`，整条管道仍可用 fixture 验收。
- G10 周线/月线由插件从日线聚合还是源直接给，S3 定；`period` 语义"以 bar 收盘日为准"不变。

## 8. 与 issue 的出入

出入：
1. issue 方向 1 "`chart.candles` 收编为它的一个 view" → 本设计保留 `chart.candles` 为独立内联 kind，`chart.series` 提供 `view:"candles"`（D1）。理由：已存文档、双前端渲染器、无 CRDT 数据迁移先例。
2. issue 方向 3 "内核在写入时钉住快照" → 写入事务不调插件，首次成功解析时钉住（D3）。理由：F2.1/F2.2 写路径在 persist 事务内。
3. issue 方向 2 "overlay 是推送不是查询 … 这是内核契约改动" → 请求-响应通道已存在（F4.7-F4.9），改动缩小为"内核作为调用者 + 插件声明一个 read-only tool"；不新增 `neige.*` 回调、不改 overlay。
4. issue 方向 4 "不新增工具" → MCP 面确实不新增；但浏览器需要一条 NEW HTTP 路由（D5），issue 未提。
5. issue "`as_of` / `frozen`" 两个名字 → 只用 `as_of`（存在即 frozen），不设布尔（一个布尔承载两种结果的反模式）。

## 9. 不确定点

- U1 历史行情源：新浪/腾讯日线端点本仓无记录，Binance klines 未核实（F5.7）。S3 前需研究并以最小 spike 验证（不联网猜）。
- U2 `SERIES_RESOLVE_TIMEOUT=8s` 与 `MAX_SERIES_REPLY_BYTES=2MiB` 是估值，S2 评审可调。
- U3 Assistant 是否该拿 `resolved`（D4 判为是）；若评审认为序列数据属于 Planner 特权，改一行角色判断即可。
- U4 `payload_hash` 用 `canonical_json` 的 SHA-256；若 dispatcher 已有同用途的 body 指纹函数（fence.rs 头注提到 SHA256 观察 hash），S5 应复用而非再写一个 — 未定位到该函数，S5 简报要先找。

## 10. 参考

- 本文 §2 所有 file:line 基于 `c534bf6b`。
- #1612 参考材料：`report-layout-contract.md`、`layout.rs`、`public.tsx`、`portfolio-template-review.md`（只读，见 D7）。
- 相关：#1556 S1（venue-qualified identity，`fdfc651d`）、#1623（`calm.report.commit`）、#960 PR3（kinds 词汇）。

## 11. 处置历史

| 轮次 | 通道 | 发现 | 处置 | 结果 |
|---|---|---|---|---|
| | | | | |
