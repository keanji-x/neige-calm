# #985 切片 6 只读施工清查

> 范围：仅以当前工作树为准；未运行 `cargo build/test`。

## A. wave 创建路径

### 1. 全部创建入口与 5b 请求形状

- 常规 REST 入口是 `POST /api/waves -> create_wave`，路由注册在 `crates/calm-server/src/routes/waves.rs:126-128`，handler 在 `crates/calm-server/src/routes/waves.rs:371-376`，再调用私有 helper `create_wave_with_spec_harness` 于 `crates/calm-server/src/routes/waves.rs:538-551`、定义于 `crates/calm-server/src/routes/waves.rs:631-635`。
- 5b 的 `CreateWaveRequest` 位于 `crates/calm-server/src/routes/waves.rs:87-105`：`cove_id/title/sort/cwd/workflow_id/workflow_input/attach_folder/theme/fork_report_from`；`fork_report_from: Option<String>` 是一次性复制源报告指令，`into_parts` 把其与 `NewWave` 分离于 `crates/calm-server/src/routes/waves.rs:107-123`。
- Today 有独立生产入口 `POST /api/today/launchpad/ensure -> ensure_today_launchpad`，注册于 `crates/calm-server/src/routes/today.rs:30-32`、handler 在 `crates/calm-server/src/routes/today.rs:209-213`，其事务 helper 是 `today_launchpad_ensure_tx` 于 `crates/calm-server/src/routes/today.rs:65-70`；它直接 `INSERT INTO waves`，不走常规 helper，见 `crates/calm-server/src/routes/today.rs:72-95`。
- 最底层事务函数是 `wave_create_tx(&mut Transaction, NewWave, &WaveCoveCache)`，定义于 `crates/calm-truth/src/db/sqlite/wave.rs:13-18`、机械 INSERT 于 `crates/calm-truth/src/db/sqlite/wave.rs:48-67`；raw 包装 `RepoSyncDomainRaw::wave_create` 声明于 `crates/calm-truth/src/db/mod.rs:830-843`、实现于 `crates/calm-truth/src/db/sqlite/session_repo_impl.rs:302-307`，但该 trait 明确不暴露给 route、会绕过 eventized write，见 `crates/calm-truth/src/db/mod.rs:830-837`。

### 2. 一次创建事务写什么

- 常规路径在一个 `write_with_actor_events_typed` 事务内按顺序可选写 `cove_folders`、写 `waves`、写 spec/report 两张 `cards`、写 layout `overlays`，对应 `crates/calm-server/src/routes/waves.rs:656-679`、`crates/calm-server/src/routes/waves.rs:718-759`、`crates/calm-server/src/routes/waves.rs:794-809`。
- fork 时，同一事务还读取源报告、直接写新 report card 的 payload+CRDT 并投影 `tasks`，对应 `crates/calm-server/src/routes/waves.rs:684-716`、`crates/calm-server/src/routes/waves.rs:762-783`、`crates/calm-server/src/routes/waves.rs:938-963`。
- 同一闭包返回 `WaveUpdated`、两条 `CardAdded`、`OverlaySet`；fork 投影变化再发 `PlanUpdated` 和投影内核事件，见 `crates/calm-server/src/routes/waves.rs:810-861`。
- eventized write 在同一 SQLite 事务先插事件、再按 wave 生成 Wave VCS commit，最后 commit 后广播，见 `crates/calm-truth/src/db/sqlite/events.rs:338-378`、`crates/calm-truth/src/db/sqlite/events.rs:388-423`；VCS 写 `wave_vcs_objects`、`wave_vcs_commits`、upsert `wave_vcs_refs` 于 `crates/calm-truth/src/wave_vcs/store.rs:140-150`、`crates/calm-truth/src/wave_vcs/store.rs:82-121`，表定义于 `crates/calm-truth/migrations/0039_wave_vcs.sql:7-34`。
- `session` 不在 wave-create 事务：常规路径提交后才 submit/wait `spec-harness-start`，见 `crates/calm-server/src/routes/waves.rs:867-925`；adapter 后续事务才调用 `session_start_runtime_tx` 并更新 spec card，见 `crates/calm-server/src/operation/spec_harness_start_adapter.rs:589-624`、`crates/calm-server/src/operation/spec_harness_start_adapter.rs:650-678`，最终 `worker_sessions` INSERT 在 `crates/calm-truth/src/db/sqlite/session_row.rs:423-449`。
- Today 的 bootstrap 事务只保证 wave、spec/report/terminal cards 和 terminal 行，见 `crates/calm-server/src/routes/today.rs:72-95`、`crates/calm-server/src/routes/today.rs:97-194`；它用无事件的 `write_in_tx_typed`，随后另起幂等 harness operation，见 `crates/calm-server/src/routes/today.rs:253-268`、`crates/calm-server/src/routes/today.rs:275-312`。
- 结论：只有“插一条 wave”的 `wave_create_tx` 可复用；“完整 wave 骨架”仍是 route 私有 helper 内的闭包编排，并没有独立的 create-wave-in-tx 服务函数，见 `crates/calm-truth/src/db/sqlite/wave.rs:13-18`、`crates/calm-server/src/routes/waves.rs:631-679`。

