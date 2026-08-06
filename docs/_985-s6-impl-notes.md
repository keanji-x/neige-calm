# #985 切片 6 PR-A 实现说明

## 门结果

所有最终门均在 `NEIGE_CODEX_BIN` 未设置、`CARGO_BUILD_JOBS=6` 下执行；Rust 命令的 PATH 含 `/mnt/data2/kenji/neige-calm/.local-bin`，web/fe 使用 Node 24.4.1。

| 门 | 实际结果 |
|---|---|
| `cargo fmt --all --check` | 修复轮 1 最终原命令 exit 0。 |
| `cargo clippy --workspace --all-targets --features calm-server/codex-e2e -- -D warnings` | 修复轮 1 首跑红在新测试元组的 `clippy::type-complexity`；抽本地类型别名后原命令复跑 exit 0，0 warnings / 0 errors。 |
| `cargo nextest run --workspace --locked --features calm-server/codex-e2e --profile ci` | 修复轮 1：**3339 passed / 0 failed / 89 skipped**，31.410s（编译另 1m21s）。 |
| web `npm run gen:api` | exit 0；生成测试 49 + 15 + 1 = **65 passed / 0 failed**，其余生成 crate 目标均为 0 run、仅 filtered。 |
| web 生成物 `git diff --exit-code` | 指定 5 组路径无漂移，exit 0。 |
| web `npm run build` | exit 0；849 modules transformed。保留既有 CSS `::highlight` 与 chunk-size warning，无错误。 |
| web `npm run test` | **85 files passed；1227 tests passed / 0 failed；0 type errors**。 |
| fe `npm run lint` | exit 0；ownership 74 entries 完整；dependency cruise 102 modules / 232 dependencies，0 violations。 |
| fe `npm run build` | exit 0；14 modules transformed。 |
| fe `npm run test` | **61 files passed / 1 skipped；758 tests passed / 1 skipped**；`test:wire` 与 `test:mock-drift` 均 exit 0。没有漂移，因此未运行 `mock:generate`。 |

针对本轮原始失败另跑过精确验证：`migration_backfills_preexisting_task_and_block_declaration_adopts_it` 为 1 passed / 0 failed / 2289 skipped。

真实 Codex e2e 没有启动：命令均显式 `env -u NEIGE_CODEX_BIN`；这些入口只解析该变量，未解析时走 `skip!`，另有显式 `#[ignore]` 用例。全门没有启动嵌套 app-server。

## 设计缺口与裁决

1. **0068 测试的后半应跑到哪个 schema。** 前半只钉 0068 backfill；后半调用当前生产投影。生产投影同时写 0070 的 `decl_*` 和 0071 的 `spawn`，所以 fixture 必须按真实顺序再执行 0069、0070、0071。只跑 0070 会缺 `spawn`；只跑 0071 会缺 `decl_ready`。没有把生产 `spawn` 写入改成条件式。
2. **§7 数量漂移。** 设计表实际有 33 个编号行（3a/3b/3c、5b、13b–13e、14b、21b/21c 等均独立成行），任务描述称 30。裁决为交付 33 行，不按错误计数删项。
3. **#11 的两个内层判定不是独立行为承重点。** 实测删除专用臂或删除 predicate 的 spawn 排除分别被另一层遮蔽而仍绿；只有把更早的 sub-wave 臂直接接到 timeout 收割器才红。保留这些仍绿结果，不声称“三站点各自可变异”。
4. **#21c 的指定 tripwire 已在修复轮 1 收敛。** 测试移到 child adapter 的真实写路径：创建第二 cove、提交真实 child operation，再断言全表无跨-cove 边且无关 cove 可删。把 adapter 的 cove reader 改成第二 cove 后，该测试自身红。
5. **read-state 边界。** `child_wave_id` 是 claim 后状态，进入 DTO；`spawn` 是 claim 前冻结输入，保持不进 DTO。#22 用序列化 JSON 断言，而不是结构体字段恒真。

## 没做到或只做到部分变异证明的验收

没有整条生产验收“未实现”，但以下子断言没有完成设计要求的独立变异证明，不能算成已执行：

- #5：只实际变异 goal；acceptance、context、cwd 的三个逐字段 mutant 未跑。
- #6：跑了 `>=`→`>`；“把直接父写成 root”的独立 mutant 未跑。
- #8：跑了零行当根；未单独构造“非环、超深且无根”的毒数据 mutant。
- #12：worker id 与 deadline 同时改坏，测试先红在 worker id；deadline-only mutant 未跑。
- #13：跑了无条件 done；删除 `TaskCompleted` 事件的独立 mutant 未跑（正向测试确实断言事件数与 gate attempt）。
- #13e：正向测试覆盖 create/bootstrap × Failed/Stuck 四臂，但实际 mutant 只删除 create/Stuck。
- #14：正向测试覆盖 Failed/Canceled/Deleted 三理由，但实际 mutant 只删除 Deleted 映射。
- #21c：首轮这里记录的缺口已由修复轮 1 补齐；真实 adapter 跨-cove mutant 现在由 #21c 自身抓红。

这些部分项在变异映射中均按实际执行范围标注，没有补写未跑结果。

## 修复轮 1 收敛结果

- B1：descendant leaf 判定移到 DELETE 的 `BEGIN IMMEDIATE` 首部，持锁穿过 turn / terminal / harness teardown 到最终删除；活 harness + terminal 进程/socket fixture 在 409 时断言 registry 与 DB 全不变。
- B2：bootstrap 测试 adapter 支持 durable Parked 阻塞；阻塞期父任务保持 `dispatched`，放行后才 `running`；阻塞点丢弃并重建 runtime 后 child/bootstrap op 各 1 条、mint 1 次。
- B3：`fail_child_wave_task` 在事务内读取 durable `tasks.child_wave_id`；真实 adapter 先创建 child 后注入 create Failed/Stuck，child 收场为 Failed，随后 leaf-first 删除 child 与 parent 均成功。
- M1–M4：共享 bounded CTE + 500ms 硬超时；running stamp 三守卫各有负例；success flip 抽出行为门并删除源码计数 oracle；fresh child 的 Draft/archive/pin/terminal 四字段均有独立负断言。
- #21c 与文档：tripwire 改走真实 adapter；两处 §7 计数改 33；权威附录 C.1 登记 `tasks.spawn` / `tasks.child_wave_id`，并明确后者不进 `TASK_COLUMNS`。
- 恢复态定向集合：**9 passed / 0 failed / 3419 skipped**。完整逐变异红绿与首次仍绿的 B2 接缝记录见 `docs/_985-s6-mutation-map.md`「修复轮 1」。
