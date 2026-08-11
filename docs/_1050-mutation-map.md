# #1050 cove 读时聚合：事件失效裁决与变异证据

## 事件 → cove 核实与失效表

核实来源：`crates/calm-types/src/event.rs` 的 `Event` 变体（生成结果同步在
`web/src/api/generated-events.ts`）。summary key 是
`['cove-task-summary', coveId]`，其前缀是 `['cove-task-summary']`。

| 事件 | payload 是否直接带 cove id | 裁决 | 理由 |
|---|---:|---|---|
| `plan.updated` | 否；只有 `wave_id` | 失效 summary 前缀 | 不依赖该 wave 恰好已载入浏览器缓存 |
| `task.dispatched` | 否 | 失效 summary 前缀 | `idempotency_key` 是相关键，不把字符串拆分当 cove 权威 |
| `task.completed` | 否 | 失效 summary 前缀 | 同上 |
| `task.failed` | 否 | 失效 summary 前缀 | 同上 |
| `task.gate_result` | 否 | 失效 summary 前缀 | `task_id`/`idempotency_key` 都不是 cove id |
| `wave.updated` | 是：`data.cove_id` | 精确失效 cove summary key | payload 已给权威映射 |
| `wave.lifecycle_changed` | 是：`data.cove_id` | 精确失效 cove summary key | payload 已给权威映射 |
| `wave.deleted` | 是：`data.cove_id` | 精确失效 cove summary key | 删除后不能再查 wave→cove，事件自带映射正合适 |

选择「缺 cove id 时失效前缀」而不是维护 wave→cove 映射：任务事件可能发生在未打开、
未缓存的 wave；缓存映射会把正确性依赖变成 UI 是否访问过该 wave。前缀失效只影响这一类
summary 查询，范围可控且不会漏后台变化。

## B1–B10 变异记录

以下每项都在最终实现上临时施加生产代码变异，实际运行命令确认红后用反向补丁恢复。
证据日期：2026-08-11。Rust 测试命令均在仓库根执行；web 命令在 `web/`、Node 22.22.2
执行。

| 验收 | 临时变异 | 实跑命令 | 红证据（关键输出） |
|---|---|---|---|
| B1 | 共享宏的 `declared_by='spec'` 改成 `user` | `cargo test -p calm-truth b1_shared_legacy_predicate` | `left: 2, right: 1`，正/反语义 fixture 红 |
| B2 | `blockLive` 的 `origin='block'` 改成 `origin!='block'` | `cargo test -p calm-truth b2_materializing_k_rows` | 桶守恒断言 `left: 6, right: 3` |
| B3 | 在 summary statement 前插入真实 `SELECT 1` | `cargo test -p calm-truth b3_static_shape_and_real_sqlite_trace` | `real sqlite statement trace: 2`，期望 1 |
| B4 | 拆成两个 autocommit summary 读：第一次 totals、第二次 rows | `cargo test -p calm-truth b4_barrier_result_is_one_complete_snapshot -- --nocapture` | `torn snapshot`：totals `pending: 40`，rows 已是 `pending: 39 + done: 1` |
| B5 | 截断时由返回的 200 行重新求 totals | `cargo test -p calm-truth b5_truncates_rows_but_keeps_full_totals` | `left: 200, right: 203` |
| B6 | 缺 cove 时返回 `Some(default)` | `cargo test -p calm-truth b6_missing_and_existing_empty_coves_are_distinct` | `is_none()` 断言红 |
| B7 | 删除 `wave_counts` 的 `WHERE w.cove_id=?1` | `cargo test -p calm-truth b7_never_leaks_tasks_or_waves_across_coves` | wave 数 `left: 2, right: 1` |
| B8 | 共享 legacy 谓词把非终结过滤改为 `status IS NOT NULL` | `cargo test -p calm-truth b8_terminal_rows_never_enter_live_origin_buckets` | live 桶 `left: (3, 0), right: (0, 0)` |
| B9 | 唯一 tie-break 从 `wave_id ASC` 改为 `DESC` | `cargo test -p calm-truth b9_sort_has_unique_wave_id_tie_break` | 顺序变为 `z,m,a`，期望 `a,m,z` |
| B10 | 删除 `plan.updated` 的 summary 前缀 key | `node node_modules/vitest/vitest.mjs run src/app/eventBridge.test.tsx` | 仅对应 case 红：`expected [] to deeply equal [['cove-task-summary']]` |

B10 使用同一个 `it.each` 表逐事件喂 bridge，并从全部 `invalidateQueries` 调用中只筛
`cove-task-summary` key 后做精确数组比较；因此删掉八行中的任一 policy，都会由它自己的
case 报出缺失，而不是靠 golden 调用总数间接覆盖。
