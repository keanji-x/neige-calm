# #985 切片 6：四个设计岔口定向调研

> 只读调研；未运行 `cargo build/test`。以下“新 SQL”是建议形状，不是现存代码。

## 1. 反链载体

1. **改父 task 块会命中根哈希。** 根投影逐键取 `ROOT_HASH_TASK_FIELDS`、跳过缺席/`null`、做 canonical JSON，再 SHA-256；实际链路是 `task_root_projection` → `context_ref`，见 `crates/calm-server/src/task_context.rs:711-735`。纳入集明确含 `goal`、`acceptance`、`context`，见 `crates/calm-server/src/task_context.rs:36-46`。因此向三者任一项追加链接都会改 hash；`context` 即使是 JSON object，只要新增/改变承载链接的值也会改 canonical 投影。现有测试已逐项证明 `goal` 等纳入字段会改根 hash，见 `crates/calm-server/src/task_context.rs:832-854`。

2. `goal`/`acceptance` 会被链接扫描，但无 `#block` 的 `neige://wave/<child>` 不扩展引用闭包：文本扫描只取这两个 task 字段，见 `crates/calm-types/src/report_blocks/kinds.rs:55-71`；闭包只把 `dst_block_id=Some` 的链接入队，见 `crates/calm-server/src/task_context.rs:738-762`。这不挽救根块，因为根块原文本仍已进入 hash。

3. **material 结果需要说准。** 复核只比较当前与冻结元组的 `wave_id/block_id/hash`，不以 rev 或 docRev 判 material，见 `crates/calm-server/src/task_context.rs:401-472`。命中后直接动作是给 in-flight 行写 `context_stale_at_ms` 并发 `TaskContextAdvanced{material}`，不是同一步 `status='failed'`，见 `crates/calm-truth/src/db/sqlite/task_projection.rs:274-324`。尚未越过 `prepare_tx` 的 worker/gate 会被 `refuse_if_context_stale` 拒绝，见 `crates/calm-server/src/operation/mod.rs:77-95`；已经启动的 ungated worker仍可走 `done`，因为完成 UPDATE 没有 stale 谓词，见 `crates/calm-truth/src/db/sqlite/task.rs:440-468`。

4. **task 后插独立 prose 块不会把该块吸进冻结集。** 闭包从唯一根 task 开始，只沿块内显式 `refs` 和带 block anchor 的 Markdown 链接 BFS，见 `crates/calm-server/src/task_context.rs:232-269`、`crates/calm-server/src/task_context.rs:299-327`；没有“相邻块”边。插块虽触发 `WaveReportEdited` 后的异步检测，见 `crates/calm-server/src/dispatcher/mod.rs:1025-1039`，但复核冻结块 hash 均相同就返回 `Same`，见 `crates/calm-server/src/task_context.rs:454-472`。claim 时 `doc_rev` 仅是提交前 fence，见 `crates/calm-server/src/scheduler/mod.rs:741-769`，不参与 post-claim material 判决。

5. 相邻 prose 的代价是语义脱钩：链接虽能被 backlink 扫描器看见，但“紧邻 task”不是任何持久关系，move/insert/delete 都可让视觉归属漂移；位置本来就不是引用语义，设计也明确允许移动且 block rev 不变，见 `docs/architecture/985-doc-as-plan.md:205-211`。这条路不会自杀，但最容易静默显示错对象。

6. **`worker_card_id` 写路径。** worker operation 成功结果取 `result.id` 后进入 `mark_running`，见 `crates/calm-server/src/scheduler/mod.rs:991-1006`；底层用 `COALESCE(worker_card_id, ?)` 且只允许 `dispatched→running`，见 `crates/calm-truth/src/db/sqlite/task.rs:275-302`。快速 worker report 也在其事件事务内用同一 two-sided/COALESCE 语义盖章，见 `crates/calm-server/src/decision_sink.rs:111-168`。