### 3. scheduler claim/dispatch 事务内创建 wave 是否可行

- 数据库层可行：claim 已在 `write_with_actor_events_typed` 的闭包里拿到 `&mut Transaction`，见 `crates/calm-server/src/scheduler/mod.rs:723-731`；`wave_create_tx` 正好接受该类型，见 `crates/calm-truth/src/db/sqlite/wave.rs:13-18`，且 scheduler 已直接使用同层 tx helper，见 `crates/calm-server/src/scheduler/mod.rs:52-57`。
- 事件出口也够用：claim 闭包已返回多条 actor-scoped 事件并由同一 eventized write 落库/VCS，见 `crates/calm-server/src/scheduler/mod.rs:850-919`、`crates/calm-truth/src/db/sqlite/events.rs:378-409`。
- 当前不能直接复用完整创建语义的障碍是 route helper 私有且参数是 `RouteState/CreateWaveOptions`，并把 route 级 cwd/workflow/plugin 校验与事务编排、提交后 harness 启动揉在一起，见 `crates/calm-server/src/routes/waves.rs:386-420`、`crates/calm-server/src/routes/waves.rs:615-635`、`crates/calm-server/src/routes/waves.rs:867-925`。
- 最小改造判断：抽一个接收 `&mut Transaction + WriteContext caches + 已验证 options`、返回 wave/cards/overlay/events 的 crate 内 helper；scheduler 在 claim 事务调用它，提交后用已有弱引用 `OperationRuntime` 启动子 wave harness即可，不需要把整个 `AppState` 搬进 scheduler，依据是 scheduler 自持 repo/events/write/runtime 于 `crates/calm-server/src/scheduler/mod.rs:326-345`，而 operation 的 DB prepare 自带事务边界于 `crates/calm-server/src/operation/repo_sqlite.rs:277-330`。

## B. tasks 表与 spawn 列

### 4. schema、列常量与模型

- `tasks` 初始定义在 migration `0041`，列与索引见 `crates/calm-truth/migrations/0041_tasks.sql:2-29`；`0058` 为加入 `claude` 重建过整表，见 `crates/calm-truth/migrations/0058_tasks_kind_claude.sql:7-54`。
- `TASK_COLUMNS` 在 `crates/calm-truth/src/db/sqlite/task.rs:19-23`；task 块允许字段集合 `TASK_FIELDS` 在 `crates/calm-types/src/report_blocks/kinds.rs:119-137`。
- SQL row 模型 `Task` 在 `crates/calm-truth/src/model.rs:375-410`，当前末端业务列包含 `context_stale_at_ms/declared_by/origin`，见 `crates/calm-truth/src/model.rs:403-407`。

### 5. 全仓 tasks 显式列 SELECT

