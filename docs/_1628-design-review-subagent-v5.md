## #1628 设计 v5 评审（通道 A，第 5 轮）

> 归档注：编排者为通过 #1316 术语棘轮把原文一处退役词替换为「OpenAPI 文件」，其余一字未动。

**基线核对**：`review-a-1628` HEAD `bd0c909c` ← `18299cbb` ← `a9315d73` ← `ec0b3590` ← `c534bf6b`，只含 docs；`DATA_KINDS` 仍 4 项（`kinds.rs:58`）、迁移止于 `0106`、market manifest 仍只暴露三个工具（`manifest.json:12/34/62`）、`Cargo.lock:3644-3645` tokio 1.52.3、`gate-web-compat-version-lockstep.sh` 只查三处相等、`local-rust-gates.sh:44-56` 两份 OpenAPI 都查 ✓。

## 0. 第 4 轮处置复核（逐条到代码与 v5 正文）

- **探测先于取数**（R4-1）：§2.5 约束 1/2 规定顺序 + 缓存页 `observed_complete_through` 取 min；seq 3k、A14 `probe_precedes_window_fetch` 有 must-red ✓ 机制。
- **纳入规则 `≤ as_of ∧ < complete_through`**（R4-2）：约束 3、D2 步骤 6「每点结束日 < complete_through」、A12 `bar_at_complete_through_is_rejected`、A7b fixture 改「points 止于周四」三处一致 ✓。
- **in-flight 键 `(track, block)`**（R4-3）：enqueue 1、resolve 1「出队时派生」、seq 3a′、A9g 两条变异一致；行身份仍含 hash、D3 身份段明写两者不同 ✓。
- **历法校验**（A M1）：D1、§2.2 结论、seq 2n/7、A2 变异、D2 步骤 1 的兜底出口（绕过校验 → 丢弃不落行）✓。
- **预检未命中不落行 + `unavailable` 2 min TTL**（A M2）：enqueue 2 `Miss(reason)`、resolve 2-4 出队否定态丢弃、D3 双 TTL、read 侧「TTL 按行状态」、A5/A9b/A9e、G19 ✓。`running_plugin_ids` 不含 `Spawning` 核实 `plugin_host/mod.rs:3256-3262` ✓。
- **读事务准入**（A m6 / F7.8）：见 §3。
- 修订者四条附带：(1) 永久否定态（`Only(other)` / ForgeAction / 非只读 / `Http`）落行让 fe 停轮询，瞬态否定态不落行——两类划分与 G19「fe 持续轮询直到条件变化」自洽；(2) 出队否定态丢弃与读端同规则 ✓；(3) `Pending { reason: Option }` 与 A5、seq 3b、D5「带 reason 时显示」一致 ✓；(4) G21 是裁决 2 的直接代价、如实登记 ✓。仅 §11 第 4 轮末行仍写「待编排者确认」（与上轮 m6 同形，v6 改「已裁决」）。

**结论：第 4 轮采纳项全部由机制落地，无一条只改措辞。**

## 1. 编排者提议：live 行放宽为「结束日 ≤ 昨天 UTC」

逐市场（bar 日期 D 的常规时段何时结束）：

| 市场 | 日期标注 | 常规收盘 (UTC) | 距 D+1 00:00 UTC 的余量 | 成立? |
|---|---|---|---|---|
| US（ifzq / Sina 兜底） | ET 交易日 = 同一 UTC 日 | EDT 20:00 / EST 21:00；半日 17:00-18:00 | ≥ 3h | ✓ 常规时段；**盘后 16:00-20:00 ET = 冬令时 21:00-01:00 UTC 跨过零点**——若源日线 bar 吸收盘后成交（未实测，需 spike），00:00-01:00 UTC 的 live 解析取到仍在变的 bar |
| HK | 本地日 = UTC 日 | 08:00 + 收市竞价 08:10；半日市 04:00 | ≥ 15h | ✓ |
| SH/SZ | 本地日 = UTC 日 | 07:00（盘后定价 07:30） | ≥ 16h | ✓ |
| CRYPTO（Binance） | UTC | **恰在** D+1 00:00:00 收盘（closeTime = 23:59:59.999） | **0** | ✗「前」不成立，是「等于」：内核时钟比 Binance 快 s 秒 ⇒ 每天 00:00:00-00:00:0s 内的 live 解析把**未收盘** D 根写进行（存活 ≤ 6h） |