7. **3c 读时回显链。** 实际函数名是 `attach_task_read_state`，不是 `attach_task_read_state_tx`；`wave_projection_state` 的单条 SELECT 给每个 task 附加 `key/status/gate_result_json/worker_card_id/context_stale_at_ms/context_closure_truncated/claim_context_json`，见 `crates/calm-truth/src/db/sqlite/task_projection.rs:154-168`、`crates/calm-truth/src/db/sqlite/task_projection.rs:413-455`。它写入 DTO `BlockVerdict {status,gate_result,worker_card_id}`，见 `crates/calm-truth/src/db/sqlite/task_projection.rs:136-152`、`crates/calm-truth/src/db/sqlite/task_projection.rs:168-223`；读 API 是 report snapshot → `task_diagnostics(..., include_read_state=true)` → `WaveReportReadResponse.task_diagnostics`，见 `crates/calm-server/src/wave_report_read.rs:91-103`、`crates/calm-truth/src/db/sqlite/read.rs:558-589`、`crates/calm-server/src/routes/waves.rs:1543-1581`。

8. 前端按 block id 把 verdict 配回 task 块，见 `web/src/pages/WaveReportPage.tsx:467-478`，最终在 `ReportTaskBlock` 渲染状态和 worker 链接，见 `web/src/pages/report-blocks/task.tsx:119-131`。新增 `childWaveId` 的直接连带面是 Rust `TaskReadState/BlockVerdict/attach`、OpenAPI schema、生成 TS、运行时 Zod、task renderer；现有落点分别见 `crates/calm-truth/src/db/sqlite/task_projection.rs:136-193`、`web/src/api/openapi.json:4104-4141`、`web/src/api/generated.ts:952-960`、`web/src/api/schemas.ts:27-35`、`web/src/pages/report-blocks/task.tsx:119-131`。保持该列为 targeted reader/stamp 可避免把它塞入通用 `Task/TASK_COLUMNS`；这种刻意缩小连带面的既有原则见 `docs/architecture/985-doc-as-plan.md:1851-1853`。

9. 三路比较：改父块＝确定改 hash，静默风险低但会不可逆 stale；相邻 prose＝不 stale、写面沿用 report 内核，但关联只靠位置，静默错链风险最高；`tasks.child_wave_id`＋读时回显＝一次 migration、一个 COALESCE stamp、一个 targeted read/DTO/UI 字段，且结构关系不会被文档编辑破坏。现有 report persist 会同时写 CRDT、tasks 投影和事件，故为了一个结构反链去改文档仍承担不必要的文档副作用，见 `crates/calm-server/src/wave_report.rs:725-772`。

**推荐：用 `tasks.child_wave_id` 的原子 COALESCE stamp + 3c 读时回显，因为它把结构事实留在执行状态表，完全避开冻结根哈希；不要改父块，相邻 prose 只适合临时展示。依据见 `crates/calm-truth/src/db/sqlite/task_projection.rs:136-193`、`crates/calm-server/src/task_context.rs:711-735`。**

## 2. 子 wave 怎么创建

1. **claim 事务内联方案。** 若完整 wave 骨架确实在 claim 的同一个 `write_with_actor_events_typed` 闭包里创建，则“claim 已提交、child 尚未创建”这个窗口不存在：两者一起提交或回滚；claim 本身就在 BEGIN IMMEDIATE 的 eventized tx 中，见 `crates/calm-server/src/scheduler/mod.rs:721-728`、`crates/calm-truth/src/db/sqlite/events.rs:338-358`。但提交后的 spec harness 启动仍是另一阶段；常规 create 也明确在 wave 事务提交后才 submit/wait，见 `crates/calm-server/src/routes/waves.rs:853-886`。内联方案因此还需同事务 stamp `child_wave_id`，并让恢复路径幂等启动该 child 的 harness，否则 skeleton 虽在，agent 可能永久 inert。

2. **child-wave operation 方案。** claim 后、op insert 前崩溃会留下 `dispatched` 行；boot/tick 的 `resume_dispatched` 在 context sweep 完成后重进同一个 `drive_spawn`，见 `crates/calm-server/src/scheduler/mod.rs:1591-1631`。`submit` 先按 `(kind,idem)` 查重、hash 相同则 drive 原 op，否则插入，见 `crates/calm-server/src/operation/driver.rs:110-128`；故没有 op 就补建，有 op 就续跑。adapter `prepare_tx` 与 op 推进到 `tx_committed` 同属一个 IMMEDIATE tx，失败回滚，见 `crates/calm-server/src/operation/repo_sqlite.rs:277-325`；child 骨架不会出现“已建但 op 仍 pending”的提交状态。