- 会解码成完整 `Task`、加列时必须同步 `TASK_COLUMNS` 的五处是：`tasks_by_wave_tx`、`task_get_tx`，见 `crates/calm-truth/src/db/sqlite/task.rs:33-39`、`crates/calm-truth/src/db/sqlite/task.rs:131-137`；以及 repo 的 `tasks_by_wave/task_get/tasks_nonterminal`，见 `crates/calm-truth/src/db/sqlite/read.rs:402-410`、`crates/calm-truth/src/db/sqlite/read.rs:413-420`、`crates/calm-truth/src/db/sqlite/read.rs:423-429`。
- 生产代码其余定向列清单是：context 两个 reader，见 `crates/calm-truth/src/db/sqlite/read.rs:438-445`、`crates/calm-truth/src/db/sqlite/read.rs:466-473`；projection 的 key/policy/frozen/existing readers，见 `crates/calm-truth/src/db/sqlite/task_projection.rs:283-289`、`crates/calm-truth/src/db/sqlite/task_projection.rs:414-437`、`crates/calm-truth/src/db/sqlite/task_projection.rs:668-675`、`crates/calm-truth/src/db/sqlite/task_projection.rs:852-858`；verify PID readers，见 `crates/calm-server/src/operation/task_verify_adapter.rs:756-762`、`crates/calm-server/src/operation/task_verify_adapter.rs:1193-1200`；context-stale scalar与 inflight-id 子查询，见 `crates/calm-server/src/operation/mod.rs:82-87`、`crates/calm-server/src/task_context.rs:620-625`。
- migration 自身还有一次整表显式搬运 SELECT，见 `crates/calm-truth/migrations/0058_tasks_kind_claude.sql:36-47`。
- truth 内测试显式 SELECT 全集在 `crates/calm-truth/src/db/sqlite/task_context_migration_tests.rs:74`、`:133`、`:139`、`:185` 和 `crates/calm-truth/src/db/sqlite/task_projection.rs:1246`、`:1269`、`:1296`。
- server 顶层测试显式 SELECT 全集在 `crates/calm-server/tests/dispatcher.rs:545`、`crates/calm-server/tests/scheduler.rs:2116`、`:2858`、`:3268`、`:3557`、`:3630`、`:3672`、`:3696`、`:3707`、`:3738`、`:3828`、`:4067`、`:4118`、`crates/calm-server/tests/codex_forge_e2e.rs:2665`。
- cases 测试显式 SELECT 全集在 `crates/calm-server/tests/cases/mcp_plan.rs:293`、`migration_0068_projection_policy.rs:58`、`:100`、`replay_fixtures.rs:738`、`wave_projection_policy_patch.rs:219`，以及 `task_projection_acceptance.rs:198`、`:299`、`:476`、`:499`、`:522`、`:559`、`:585`、`:814`、`:897`、`:1061`、`:1188`、`:1330`、`:1575`。
- 另有一个 route 内 fork 单测的 scalar SELECT 在 `crates/calm-server/src/routes/waves.rs:1855`；以上完整 `Task` 五处是 sqlx 运行时“列数/字段缺失”风险面，其余是有意的定向 tuple/scalar reader，依据其各自 `query_as/query_scalar` 形状见 `crates/calm-truth/src/db/sqlite/read.rs:406-407`、`crates/calm-server/src/operation/task_verify_adapter.rs:756-763`。

### 6. 3b/3b′ 列先例与 pending 守卫

- 3b 的 `declared_by/origin` 用 `ALTER TABLE + NOT NULL DEFAULT + 显式 backfill`，并给 waves 加两个策略列，见 `crates/calm-truth/migrations/0068_projection_policy_columns.sql:2-13`；二者进入 `Task` 与 `TASK_COLUMNS`，见 `crates/calm-truth/src/model.rs:406-407`、`crates/calm-truth/src/db/sqlite/task.rs:19-23`。
- 3b′ 的 `decl_ready/decl_released_by_user/context_verify_failures` 刻意不进 `TASK_COLUMNS/Task`，只供定向 reader 使用，这一意图和列定义写在 `crates/calm-truth/migrations/0070_task_context_withdrawal_and_verify.sql:1-12`；`context_closure_truncated` 同样由定向 context reader取，见 `crates/calm-truth/src/db/sqlite/read.rs:438-473`。
- task 块投影 DTO `TaskDeclaration` 当前有 provenance/readiness 但没有 `spawn`，见 `crates/calm-types/src/report_blocks/tasks.rs:97-119`；投影构造只读取这些字段，见 `crates/calm-types/src/report_blocks/tasks.rs:510-579`，所以今天合法的 `spawn` 会被静默丢弃。
- 投影 INSERT/UPSERT 在 `crates/calm-truth/src/db/sqlite/task_projection.rs:977-985`，更新守卫正是 `WHERE tasks.status='pending'`；通用 pending 更新的同类形状在 `crates/calm-truth/src/db/sqlite/task.rs:93-105`。
- 最省事先例应照抄 `declared_by` 而不是 `decl_ready`：`spawn` 是 claim 后即时与恢复路由都要读的冻结行字段，因此须同时进入 migration、`TaskDeclaration`、投影 SQL、`Task`、`TASK_COLUMNS`，依据现有冻结重读返回完整 `Task` 于 `crates/calm-server/src/scheduler/mod.rs:804-812`。

