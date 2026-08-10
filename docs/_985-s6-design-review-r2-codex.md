# #985 切片 6 设计增量 v2 · 对抗性评审 r2（codex）

结论：**NO，PR-A 仍不可施工。** v2 修复了多数 r1 缺口，但新引入的 `CASCADE`、把 `Done` 当作“子树已静止”，以及未定型的 `kind/spawn` 语义仍是阻塞项。以下攻击均以当前工作树为准。

## Findings

### [BLOCKER] `Done` 不代表子 wave 已静止，父 gate 可与子 gate 在同一 cwd 并发
**攻击**：子 wave 有两个 gated task；第一个 gate 结束把 lifecycle 推到 `Reviewing`，第二个 gate 仍在跑。spec 此时合法写 `Reviewing→Done`；父闭合立即把父 task 推到 `verifying` 并启动父 gate。父 task 未声明 `cwd` 时，父 gate 落父 wave cwd；子 wave 继承同一 cwd，子 task gate 也回退到该 cwd，于是两个 shell gate 并发改同一目录。更基本地，父任务已经“成功”，但子 wave 仍有非终结任务。
**证据**：`docs/_985-s6-design.md:304`、`:369-377`；`crates/calm-types/src/wave_lifecycle.rs:279-286`；`crates/calm-server/src/operation/task_verify_adapter.rs:668-681`；`crates/calm-server/src/scheduler/mod.rs:143-146`。
**为什么 v2 的验收抓不到**：§7 #13 只造 `child Done→父 gate`，不在 child 留第二条 running/verifying task；§7.1 反而强制父 fixture 带非默认 `cwd`，恰好绕开两个 gate 都回退到 wave cwd 的生产分支（`docs/_985-s6-design.md:471,486-488`）。
**建议**：在 child `Done→父成功` 的同一 IMMEDIATE tx 内先断言 child 无 `pending/dispatched/running/verifying` task；否则 fail-closed（或 no-op 并诊断）。新增“双 gated task、一个仍 parked”交错，并让父 task/gate cwd 均缺席。

### [BLOCKER] `ON DELETE CASCADE` 打开了真实的第三条父删路径，并且 B1 的前提把 `NO ACTION` 当成了 `RESTRICT`
**攻击**：调用内核 raw `RepoSyncDomainRaw::wave_delete(parent)`，不经过 REST descendant guard。helper 只清 parent 的 task/session，随后删 parent；自 FK 的 `CASCADE` 静默删 child wave/cards，却不清 child 的无-FK task，且不做 child 进程 teardown/`WaveDeleted`。有 child session 时则会在更深 FK 上失败。另一方面，删 cove 是**一条** `DELETE FROM coves` 语句，所有同 cove waves 都由 `cove_id CASCADE` 在该语句内消失；self-FK `NO ACTION` 在语句结束时已无引用，不需要自 FK `CASCADE`。v2/r1 所称“逐行立即失败”是 `RESTRICT` 语义。
**证据**：`docs/_985-s6-design.md:178-192,420-424`；`crates/calm-truth/src/db/sqlite/session_repo_impl.rs:316-323`；`crates/calm-truth/src/db/sqlite/wave.rs:218-245`；`crates/calm-truth/src/db/sqlite/cove.rs:182-185`；`crates/calm-truth/migrations/0001_init.sql:20-29`；仓库需要“立即强迫 teardown”时明确选的是 `RESTRICT`（`crates/calm-truth/migrations/0011_terminals_card_id_restrict.sql:11-16`）。
**为什么 v2 的验收抓不到**：§7 #20 只测公开单-wave 删除，route 加 guard 即绿；#21 把 self FK 变回 `NO ACTION` 仍会删完整 cove 成功，所谓“多次重复/强制删除序”也制造不出第二条 SQL（`docs/_985-s6-design.md:478-479`）。
**建议**：self FK 保持 `NO ACTION`；descendant guard 下沉到 `wave_delete_tx`（route 可保留早拒绝），cove 仍走单语句整棵删除。分别测 route、raw repo、cove 三入口，并断言 child tasks/sessions/events。

### [BLOCKER] §1.6 把已经能从代码裁决的产品语义推给实现方，#23 因而没有确定 oracle
**攻击**：实现者让 `spawn='sub-wave', kind='terminal'` 通过并忽略 kind；另一实现者全部拒绝；两者都可声称“实现前裁决”。现有 spec harness 从 session row、thread attribution到 MCP identity都硬编码 `AgentProvider::Codex`，没有 kind/provider 选择口，因此当前代码已经给出“不可选”的答案。
**证据**：未定型规则在 `docs/_985-s6-design.md:136-144,580-581`；硬编码在 `crates/calm-server/src/operation/spec_harness_start_adapter.rs:381-383,589-594,627-633`；#23 仍写成条件验收（`docs/_985-s6-design.md:481`）。
**为什么 v2 的验收抓不到**：§7 #23 的 expected 取决于被测实现自己先选哪条语义，断言与被测代码共用同一事实来源；静默忽略可以被“裁决为可选”包装成绿。
**建议**：设计现在定型：当前 PR-A 下仅默认 `kind=codex` 可与 `sub-wave` 组合，`claude/terminal` 在 task 块公共写口拒绝；未来增加可选 spec provider 时另开设计。