3. partial unique index是 `(kind,idempotency_key) WHERE idem IS NOT NULL`，见 `crates/calm-truth/migrations/0042_operations_parked.sql:96-98`。所以同一个 `task.id` 可同时拥有例如 `codex-worker` 与 `child-wave` 两条 op；只有同 kind 内冲突。scheduler 今天已用 `task.id` 作 worker idem，见 `crates/calm-server/src/scheduler/mod.rs:948-958`。

4. `prepare_tx` 的参数就是 `&mut Transaction<Sqlite>`，见 `crates/calm-server/src/operation/mod.rs:612-634`，技术上可写 `waves`、两张 `cards`、layout `overlay` 和 events。完整骨架现有 DB 步骤是 wave、spec/report cards、overlay，见 `crates/calm-server/src/routes/waves.rs:679-680`、`crates/calm-server/src/routes/waves.rs:720-756`、`crates/calm-server/src/routes/waves.rs:794-807`，四个创建事件见 `crates/calm-server/src/routes/waves.rs:808-850`。adapter 的事件出口是事务内 `append_decision_event(s)_in_tx` 持久化/VCS，再把 `BroadcastEnvelope` 放进 `TxOutput.post_commit_events`；既有 codex adapter 示例见 `crates/calm-server/src/operation/codex_adapter/mod.rs:370-410`，提交后 driver 广播见 `crates/calm-server/src/operation/driver.rs:425-427`。

5. **route/DB 分界。** 必须留在 route/preflight 的是：解析 running trusted plugin 并校验 workflow binding，见 `crates/calm-server/src/routes/waves.rs:399-421`；cwd 绝对路径、normalize、cove/folder ownership 与冲突 HTTP 形状，见 `crates/calm-server/src/routes/waves.rs:423-521`；事务外文件系统/git probe，见 `crates/calm-server/src/routes/waves.rs:524-537`。纯 DB 编排是 folder/wave insert、cards、overlay、events，见 `crates/calm-server/src/routes/waves.rs:668-679`、`crates/calm-server/src/routes/waves.rs:720-850`。子 wave 若只继承已存在 parent 的 cove/cwd、且不绑定 workflow/attach/fork，可跳过这些面向不可信 REST 输入的 route preflight；完整骨架仍应抽成 crate 内 tx helper。

6. 内联 claim 的优势只在消灭 skeleton 窗口；其幂等载体必须另造 `child_wave_id`/unique guard，并另外恢复 harness。operation 已有 durable op row、kind-scoped idem、payload hash、pending/tx_committed recovery；这些正是当前 `resume_dispatched` 所依赖的恢复漏斗，见 `crates/calm-server/src/operation/driver.rs:140-149`、`crates/calm-server/src/scheduler/mod.rs:1591-1595`。

**推荐：新增 task-bound `child-wave` operation，`idem=task.id`，在其 `prepare_tx` 原子做树检查、完整骨架、`tasks.child_wave_id` stamp 和创建事件；因为 claim→submit 的崩溃窗口由 `resume_dispatched` 补齐，而骨架内部的崩溃窗口由 op 的 IMMEDIATE prepare 事务消除，幂等载体也已经存在。依据见 `crates/calm-server/src/scheduler/mod.rs:1591-1631`、`crates/calm-server/src/operation/repo_sqlite.rs:277-325`、`crates/calm-truth/migrations/0042_operations_parked.sql:96-98`。**

## 3. 子 wave 的输入

