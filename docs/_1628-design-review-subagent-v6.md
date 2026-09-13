# #1628 设计 v6 评审（通道 A，第 6 轮）

**基线**：`review-a-1628` HEAD `4caa4669` ← `28faf40d`(v6) ← `bd0c909c`(v5)，只含 docs；`DATA_KINDS` 仍 4 项（`kinds.rs:58`）、迁移止于 `0106`、manifest 仍三个工具（`manifest.json:12/34/62`）。

## 0. 第 5 轮处置复核（逐条到代码与 v6 正文）

- R5-1 `row_ttl`：D3「按行状态与 series 结果选」、D2 步骤 1 准入、§2.8、seq 3d′、S2.7、A9b(iii) 三向变异 ✓ 机制。
- R5-3 `mode`：D1 派生（不入指纹、由 `as_of` 有无派生）、D2 请求形状示例含 `"mode"`、S2.8/S3.8 ✓。
- 放宽范围：§2.5 约束 3、S3.5、seq 3l 三处一致写 `live ∧ day ∧ venue ∉ {CRYPTO}`；CRYPTO/周月/frozen 严格；内核 S2.9 按 `(frozen ∨ period ≠ day)` 分支、venue 只在插件 ✓。`Venue` 闭集含 `Crypto`（`main.rs:175-176`）、`parse_asset` 判 venue（`:295-300`）✓。
- R5-2 缓存日期键：§2.5 约束 2、S3.4、A14 `cache_page_never_crosses_utc_midnight` ✓。
- A m3 同源探测：约束 2、S3.3、A14 `fallback_source_reprobes` ✓。A m1/m2/m4/m5/m6：S2.5/S2.6、S2.3（`Repo::sqlite_pool` 默认 `None` `db/mod.rs:1236-1238`、`state.rs:686` 透传 ✓）、D6 改「非 `None`」✓、§11 第 4 轮末行「已裁决」✓、S4.3/A21（`invalidation-plan.ts:291-294` plan 只有 `track_id` ✓）。
- 收尾：§5.1 编号 S1.1–S4.5，§9 只剩 U6/U8/U9 ✓。

**结论：第 5 轮采纳项全部由机制落地。**

## 1. 攻击最终矩阵（venue × period × mode × 缓存页年龄 × 时钟偏斜）

逐格构造，均未证伪钉住不变式：
- 股票 live day + 内核快 s 秒：`as_of` 仍 = D-1，常规收盘余量 ≥ 3h（US 冬令）/ ≥ 15h ⇒ 偏斜无关 ✓；内核慢 ⇒ 更保守 ✓。
- CRYPTO live day 双向偏斜：快 ⇒ 探测最新 = D = `as_of` 被 `<` 排除；慢 ⇒ `as_of` = D-1 < D+1 纳入、D 被 `≤ as_of` 排除 ✓。
- 混合 venue 一块（US + CRYPTO，live day）：插件分条判、内核弱检查两条都过 ✓。
- 23:59 缓存页 + 00:01 任何 mode：`fetched_on` 不等 ⇒ 重取 ✓（R5-2 关闭）。
- live week 周一 00:30：需 `complete_through ≥ 周一` ⇒ 上周不出，与 G21「一周期加一交易日」一致 ✓。
- frozen `as_of` = 昨天、当天各时刻：无 D bar ⇒ 不钉，钉住必须跨到 D bar 出现 ✓。
- 时区标注：ifzq 无论按 ET 日还是北京日标 US bar，日期 ≤ D-1 的 bar 常规时段都在 D 00:00 UTC 前结束（按北京收盘日标时更早一天）⇒ 放宽在两种标注下都安全；盘后归 U9 ✓。

找到的都是**保守方向的滞后**与文档过宽句，见 MINOR m1–m3。

## BLOCKER
无。

## MAJOR
**无。MAJOR 及以上清零。** 最接近线的是 m1（同日缓存页让「不滞后」「立即钉住」两句在源迟发布/首次解析早于源推进时不成立，但方向保守、上界一个 UTC 日、数据不错），判 MINOR 交编排者复核。

## MINOR

