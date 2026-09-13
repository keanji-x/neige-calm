## #1628 设计 v4 评审（通道 A，第 4 轮）

基线核对：`review-a-1628` HEAD `18299cbb` ← `a9315d73` ← `ec0b3590` ← `c534bf6b`，只含 docs；迁移目录仍止于 `0106`；`REST_API_VERSION="7"`/`WEB_COMPAT_VERSION=27`/`SYNC_EVENT_VERSION=20` 与 F7.2-7.4 一致；`gate-web-compat-version-lockstep.sh` 只查三处相等，不因 OpenAPI 变动要求 bump ✓。

### 0. 第 3 轮处置复核

逐条到代码与 v4 正文核对，标「采纳」的全部落地为机制：交叉 1（D1 窗口定义 + 请求带 `start` + 校验清单 `≥ start` + S3 约束 1/4 + seq 3i/3i′ + A14）、交叉 2（D3 严格 `>` + 日线定义 + 逐场景表 + live 永远 `pinned=0`）、R3-3（S3 约束 3 聚合规则 + 内核边界周一/1 日/周期结束日检查 + seq 3j）、R3-4（job 携带 `NotInstalled|NotRunning|NotExposed`，一次性任务不取 `connector_client`；核实 `mod.rs:3258-3262` `running_plugin_ids` 与 `:3312` `connector_client` 各 `lock_table()` 一次且前者明写不含 `Spawning`）、R3-5/A m1（unbounded + 锁内 entry/is_finished/send；核实 tokio 1.52.3 `Cargo.lock:3645`，`chan.rs` `Rx::drop` → `close()` → `drain` 循环属实）、R3-MINOR-1（§2.8 例外 (a)-(d)）、R3-MINOR-2（`manifest.rs:2305` `len < 2` 属实，fixture 改 `aa`）、R3-MINOR-3（`manifest.json:12/34/62` 只有三个工具，`NotExposed` 先于插件）、A m2（`tool_visibility.rs:141` `Only(plugin.id)`；`grep '"templates"' plugins/*/manifest.json` 只中 git-forge:302；G17）、m3（nullable）、m4（`track_report.rs:73` `pub(crate) async fn report_blocks_snapshot_tx(tx, track_id) -> (String, Vec<ReportBlock>)`，先例 `admission.rs:158`、`file_delivery/repair.rs:101` 属实）、m5/m6（26/5 实跑相符）/m7 ✓。

修订者附带改动（请求 `start` 替代 `range`）在 D1、D2「请求窗口」、请求形状、校验清单（`∈ [请求 start, 请求 as_of]`）、S3 约束 1、seq 3、A8/A14 之间一致 ✓；只有 §11 末行仍写「待编排者确认」（m6）。

### 1. §2 v4 新增 [实测] 核对

F3.9（26 / 5）、F4.16（`:2303-2314`）、F4.7（`:3260`/`:3296-3320`）、F4.17、§2.5 Binance 未收盘 K（无法复跑网络，接受为修订者实测）、D2 步骤 3 影响面（`tracks.rs:1683-1695`、`:1750-1752`）、D3 fork（`tracks.rs:2322-2329` 在事务内取快照）、tokio `Rx::drop` 全部属实。附带核实：fork 建的 track `plugin_scope: None`（`tracks.rs:479`、`:497`）→ 复制 `report_series` 行不会绕过 G17 的作用域规则（非 finding）。

### MAJOR

**M1. `as_of` 只查形状不查历法，历法非法日期让窗口算术没有定义的出口。**
- 证据：D1「只查 YYYY-MM-DD 形状（月 01-12、日 01-31），不查历法」；D2「`start = as_of − RANGE_DAYS`（calm-server `chrono` 做日期减法）」；seq 4 不变式「read 永远 200」；校验清单只覆盖插件回复。
- 构造：`commit{… as_of:"2026-02-31"}` → S1 通过、落盘。任一读者 read → `SeriesRequest::from_payload` 需要算 hash 并查行（hash 盖 `as_of` 字符串，可算），但 `start` 由 `NaiveDate::parse("2026-02-31")` 得 `Err`。若 `start` 在 read 路径的 `enqueue` 里算 → read 报错/panic，seq 4 被证伪；若在 drain 里算 → 不在任何清单里，最可能落成每 6h 重投一次的永久 `unavailable`，文档没有写。「calm-types 无时钟」（F1.12）不推出「不能查历法」：闰年 + 每月天数是纯函数。
- 处置：S1 `validate_chart_series` 加历法校验（纯，~10 行）；D1 改「查历法、不与今天比较」；seq 2n 加 `2026-02-30` 反例；A2 加变异「删历法检查 → `commit_rejects_non_calendar_as_of` 红」。