1. spec 启动有两条输入。developer instructions 先渲染通用 spec template；若 `workflow_id` 能解析到 running trusted descriptor，再追加 workflow instructions/plan/gates，最后原样 JSON-fence `workflow_input`，见 `crates/calm-server/src/operation/spec_harness_start_adapter.rs:130-179`、`crates/calm-server/src/operation/spec_harness_start_adapter.rs:209-261`。真正 mint thread 时才读取 binding并把结果交给 `SharedThreadStartParams.developer_instructions`，见 `crates/calm-server/src/operation/spec_harness_start_adapter.rs:505-531`。

2. wave goal不是上述 developer prompt 的参数；`SpecHarnessStartOperationPayload.goal` 在 prepare 中转成初始 snapshot，见 `crates/calm-server/src/operation/spec_harness_start_adapter.rs:191-207`、`crates/calm-server/src/operation/spec_harness_start_adapter.rs:323-368`。`initial_snapshot_with_goal` 把非空文本变成首个 `Observation::WaveGoal`，见 `crates/calm-server/src/harness/mod.rs:482-487`。常规 wave 创建已经用 `wave.title.trim()` 同时播种 spec card prompt 与 start payload goal，见 `crates/calm-server/src/routes/waves.rs:679-729`、`crates/calm-server/src/routes/waves.rs:856-867`。

3. `workflow_input` 是 `Option<serde_json::Value>`，持久为 nullable JSON TEXT，见 `crates/calm-truth/src/model.rs:93-118`、`crates/calm-truth/migrations/0061_waves_workflow_input.sql:8`。但它不是自由载体：无 `workflow_id` 必拒；descriptor 无 `input_schema` 也必拒；有 schema 则必须是受大小限制、字段封闭的 JSON object，见 `crates/calm-server/src/routes/waves.rs:573-614`、`crates/calm-server/src/plugin_host/workflow_input.rs:238-283`。而 plugin 停止/失信时 binding 会降级 vanilla prompt并丢弃 input，见 `crates/calm-server/src/operation/spec_harness_start_adapter.rs:170-179`。所以把任意父 task 声明塞进去通常不合法，也有静默不注入风险。

4. 其它载体没有更直接：`purpose` 是 server-owned structural marker，见 `crates/calm-types/src/model.rs:408-410`；普通 report 初值为空，见 `crates/calm-server/src/routes/waves.rs:736-756`。Today 更能证明它不是“播种模板”：它只建题为 Today 的 wave、空 spec snapshot、空 report、terminal，见 `crates/calm-server/src/routes/today.rs:86-95`、`crates/calm-server/src/routes/today.rs:124-188`，启动时 `goal=None`，见 `crates/calm-server/src/routes/today.rs:272-281`。

5. child 应复制 parent `cove_id` 与 `cwd`：二者是 Wave 的 workspace 身份/运行目录，见 `crates/calm-types/src/model.rs:370-404`；task 自己声明的 `cwd` 仍应作为声明内容进入首轮 seed（其合法值已要求绝对路径，见 `crates/calm-types/src/report_blocks/kinds.rs:190-199`），不要未经 cove-folder ownership 复核就把它提升为 child wave cwd。`theme` 不在 Wave row，且 `wave_create_tx` 完全不消费 `NewWave.theme`，见 `crates/calm-truth/src/model.rs:130-143`、`crates/calm-truth/src/db/sqlite/wave.rs:47-84`；后台创建若 API 形状需要占位，沿既有内核路径用 `default_dark`，见 `crates/calm-server/src/routes/today.rs:172-187`。

6. 不应复制 parent 的 archive/pin/lifecycle/terminal/purpose：新 wave 固定 Draft、其余为空，见 `crates/calm-truth/src/db/sqlite/wave.rs:33-50`、`crates/calm-truth/src/db/sqlite/wave.rs:70-84`。也不应默认复制 `workflow_id/workflow_input`，否则 child 会再次吃到 parent workflow 的 plan/gates/input，而 parent task 已是本次具体声明；workflow input 与模板机制本来就正交，见 `docs/architecture/985-doc-as-plan.md:1155-1158`。`parent_wave_id` 必设 parent，`tree_task_budget` 必为 NULL，依据结构列定义见 `docs/architecture/985-doc-as-plan.md:1855-1862`。

