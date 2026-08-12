# #985 切片 6 PR-B：树项 rebuild 序依赖的定向调研（只读，基于 `c71e4132`）

## A. rebuild 的实际契约与工作单元

**A1 入口 `tasks_rebuild_tx`，工作单元 = 一个 wave，生产调用方唯一一处。**
`crates/calm-server/src/wave_report.rs:116-148`：读该 wave 的 `wave-report` 卡（`:120-121`）→ CRDT 快照 → `project_task_declarations` → `project_tasks_tx`（`:147`）。签名只吃一个 `wave_id`，无 cove / 树 / 全库变体。
生产调用方**只有** `crates/calm-server/src/routes/waves.rs:1318`，条件 `projection_policy_changed = p.spec_task_ceiling.is_some() || p.automation_policy.is_some()`（`:1310`）—— 即 `PATCH /api/waves/{id}` 改这两列时在同一写事务重投影**该 wave**。无启动路径、无 cove 批量。测试调用方：`tests/cases/task_projection_acceptance.rs:155-160`（helper `rebuild`）、`tests/scheduler.rs:3431`。
**全仓没有任何「按树 / 按 cove 批量重建」先例。**

**A2 D.1 #11 的载体是三个单-wave 测试；改成树级工作单元不打断它们，但它们对树项零覆盖。**
- `tests/cases/task_projection_acceptance.rs:1096-1162`：走生产写路径做对抗编辑（重复 key / 环 / 撤回 ready），快照 `task_bytes`（`:1091-1094`，27 列 `json_object`，含全部状态列）→ 裸 SQL **损坏**物化行（`:1139-1148`）→ 一次 `tasks_rebuild_tx`（`:1151`）→ 断言逐字节回到损坏前（`:1155-1159`）。
- `:1164-1285`：增量与 rebuild 各跑一次，比 `task_bytes` + `kernel_events` payload（`:1259-1277`），再断重复 rebuild 零事件（`:1278-1281`）。
- `:1541-1567` `ceiling_rebuild_is_stable_and_only_new_candidate_is_rejected`：ceiling=2、三条声明，断言两次 rebuild 逐字节相同 —— **今天 ceiling 项幂等性的唯一哨兵**。

**A3 D.5 五条的载体**（`crates/calm-truth/src/db/sqlite/task_projection.rs`）：
#1 删除阶段 = `:962` 调 `task_delete_pending_tx`（`:850-858`，SQL 自带 `AND status='pending'`），仅在 `!schedulable || withdrawal` 且非在飞时触发（`:897-898`）；谓词全形在 `evaluate_schedulability`（`:535-848`）。
#2/#3（存活行状态列逐字节不变）由 upsert 的 SET 列表 + WHERE 共同保证：`:1008-1039` 的 `DO UPDATE SET` **只含声明列**，status / worker_card_id / gate_* / finished_at_ms / claim_context_json 一个都没有，且整条受 `WHERE tasks.status='pending'`（`:1023`）钳制；非 pending 行只被 `:973-980` 的 legacy 收养 UPDATE 摸到（只改 `origin`/`decl_ready`，即 D.5 #2 的豁免）。唯一写状态列的是 `mark_context_material_tx`（`:927-943`，写 `context_stale_at_ms`）—— 这正是 A2 第二个测试要存取该列绕开比对的原因（`:1246`/`:1253`）。
#4 legacy：`:1024` 的 `origin='block' OR origin='legacy'` + `:962` 的 pending 限定。#5：`:1017` `declared_by=excluded.declared_by`。

## B. 现有 ceiling 项读了什么（关键节）

**B4 输入今天全部来自本 wave（外加被引用目标的存在性），三处，无第四处。**
1. `wave_projection_state`（`:421-474`）单语句读 `waves w WHERE w.id=?1` 的 `automation_policy, spec_task_ceiling, require_task_gates, cove_id`（`:427/448`），加两个相关子查询：`tasks t WHERE t.wave_id=w.id AND t.status IN ('dispatched','running','verifying')` 取 `key/status/declared_by/origin`（`:428-433`），以及（仅读路径）同 wave 的读时状态列（`:434-445`）。
2. 引用校验 `waves w JOIN coves c` + `cards`/`json_each`（`:620-632`）——**已经是跨 wave 读**，但只问「目标存不存在 / 属哪个 cove」，不做任何计数。
3. 冻结列 `SELECT … FROM tasks WHERE wave_id=?1 AND status!='pending'`（`:687-692`）。
`occupied` = 上述在飞行中 `declared_by='spec' AND origin='block'` 的计数（`:552-556`），`capacity = ceiling - occupied`（`:557`），准入 `take(capacity)`（`:817`）。