## C. claim 与派发路由

### 7. claim SQL 与冻结重读

- `TASK_CLAIM_PENDING_SQL` 位于 `crates/calm-truth/src/db/sqlite/task.rs:223-229`，执行 `pending -> dispatched`，同时写 `claim_context_json/context_closure_truncated/updated_at_ms`，且 `WHERE id=? AND status='pending'`。
- `task_claim_pending_tx` 位于 `crates/calm-truth/src/db/sqlite/task.rs:231-263`，返回 `rows_affected: u64`，胜出时同事务重建 `task_ref_index`。
- scheduler 的 `claim_task` 返回 `Result<Option<Task>>`，定义于 `crates/calm-server/src/scheduler/mod.rs:675-698`；claim 成功后在同事务 `task_get_tx` 重读冻结行于 `crates/calm-server/src/scheduler/mod.rs:804-812`，最终映射为 `Some(frozen)` 于 `crates/calm-server/src/scheduler/mod.rs:920-925`。

### 8. 即时派发与崩溃恢复

- 即时链是 `schedule_pass -> dispatch_task -> claim_task -> drive_spawn`，入口见 `crates/calm-server/src/scheduler/mod.rs:548-584`、函数见 `crates/calm-server/src/scheduler/mod.rs:615-647`、`crates/calm-server/src/scheduler/mod.rs:675-723`、`crates/calm-server/src/scheduler/mod.rs:940-971`。
- `drive_spawn` 以冻结 `Task` 调纯函数 `build_worker_payload`，当前仅按 `TaskKind` 选 `codex-worker/claude-worker/terminal-worker`，见 `crates/calm-server/src/scheduler/mod.rs:207-274`、`crates/calm-server/src/scheduler/mod.rs:947-960`。
- `OperationRuntime::submit` 先按 kind+idem 查重、校验 hash、插 op 并 drive，见 `crates/calm-server/src/operation/driver.rs:108-134`；operation prepare 在独立 immediate tx 中调用 adapter 并把 op 推到 `tx_committed`，见 `crates/calm-server/src/operation/repo_sqlite.rs:277-330`。
- 三种 worker adapter 随后创建 worker card/terminal/session：Codex 在 `crates/calm-server/src/operation/codex_adapter/mod.rs:766-795`，Claude 在 `crates/calm-server/src/operation/claude_adapter/mod.rs:768-808`，Terminal 在 `crates/calm-server/src/operation/terminal_adapter.rs:576-595`；底层 composite helpers 分别在 `crates/calm-truth/src/db/sqlite/card_composite.rs:229`、`:508`、`:34`。
- 崩溃恢复由 `sweep_reconcile` 扫 `tasks_nonterminal`，把 `Dispatched` 交给 `resume_dispatched`，见 `crates/calm-server/src/scheduler/mod.rs:1240-1270`；`resume_dispatched` 只读冻结 `Task`、其 `Wave`，再调用同一个 `drive_spawn`，见 `crates/calm-server/src/scheduler/mod.rs:1596-1631`。
- 恢复路由今天不会重新读取当前报告块：恢复输入来自 `tasks_nonterminal` 的 `TASK_COLUMNS` 行，见 `crates/calm-truth/src/db/sqlite/read.rs:423-429`，并直接进入 `drive_spawn`，见 `crates/calm-server/src/scheduler/mod.rs:1624-1631`；报告 payload/docRev 的读取只发生在 claim 前 context fence，见 `crates/calm-server/src/scheduler/mod.rs:744-774`。