7. 建议把父 task 的冻结 `goal/acceptance/context/cwd` 渲染为一段稳定 Markdown/JSON 文本，直接放进 child 的 `SpecHarnessStartOperationPayload.goal`；child title 只放短标题（如 task key + goal 摘要）。这复用现有 `WaveGoal` 队列和重启 snapshot，不新增 developer-prompt 分支；冻结 Task 已完整保存这些字段，见 `crates/calm-truth/src/model.rs:383-410`。

**推荐：用现成 `SpecHarnessStartOperationPayload.goal → Observation::WaveGoal` 播种完整父 task 声明，child wave 只继承 parent `cove_id/cwd`，因为这是零新 prompt 管线、无 plugin/schema 依赖且已有 snapshot 恢复语义的路径。依据见 `crates/calm-server/src/harness/mod.rs:482-487`、`crates/calm-server/src/routes/waves.rs:856-867`。**

## 4. 树预算的查询与原子性

1. SQLite 能力无阻碍：两 crate 都启用 sqlx `sqlite` feature，见 `crates/calm-truth/Cargo.toml:20-27`、`crates/calm-server/Cargo.toml:144-149`；锁定版本是 sqlx/sqlx-sqlite 0.8.6 与 libsqlite3-sys 0.30.1，见 `Cargo.lock:2673-2684`、`Cargo.lock:2842-2857`、`Cargo.lock:1667-1676`。仓内还记录 bundled SQLite 3.46.0，见 `crates/calm-truth/src/db/sqlite/read.rs:365-370`，并已有生产 `WITH RECURSIVE` 在事务内执行，见 `crates/calm-truth/src/wave_vcs/gc.rs:266-283`。

2. 树 occupied 精确谓词应是 root 的整棵 descendant closure 上 `t.declared_by='spec' AND t.status IN ('pending','dispatched','running','verifying')`；七态全集见 `crates/calm-truth/migrations/0058_tasks_kind_claude.sql:19-21`，现有 nonterminal reader也是这四态，见 `crates/calm-truth/src/db/sqlite/read.rs:422-431`。现有 `evaluate_schedulability` 不能复用：它只查当前 wave、只把三种 in-flight 态装入 JSON、排除 pending，见 `crates/calm-truth/src/db/sqlite/task_projection.rs:413-420`；又额外要求 `origin='block'`，见 `crates/calm-truth/src/db/sqlite/task_projection.rs:531-540`。树谓词则跨所有后代、包含 pending、不限 origin，只保留 `declared_by='spec'`，与设计的第三层约束一致，见 `docs/architecture/985-doc-as-plan.md:1162-1178`。

3. 原子先例是 `project_tasks_tx`：同一 caller transaction 先 `evaluate_schedulability` 读容量/存量，再只为 schedulable declarations INSERT/UPSERT pending rows，见 `crates/calm-truth/src/db/sqlite/task_projection.rs:839-846`、`crates/calm-truth/src/db/sqlite/task_projection.rs:948-985`；注释明确写路径把同一函数放在 IMMEDIATE tx 中且 admission 原子，见 `crates/calm-truth/src/db/sqlite/task_projection.rs:459-464`、`crates/calm-truth/src/db/sqlite/task_projection.rs:500-503`。child operation prepare 本身也是 BEGIN IMMEDIATE，见 `crates/calm-server/src/operation/repo_sqlite.rs:277-325`，所以“读根/深度/计数→判定→INSERT child”应全放其中。

4. **单一真源方案兼容现状。** public root 创建沿固定 INSERT 列表，可让 migration 的 `tree_task_budget DEFAULT 32` 生效；child helper必须显式写 `parent_wave_id=?`、`tree_task_budget=NULL`，因为现有 `wave_create_tx` 不命名新列，见 `crates/calm-truth/src/db/sqlite/wave.rs:47-63`。既有 `Wave/WaveRow` 采用固定字段，不会因表多列自动解码失败，见 `crates/calm-types/src/model.rs:368-420`、`crates/calm-truth/src/db/rows.rs:79-103`；若只做 targeted tree query，可以像现有策略列一样不扩大公共 DTO，现有先例见 `crates/calm-truth/migrations/0068_projection_policy_columns.sql:10-13`。

