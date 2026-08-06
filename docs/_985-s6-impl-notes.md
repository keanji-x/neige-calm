# #985 切片 6 PR-A 实现说明

## 门结果

所有最终门均在 `NEIGE_CODEX_BIN` 未设置、`CARGO_BUILD_JOBS=6` 下执行；Rust 命令的 PATH 含 `/mnt/data2/kenji/neige-calm/.local-bin`，web/fe 使用 Node 22.22.2。

| 门 | 实际结果 |
|---|---|
| `cargo fmt --all --check` | 首跑发现 3 处格式漂移、exit 1；运行 `cargo fmt --all` 后原命令复跑 exit 0。 |
| `cargo clippy --workspace --all-targets --features calm-server/codex-e2e -- -D warnings` | exit 0；0 warnings / 0 errors，1m19s。 |
| `cargo nextest run --workspace --locked --features calm-server/codex-e2e --profile ci` | 首跑：3338 passed / 1 failed / 89 skipped，唯一失败为 `terminate_all_kills_grandchild_of_live_leader` 的 5s reply timeout；不枚举 target，原门整条复跑：**3339 passed / 0 failed / 89 skipped**。 |
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
4. **#21c 的指定 tripwire 不驱动写路径。** adapter 写错 cove 时 #21c raw-SQL 测试仍绿，而真实 adapter #6 测试红。现状的生产行为有保护，但 §7 所称 tripwire 本身没有归因能力；后续应把 #21c 改成创建第二 cove、驱动真实 child adapter，再断言全表无跨-cove 边。
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
- #21c：指定测试在设计 mutant 下仍绿，是现存验收缺口；不能把 #6 的旁路红冒充成 #21c 自己会红。

这些部分项在变异映射中均按实际执行范围标注，没有补写未跑结果。
