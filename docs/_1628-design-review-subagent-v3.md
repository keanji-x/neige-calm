## #1628 设计 v3 评审（通道 A，第 3 轮）

基线核对：`review-a-1628` HEAD `a9315d73` ← `ec0b3590` ← `1d2d3706` ← `c534bf6b`，diff 只含 docs。`origin/main` 领先 3 提交，迁移目录仍止于 `0106`。

### 0. 第 2 轮处置复核

逐条到代码与 v3 正文核对，全部落地为机制而非措辞：R2-1（`ResponderSlot` guard，`mcp.rs:667/676` 之间，`responders` 是 `std::sync::Mutex`，Drop 内加锁可行）、R2-3（指纹含 `plugin_id,tool`）、R2-5（drain 准入）、R2-7（收窄为钉住行）、R2-8（`complete_through` + fork 复制全部行）、R2-9/m7（≥2 点）、A M2（删时钟检查，F1.12 属实）、A M3（昨天 UTC）、A M4（RAII + JoinHandle）、m3（`plugin_tool_entry` 四态，`registry.rs:292` `get` 属实）、m4（256）、m6/m8/m11 ✓。R2-2"部分采纳"的落点 #1634 确为 OPEN 且正文与之一致。

残留措辞：`grep -n "封顶\|内存上界\|上界为 1"` 命中 L7/248/249/360/475/476/517/518/532，**全部是引述被证伪的旧说法或明写"不是内存上界"**，无一处仍作断言。✓

### 1. 事实表（v3 新增项）

- F1.12 ✓（`Cargo.toml:8` "NO sqlx, NO axum, NO tokio"，无 chrono/time/jiff；src 无 `Utc::now`/`SystemTime::now`）。
- F4.16 ✓（`manifest.rs:2303-2314`，测试 `:3172-3177`；`registry.rs:292 get`、`:299 list`）。
- F4.17 ✓（#1634 OPEN，body 与 F4.6 一致）。
- F4.6 ✓ 逐行核对 `:655-679/:783-810/:867-874`。`Cli` 变体 `cli_query/mod.rs:215 kill_on_drop(true)` 自带超时，30s timeout drop 不会留孤儿进程（文档未提，非 finding）。
- F2 穷举重跑：额外命中 `routes/cards.rs:456/634`、`calm-truth/validation.rs:679`（card_kind registry 同名）、`tasks.rs:715`（读侧诊断）、导入行与测试——9 处生产入口成立。
- fe 枚举 kind 的非测试文件只有 `document/public.tsx`、`report.ts`（+ 渲染器本身）；web 三处均落 opaque。✓
- `rev` 规则 ✓ `track_report_doc.rs:592-599`：字节相同不 bump，否则 +1（D5 的 409 绑定成立）。
- **F3.9 计数错**：文档说该命令"共 7 处"，实跑 `grep -rn "TrackReportEdited" crates/calm-server/src --include='*.rs' | wc -l` = **26**（多出的是 doc 注释与 `dispatcher/tests.rs` 的 4 处测试构造）。实质结论"生产构造点只有 `write.rs:1098`"成立（`grep "Event::TrackReportEdited {"` 排除 tests.rs 后：1098 构造 + 4 处 match）。

### MAJOR

**M1. `range` 的锚点未定义，frozen 历史图对旧 `as_of` 会永久 `unavailable`。**
- 证据：D1 只给 `range` 枚举；D2 步骤 6 `max_points(range, period)` 按区间长度算；§2.5 "插件总是拉最新 N 根再按 `as_of` 过滤"；A14 只测"按 `as_of` 截止"。
- 构造：`{series:["US:NVDA"], range:"1Y", as_of:"2024-12-31"}`。插件按 §2.5 拉最新 N（≈1Y）根 → 全部日期 > `as_of` → 过滤后 0 点 → 插件报 `unavailable "no data in range"`；每 6h 重投、永远如此。issue 的"时点论证"核心场景（as of 去年年报日）落空。
- 处置：D1 明确窗口 = `[as_of − range, as_of]`（live 即 `[昨天−range, 昨天]`）；S3 按 `today − (as_of − range)` 反推拉取深度（腾讯 `<n>` 是条数，需实测上限；超出上限 → `unavailable, reason:"lookback exceeds source depth"` 并登记 G）；A14 加"旧 `as_of` 窗口非空"用例。

