# #1628 设计 v1 评审（通道 A）

基线核对：`review-a-1628` HEAD `1d2d3706` ← `c534bf6b` ✓。事实表逐条打开；穷举命令在该树重跑。

## BLOCKER

**B1. D2/D6 把"内核代读者调用插件工具"做成了无守卫的通用 RPC 代理，绕过 `PLUGIN_TOOL_ROLES` 与 manifest 工具表。**
- 证据：`mcp_server/transport.rs:88` `PLUGIN_TOOL_ROLES = [Planner, Worker]`——**Assistant 不能调插件工具**；但 `track_report_blocks.rs:121/134/209/262/296` upsert/commit/write_markdown 都放行 `[Planner, Assistant]`，且 D4 明写"Assistant 也拿 `resolved`"。`tool_visibility.rs:50-53` 未绑定 track ⇒ `TrackPluginScope::All`（任何运行中插件）。agent 路由 `transport.rs:693-696` 先经 `plugin_tool_route` 按 manifest `exposes_tools` 解析 + `kind` 分派（ForgeAction 走 `trusted_forge_plugin` 专用臂 `:748-760`）；D2 的 resolver 直接 `connector_client(plugin_id).tools_call(tool)`，**不查 manifest、不查 `kind`、不查 `readOnlyHint`**。`connector.rs:44-50` `ConnectorClient` 含 `Http`（远端 mcp-http，`transport.rs:722-727` 注释明说"somebody else's service"）。
- 构造 1（越权）：Assistant `commit{upsert chart.series, source:"neige://plugin/<任一运行插件>/<任意工具名>", series:["US:X"]}` → 自己 `calm.report.read` → 内核以自己的身份调该工具并把 `structuredContent` 回给 Assistant。F1.4 "只查形状" + D6 "内核不解释" ⇒ 零拦截。证伪 D6 "同源/鉴权同其它 track 路由"。
- 构造 2（对外发请求）：operator 装了 `mcp-http` 连接器 `acme`；任何写者写 `source:"neige://plugin/acme/x"`，`series` 8×32 字符是攻击者控制的载荷；此后**每个读者**（含 LAN 上任意浏览器 GET 新路由）都让内核向第三方发一次 tools/call。D6 "从不构造 URL" 只是字面为真——URL 来自连接器配置，但请求由文档内容触发、由读者身份发出。
- 构造 3：`source` 指向 `kind: ForgeAction` 的工具 → 绕过 `trusted_forge_plugin` 直接触发插件的 forge 臂。
- 处置：resolver 必须复用 `plugin_tool_route`（manifest 内、`kind == None`），额外要求 `annotations.readOnlyHint == true`；只允许 `ConnectorClient::Stdio`/`Cli`（本地），拒绝 `Http`；解析身份用**写者角色**或固定 Planner 语义——至少 Assistant 不得经 read 拿到 `resolved`（U3 不是"改一行"的可选项，是 blocker 的一半）。fe 路由同样只走这条守卫。

## MAJOR

**M1. D3 快照身份 `payload_hash = sha256(canonical_json(payload))` 含 `caption/view/overlays`，改说明文字就解冻。** 构造：frozen 块钉住后作者改 `caption`（`rev+1`，hash 变）→ 新 `(block_id, hash)` 无行 → 重新问插件 → 源已复权 → 图变。证伪 7b "文档论述下面的图不动"。处置：hash 只覆盖决定请求的元组 `(series, fields(由 view 派生), range, period, as_of)`，文档里写明哪些字段是"表现层、不入指纹"。

**M2. D3 "首次 `status=ok` 钉住"把部分失败永久化。** 3d 定义：某条 `unknown_asset` 时块级仍 `ok`。构造：首读时 Sina 对 HK 限流 → `series[1].status="unavailable"`、块级 `ok` → `INSERT OR IGNORE` 钉住 → 该序列永远缺失，`pinned=true` 还让读者以为是终态。处置：钉住条件改为"每条 series 都 ok 且 `resolved.as_of == payload.as_of`"，或按序列分别钉。

**M3. `as_of` 无上界校验，frozen 语义退化为"首读时刻"。** 构造：`as_of:"2099-01-01"` 合法（D1 只查 `YYYY-MM-DD`）→ 插件返回全部 ≤ as_of 的 bar = 到今天 → 首读钉住 → "钉在时点"变成"钉在第一个读者打开的时刻"；`as_of = 今天` 同理钉住未收盘 bar。处置：内核校验 `as_of < 今天（UTC）`；插件回复 `as_of` 必须等于请求 `as_of` 才可钉（与 M2 合并）。