- **m1 [实现简报 S3.4 + doc]** 同日缓存页无近端刷新规则。构造 A（frozen）：`as_of` = D-1，首次解析 00:05 UTC D → 探测 = D-1、页缓存 `observed` = D-1；18:05 UTC D 再解析：新探测 = D（ifzq 已列 D bar）但页命中（`fetched_on` = D）、min 规则 ⇒ `complete_through` = D-1 ⇒ 不钉、D-1 bar 仍不出，直到 D+1。D3「`as_of` = 昨天 → 若探测含今天的 bar 立即钉住」在这一天为假。构造 B（live 股票 day）：页在源列出 D-1 bar 之前取得（doc 自己用的「ifzq 批量滞后」前提），同日 6h 重投全部命中同页 ⇒ §2.8「昨天的 bar 只要源已列出就在图上」为假，滞后到 D+1。处置：S3.4 加一句「覆盖 `as_of` 的近端页只在 `page.observed_complete_through == 本次探测值` 时复用，否则重取该页并更新 `observed`」（其余页照旧）；A14 加变异「近端页 observed 落后于新探测仍复用 → 红」；D3/§2.8 两句加「（缓存页复用除外，S3.4）」或按新规则保留原句。
- **m2 [doc §2.5 表 + U9]** U9 spike 写成「冬令时段 00:00–01:00 UTC」：今天 2026-09-13 为 EDT（至 11-01），S3 若在 11 月前实现则按字面无法执行。EDT 下盘后 = 20:00–00:00 UTC，**恰在** D+1 00:00 结束（余量 0，与 CRYPTO 同形），表里只写了冬令时「跨过零点」。改为「在盘后时段（EDT 20:00–00:00 / EST 21:00–01:00 UTC）对同一美股连取两次，比对当日 ET 交易日 bar 的 close/volume 是否仍在变」；表中 US 行补「夏令时盘后止于 00:00 UTC（余量 0）」。结论不变：吸收 ⇒ US 全年严格。
- **m3 [doc → KNOWN GAP]** `ok` 行重投失败（超时/isError/malformed）时步骤 7 整行改写为 `unavailable`，先前的好数据（含部分成功行的成功 series）消失 ≥ 2 min。v5 已有此形状，v6 的 2 min 短 TTL 让部分成功行更频繁经历它。不建议本轮改结构（保留旧数据需 `last_attempt_at` 列才能同时维持重试上界）；登记一条 G 并注明可选方案。
- **m4 [实现简报 S3.8]** `market.series` 对 agent 可见（G6），agent 直调可缺 `mode`/`start`/`as_of`/`fields`/`period` 或给非法值；S3.8 只写了 `deadline_ms` 的 `tool_error`。补「任一必填缺失或非法 → `tool_error`，不默认 live、不默认放宽」。
- **m5 [实现简报 S2.7/A9b(iii)]** 「新行保留成功那条的数据」应改为「新行 = 第二次回复」（整行替换语义，D3）；假插件第二次回复若与第一次相同，该断言空过。
- **m6 [doc G13]** `parse_asset` 先 `to_ascii_uppercase`（`main.rs:296`），内核正则允许小写 symbol，`US:nvda`/`US:NVDA` 与 `HK:9988`/`HK:09988` 同形，G13 补一句。
- **m7 [doc D1]** 远未来 `as_of`（如 2099）窗口全空 ⇒ 落 `unavailable`（2 min TTL、每次读一次网络探测），不是 D1 所写的「`ok` 未钉住按 TTL 重投」；改一句并指向 G22 同类代价。

## 其它核对（无 finding）
- Oracle：每个 ⚠️/❌ 行恰一片（新增 3j′/3l 归 S3）；负路径可达；事件 `TrackDeleted/CardUpdated/TrackReportEdited` 在 `calm-types/src/event.rs:359/392/579`（`ws/events.rs` 无这些名字，非权威）。
- 切片：S1 独立合入 web/ 落 opaque、fe 占位；S2 先于 S3 走 `NotExposed → pending` 零调用；S3 先于 S2 只多一个 agent 可见工具；两前端与两份 OpenAPI 在 S2.16/S4.1/A17 计入。
- 纪律：无已发布迁移改动、号未占（`0106` 为最新）；`SYNC_EVENT_VERSION`/`REST_API_VERSION`/`WEB_COMPAT_VERSION`/`SCHEMA_VERSION` 不 bump 理由成立，`gate-web-compat-version-lockstep.sh:50-55` 只查三处相等；`Resolved` 枚举、`mode` 为必填派生字段，无 `Option`+default 必填。
- S1.2 结构下限表与 `max_points` 逐格核算 ✓。

APPROVE