**B5 是的 —— 但「从没人写出来」这个前提是错的，两处白纸黑字。**
- 代码 `task_projection.rs:798-799`：`// Pending rows are outputs, never inputs. Only clean declarations that can produce a live row compete for remaining capacity.`；候选集 `:800-810` 确实只由 declarations + `inflight_key_set` + `frozen_by_key` 决定，不读任何 pending 行。
- 权威文档 `docs/architecture/985-doc-as-plan.md:366-370`：「`pending` 行是本次求值的**产物**，把产物数进输入会让投影函数不幂等 ⇒ rebuild ≢ 增量 ⇒ 击穿 §10.1」。PR-B 设计 `docs/_985-s6-design.md:770-776`（BLOCKER-A3）又复述一遍。
- **有没有别的机制在承担它**：没有第二道。写路径（`:867`）与读路径（`db/sqlite/read.rs:588`）共用同一个谓词，`ceiling_rebuild_is_stable_…`（`:1541-1567`）是唯一回归哨兵，而它成立**完全**依赖这一条。
- 附带事实：未准入的 pending 行会被当场删掉（`:962`），所以 per-wave 上界是硬的：`live_spec(v) = inflight(v) + pending(v) ≤ ceiling_v`。

## C. 两个方案的实际代价

**C6 方案甲的精确退化上界；默认常数下等于什么都没做。**
记树 `T`、`B = tree_task_budget`、`I = Σ_{v∈T} inflight_spec(v)`。甲下 `admitted_W ≤ min(ceiling_W − inflight_W, B − I)`，最坏情形（各 wave 都在 `I=0` 时投影）：

> `Σ_{v∈T} live_spec(v) ≤ Σ_{v∈T} min(spec_task_ceiling_v, B) ≤ min(Σ_v ceiling_v, N_open·B)`

即甲只把每个 wave 的有效 ceiling 收窄成 `min(ceiling_v, B)`。**默认 `ceiling = 32`（`task_projection.rs:17`）、`B = 32`（`985-doc-as-plan.md:1179`）⇒ 收窄量为 0。**
且 `N_open`（有存量的 wave 数）已被**强制点一独立封顶**：每个未闭合子 wave 恰对应一条仍 `dispatched/running` 的父任务行（`scheduler/mod.rs:398-424` 的成功 flip 要求子 wave `lifecycle='done'` 且子 wave 无未结行；`:769-771` sweep 同口径），该行必被强制点一的「全树非终结」计数数到 ⇒ `N_open ≤ B`。所以强制点一单独已给 `L ≤ B·ceiling = 1024`，**甲把它「改进」到 1024 ⇒ 恒真门，典型 fake gate 形状，不值得做**。甲也挡不住在飞：per-wave `DEFAULT_WAVE_TASK_BUDGET = 1`（`scheduler/mod.rs:79`）本就给 `I ≤ N_open`。

**C7 方案乙：改动面可控，但「整树 rebuild」救不了 D.1 #11。**
改动面：① `tasks_rebuild_tx` 之上加 `tree_rebuild_tx`（解根复用 `WAVE_ROOT_DEPTH_SQL`，`child_wave_adapter.rs:49-52`；向下 CTE 新写）按 root-first DFS 逐 wave 调用，约 60–90 行；② 唯一生产调用方 `routes/waves.rs:1318` 换成树版；③ `evaluate_schedulability` 加 `external_occupied`（`_985-s6-design.md:777`）。**无先例可抄**（A1）。
真正的代价不在行数：共享预算的先到先得分配是**路径依赖**的。反例 —— 兄弟 C、D 各声明 2 条，`B=2`，两边都是 0 存量（刚 PATCH 抬高预算）。增量顺序由人的编辑顺序决定：先写 C ⇒ C 得 2、D 得 0。整树 rebuild 只能按**确定性**顺序（DFS/id）得到规范不动点，与增量历史留下的不动点在一半情形下不同；rebuild 无从得知「谁先来」（pending 是产物，按 B5 不能当输入；当 tie-break 就是 BLOCKER-A3 的同一个坑）。⇒ **乙 = 明确牺牲 D.1 #11**，整树 rebuild 只是把不确定变确定。