关键事实：**对 crypto，v5 严格规则本来就不滞后**——不带 end 的探测必然含今天未收盘 K [实测 §2.5]，`complete_through` = 今天 > 昨天恒成立，昨天的 bar 任何时刻都纳入（G21 也这么写）。所以放宽对 crypto**零收益、只添时钟偏斜窗口**；而且严格规则在偏斜下反而安全（内核以为 D+1、Binance 仍在 D ⇒ 探测最新 = D ⇒ `as_of` = D 被排除）。

是否重开旧构造：
- 盘中 bar（A M3）：仍被 `as_of = 昨天` 排除 ✓。R4-1 / R3-1 / R3-3 / R4-2 全部关于钉住或未来截止日，live 永不钉住、live `as_of` 永不在未来，不重开 ✓。
- **live 周/月线部分周期：会以新形态重开。** 构造：周一 00:30 UTC，live `as_of` = 周日；ISO 周结束日 = 周日 ≤ `as_of` → 纳入；但 ifzq 批量更新滞后、周五的日线 bar 尚未发布 → 周 K 由周一-周四聚成，close = 周四，标成完整周，存活 ≤ 6h。v5 的「存在更晚日线 bar」正是「该周期所有成员 bar 已发布」的证明（D3 前提：按序发布）；放宽后周/月聚合失去这个证明。日线不受此影响（bar 要么缺、要么已收盘）。月末同形。
- 现有 wire 形状「两态同形、插件不区分」（D2 请求窗口）与提议冲突：插件必须知道 mode 才能选规则 → 请求要加 `mode: "live"|"frozen"`（或 `require_later_bar: bool`）；内核校验清单的「结束日 < complete_through」对 live 要改为不查（否则回复被判 malformed）。
- 冻结不连续：live 显示到昨天，用户写 `as_of` = 昨天冻结后新行少最后一根直到明天的 bar 出现——不是缺陷，但要写进 G21 的替代文字。

**判定**：提议对 US/HK/SH/SZ **日线**常规时段成立且值得采纳（G21 的「周日看周四收盘」对 live 是真实产品缺陷；live 行永不钉住 + 6h 自愈使错误有界）；对 crypto 与 live 周/月线不应放宽。建议形态：**`period = day ∧ live` 才放宽为 `≤ as_of`；周/月线与 frozen 保持 `< complete_through`；插件对 `CRYPTO` venue 在任何模式下保持严格（免费、闭合偏斜窗口，venue 语义本就归插件 F5.1）**；内核清单：`(frozen ∨ period ≠ day)` 时执行 `< complete_through`，纯 payload 可判、无 venue 知识。落点：D2 请求形状加 `mode`、步骤 6 分支、§2.5 约束 3、§2.8 首条、seq 8 fixture（不再需要 `complete_through` 推进）、A8/A12/A14 的三条变异改为只对 frozen/周月、G21 改写、新增 G「live 日线依赖『源日线 bar 在 D+1 00:00 UTC 前定稿（常规时段）』；错误存活 ≤ 6h 且永不钉住」、新增 U「ifzq/Sina 美股日线是否吸收盘后成交」（S3 spike，与 U7/U8 同列）。

## 2. 攻击 v5（构造与结果）

- 改源换 lane：块从 P 改到 Q 并读 → 键在飞 → `InFlight`；P lane 出队 → `(plugin, tool)` 不属本 lane → 丢弃释放键 → 下次读投 Q。只有延迟（排在 P 的积压后），无错误 ✓。
- 出队时块已被删 / 改成 prose：准入读不到 → 丢弃 ✓；删 track 在调用期间 → FK 拒绝 ✓。
- 两读者 check-precheck-insert 交错：`HashSet::insert` 在锁内原子，后到者 `InFlight`、不构造 guard ✓。
- 源挂死 + 200 块：2 min TTL 下 lane 以 30s/job 爬行、每块每 100 min 重试一次，串行自限，不成风暴；新块排在重试后面（G20 已含）✓。
- `deadline_ms` 时钟：stdio/Cli 都是本机子进程，偏斜 0 ✓。
- `as_of` = `0000-01-01`、`9999-12-31` + 5Y：chrono `NaiveDate` 范围 ±262143 年，减法不失败 ✓。
- 跨源 fallback（ifzq 探测、Sina 取窗）：收盘是市场事实不是源事实，论证仍成立；但需同一条 series 内探测与取数不交叉（见 MINOR）。
- 3k 缓存 min 规则、A7b「周五/周五」fixture、§5.1 组合表（1M+month 最大 1 < 2；3M+month 最小 2）逐格核算 ✓。
- `Only(other)` 行 2 min TTL：每次被读到且过期 → job → 零调用但一次写事务；仅 Owned track（今天只有 git-forge）✓ 可接受。

