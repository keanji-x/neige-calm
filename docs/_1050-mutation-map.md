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

## 原实现 B1–B10 变异记录

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

## 第 1 轮修复后：评审发现的 6 条覆盖缺口

以下 6 个评审变异于 2026-08-11 在修复后的工作树逐条重新注入、实跑并反向恢复。Rust
命令环境均为 `RUSTC_WRAPPER=`、`CARGO_TARGET_DIR=/mnt/data2/kenji/neige-calm/target`、
`PATH=/mnt/data2/kenji/neige-calm/.local-bin:$PATH`；web 使用 Node 22.22.2。表中的失败均为
预期红灯，没有仍然全绿的变异。

| 变异 | 临时改动 | 实跑命令 | 失败测试与断言消息 |
|---|---|---|---|
| codex SELECT 1 | `RepoRead::cove_task_summary` acquire 后插入 `sqlx::query("SELECT 1")` | `cargo nextest run -p calm-truth --lib -E 'test(b3_static_shape_and_real_sqlite_trace_prove_one_statement)'` | **红** `b3_static_shape_and_real_sqlite_trace_prove_one_statement`：`public cove_task_summary must execute exactly one sqlite statement; traced 2`（left 2 / right 1） |
| MR1 | 外层 `ORDER BY r.ordinal` 改为 `r.wave_id` | `cargo nextest run -p calm-truth --lib -E 'test(b5_truncates_rows_but_keeps_full_totals)'` | **红** `b5_truncates_rows_but_keeps_full_totals`：`legacy-heavy waves must sort first even when their ids sort last`；left `[a-zero-003, a-zero-004, a-zero-005]`，right `[z-legacy-a, z-legacy-b, z-legacy-c]` |
| MR2 | `wave_count > ?2` 改为 `>= ?2` | `cargo nextest run -p calm-truth --lib -E 'test(b10_truncation_boundary_is_strictly_above_limit)'` | **红** `b10_truncation_boundary_is_strictly_above_limit`：`200 waves: truncated must be true only strictly above the 200-wave limit`（left true / right false） |
| MW1 | `其中已投影` 从 `blockLive` 换成 `pending` | `npm run test -- --run src/pages/Cove.test.tsx` | **红** `renders every distinct count with explicit, internally consistent scopes`：`Unable to find an element with the text: 其中已投影 5`（DOM 实际为 8） |
| MW2 | 删除全部任务排队/在飞/完成/失败/取消五个 span | `npm run test -- --run src/pages/Cove.test.tsx` | **红** 同一测试：`Unable to find an element with the text: 全部任务排队 8` |
| MW3 | (a) 删除 `router.tsx` 的 `taskSummary={taskSummaryQ.data}`；(b) 独立恢复后把 client URL 的 `/task-summary` 删除 | `npm run test -- --run src/integration/cove-task-summary-route.test.tsx`（两次） | **两次均红** `fetches the encoded production URL and renders its summary through CoveComponent`：`Unable to find an element with the text: 其中已投影 5`；因此 prop 与 URL 两处生产接线均被同一真实路由→fetch oracle 覆盖 |

MW3 的测试只 mock 全局 `fetch`；真实执行 `coveRoute.loader`、`api.coveTaskSummary`、
`coveTaskSummaryQueryOptions` / `useCoveTaskSummaryQuery`、`CoveComponent`、`CovePage` 和
`WaveRow`，并要求请求精确命中 `/api/coves/cove%20route/task-summary`。

## 可登记项裁决

- F6：本轮选择补最小提示；`truncated=true` 时 Cove 页明确说明只显示 legacy 存量最高的前
  200 个 wave、汇总仍覆盖全部 wave。cove totals 保留为后端契约，暂不重复渲染九项总计。
- F7：带 wave 的 HTTP wire 形状仍未由 Rust HTTP contract 逐字段观察，登记后续补齐。
- F8：本轮随口径重写移除了无 role `div` 上的 summary `aria-label`；wire 冗余字段消费仍登记。