**C8 第三条路（丙）：把树级判据换成与 pending 无关、且不含历史的量。**
- **丙-1（在飞版，不能单独用）**：树预算改判在飞，在 claim 事务里加树级 in-flight 检查 —— 先例现成：`scheduler/mod.rs:1270-1287` 已在 claim 后重读 siblings 并 `if in_flight > budget { race_lost }`。全读稳定状态，rebuild 不碰，D.1 #11 零风险；**但它约束并发不是存量**，与 §8「为什么并发容量挡不住存量」（`985-doc-as-plan.md:1181-1183`）正面冲突 ⇒ 只能当补充。
- **丙-2（确定性配额分割，推荐）**：`effective_ceiling_W = min(spec_task_ceiling_W, share(W, T))`，`share` 只是 `B` 在树内 wave 集合上的确定性划分（`floor(B/N)`，余数按 `(created_at, id)` 升序前 r 个 +1，`Σ share = B`）。
  - 输入只有本 wave 文档 + 本 wave 在飞 + **树的形状**（`waves` 行），三者都不是投影产物 ⇒ **rebuild 序无关、D.1 #11 原样成立、`external_occupied` 补丁被消掉**。
  - 上界 `Σ_v live_spec(v) ≤ Σ_v share_v = B` —— **这才是 D.4 #7 想要的那条**。
  - 查询面与乙相同（解根 + 向下 CTE），但只数 wave 不数 task，更轻（走 `idx_waves_parent_wave_id`，`migrations/0071_sub_wave_tree.sql:7-8`）。
  - 唯一新语义：树变大 ⇒ 份额缩小 ⇒ 超额 pending 在其下次投影按既有顺序被裁。这与「人把 ceiling 调低到在飞数以下」**完全同形**，而那条退化语义文档已批准写死（`985-doc-as-plan.md:1194-1196`）⇒ 复用，不是新风险。
  - 成本：树深/宽时份额偏紧（N=10 ⇒ 每 wave 3 条）。属常数标定问题（`:1198-1204` 已把所有常数登记为「猜的、按可观测量标定」）。

## D. PR-A 的坑在 PR-B 会不会复发

**D9 会，且门禁本身要改。** PR-A 静态门禁 `crates/calm-server/src/operation/child_wave_adapter.rs:497-503`：对 `WAVE_ROOT_DEPTH_SQL`(`:49-52`) 与 `WAVE_BOUNDED_PATH_SQL`(`:54-57`) 断言 `sql.contains("WHERE up.depth <= ?2")` 与 `sql.contains("UNION ALL")`；截断常量 `MAX_WAVE_TREE_DEPTH: i64 = 3`（`:22`），绑定传 `MAX_WAVE_TREE_DEPTH + 1`（`:103`、`:119`），片段由 `macro_rules! bounded_wave_ancestor_cte!`（`:35-47`）统一产出。
向下 CTE 要接进去需：新写同形宏（只投影 `id` + 同一 depth 截断）**并把新常量加进 `:499` 那个硬编码数组**。建议顺手改成 `pub const BOUNDED_WAVE_TREE_SQL: &[&str]` 清单、门禁遍历清单 —— 否则「新增 SQL 漏登记」不会有任何东西变红。

**D10 `tree_task_budget` 今天全仓不存在**（`.rs/.sql/.ts` 零命中；`0071_sub_wave_tree.sql` 只加 `parent_wave_id` / `spawn` / `child_wave_id`）。`NewWave`（`crates/calm-truth/src/model.rs:93-144`）**无任何预算列**；`WavePatch`（`:147-182`）。**3b 给 `spec_task_ceiling` 加写入面的完整清单（模板）**：
1. `crates/calm-truth/migrations/0068_projection_policy_columns.sql:12` —— `ALTER TABLE waves ADD COLUMN … INTEGER NULL DEFAULT 32`；
2. `crates/calm-truth/src/model.rs:171-173` —— `WavePatch.spec_task_ceiling: Option<Option<i64>>` + `deserialize_double_option`（present-null = 复位默认）；
3. `crates/calm-truth/src/db/sqlite/wave.rs:200-206` —— `wave_update_tx` 里的定向单列 UPDATE；
4. `crates/calm-server/src/routes/waves.rs:1221-1226` —— user-only 闸（非 `EditAuthor::User` ⇒ 403）；
5. `:1272-1278` —— 取值校验（`>= 0` 否则 400，文案指明 null 复位）；
6. `:1297` —— 计入 `patch_has_other_changes`（否则纯该列的 PATCH 被当空补丁短路返回）；
7. `:1310, :1318` —— 计入 `projection_policy_changed` 并触发 `tasks_rebuild_tx`；
8. `web/src/api/generated.ts:2235` —— OpenAPI/TS 生成物重生成并提交；
9. `crates/calm-types/src/report_blocks/tasks.rs:38, 58, 223, 816` —— 诊断码登记 + 路径映射 + 人话文案 + 元测试；
10. 验收 `crates/calm-server/tests/cases/wave_projection_policy_patch.rs:91-170`（生产 PATCH 路径：写、复位、403、400 全覆盖）。
`tree_task_budget` 多一条：**只在 root 有意义** ⇒ 第 4/5 项还要加「非 root ⇒ 拒绝」。

