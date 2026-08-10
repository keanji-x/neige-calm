# #985 切片 6 PR-A 实现评审 r3（codex）

对象：`9d30006a..83e98325`。结论：**NO**。两阶段方向正确，但重启会先恢复已标删 wave
的 harness，且 WS terminal 懒重连绕过 operation 串行点，二者都能在删除已开始后重新起活。
命令均保持 `NEIGE_CODEX_BIN` 未设置、`CARGO_BUILD_JOBS=6`，PATH 含指定 `.local-bin`。

## BLOCKER

### B1. 重启先复活 harness，约 30 秒后才恢复删除

- **结论**：生产启动没有同步执行 `resume_marked_wave_deletions`；`AppState` 只 spawn sweeper，
  sweeper 明确跳过首 tick，而 main 在此前已经恢复 harness。恢复查询只排除 terminal lifecycle，
  不排除 `wave_deletions`（`crates/calm-server/src/state.rs:1142`、`:1148`；
  `crates/calm-server/src/terminal_sweeper.rs:101`、`:104`、`:109`；
  `crates/calm-server/src/main.rs:48`、`:55`；
  `crates/calm-truth/src/db/sqlite/session_projection.rs:495`、`:500`、`:510`）。
- **触发条件 / 交错**：① phase 1 提交 marker；②进程在 teardown 前崩溃；③重启后
  `boot_harnesses` 查到 active session 并 `spawn_recovered_harness`；④该 agent 最长约 30 秒继续
  外部工作/写报告；⑤首个周期 sweep 才删除（`crates/calm-server/src/harness/mod.rs:320`、`:323`；
  `crates/calm-server/src/terminal_sweeper.rs:83`、`:123`）。
- **实际验证**：临时在
  `spec_harness_boot_recovery::boot_recovery_respawns_harness_with_snapshot` 的生产恢复调用前写入
  marker；测试仍 **PASS 0.126s**，`recovered == 1`。另把 sweep 周期改成 86400s，
  `durable_wave_delete_marker_refuses_new_resources_and_restart_sweep_finishes` 仍 **PASS 0.131s**，
  因测试手工调用 `sweep`，未覆盖生产启动接线（`crates/calm-server/tests/cases/terminal_lifecycle.rs:588`、`:592`）。
- **最小修法**：在任何 harness/supervisor/operation/scheduler boot recovery 之前同步恢复 marked
  deletions；同时给 harness recovery SQL 加 `NOT EXISTS wave_deletions` 作为纵深防御，并加真实
  main 启动顺序行为测试（`crates/calm-server/src/main.rs:50`、`:55`、`:64`、`:80`）。

### B2. WS terminal 懒重连不受 marker/串行点约束，可在清理后重新挂回 renderer

- **结论**：WS 先读 terminal，命中 registry 立即返回 Alive；否则先 probe supervisor，再调用
  `spawn_terminal_for`。该 helper 收到 repo 却完全不用它，因此两处都不检查 deleting；这条路径也
  不持 `drive_mutex`（`crates/calm-server/src/ws/terminal.rs:114`、`:124`、`:148`、`:154`；
  `crates/calm-server/src/routes/terminal.rs:80`、`:83`、`:117`）。
- **触发条件 / 交错**：① WS probe 得到 live；② DELETE marker+snapshot 提交并释放 operation fence；
  ③ teardown kill/drop 该 terminal；④ WS 继续 `EnsureProc`，在 cleanup 后重新建 registry entry/进程；
  ⑤ phase 2 删除 row/wave，新进程及 entry 无 owner（删除 fence 仅围住快照，清理前就释放：
  `crates/calm-server/src/routes/waves.rs:1484`、`:1486`；快照/清理在 `:1382`、`:1420`）。
  `EnsureProc` 确实可返回新 `Spawned{pid}`，而 row 已删时 PID 回写只 warn（
  `crates/calm-server/src/terminal_renderer/mod.rs:497`、`:526`、`:528`、`:534`）。
- **实际验证**：临时让
  `ws_resolve_live_renderer::live_renderer_entry_returned_when_registry_has_entry` 在 resolve 前提交
  marker；它仍返回 Alive，**PASS 0.108s**。现有重启删除测试只检查预先快照到的 PID 被杀，
  没有并发 WS（`crates/calm-server/tests/cases/terminal_lifecycle.rs:520`、`:595`）。