### [MAJOR] 带 `depth` 的 `UNION` 不会按节点去重；B2 实际由截断修好，而 #7 的指定变异仍绿
**攻击**：2 环 `A.parent=B, B.parent=A`。行依次为 `(A,B,0),(B,A,1),(A,B,2)…`；`UNION` 按整行去重，所以没有重复行。把 SQL 的 `UNION` 变回 `UNION ALL`，保留 `WHERE up.depth <= MAX+1`，两版都会有限步停下、零 root、及时拒绝。
**证据**：CTE 三列及递增 depth 在 `docs/_985-s6-design.md:211-221`，截断/零行拒绝在 `:224-229`；#7 只变异集合算子（`:465`）。生产先例只投影节点列，不能证明这个三列 CTE（`crates/calm-truth/src/wave_vcs/gc.rs:271-280`）。
**为什么 v2 的验收抓不到**：§7 #7 的 `UNION→UNION ALL` 变异仍在 1s 内返回同一拒绝，测试全绿；断言和实现共同依赖真正起作用的 depth cutoff。
**建议**：要么 CTE 只递归 `(id,parent)`、外层另算步数/visited；要么承认唯一终止保证是 cutoff，删掉 UNION 变异，改测“去掉 cutoff 必红”并对 cycle/truncated 给不同诊断。

### [MAJOR] §4 的 liveness 负例只跑一轮时，删掉短路仍然绿
**攻击**：sub-wave 成功专用 stamp 留 `running_deadline_ms=NULL`；删掉 sweep 的 spawn 短路。把时钟推进超过 timeout 后第一次 sweep 不会 fail，它只从**当前时刻**补盖一个未来 deadline；第二次再推进/再 sweep 才杀父任务。
**证据**：v2 要改专用 stamp、predicate 和 Running arm 三处（`docs/_985-s6-design.md:341-350`）；现生产 sweep 先补盖再判断（`crates/calm-server/src/scheduler/mod.rs:1271-1296`），补盖值为 `now+timeout`（`:1318-1347`）。
**为什么 v2 的验收抓不到**：§7 #11 只说“有时钟推进的时间轴”，没有要求两轮 sweep，也不在该条断言 deadline 始终 NULL；#12 只验刚 spawn 后两列（`docs/_985-s6-design.md:469-470`）。两条可同时绿。
**建议**：#11 明写两次 `advance(timeout+1)+sweep`，每轮都断言父仍 running 且 deadline 为 NULL；代码顺序要求 `spawn` arm先于 terminal/kind 两臂。

### [MAJOR] “每次 drive 都幂等启动 harness”没有 exactly-once oracle，重复起 harness 仍可过 #19
**攻击**：崩在 child op 成功后；恢复的 `drive_spawn` 再 submit harness。实现若误抄普通 wave create 的 `idempotency_key=None`，会建第二条 start op/第二个 thread；adapter 的 replace 逻辑最终仍留下活 harness，child 仍离开 Draft，#19 全绿。
**证据**：v2 的修法与验收只要求“拿到 harness/离开 Draft”（`docs/_985-s6-design.md:268-287,477`）；普通 create 使用 `idempotency_key: None`（`crates/calm-server/src/routes/waves.rs:868-883`），today 先例才用稳定 key（`crates/calm-server/src/routes/today.rs:288-303`）；submit 仅在 `(kind,key)` 相同且 hash 相同时复用（`crates/calm-server/src/operation/driver.rs:110-128`）。
**为什么 v2 的验收抓不到**：#19 不数 start-op、thread mint、superseded session；“至少一个活着”无法证明“没有重复起”。
**建议**：规定 key（例如 `child-wave:<child_id>:bootstrap`）和逐字节稳定 payload；崩溃恢复后断言恰好一条 start-op、一次 thread mint、无 superseded runtime。