**M2. 每次进程/插件重启后，最先几秒被读到的全部图表 `unavailable` 六小时。**
- 证据：`plugin_host/mod.rs:3258-3262` `running_plugin_ids` 明写不含 `Spawning`；D2 enqueue 步骤 2 未命中 → 一次性任务写 `unavailable` 行；D3「失败行同一 TTL（6h）」；D2 resolve 步骤 1「行存在且 `resolved_at ≥ now − TTL`（含 `unavailable` 行）→ 丢弃」；R3-4 使一次性任务不再二次查找（正确，但把「预检时刻的事实」固化了 6h）；D5 fe 在重连后按 query key 重取；F3.3 Planner 每轮必 read。
- 构造：calm-server 重启 → market 插件 `Spawning`（handshake 数秒）→ 浏览器 WS 重连触发 `['track-report-series', …]` refetch / Planner 首轮 read → 每个 `chart.series` 块得 `NotRunning`（Owned track 则 `TrackPluginScope::None`）→ 行 `unavailable, "plugin dev-neige-market is not running"`，`resolved_at = now` → 3 秒后插件 Running，但此后 6h 内每次读都返回该行，`enqueue` 在准入处被丢弃，用户刷新无效。同一形状：ifzq 一次瞬时 5xx → 该块 6h 无图。G19 登记了机制，没登记「触发条件是每一次重启」；§2.8「读者兜底」在最常见的失败上失效。
- 处置：`unavailable` 行用独立短 TTL（`SERIES_UNAVAILABLE_TTL` ≈ 1-5 min，仍 ≫ 30s 不构成风暴），或预检 `NotRunning`/作用域 `None` 不落行、返回 `pending`；改 D3 TTL 段、§2.8「失败行也按同一 TTL」、seq 3b、A9b（区分两种 TTL）、G19。

### MINOR（含是否可只作实现简报约束）

- m1 [改文档] D2 enqueue 3「排队 job 总数 ≤ 工作区 `chart.series` 块数（每块一个当前 hash）」过宽：写 h2 时 h1 的 job 仍在 in-flight，同一块可同时有多个键；真实上界是「已被读到且尚未出队的 `(块, hash)` 对数」。改措辞即可（陈旧 job 在准入处丢弃，机制本身没问题）。
- m2 [改文档] §2.8 例外清单漏 (e)：`lanes`/`inflight` 全在内存，进程重启丢掉全部排队 job，靠下次读重投。一句话补上。
- m3 [简报约束] `{range:"1M", period:"month"}` 结构上永远 `<2 点`（31 天窗口至多含 1 个完整月）→ 永久 `unavailable` 且每 6h 重投。S1 在 `validate_chart_series` 里拒绝该组合（纯形状），或登记 G。
- m4 [简报约束] ifzq 窗口 > ~640 根时截哪一端未实测（spike 第 3 行没带 start/end）。若源保留最早 N 根，「分页向前直到覆盖 `start − 14`」第一页就停，窗口近端静默缺失；内核清单（日期 ∈ 窗口、≤ max_points、≥2 点、`complete_through ≥ 末点`）检测不到近端缺口 → frozen 块钉在截断数据上。S3 先 spike 截断方向、fixture 照实模拟，并加对称检查「窗口内最晚 bar ≥ `as_of − 14d`，否则 `unavailable`」（与 G18 同形）。
- m5 [简报约束] S3 约束 2「最新 2 根」探测：us 实测用的是 n=3（回 2 行含 2011 基准行），n=2 未测；若只回基准行则 `complete_through = 2011` 永不钉住。用 n=3 并要求至少一根非基准行，否则该条 `unavailable`。
- m6 [改文档 + 简报] §11 末行「待编排者确认」应改为已裁决；D2 步骤 1 准入用 `write_in_tx_typed` 做只读判定会与报告写入争 SQLite 单写者，准入用读事务、步骤 7 才用写事务（`report_blocks_snapshot_tx` 只要 `&mut Transaction`）。
- m7 [简报约束] `inflight`/`lanes` 都是 `std::sync::Mutex`：`InflightGuard::drop` 在 unwind 中跑，`lock().unwrap()` 遇 poison 会在 Drop 里二次 panic → abort；`lanes` 一旦 poison，`enqueue` 在读路径上每次 panic。用 `unwrap_or_else(PoisonError::into_inner)` 或 `parking_lot`，A9d 的 panic failpoint 顺带覆盖。
- m8 [简报约束] D5：`track.report_edited` 同时失效报告与 series 查询，series 可能先带旧 `rev` 重取 → 409；fe 要把 409 当「等报告刷新」不当错误，否则每次编辑闪一次错误态。
- m9 [改文档一句] lane 是按插件、跨 track 全局串行：一篇 200 块的报告占住 market lane 至多 200×30s，其它 track 的图排在后面。与 §2.8 延迟公式一致，但应在 G 表登记。

### 2-5 其它核对

- Oracle：每个 ⚠️/❌ 行恰好一片 ✓；`CardUpdated`/`TrackReportEdited`/`TrackDeleted` 在 `calm-types/src/event.rs:392/579/359`，`ws/events.rs` 存在但非权威 ✓；3b(iii) 需 fixture 插件认领模板（`boot_plugin_host` 可达）；3h failpoint ✓；3i′/3j 插件级 fixture ✓。
- 可合入性：S1 含 Rust + fe zod/占位，web 落 opaque（`track-report.tsx:200-226`）✓；`DATA_KINDS` 消费者只有 `mod.rs:28/229` ✓；S2 的 `resolved` 只在 MCP 面；S4 两份 OpenAPI（`local-rust-gates.sh:44-51` 两份都查）✓。
- 纪律：无已发布迁移改动、号推迟、三常量不 bump 理由成立、`Resolved` 枚举、`points: Option` 是合法的按需字段 ✓。

**MAJOR 未清零**（M1、M2 两条，都是小改动但改的是契约/TTL 语义，须回到文档）。MINOR 中 m3/m4/m5/m7/m8 与 m6 后半可作实现简报约束，m1/m2/m9 与 m6 前半是文档一句话修订。

REVISE