**M4. D4 默认水合把插件调用挂到 CAS 握手路径上，且串行。** `track_report.rs:148-152` 注释：read 是 `docRev`/`rev` 的**唯一来源**，每次写前必 read。`main.rs:2592-2603`（[实测]）market 插件只有**一个** tool worker 线程，`McpClient::call` 无超时（F4.7 ✓）。构造：3 个 live 块、Sina 挂起 → 单次 read = 3×8s = 24s（3c 只承诺"每块 ≤8s"，未承诺整次 read）；两个 Planner 并发 read 在插件单线程后排队 → 第二个的 8s 超时在排队里就耗尽 → `unavailable`（不是真不可用）。处置：块间 `join_all` + 整次 read 预算；MCP `read` 默认 `resolve` 改为 `none`（Planner 显式要），HTTP 路由默认 `summary`；或内核加 (block, hash) 级 60s 内存缓存以吸收 CAS 重读。G4 现有文字低估了形状。

**M5. 事实表 F4.6 "写方只有插件回调与 M1 手测路由" 与代码自述矛盾。** `routes/overlays.rs:83-85` 注释列出内核内部写者 `card_fsm`、track structure creation、`child_track_adapter`；`grep -rn overlay_upsert_tx crates/calm-server/src` 命中 `card_fsm.rs:558,683`。结论未受影响（D2 否决 (a) 的理由不靠它），但"唯一"是假的，改为"外部写方只有…"。

**M6. F2 穷举漏项。** 在该树跑文档给的 grep 并去掉自身：`routes/track_recipes.rs:275 validate_recipe_body`（recipe HTTP 写口）、`routes/tracks.rs:1006`（模板 body，`:941` 是注释不是代码）、`:2795`（fork prose 内 fence）、`track_report.rs:643/660`（`Replace`/`WriteMarkdown` 臂；F2.3 引的 guard `:217` 是 `validate_block_content`，服务 `UpsertBlock` 臂）。它们全部经 `validate_payload`，"自动覆盖"结论成立，但"7 处"是错的（≥9），且 F2.3 的载体指错。

## MINOR

- m1. F7.3 `compatibility.rs:5` 实在 `crates/calm-types/src/`，不在 calm-server；`WEB_COMPAT_VERSION=27`（`routes/version.rs:116`），门禁只查三处相等，加法路由不 bump 的判断成立。
- m2. U4 "未定位到 SHA256 函数"：`track_report_doc.rs:235` `Sha256::new()` 就是观察 hash；S5 简报可直接指向。
- m3. D7 引用 `ref1612/layout.rs:160-170`、`report-layout-contract.md`：该树 `find` 零命中，`layout.rs` 只有 `dedicated_codex/layout.rs`。要么标"未在基线"，要么给出 ref 所在分支/SHA。
- m4. Oracle 3a 写 `connector_client(...).tools_call(tool, args, Some(track_id))`：`ConnectorClient` 是枚举，`Http/Cli` 变体的 `tools_call` 是双参（`transport.rs:734/738`）；随 B1 一并改为只匹配本地变体。
- m5. Oracle "可观察事件"：`card.updated`/`track.report_edited`/`track.deleted`/`overlay.set` 均存在于 `calm-types/src/event.rs:1414-1429`；`ws/events.rs` 是转发层，不枚举 kind——文档写"`calm-types/src/event.rs`"是对的，简报指向的文件不是权威源。
- m6. D1 `series` 去重按字面：`HK:9988` 与 `HK:09988` 经 `canonical_symbol`（`main.rs:349`）折成同一身份，内核去重不到，插件会回两条同资产序列。声明为插件侧去重或内核不做去重。
- m7. fork（`routes/tracks.rs:2717` 保留 block id，换 track_id）：frozen 块 fork 后新 track 无快照行 → 重新解析 → 与源文档图不同。登记 G 或 S5 复制快照行。
- m8. `Option<T>`+default：D4 `resolved` 用 `status` 字符串 + 一堆条件字段（`reason` 仅非 ok、`as_of` 仅 ok、`points` 仅 full）。Rust 类型请写成枚举 `Resolved::{Ok{..}, Pending{reason}, Unavailable{reason}}`，序列化再摊平；避免"一个结构体多种含义"。
- m9. 切片：S1 `document/public.tsx` 的 `switch(block.kind)` 无 default，加 zod 分支不加 case 会 TS 不过——S1 已含 case ✓；两份 OpenAPI 只在 S4 动 ✓；S2 先于 S3 合入时 market 插件对未知工具回 `-32601` → resolver 须把 `Err` 也归 `unavailable`（D4 fail-open 已写 ✓）。S1-S5 各自可合入判断成立。
- m10. 纪律：无已发布迁移改动 ✓；迁移号推迟 ✓；不 bump `SYNC_EVENT_VERSION`/`SCHEMA_VERSION` 判断成立 ✓；文档跑 #1316 五条 pattern 零命中 ✓。

## 事实表其余核对
F1.1-F1.10、F2.1/F2.4/F2.7-F2.9、F3.1-F3.6、F4.1-F4.5/F4.7-F4.12、F5.1-F5.8、F6.1-F6.10、F7.1/F7.2/F7.4/F7.5 行号与内容均与 `c534bf6b` 一致。

REVISE