### [MAJOR] reopen 守卫没有指定落层，“一律禁止”可被 raw update 绕过
**攻击**：REST route 加反查并让 #17 通过，但内部 `RepoSyncDomainRaw::wave_update(child, lifecycle=Planning)` 仍直达机械 `wave_update_tx`；父任务终态后 child 重开，原 r1 失联重现。MCP/spec 与 kernel 当前因 actor 表挡住 user-only reopen，但这不替代 DB 结构守卫。
**证据**：v2 只写产品裁决，未指定函数/事务落点（`docs/_985-s6-design.md:380-393`）；DB writer 明说校验在调用者、自己机械写（`crates/calm-truth/src/db/sqlite/wave.rs:115-147`），raw 实现直接调用它（`crates/calm-truth/src/db/sqlite/session_repo_impl.rs:309-313`）；reopen 是 user-only（`crates/calm-types/src/wave_lifecycle.rs:236-252`）。
**为什么 v2 的验收抓不到**：§7 #17 的 Conflict 测公开 PATCH 即可全绿（`docs/_985-s6-design.md:475`），没有 raw/internal lifecycle writer 变体。
**建议**：把“terminal→Planning 且被 child_wave_id 引用则 Conflict”的查询放进所有 lifecycle writer 共用的 in-tx helper；若刻意只保证产品入口，就收窄“一律”措辞并加源码闭集测试。

### [MAJOR] #5 只编辑一个冻结字段，字段选择性重读仍能全绿
**攻击**：错误 payload builder 仅从当前报告重读 `acceptance`（或 `context/cwd`），仍从 task row读 `goal`；测试若只在 op insert 后编辑 goal，hash/seed/child id 全保持，性质“payload 全来自冻结行”为假但全绿。
**证据**：payload 包含四个独立输入（`docs/_985-s6-design.md:291-293`），#5 只要求“编辑纳入哈希的字段”单数（`:463`）；§7.1 只是给 fixture 加 gate/depends_on/cwd，不要求逐字段编辑和断言（`:486-488`）。
**为什么 v2 的验收抓不到**：被测代码与 expected 在未改字段上共用同一报告事实；这是“fixture 加数据但不对每项制造分歧”。
**建议**：对 `goal/acceptance/context/cwd` 做表驱动四次差分（或一次同时改成四个手写 sentinel），逐字段断言 child seed 文本；expected 不调用 payload/seed builder。

### [MAJOR] PR-B 的向下 CTE只写了算子名，可能复制 PR-A 的同形空洞
**攻击**：PR-B 为了同时限深而实现 `down(id,depth)`，再相信 `UNION` 去环；环上 depth 递增，仍不去重。若漏 cutoff，强制点一握着 writer slot不终止；若保留 cutoff，`UNION ALL` 变异仍绿。
**证据**：PR-B 只定型为“向下 CTE（UNION）”，没有列形状、visited/cutoff或 cycle 诊断（`docs/_985-s6-design.md:494-503`）；同文错误地把 UNION 当去环保证（`:202-209`）。
**为什么 v2 的验收抓不到**：§7 的 23 条全属 PR-A，PR-B 没有向下环/超深/耗时变异；#7 只打向上 CTE（`docs/_985-s6-design.md:450-481`）。
**建议**：PR-B 定型成只投影 wave id 的 `UNION`，或显式 path/visited+硬 cutoff；登记向下 2 环与超深中毒数据的耗时测试。

## r1 的哪几条 v2 没修好 / 修出了新洞

| r1 编号 | r2 结论 |
|---|---|
| codex B1（第五强制点） | **已修好**：第 0 步在同一 IMMEDIATE tx、首个副作用前检查，符合权威漏斗（`docs/_985-s6-design.md:244-266`；`docs/architecture/985-doc-as-plan.md:835-852`）。 |
| codex B2 / subagent B4（liveness） | **机制写对、验收没修好**：首次补 deadline 使删短路变异仍绿，见 Finding 5（`docs/_985-s6-design.md:327-350`）。 |
| codex B3（pending 自计数） | **PR-B 纸面修好**：减本 wave pending 再重准入（`docs/_985-s6-design.md:504-514`）。 |
| codex B4（终态非单调） | **部分修**：同事务复核写清了；reopen 守卫落层未定，见 Finding 7（`docs/_985-s6-design.md:380-393`）。 |
| subagent B1（cove delete） | **修出更大洞**：`CASCADE` 让 raw 父删越过 teardown，且 #21 的 NO ACTION 变异不红，见 Finding 2（`docs/_985-s6-design.md:178-192`）。 |
| subagent B2（环） | **安全性靠 cutoff 修好，UNION 论证/变异未修好**，见 Finding 4（`docs/_985-s6-design.md:202-229`）。 |
| subagent B3（UPSERT 析取） | **已修好**：INSERT/SET/析取三处均登记（`docs/_985-s6-design.md:67-83`）。 |
| subagent B5（harness 恢复） | **恢复臂有了，exactly-once 验收缺失**，见 Finding 6（`docs/_985-s6-design.md:268-287`）。 |
| codex M1（eventized gate） | **已修好**：要求 flip+事件同事务且 gate 真启动（`docs/_985-s6-design.md:407-416`）。 |
| codex M2（CTE 环） | **部分修**：硬截断有效，UNION 去环主张仍假（`docs/_985-s6-design.md:211-229`）。 |
| codex M3 / subagent M1（非树零查询恒真） | **已识别并推 PR-B**，要求计数接缝（`docs/_985-s6-design.md:523-525`）。 |
| codex M4（冻结 payload） | **改进但字段覆盖不足**，见 Finding 8（`docs/_985-s6-design.md:128-134,463`）。 |
| codex M5（tx 内重读） | **已修好**：API 收窄+事务 seam（`docs/_985-s6-design.md:106-118`）。 |
| subagent M2（求根 fail-open） | **PR-B 纸面修好**：`tree_root_unresolved` fail-closed（`docs/_985-s6-design.md:518-519`）。 |
| subagent M3（预算无写入面） | **PR-B 纸面修好**：要求 NewWave/WavePatch 写口（`docs/_985-s6-design.md:520-522`）。 |
| subagent M4（reopen） | **未完全修好**：裁决有了，闭集落点没有，见 Finding 7（`docs/_985-s6-design.md:386-393`）。 |
| subagent M5（parent 索引） | **已修好**：partial index 已进载体（`docs/_985-s6-design.md:173-195`）。 |
| subagent M6（rebuild 序依赖） | **PR-B 仍待二选一，但坑已显式登记并给验收**（`docs/_985-s6-design.md:526-529`）。 |
| subagent M7（父 cancel） | **只给人工收场，不是自动闭合；作为已知产品限制可接受**（`docs/_985-s6-design.md:425-429,568`）。 |