**D11 `wave_create_tx` 的固定列清单在 `crates/calm-truth/src/db/sqlite/wave.rs:48-50`**（列表 `:49`、`VALUES` `:50`，共 14 列；`spec_task_ceiling` / `automation_policy` / `task_budget` 都**不在**其中，全靠 DB DEFAULT）。PR-A 的子 wave 建点是 `child_wave_adapter.rs:180-194`（`wave_create_tx(tx, NewWave{…})`），紧跟 `:197-201` 的 `UPDATE waves SET parent_wave_id=?1`。**今天没有任何一处写 `tree_task_budget`**，若照 `spec_task_ceiling` 加 `DEFAULT 32`，每个子 wave 都会拿到自己的 32（`_985-s6-design.md:798-801` 已预警）。落点二选一：在 `:49-50` 的固定列清单里显式写 `NULL`（覆盖所有建 wave 路径），或在 `:197-201` 那条 UPDATE 顺带 `tree_task_budget=NULL`（只覆盖子 wave 路径）。**推荐前者** —— `wave_create_tx` 的注释 `:34-40` 已把「显式写进 INSERT 列表而不是靠 DEFAULT」立为该函数的既定规矩。

## 纠正发问者的前提

1. 「『不数 pending』这个职责从没人写出来」—— **错**：`task_projection.rs:798-799`、`985-doc-as-plan.md:366-370`、`_985-s6-design.md:770-776` 三处明写。
2. 「乙 除非把 rebuild 工作单元改成一棵树，否则序依赖」—— **改成一棵树也不够**（C7）：整树 rebuild 只消除 rebuild 内部的不确定性，消不掉「增量是先到先得、rebuild 无从得知」这条鸿沟。
3. 「甲仍值得做」—— **默认常数下等于没做**（C6），且造出一个恒真门。

---

推荐：**丙**（C8 的丙-2「确定性配额分割」），因为它把树级判据换成 `min(spec_task_ceiling, share(W, 树形状))` —— 三个输入没有一个是投影产物，于是 rebuild 序无关、D.1 #11 与 `ceiling_rebuild_is_stable_…` 原样成立、BLOCKER-A3 的 `external_occupied` 补丁被消掉，同时给出**真正的**硬上界 `Σ_v live_spec(v) ≤ tree_task_budget`（甲给不出，乙要拿 D.1 #11 换）。

D.4 #7 建议措辞（替换 `985-doc-as-plan.md:2018` 该行的树预算半句）：

> 树内 `declared_by='spec' AND origin='block'` 的非终结行数 ≤ `tree_task_budget`。载体是两个强制点：`child-wave` 的 `prepare_tx` 创建准入（向下 CTE，只投影 `id` + `depth <= MAX_WAVE_TREE_DEPTH` 硬截断，共用静态门禁），与 `evaluate_schedulability` 的树项 —— **树项形如「有效 ceiling = min(`spec_task_ceiling`, 该 wave 在树内的确定性配额份额)」，份额只是 `tree_task_budget` 在树的 wave 集合上的确定性划分，不读任何 `pending` 行**（同 §4.2 规则 3 的幂等要求，理由同 §10.1）。因此本条**不依赖 rebuild 顺序**，D.1 #11 在树上原样成立。与 §4.2 一致的退化条款：树形状变大（新建子 wave）或人调低 `tree_task_budget` 时，上界暂时退化为那一刻树内的在飞行数，随这些行终结**单调收敛**回新上界；期间超额 wave 的 `capacity = 0`，超额 `pending` 行按块序 + `key` 逆序被裁掉。

验收随之改成：「同一棵树、两种 rebuild 顺序 ⇒ 逐字节同一结果」**且**「树内任一 wave 单独 rebuild ⇒ 与整树 rebuild 同一结果」—— 后者在丙下恒成立，是丙相对乙多出来的可证伪收益；在乙下必红。