**M2. 钉住判据应改为严格 `complete_through > as_of`（编排者开放问题，裁决：改）。**
- `>=` 与 `>` 只在 `complete_through == as_of` 一种情形分歧，而这恰是"源列出的最新 bar 是否已收盘"无法由内核判断的那一根。**Binance `/api/v3/klines` 总是返回当前未收盘的日 K**（closeTime 在未来），所以对 crypto U7 不是"若为真"而是**必然为真**：`as_of = 今天` 的 crypto frozen 块在 `>=` 下会钉在解析那一刻的半根 bar 上；`>` 下 `今天 > 今天` 为假，明天出新根后钉住、数据 ≤ 今天已收盘。
- 逐场景：周末（`as_of` 周日、源周五）两种判据都等到周一，无差异；`as_of` = 昨天：`>` 需要一根今天的 bar——若源列盘中 bar 则立即钉住（昨天已收盘，正确），否则等今天收盘，都对；`as_of` = 今天：`>` 永不在当天钉住（正确）；未来 `as_of`：两者同样不钉；停牌/退市：两者同样 G3。
- 代价：`as_of` = 最近一个交易日（最常见写法）的钉住统一推迟到下一根 bar 出现（周末最多 3-4 天），图上已有"not pinned — source data through"标记，且未钉住行数据与钉住后相同。
- 附带：U7 提议的"取前一天作保守值"是同一件事的插件侧启发式，且假日会多退一天；`>` 在内核里干净地包含它，U7 可关闭。**`complete_through` 必须定义为"源未过滤的最新*日线* bar 日期"，与 `period` 无关**：若 week/month 用聚合 bar 的日期（进行中的周 bar 标成周五），`as_of` = 周五时周三就会满足 `>=`（`>` 也会被未来日期的幻影 bar 骗过）。
- 处置：D3 判据、seq 7a/7d、A7b 变异靶点改 `>`；D2 回复形状注明 `complete_through` = 日线；U7 关闭。

### MINOR

- m1. **lane 重建有竞态**：两个并发 `enqueue` 都读到 `drain.is_finished()`（或都 `send` 失败）→ 各建一条 lane，后者替换前者，前者 receiver 尚未 drop 时两条 drain 同时跑同一插件，"按插件串行"短暂失效；被替换 lane 队列里的 job 随 receiver drop 丢失（键释放但不执行）。处置：检查-重建-投递在 `lanes` 锁内完成，通道用 unbounded/`try_send`（同步）；文档写明"被替换 lane 的排队 job 丢弃，靠下次读重投"。
- m2. **Owned track 只能解析 owner 的工具**（`tool_visibility.rs:141` `Owned → Only(plugin.id)`）：任何绑定了非 market owner 的 track 上，market 图永久 `unavailable "outside this track's plugin scope"`。D2 步骤 3 定义了行为，但 oracle 无此行、G 表未登记；投研 track 若有 owner 就是这种情况。处置：登记 G + seq 3b(iii)；或论证内核发起的只读调用可放宽到 `All`。
- m3. `summary TEXT NOT NULL` 与 `unavailable` 行矛盾（D4 的 unavailable 只有 `reason/resolved_at`）。改 nullable 或规定存 `null`。
- m4. drain 准入用 `load_report_read_snapshot`（`track_report_read.rs:43`，带 `task_budget` 算 `task_diagnostics`）每 job 一次，比需要的重；用 `report_blocks_snapshot_tx`（F3.8）即可。
- m5. A10b "commit 后等 2s 不 read，断言零调用"是计时型负测试（harness 会压缩窗口）；改为 `new_unstarted()` 记录器断言 `enqueue` 调用数为 0 + `report_series` 无行。A5 的"测试超时 40s"应注入 `SERIES_RESOLVE_TIMEOUT` 缩到毫秒级。
- m6. F3.9 "共 7 处"改为实跑数字并附精确命令（见 §1）。
- m7. `INSERT … ON CONFLICT … WHERE pinned=0` 对 `pinned=1` 行是 no-op，但对**同 hash 的旧 `unavailable` 行**用 `DO UPDATE` 覆盖 `as_of` 列时 live 的截止日会前进——正确，只是 D3 的"live 行 `as_of` 每次刷新变化"没在 seq 8 断言里；加 `summary.as_of` 前进断言。

### 切片、oracle 与纪律

- oracle：每个 ⚠️/❌ 行恰好一片 ✓；`CardUpdated`/`TrackReportEdited`/`TrackDeleted` 均在 `calm-types/src/event.rs:392/579/359`（`ws/events.rs` 存在但非权威源）；3f 的 `a_b` 经 F1.4 字符集可达 ✓；3b(ii) 经 `OwnerUnavailable` 可达 ✓；3h 仅 failpoint 可达（已标）。
- 可合入性：S1 单独合入后旧 fe 走 zod `unsupported`、web opaque ✓；S2 的 `resolved` 是 MCP 面，不动 OpenAPI；S4 两份 OpenAPI 文件一起重生成 ✓（A17）。[修订者 2026-09-13：此处原文用了一个 #1316 棘轮的退役词，为过 `gate-1316-terminology-ratchet.sh` 替换为「OpenAPI 文件」，语义不变]
- 纪律：无已发布迁移改动 ✓；号推迟 ✓；三个版本常量不 bump 的理由成立（无新 Event、路由加法、payload 形状不变）✓；`Resolved` 枚举 ✓；仓内无 sqlx 离线宏，S2 无需 `sqlx prepare`。

REVISE