## §7 的 23 条里，哪几条在我设计的变异下仍然绿

| # | 仍绿的变异 |
|---|---|
| 4 | 只把显式 `null` 规范化错；该条只比缺席与显式 `in-wave`（`docs/_985-s6-design.md:462`）。 |
| 5 | 只重读未被 fixture 编辑的 `acceptance/context/cwd`（`docs/_985-s6-design.md:291-293,463`）。 |
| 7 | `UNION→UNION ALL` 但保留 cutoff，结果和耗时相同（`docs/_985-s6-design.md:214-229,465`）。 |
| 11+12 | 去掉 sweep 短路；首轮只补未来 deadline，刚 spawn 时仍为 NULL（`docs/_985-s6-design.md:469-470`；`crates/calm-server/src/scheduler/mod.rs:1271-1296`）。 |
| 13 | child `Done` 时仍留另一条 verifying gate；父 gate照样启动（`docs/_985-s6-design.md:471`）。 |
| 17 | guard 只放 REST；raw lifecycle update仍可 reopen（`docs/_985-s6-design.md:475`）。 |
| 19 | harness start 用 fresh/NULL idem；重复启动但最终仍离开 Draft（`docs/_985-s6-design.md:477`）。 |
| 20 | guard 只放 route；raw `wave_delete` 级联删后代（`docs/_985-s6-design.md:478`）。 |
| 21 | self FK 改回 `NO ACTION`；单条 cove delete 仍成功且无孤儿（`docs/_985-s6-design.md:479`）。 |
| 23 | 保持“实现时裁决”，测试 expected 跟随实现选择，静默忽略可伪装为合法选择（`docs/_985-s6-design.md:481`）。 |

## migration `0071_` 代码核实

不需要重建 `waves`：新列 nullable/default NULL，可原地 `ALTER TABLE ... ADD COLUMN parent_wave_id TEXT REFERENCES waves(id) ON DELETE ... CHECK (...)`，再建 parent partial index；新增 CHECK 的原地先例见 `crates/calm-truth/migrations/0067_task_context_freeze.sql:2-6`，仓库也明确只有“NOT NULL + REFERENCES”不能一步加（`0054_worker_sessions_card_id.sql:1-13`）。`tasks.spawn` 与 `child_wave_id` 同样可 additive，再建 unique partial index。`0058_` 重建是因为要**修改既有** `kind CHECK`（`0058_tasks_kind_claude.sql:1-7`），不适用于本片；且被引用表重建会触发 ON DELETE actions 的风险已有警告（`0054_worker_sessions_card_id.sql:3-10`）。PR-A 必须把这条“禁止整表重建”写进 migration 方案。

## 可以施工了吗

**NO。** 最小阻塞集只有三项：

1. `Done` 成功映射必须增加 child-task quiescence 条件，并补同 cwd 双 gate 交错；
2. 撤回 self-FK `CASCADE`，用 `NO ACTION` + `wave_delete_tx` 下沉 descendant guard，补 raw/cove 双入口；
3. 现在定型 `kind/spawn`：本 PR 仅允许默认 Codex kind，非默认组合在公共写口拒绝。

其余 MAJOR 可与施工前设计修订一起收口，但 §7 的对应变异必须先实际证明会红。