5. 求根与当前深度可合并一次向上 CTE（待加列依据 `docs/architecture/985-doc-as-plan.md:1855-1862`）：

```sql
WITH RECURSIVE up(id,parent_wave_id,depth) AS (
  SELECT id,parent_wave_id,0 FROM waves WHERE id=?1
  UNION ALL
  SELECT w.id,w.parent_wave_id,up.depth+1 FROM waves w JOIN up ON w.id=up.parent_wave_id
)
SELECT id AS root_id,depth AS current_depth,tree_task_budget
FROM up WHERE parent_wave_id IS NULL;
```

创建 child 的深度是 `current_depth+1`；根=0、上限=3，判定写成 `current_depth >= MAX_WAVE_TREE_DEPTH` 即拒绝，所以允许 0..=3。常数与语义见 `docs/architecture/985-doc-as-plan.md:1170-1186`。创建期 parent 总是既存、新 child 尚无后代，配合 parent 关系只允许内核创建，可避免环；至少还应加 `CHECK(parent_wave_id IS NULL OR parent_wave_id<>id)`。

6. 用上一步的 `root_id` 再做向下 CTE计数（同一 prepare tx），形状依据“树内 spec 非终结”不变量 `docs/architecture/985-doc-as-plan.md:1983-1993`：

```sql
WITH RECURSIVE subtree(id) AS (
  SELECT ?1 UNION ALL
  SELECT w.id FROM waves w JOIN subtree s ON w.parent_wave_id=s.id
)
SELECT COUNT(*) FROM tasks t JOIN subtree s ON s.id=t.wave_id
WHERE t.declared_by='spec'
  AND t.status IN ('pending','dispatched','running','verifying');
```

达到 root budget 时拒绝 child INSERT；`NULL` root 值按内核默认 32 解释。BEGIN IMMEDIATE 在取得 writer slot 后运行，见 `crates/calm-truth/src/db/sqlite/infra.rs:10-24`，所以两个并发 child 创建不能都基于同一旧计数提交。

**推荐：只让根 wave 持有 `tree_task_budget`，child 恒 NULL；child-wave operation 的同一 IMMEDIATE `prepare_tx` 先用向上 CTE一次求根+当前深度，再向下 CTE计数并决定是否插入，因为这样只有一个预算真源且检查与结构写不可分割。依据见 `docs/architecture/985-doc-as-plan.md:1855-1862`、`crates/calm-server/src/operation/repo_sqlite.rs:277-325`。**

## 我认为你（发问者）判断错的地方

1. “改父块 ⇒ material ⇒ 行立刻 failed”不精确：前两步成立，但 material 原语只 stamp `context_stale_at_ms`，见 `crates/calm-truth/src/db/sqlite/task_projection.rs:298-324`；只有尚未 prepare 的 worker/gate 被拒后才会沿既有失败臂终结，已启动 ungated worker仍可能 `done`，见 `crates/calm-server/src/operation/mod.rs:82-95`、`crates/calm-truth/src/db/sqlite/task.rs:448-468`。所以它是确定的“冻结污染/后续起活拒绝”，不是无条件同步 `failed`。

2. `attach_task_read_state_tx` 这个函数名不存在；实际是纯内存 `attach_task_read_state`，SQL 在 `wave_projection_state` 中一次取回，见 `crates/calm-truth/src/db/sqlite/task_projection.rs:165-193`、`crates/calm-truth/src/db/sqlite/task_projection.rs:408-455`。

3. “树预算检查只需和 child INSERT 原子”不够保证设计声称的稳态上界：child spec 后续还能经 `project_tasks_tx` 新增 pending 行，现有插入点见 `crates/calm-truth/src/db/sqlite/task_projection.rs:948-985`；而不变量要求任一时刻整树 spec 非终结行不超预算，见 `docs/architecture/985-doc-as-plan.md:1989-1993`。因此相同 root-budget admission 还必须接入所有会新增 spec pending 行的投影路径；child INSERT 内检查是必要条件，不是充分条件。