### 9. 幂等

- scheduler 同时把 `task.id` 写进 worker payload，并用它作为 `OperationKey.idempotency_key`，见 `crates/calm-server/src/scheduler/mod.rs:210-264`、`crates/calm-server/src/scheduler/mod.rs:953-960`；`OperationKey` 形状在 `crates/calm-server/src/operation/mod.rs:105-109`。
- DB 对 `(kind,idempotency_key)` 建 partial unique index，见 `crates/calm-truth/migrations/0029_operations.sql:36-38`（0042 重建后重新建立于 `crates/calm-truth/migrations/0042_operations_parked.sql:96-98`）；插入前/冲突后均比较 payload hash，见 `crates/calm-server/src/operation/repo_sqlite.rs:82-138`。
- 子 wave 最可复用的形状是新增 child-wave operation kind，以冻结 `task.id` 为 idem、冻结行生成稳定 payload hash，并在 adapter `prepare_tx` 原子创建子 wave 骨架；这样 crash 前后 submit 返回同一 op/child，依据现成 submit 去重语义 `crates/calm-server/src/operation/driver.rs:108-124` 和原子 prepare 语义 `crates/calm-server/src/operation/repo_sqlite.rs:277-330`。

## D. 预算

### 10. ready/capacity 与 spec ceiling

- scheduler 运行容量由 `wave_capacity` 计算，且 occupied 只数 `Dispatched|Running|Verifying`，见 `crates/calm-server/src/scheduler/mod.rs:172-182`；`compute_ready` 在依赖满足后按容量截断，见 `crates/calm-server/src/scheduler/mod.rs:185-202`。
- `spec_task_ceiling` 默认常数 32 在 `crates/calm-truth/src/db/sqlite/task_projection.rs:17`；准入函数是 `evaluate_schedulability` 于 `crates/calm-truth/src/db/sqlite/task_projection.rs:518-540`。
- ceiling 的 occupied 明确只数 `declared_by='spec' && origin='block'` 的 inflight 行，SQL inflight 状态限定和计数分别见 `crates/calm-truth/src/db/sqlite/task_projection.rs:414-420`、`crates/calm-truth/src/db/sqlite/task_projection.rs:535-540`；按候选顺序 take capacity 并发诊断于 `crates/calm-truth/src/db/sqlite/task_projection.rs:778-821`。

### 11. waves schema/model 与 3b 模板清单