- **最小修法**：WS resolve/reattach 与 delete marker 使用同一短串行门；在门内事务性复核
  terminal 所属 wave 未标删，并让“probe 后标删、cleanup 后 reattach”确定性交错测试必须拒绝/红。

## MAJOR

### M1. 半删 wave 对读端仍伪装成 live，永久失败会形成不可诊断 zombie

- **结论**：REST list/get/window 都不滤 marker，MCP tool visibility 也把 `wave_get(Some)` 当 live；
  report 写仍可更新 card 并投影/新建 task。只有 scheduler claim 把 marked wave 当不存在。因此失败
  持续时，UI/MCP 看见正常 wave，但任务永不起活（`crates/calm-truth/src/db/sqlite/read.rs:120`、
  `:131`、`:155`；`crates/calm-server/src/mcp_server/tool_visibility.rs:89`、`:109`；
  `crates/calm-server/src/wave_report.rs:732`、`:738`；
  `crates/calm-truth/src/db/sqlite/task.rs:203`、`:210`）。
- **触发条件 / 交错**：① marker 提交；② teardown 的 harness `persist_snapshot` 或 phase 2 DB 写
  持续失败；③ recovery 只 warn 后留到下一 sweep；④固定 30s 重试，无 per-wave backoff、失败计数、
  状态 API 或人工收敛口，故同一确定性错误可永久半删（`crates/calm-server/src/routes/waves.rs:1423`、
  `:1425`、`:1510`、`:1519`；`crates/calm-server/src/terminal_sweeper.rs:83`、`:110`）。
  `harness.shutdown` 在真正 interrupt 前先持久化 snapshot，故持久化错误正是可达重试错误（
  `crates/calm-server/src/harness/run_loop.rs:184`、`:187`、`:191`）。
- **实际验证**：基线
  `durable_wave_delete_marker_refuses_new_resources_and_restart_sweep_finishes` **PASS 0.119s**；
  但 86400s 周期变异仍 PASS，证明它只测手工成功恢复，不约束启动、失败或退避。
- **最小修法**：明确读语义：列表隐藏 deleting，detail/MCP 返回 typed deleting/409；所有 mutation
  （含 report projection）拒绝。为恢复保存 attempt/last_error/next_retry，指数退避并导出 health；
  永久错误必须可见且有管理员 retry/force-cleanup 路径。

### M2. 新状态机的关键准入与 operation 分支仍是假绿覆盖

- **结论**：operation 有 TxCommitted/AppServerInteract/SpawnStarted 三个外部 IO 站点，但唯一
  deleting 行为测试只走第一个；marker 测试只验证 card/terminal/child，没有验证 migration 中仅靠
  trigger 承重的 lease，以及 session（`crates/calm-server/src/operation/driver.rs:456`、`:524`、`:576`；
  `crates/calm-server/tests/scheduler.rs:6202`；
  `crates/calm-server/tests/cases/terminal_lifecycle.rs:538`、`:549`、`:577`；
  `crates/calm-truth/migrations/0072_wave_deletions.sql:46`、`:53`）。
- **触发条件**：未来删坏 AppServerInteract/SpawnStarted marker check，或删坏 session/lease trigger；
  当前声称覆盖 deleting 的测试仍绿。lease writer直接 INSERT 后创建目录，Rust 无前置 typed guard，
  trigger 是唯一 phase-1 拒绝点（`crates/calm-server/src/operation/workspace_lease/mod.rs:167`、`:180`、`:205`）。
- **实际验证**：变异 `fail_if_wave_deleting...` 仅让 TxCommitted 生效，
  `deleting_wave_fails_queued_bootstrap_before_external_io` **PASS 0.135s**；再把 session+lease trigger
  改成 `WHEN 0`，`durable_wave_delete_marker_refuses_new_resources_and_restart_sweep_finishes`
  **PASS 0.153s**。
- **最小修法**：三 phase 各一条从 durable op row 驱动到外部 adapter 的负例，并分别对
  card/terminal/session/lease/child INSERT 做 marker 后行为测试；lease 另加 typed in-tx guard。

## MINOR