**未找到能证伪 v5 任一不变式的构造。**

## 3. v5 新增 [实测] 核对

- **F4.18** `tool_visibility.rs:87-152`：头注 `:91-94` 自述 tools/list 与 tools/call 热路径 ✓；`track_get` `:113`；不存在 / 出错 → `None` `:115-131` ✓；绑定 track 再经 `resolve_track_owner_binding`（`track_binding/mod.rs:314-341`：`running_plugin_ids().await` `:327` + `registry().get` `:332`）——「一次 plugin host 表读」对绑定 track 实为 lock_table + registry 各一次，与 enqueue 步骤 2 自己的 `running_plugin_ids` 合计两次；量级不变，非 finding。
- **F7.8** `events.rs:872-874` `begin_immediate_tx` ✓；`infra.rs:10-15` `BEGIN IMMEDIATE` + busy 重试 ✓；`state.rs:686` `sqlite_pool() -> Option<SqlitePool>` ✓；`settled.rs:148-151` pool 直读先例 ✓；`track_report.rs:73-76` `report_blocks_snapshot_tx(tx: &mut Transaction<'_, Sqlite>, ..)` ✓；`sqlite/mod.rs:279` WAL ✓；调用方 `admission.rs:158`、`repair.rs:101` ✓。

## 4. Oracle / 切片 / 纪律

`CardUpdated`/`TrackReportEdited`/`TrackDeleted` 在 `calm-types/src/event.rs:392/579/359`（`ws/events.rs` 无这些名字，非权威）✓；每个 ⚠️/❌ 行恰一片 ✓；S1 独立合入后 web/ 落 opaque（F6.8）、fe 占位、`is_data_kind` 消费者只有 `track_report_blocks.rs:584` 与 `mod.rs:229` ✓；`scannable_text_fields` 对 `chart.candles` 也不扫 `caption`，新 kind 不扫是一致的 ✓；两前端两份 OpenAPI 文件均计入（D5、A17）✓；无已发布迁移改动、号未占、三常量不 bump 理由成立、`Resolved` 枚举 ✓。

## BLOCKER
无。

## MAJOR
**无。第 4 轮两条 MAJOR（M1 历法、M2 重启冻结）与 codex 三条均已由机制关闭；本轮未产生新 MAJOR。**

## MINOR

- m1 [简报约束 S2/S4] 读端与准入的行查询要按全主键 `(track, block, request_hash)`，且默认摘要读**不 SELECT `data` 列**：D4 与 D2 步骤 1 都写「按 `(track_id, block_id)` 的查询」；G8 不 GC ⇒ 改过 50 次参数的块有 50 行 × ≤1 MiB，落在 CAS 必经的 read 路径上。
- m2 [简报约束 S2] `repo.sqlite_pool()` 是 `Option`（trait 默认 `None`，`calm-truth/src/db/mod.rs:1236-1238`）：D2 步骤 1 要定义 `None` 分支（丢弃 + warn，不落行）。
- m3 [简报约束 S3] 同一条 series 的探测与窗口取数必须来自同一源；ifzq 中途失败切 Sina 兜底时**重新探测**再取窗，缓存页的 `observed_complete_through` 按源分别记。
- m4 [文档一句] D6「lane 只为路由命中且**作用域允许**的插件建」与 enqueue 步骤 2「`All | Only(_)` 都进 lane」不一致（`Only(other)` 不允许却进 lane 落永久行）：改为「作用域非 `None`」。
- m5 [文档一句] §11 第 4 轮末行「待编排者确认」→「已裁决」（同上轮 m6 形状）。
- m6 [简报约束 S4，可选] `track.report_edited` 失效 `['track-report-series', trackId]` 前缀会让**每个**块重取默认 `full`（≤1 MiB/块），即使该块 `rev` 未变；query key 已含 `rev`，可改为只失效 `rev` 变化的块或给 series 查询设 `staleTime`。
- m7 [文档] 若采纳 §1 的提议，按上面列的落点改；否则 G21 维持。

APPROVE