- `waves` 基表在 `crates/calm-truth/migrations/0001_init.sql:20-29`；后续列依次见 lifecycle `0012_waves_lifecycle.sql:33`、pinned `0021_waves_pinned_at.sql:1`、cwd/terminal `0018_wave_cwd_terminal_at.sql:48-49`、task policy `0041_tasks.sql:38-40`、root session `0045_worker_sessions.sql:49`、workflow id `0059_waves_workflow_id.sql:6`、workflow input `0061_waves_workflow_input.sql:8`、purpose `0064_waves_launchpad_purpose.sql:1-4`、projection policy `0068_projection_policy_columns.sql:12-13`。
- 公共 `Wave` 在 `crates/calm-types/src/model.rs:370-420`，DB DTO `NewWave/WavePatch` 在 `crates/calm-truth/src/model.rs:93-145`、`crates/calm-truth/src/model.rs:147-184`，内部 `WaveRow` 在 `crates/calm-truth/src/db/rows.rs:81-98`。
- 3b 的完整后端模板是：migration 加列 `crates/calm-truth/migrations/0068_projection_policy_columns.sql:10-13`；只给 `WavePatch` 加双层 Option、刻意不进 `Wave/WaveRow`，见 `crates/calm-truth/src/model.rs:169-177` 和 migration 注释 `crates/calm-truth/migrations/0068_projection_policy_columns.sql:10-11`；`wave_update_tx` 定向写列，见 `crates/calm-truth/src/db/sqlite/wave.rs:188-201`。
- route 侧再加 user-only gate、值校验、非空 patch 判断和投影 rebuild，见 `crates/calm-server/src/routes/waves.rs:1164-1169`、`crates/calm-server/src/routes/waves.rs:1215-1242`、`crates/calm-server/src/routes/waves.rs:1253-1262`；projection 侧加状态读取/默认值/准入/诊断，见 `crates/calm-truth/src/db/sqlite/task_projection.rs:371-454`、`crates/calm-truth/src/db/sqlite/task_projection.rs:518-540`、`crates/calm-truth/src/db/sqlite/task_projection.rs:778-821`。
- 对应契约与测试清单是 diagnostic 代码/消息 `crates/calm-types/src/report_blocks/tasks.rs:38`、`:58`、`:220`；PATCH 集成测试 `crates/calm-server/tests/cases/wave_projection_policy_patch.rs:91-219`；projection/migration 验收 `crates/calm-server/tests/cases/task_projection_acceptance.rs:1015-1018`、`:1513`、`:1541`、`:1564`、`crates/calm-server/tests/cases/migration_0068_projection_policy.rs:46-100`；生成 API `web/src/api/generated.ts:2208`、`:2229` 和 `web/src/api/openapi.json:6709`、`:6749`；UI 消费于 `web/src/pages/WaveReportPage.tsx:87-104`、`web/src/pages/report-blocks/task.tsx:17-60`，对应 UI 测试在 `web/src/pages/task-actions.test.ts:25-46`、`web/src/pages/WaveReportPage.test.tsx:1009-1018`、`web/src/pages/report-blocks/report-blocks.test.tsx:795-796`。
- 对本片的模板判断：`parent_wave_id/tree_task_budget` 是读取与创建都需要的结构列，不能完全照搬“刻意不进 Wave”的策略列做法；至少创建用 DTO/tx insert、树查询的 targeted row 必须覆盖，依据常规 INSERT 的固定列清单 `crates/calm-truth/src/db/sqlite/wave.rs:48-67` 和 Today 的另一份固定清单 `crates/calm-server/src/routes/today.rs:89-95`。

### 12. migration 序号

- 已发布最大序号是 `0070_task_context_withdrawal_and_verify.sql`，其首行即标识当前 slice，见 `crates/calm-truth/migrations/0070_task_context_withdrawal_and_verify.sql:1`；因此当前树中 `0071_` 未被占用，下一片可用 `0071_`，相邻前序见 `crates/calm-truth/migrations/0069_clear_pending_context_stale.sql:1`、`crates/calm-truth/migrations/0070_task_context_withdrawal_and_verify.sql:1`。

## E. 反链与块写入

### 13. 5b source-aware `neige://` 重写

- helper 是 `rewrite_wave_links(markdown, source_wave_id, target_wave_id, copied_block_ids) -> Result<String, Vec<UnsafeWaveLink>>`，定义于 `crates/calm-types/src/report_links.rs:98-104`；它按 Markdown source range 重写而不是拿渲染文本 offset 改源串，说明见 `crates/calm-types/src/report_links.rs:91-97`。
- 裸 destination 对应 `rewrite_wave_destination(&str, &str, &str, &HashSet<String>) -> String`，见 `crates/calm-types/src/report_links.rs:213-228`；fork 对 prose markdown、task goal/acceptance/refs 的调用在 `crates/calm-server/src/routes/waves.rs:966-1029`。

### 14. 报告块生产写路径与 child 反链