### m1. `wave_deletions` 未登记权威附录 C 四格

- **结论 / 证据**：附录要求每个新载体登记且“少一格即缺口”，但 C.1/C.2/C.3 均无
  `wave_deletions`（`docs/architecture/985-doc-as-plan.md:1851`、`:1853`、`:1855`、
  `:1857`、`:1876`、`:1885`）。
- **实际验证**：`rg -n wave_deletions docs/architecture/985-doc-as-plan.md` 为 **0 matches**。
- **最小修法**：登记：载体=`wave_deletions(wave_id,requested_at_ms)`；谁写=DELETE phase 1；
  rebuild=运行期 intent，不由事件/投影重放；migration backfill=空（升级前无可达 deleting intent）。

## 新增的两阶段删除机制有没有引入新问题

**有：B1、B2、M1。** 中间态目前是：REST list/detail 与 MCP **live**；报告快照/投影仍可写
task；scheduler **inert**；新 card/terminal/session/lease/child 理论上由 typed guard/trigger 拒绝
（`crates/calm-truth/migrations/0072_wave_deletions.sql:13`、`:29`、`:36`、`:46`、`:53`），但 WS
reattach 可起活。operation 串行点在清理前释放本身对 operation 安全，因为三个外部 phase 都复核
marker（`crates/calm-server/src/operation/driver.rs:459`、`:527`、`:579`）；问题是 B2 的非-operation
路径和 B1 的启动恢复完全不参加该串行协议。

恢复不是“启动+周期”：只有周期，首轮约 30s；逐 wave 失败互不阻塞是对的，但没有失败退避/状态，
确定性失败会永久 marked（`crates/calm-server/src/routes/waves.rs:1495`、`:1509`、`:1519`）。

## 迁移、旧问题与 #21c 复核

- `0072` 只 CREATE 新表/trigger，additive；存量无 intent，正确 backfill 是空。`0071` 先加列/索引并
  显式 backfill spawn（`crates/calm-truth/migrations/0071_sub_wave_tree.sql:1`、`:4`、`:10`、`:18`；
  `crates/calm-truth/migrations/0072_wave_deletions.sql:5`、`:13`）。
- replay harness 枚举每个版本，先 stage 再由生产 MIGRATOR 补到 head并比 schema；已覆盖 71→72
  及 72 no-op replay（`crates/calm-server/tests/cases/migration_replay_harness.rs:48`、`:56`、`:61`、
  `:69`、`:73`）。实跑 `synthetic_fixture_replays_from_every_version`：**PASS 104.676s**。
- 上轮 child-Done 判定现在恰有两个生产站点且各有独立行为测试，没有第三处同类判定：
  `crates/calm-server/src/scheduler/mod.rs:367`、`:396`；测试在
  `crates/calm-server/tests/scheduler.rs:6115`、`:6156`，两条均 PASS。
- #21c 两性质已并存：真实 adapter 后全表零跨-cove 边（
  `crates/calm-server/src/operation/child_wave_adapter.rs:657`、`:688`），手工毒边删除 cove 失败
  （`crates/calm-truth/src/db/sqlite/sub_wave_tree_tests.rs:108`、`:117`、`:132`）；两条均 PASS。

## 我自己设计的断言有效性验证里，哪几条发现了问题

1. **marked harness boot diagnostic：发现 B1**，marker 后仍 recovered=1、PASS。
2. **marked WS diagnostic：发现 B2**，marker 后仍 Alive、PASS。
3. **周期 30s→86400s：发现启动接线假绿**，手工 sweep 测试仍 PASS。
4. **只保留 TxCommitted guard：发现 M2**，唯一 operation deleting 测试仍 PASS。
5. **禁用 session/lease triggers：发现 M2**，durable marker 测试仍 PASS。

所有诊断/变异后均执行 `git checkout -- .`；恢复态定向门 **7/7 passed 0.226s**。

## 可以合入了吗

**NO。** 最小阻塞集：启动时先同步恢复删除并禁止 marked harness 恢复（B1）；把 WS
resolve/reattach 纳入 marker 串行协议（B2）。M1/M2 应随同补齐；m1 单独不阻塞。

最终 `git status --short`：

```text
?? docs/_985-s6-impl-review-r3-codex.md
```