- 统一生产写内核是 `ReportDocOp::{Replace,WriteMarkdown,UpsertBlock,MoveBlock,DeleteBlock}`，定义于 `crates/calm-server/src/wave_report.rs:173-225`、应用并统一过 `guard_task_declarations` 于 `crates/calm-server/src/wave_report.rs:239-383`，最终由 `persist_report_with_shadow` 同事务写 card CRDT、投影 tasks、发事件于 `crates/calm-server/src/wave_report.rs:567-610`、`crates/calm-server/src/wave_report.rs:681-770`。
- REST 的块 create/update/delete/move 四条路都进共同 `commit -> persist_report_with_shadow`，见 `crates/calm-server/src/routes/wave_report_blocks.rs:100-128`、`:132-260`；REST 整文写走 `persist_report`，见 `crates/calm-server/src/routes/waves.rs:1684-1702`。
- spec MCP 的整文 `Replace`、typed block 三操作和 marker-aware `WriteMarkdown` 都经 `CardDecisionSink::commit_report_op`，见 `crates/calm-server/src/mcp_server/tools/wave_report.rs:533-552`、`crates/calm-server/src/mcp_server/tools/wave_report_blocks.rs:208-325`、`crates/calm-server/src/decision_sink.rs:416-433`。
- 内核当前唯一特殊生产写是 fork：直接 `card_update_with_crdt_tx + project_tasks_tx`，见 `crates/calm-server/src/routes/waves.rs:938-963`，并先走专用 fork guard，见 `crates/calm-server/src/routes/waves.rs:1072`；通用 persist 的 `EditAuthor::Kernel` 明注明“today 无 caller”，见 `crates/calm-server/src/wave_report.rs:487-493`。
- 给既存 spec task 追加 child 反链，最省事形状是新增一个 Kernel 调用者：重读 block/rev，向 `goal` 或 `acceptance` 追加 Markdown `[child](neige://wave/<id>)`，再用 `ReportDocOp::UpsertBlock{id:Some,if_rev}` 走统一 persist；现成替换形状见 `crates/calm-server/src/routes/wave_report_blocks.rs:183-202`，guard 允许非用户修改 spec-owned 既存块但禁止修改 user-owned 块，见 `crates/calm-server/src/wave_report_edit_guard.rs:94-126`、`:168-177`。
- 不应写进 task `refs`：其校验要求必须带 `#b_xxxx`，见 `crates/calm-types/src/report_blocks/kinds.rs:230-235`；Markdown 全 wave 链接会被 backlink 扫描器记录为 `dst_block_id=None`，见 `crates/calm-server/src/report_backlinks.rs:174-194`、`crates/calm-server/src/report_backlinks.rs:521-529`。

### 15. fork_guard 公开面与“恰好一个”测试

- `routes::waves` 以私有 `mod fork_guard` 声明并私用导入，见 `crates/calm-server/src/routes/waves.rs:81-83`；唯一入口 `guard_forked_blocks` 的可见性是 `pub(in crate::routes::waves)`，实现函数保持 module-private，见 `crates/calm-server/src/routes/waves/fork_guard.rs:13-20`。
- 本模块行为单测在 `crates/calm-server/src/routes/waves/fork_guard.rs:53-62`；结构枚举测试在 `crates/calm-server/tests/cases/fork_guard_exemption_invariant.rs:6-9`，它校验可见性、收集 export 后执行 `assert_eq!(exported_entries, expected_entries, ...)` 于 `:21-39`，并继续断言 impl 私有且无 exemption enum 于 `:41-53`。

## F. 现有 spawn 处理

### 16. task 块字段 `spawn` 的全部位置

- Rust schema 允许 `spawn` 的位置是 `TASK_FIELDS`，见 `crates/calm-types/src/report_blocks/kinds.rs:119-137`；值校验只接受 `in-wave|sub-wave`，见 `crates/calm-types/src/report_blocks/kinds.rs:252-255`，正反测试在 `crates/calm-types/src/report_blocks/kinds_tests.rs:225`、`:244`。
- MCP task-block JSON Schema 把 tombstone 与 spawn 做互斥，并给 spawn enum/default，见 `crates/calm-server/src/mcp_server/tools/wave_report_blocks/contracts.rs:190-197`、`:230-234`。
- Web Zod/TS 来源是 `taskBlockSchema` 的 optional enum，见 `web/src/cards/builtins/wave-report.tsx:130-150`；`TaskBlockPayload` 是该 schema 的推导类型，见 `web/src/cards/builtins/wave-report.tsx:169`。
- 当前 oracle 没有另写 spawn 契约，而是声明 wave-report schema 以 web 源为准，见 `docs/oracle/pages-shared.yaml:444-445`；`fe/**` 与 golden 中也没有第二份 task-spawn 定义，因而可执行契约的穷举面就是上述 Rust/MCP/Web 三处，参见 MCP 的完整 properties 收口 `crates/calm-server/src/mcp_server/tools/wave_report_blocks/contracts.rs:205-235`。
- 架构文档中的 task-field 命中为：示例与裁决 `docs/architecture/985-doc-as-plan.md:162`、`:215-216`；哈希排除及其临时性 `:494`、`:519`、`:1331`；切片 6 要求和重裁提醒 `:1519-1522`、`:1553`、`:1918`、`:1963`；字段总表 `:1663`。
- 当前执行链没有消费 spawn：`TaskDeclaration` 不含它，见 `crates/calm-types/src/report_blocks/tasks.rs:97-119`；scheduler 只按 `Task.kind` 分支，见 `crates/calm-server/src/scheduler/mod.rs:210-274`；根 task context hash 还明确把 spawn 放在 excluded 集，见 `crates/calm-server/src/task_context.rs:36-55`，对应测试证明 root hash 不变于 `crates/calm-server/src/task_context.rs:840-865`。
- manifest plan template 也明确排除 spawn，见 `crates/calm-server/src/mcp_server/tools/plan.rs:915-932`；仓内其它 `tokio::spawn`、进程 spawn、`fail_spawn` 命中均不是 task 块字段，task 字段的合法字面值穷举由 validator 自身固定在 `crates/calm-types/src/report_blocks/kinds.rs:252-255`。

## 施工估算

- 生产代码估计 **350–500 行**：wave-in-tx 抽取/child operation、树预算查询、Task spawn 冻结投影、scheduler 双路由、Kernel 反链写入；主要现有接缝是 `crates/calm-server/src/routes/waves.rs:631-861`、`crates/calm-server/src/scheduler/mod.rs:615-971`、`crates/calm-truth/src/db/sqlite/task_projection.rs:839-985`。
- 测试估计 **450–700 行**：覆盖 claim 前归一化、claim 后报告变化、三类 crash 窗口、operation 幂等、深度 0/3/4、跨 sibling 树预算竞争、反链与 Today/普通创建回归；现有同类测试规模入口见 `crates/calm-server/tests/scheduler.rs:2858-2858`、`crates/calm-server/tests/cases/task_projection_acceptance.rs:1513-1575`、`crates/calm-server/tests/cases/wave_report_fork.rs:145-145`。
- migration 估计 **15–30 行**：给 waves 两列/索引或约束、tasks.spawn 默认与 backfill；先例分别见 `crates/calm-truth/migrations/0068_projection_policy_columns.sql:2-13`、`crates/calm-truth/migrations/0070_task_context_withdrawal_and_verify.sql:5-12`。
- 文档/生成契约估计 **20–50 行**：主要是 OpenAPI/TS 生成差异与架构状态更新，现有生成落点见 `web/src/api/generated.ts:2208-2229`、`web/src/api/openapi.json:6709-6749`，原设计切片预算约 700 行见 `docs/architecture/985-doc-as-plan.md:1519-1523`。

最容易出静默错误的三处：

1. **合法 spawn 在投影时消失或完整 `Task` SELECT 漏列**：schema 接受它，但 `TaskDeclaration` 无字段且五处 `TASK_COLUMNS` reader 运行时解码，见 `crates/calm-types/src/report_blocks/kinds.rs:252-255`、`crates/calm-types/src/report_blocks/tasks.rs:97-119`、`crates/calm-truth/src/db/sqlite/read.rs:402-429`。
2. **恢复改成重读报告、或 spawn 仍被根哈希排除**：今天 crash 恢复只信冻结行，但 hash 明确忽略 spawn，见 `crates/calm-server/src/scheduler/mod.rs:1596-1631`、`crates/calm-server/src/task_context.rs:36-55`。
3. **树预算计数谓词或原子性写错**：现有 ceiling 只数 spec+block 的 inflight，而树预算要求递归子树的 spec 非终结量（包括 pending），不能直接复用当前 occupied；现有谓词见 `crates/calm-truth/src/db/sqlite/task_projection.rs:414-420`、`:535-540`，树预算不变量明确写于 `docs/architecture/985-doc-as-plan.md:1992`，检查与 child INSERT 必须共用 operation prepare 事务的形状见 `crates/calm-server/src/operation/repo_sqlite.rs:277-330`。
