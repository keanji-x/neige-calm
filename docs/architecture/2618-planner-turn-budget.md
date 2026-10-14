# #2618 Planner 单轮预算设计

状态：仅设计申请，尚未实施或广播。依据本 Track Report 的已定契约与
`timeout-settings-callpath-analysis`；本提交只新增本文。实现为 L2（生命周期、持久化与公共契约），
需两个独立评审渠道；本次文档门禁为 `scripts/local-ratchet-gates.sh`，不跑代码测试或生成器。

## 四项验收与既有入口

| 验收 | 设计与源码证据（当前基线） |
| --- | --- |
| Settings → Planners 配置 | [settings-overlay.tsx:150](../../fe/web/src/app/shell/settings-overlay.tsx#L150) → [planners.tsx:16](../../fe/web/src/features/settings/planners.tsx#L16)；复用 [queries.ts:558/1230](../../fe/web/src/app/providers/queries.ts#L558) 的 GET/PUT 与 invalidate，旧 PUT 响应不倒灌缓存。 |
| 硬停止前预警 | 每轮初始输入包含预算和软收尾阈值；[backend.rs:327](../../crates/calm-server/src/harness/backend.rs#L327) 仅 Codex 支持 steer，支持者另做临近提醒；Claude 明确只有初始预警。 |
| 下一轮告知原因与时间 | 从 [turn_outcome.rs:15](../../crates/calm-server/src/harness/turn_outcome.rs#L15) 的可信确认结果建 durable notice，下一次真实唤醒随输入交付，不自唤醒。 |
| Track 可见且有限硬停止保留 | 新 `planner_turn_timeout` source，直到 dismiss 或后续轮成功；保留 [run_loop.rs:3818](../../crates/calm-server/src/harness/run_loop.rs#L3818) 的 watchdog、30s interrupt confirmation 与 Wedged，不能把未确认停止说成超时完成。 |

## 设置与预算所有权

全局唯一 key `planner_max_turn_duration_minutes`，ASCII 十进制整数 1..240，默认 30；
`null`/空串删除并恢复默认。拒绝空白、小数、指数、符号、溢出和单位后缀；合法串规范化为十进制。
[settings.rs:18/55](../../crates/calm-server/src/routes/settings.rs#L18) 的 string bag wire 与未知 key merge 保持；
Rust 建议新增 `planner_turn_budget.rs`，定义私有字段 `PlannerTurnLimit`、校验构造与唯一默认；
FE [core/domain/settings.ts](../../fe/core/domain/settings.ts) 提供纯 codec，组件不复制解析规则。

先验证整份 incoming patch（包括所有已知字段）再写任何 key；建议 Repo 提供一次事务的 settings patch，
提交成功后才触发既有 proxy respawn side effect。不要以加载全部强类型配置为 PUT 前置条件：
GET 仍返回原始 bag，损坏 key 可读取、显示错误、用 null 重置；损坏的持久值不能静默采用默认。
预算读取失败沿现有 `IssuanceRefusal` 可见拒发并保留队列；用户修复后仅下次合法发轮恢复。
UI 分钟输入、Enter/blur 保存、失败状态、恢复默认、外部刷新和乱序保存遵循已有 Settings 行契约。

在通用发轮 [run_loop.rs:3439](../../crates/calm-server/src/harness/run_loop.rs#L3439) 的 selection/permission
解析附近读取预算，早于任何 provider start；采样即冻结点。设置提交后采样的新轮用新值，当前轮不重定时。
每个 start attempt 持久化必需 `FrozenTurnBudget { limit_ms, sampled_at_ms, warning_remaining_ms }`；
同一不确定 start 的重试沿用冻结值，明确失败并 rebuffer 后的新 attempt 重新采样。
这覆盖早已创建的 harness；不能只改两个 `HarnessConfig::default()` 生产构造
（[start_adapter:1533](../../crates/calm-server/src/operation/planner_harness_start_adapter.rs#L1533)、
[mod.rs:311](../../crates/calm-server/src/harness/mod.rs#L311)）。

角色读取复用 [profile.rs](../../crates/calm-server/src/harness/profile.rs) 的 `PlannerBinding`/`HarnessProfile::Planner`
判定（参考 [turn_permission.rs](../../crates/calm-server/src/harness/run_loop/turn_permission.rs)）；
禁止按 template/provider 名推断。Assistant/PlainChat 不消费该 key；compaction 保留原独立默认预算。
运行轮冻结角色/profile；通知投递时再确认目标 card 仍是本 Track 的 Planner，角色变化保留未投递证据，不泄漏给 Assistant。

有效执行耗时用 [state.rs:20](../../crates/calm-server/src/harness/state.rs#L20) 的 watchdog 基准，
不使用 UI 墙钟 `started_at`；[held.rs:162](../../crates/calm-server/src/harness/run_loop/held.rs#L162)
的 hold 暂停同时作用于预警与硬上限。软剩余量为 `min(5分钟, limit/5)`（按 Duration，无整数分钟截断）；
1 分钟在 48s 收尾、30 分钟在 25min 收尾。初始可信 system context 明示预算、有效耗时口径、收尾阈值、
保存成果并更新 Report 的要求，不承诺固定墙钟截止时间，也不编造被截断步骤。

动态提醒独立有界任务，携带 session/card/thread/turn fence，结果回 loop；预警只尝试一次，
晚到不得 steer 新轮，不伪装用户 queue entry。硬截止优先，steer 超时/失败/回执丢失不延迟中断；
同一 tick 跨过软硬阈值直接停止，主机停顿无法保证临近消息必达。hold 期间不 steer。
Claude [session.rs:495](../../crates/calm-server/src/claude_planner/session.rs#L495) 仅轮初写 user line，
[protocol.rs:689](../../crates/provider/src/claude/protocol.rs#L689) control 只有 Interrupt；
写 stdin 不证明当前轮消费。设置说明和验收必须明确 Claude 无临近动态提醒，ACP 同样按 capability 降级。

## 可信超时证据

在 [run_loop.rs:3931](../../crates/calm-server/src/harness/run_loop.rs#L3931) 持久化 intent 后才请求中断；
新 versioned intent/outcome metadata 包含必需 session/card/track/thread/turn identity、
`requested_at_ms`（内核请求墙钟）、`limit_ms`、`active_elapsed_ms`（请求时有效执行耗时）与原因。
结算时间取 outcome row 的 `created_at_ms`，与请求时间分别命名；确认耗时不冒充执行耗时。
旧 intent 显式 Legacy 分支，旧 outcome 可显示“请求时间/预算未知”，不得默认填当前时间、30分钟或 error 文本推断。

扩展唯一 `turn_outcome::record`：清除所有 provider supplied reserved metadata，仅完整身份匹配、
持久 intent 匹配且 terminal `interrupted` 才附可信字段。completed 竞态、人工 interrupt、foreign completion
均不建 timeout notice；TurnAborted sibling 只在有同样可信终态证据时允许。
扫 Claude driver、Codex preserving recovery、ACP recovery 的 record 调用；Claude “server restarted”合成结果
不是 timeout confirmation，必须区分，不能仅凭此前有 intent 就伪造已确认 timeout。
[out_of_domain.rs:488](../../crates/calm-truth/src/db/sqlite/out_of_domain.rs#L488) first-insert-wins，
因此完整 metadata 必须随首次 outcome 落库；重复回调不得改写既存正常 outcome。

## 建议 durable outbox + receipt（待 Planner 批准新表）

建议新增 migration（编号实现时取最新序号；不改任何旧 migration），两表由 calm-truth 独占写：
`planner_timeout_notices`：PK `outcome_row_id`，identity、versioned payload、目标 card/track；
`planner_timeout_deliveries`：PK `(outcome_row_id, batch_client_id)`，target session/thread、
状态 `prepared/accepted`、accepted turn identity。notice identity 为 `planner-timeout:<outcome_row_id>`。
明确区分通知事实、投递 attempt 与 Track dismissal；notice 不是 snapshot queue，快照不能删除或替换它。

将 outcome insert + notice insert 放进同一 `BEGIN IMMEDIATE` 事务；Repo 接受类型化可信证明，
SQL 再检查 session/card/thread/turn 和 runtime generation，重复调用按已有 outcome 内容幂等建 notice，
不能用重复调用的新 payload 为旧 outcome 增添超时。已有 legacy outcome 不 backfill 假时间。
失败返回错误，intent 仍留存并重试；禁止只打日志清 intent。无需 outcome→notice 补齐扫描，因为同事务。

发轮仅由原 queue 的真实 observation 与 debounce 决定资格；notice 不进入 hard-fire/SystemContext 资格判定。
有真实唤醒后，按当前 Planner card 查未 accepted notice，按 row ID 排序，构建 typed system input segments；
在同事务写 projection 与 prepared receipts，绑定已有 [client ID:3367](../../crates/calm-server/src/harness/run_loop.rs#L3367)。
禁止快照把普通 queue 的旧副本变成 receipt；projection 只是“准备发送”，不是接受证明。
backend 接受 start 后，以 thread/turn/client ID 核对并 CAS receipt 到 accepted；更新所有权沿现有
`IssueTurnHandle`/transcript reconciliation，接收后落库失败必须保留 prepared，不启动并行第二轮。

恢复时先解析所有 prepared：只有 provider 报告的公开 client ID/transcript 关联或本地已确认 start 记录
可证明 accepted；凭 projection、墙钟或外部私有文件名都不行。已明确拒绝 start 删除 projection、
取消 prepared attempt，notice 仍 pending，普通唤醒输入 rebuffer；明确终态但无法证明输入消费可重投同 notice ID。
不确定 live start 保持有限 recovery protection，先 reconcile/stop，不盲目重复 start。
Claude 若没有公开 client ID 回显，需其本地 session/driver 持久化 accepted turn 关联；crash 在进程接受与本地落库间
无法 exactly-once，允许下次真实唤醒至少一次重投同 ID。模型级 exactly-once 不在承诺内。

| crash 窗口 | 恢复不变量 |
| --- | --- |
| intent 落库前 | 未请求 backend，无可信 timeout；恢复保护不能冒充 timeout。 |
| intent 后、interrupt 前/回执丢失 | 按同 identity reconcile/retry stop，30s 有界确认；仅 intent 不产生 notice。 |
| terminal outcome 事务提交前 | 全部回滚；terminal history 重放经唯一 record 重试。 |
| outcome 提交后、内存更新前 | notice 已同事务提交；任何旧 snapshot 写入不能覆盖独立表。 |
| notice 存在、真实唤醒前 | pending 可重启恢复，无自唤醒；Track 已可见。 |
| prepared/projection 后、start 前 | 不能标 accepted；真实唤醒 batch 及冻结预算按相同 client ID 恢复。 |
| start 明确失败 | notice 不消费；重排普通输入，下一 attempt 可交付。 |
| start 成功、receipt 回执丢失/写失败 | 公开证据补 receipt；不确定时先隔离活轮，有界停止，再至多重复同 ID。 |
| accepted 后、snapshot 前 | durable receipt 排除已交付 notice；snapshot 重放去重读取表，不倒退状态。 |

outbox 投递与 Track 展示互不消费：dismiss 只隐藏标记，仍在下一真实唤醒告知模型。
[notifications.rs:95](../../crates/calm-server/src/track_activity/notifications.rs#L95) 与
[sql.rs:22](../../crates/calm-server/src/track_activity/sql.rs#L22) 当前排除 interrupted；新增
`PlannerTurnTimeout` ActivityItem/source 和 `planner_turn_timeout:<row_id>` dismiss key。
查询同 Planner card 的可信 notice，按 outcome 顺序判定其后是否有 `completed` 成功轮；failed、人工中断、
用户发消息均不清标记，后续成功或该 key dismiss 才隐藏。不要复用 PlannerDown 或改变生命周期到 Failed。
建议聚合 attention 使用现有 failed 可见级别，但文案明确“单轮超时，可继续”；此映射待 Planner 判断。

## 恢复活轮边界（策略申请）

[run_loop.rs:4319](../../crates/calm-server/src/harness/run_loop.rs#L4319) 目前活轮重启为 Resumed，5s 后 Idle，
不恢复 monotonic elapsed。建议持久冻结预算与 active elapsed checkpoint，禁止用外部私有 start time。
原 timeout intent 已存在者立即恢复同目标 interrupt confirmation；不得因重启给新的执行预算。
其他仍活跃轮：建议 5s reconcile 后若未证明已终结，立即发独立 `recovery_stop` 并有限确认，未确认则 Wedged；
不把它算预算 timeout，不生成 timeout notice。旧无冻结字段轮也走此有限保护，耗时未知明确展示未知。
hold 仍是暂停执行计时；恢复无法确认 hold/live 状态时不得推测 elapsed 或允许并行新轮。
**Planner 待定**：批准这种保守 recovery stop（会提前停止恢复轮），或另定有持久计时证据的继续执行策略；
实现前必须决定，不能静默恢复完整预算、反复重启延寿或默认 5s 后 Idle。角色变更也先清理旧活轮再发新角色轮。

## 窄 owner change requests（仅申请，不授权广播）

| 请求与 owner | 建议精确路径与新增面 |
| --- | --- |
| CR-2618-budget；kernel lifecycle/settings | `crates/calm-server/src/planner_turn_budget.rs`（新）；`src/routes/settings.rs`、`src/harness/{run_loop.rs,state.rs,snapshot.rs,turn_outcome.rs}`（以上相对 calm-server）：预算、intent V2、异步提醒、恢复边界；calm-types `src/harness.rs` 定义必需 V2 字段和显式 Legacy。 |
| CR-2618-outbox；calm-truth persistence/Repo | `crates/calm-truth/src/db/mod.rs`、`src/db/sqlite/out_of_domain.rs` 与新 migration；calm-server `src/db/mod.rs` 转接：原子 outcome/notice、projection/receipt、CAS 与身份 fence；不把业务规则复制到 provider。 |
| CR-2618-settings；core/domain、features/settings、app/shell | `fe/core/domain/settings.ts` codec/400；`fe/web/src/features/settings/planners.tsx` props；`fe/web/src/app/shell/settings-overlay.tsx` 注入现有查询/保存，不增 feature 横向依赖。 |
| CR-2618-track；core/domain、features/track、app/router | `fe/core/domain/activity.ts`、`track.ts` timeout variant；`fe/web/src/features/track/page/public.tsx` 标题/打开/dismiss；`fe/web/src/app/router/public.tsx` 映射；Rust `track_activity/{notifications.rs,sql.rs}` 新 source。 |
| CR-2618-generated；core/api（readonly） | `fe/core/api/generated/openapi.json`、`fe/core/api/generated/wire.ts`；由生成器实际输出决定，API string bag 不变，新增 400/source schema 扫所有 consumer。 |

FE owner 名依据 [module-file-inventory.yaml](../../fe/module-file-inventory.yaml)，Rust owner 为责任申请名，非已授权人员。
遵守 [ownership README](../../fe/tools/ownership/README.md)；仅实际修改 frozen 路径的实施提交携带精确 trailer，
其他接口也须 owner 决定并广播，但不伪称 inventory readonly。建议生成物 trailer（本设计提交无需它们）：

```text
OWNERSHIP-CHANGE: fe/core/api/generated/openapi.json — Add Planner timeout source and settings validation response (#2618)
OWNERSHIP-CHANGE: fe/core/api/generated/wire.ts — Regenerate approved Planner timeout contracts (#2618)
```

若需新增 FE 模块，先向 tooling/architecture 申请 `fe/module-file-inventory.yaml` 的窄登记并附该精确路径 trailer；
优先扩展现有文件与测试，避免新增通用 UI、styles 或 ownership allowlist。源文件超过800行时按职责拆 budget/notice，
不继续扩大 run_loop 的既有例外。Planner 还需批准新表、attention 映射和 receipt 不确定分支策略。

## 实施验证计划（下列命令本次均不运行）

实际注册目标：[domain_api_suite.rs](../../crates/calm-server/tests/domain_api_suite.rs) 的 `settings::`、
`track_notifications::`、`track_notification_dismissals::`；[planner_harness_suite.rs](../../crates/calm-server/tests/planner_harness_suite.rs)
的 `planner_harness_fsm::`、`planner_harness_` 的 `items_persist::` 模块、`planner_hold_ask::`、`planner_codex_approvals::`、
`claude_planner_wiring::`、`planner_transcript_projection::`；库内 `track_activity::`。
新增测试放上述模块：整 patch 无写入/损坏重置；既存 harness 下轮采样/当前冻结/Assistant与compaction隔离；
1分钟12s阈值/hold/迟到steer/Claude初始输入；trusted/foreign/manual/completed竞态；每个表列 crash 窗口、
start失败与lost receipt、旧 snapshot覆盖、无自唤醒、恢复活轮有限保护、Track reload/dismiss/成功清除。
先用生产 REST/start/watchdog 路径确认红；使用已有可控计时/backend fixtures，不在 fixture 复制预算规则。

```sh
# worker focused；gate replay 同选择，nextest 默认拒绝空选择，显式强化为 fail
# 新增测试先实际注册并确认 list 非空，不提前用尚不存在的 target/filter 设 gate
 env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --test domain_api_suite -E 'test(/^settings::/) | test(/^track_notifications::/) | test(/^track_notification_dismissals::/)' --no-tests=fail --test-threads 8
 env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --test planner_harness_suite -E 'test(/^planner_harness_fsm::/) | test(/items_persist::/) | test(/^planner_hold_ask::/) | test(/^planner_codex_approvals::/) | test(/^claude_planner_wiring::/) | test(/^planner_transcript_projection::/)' --no-tests=fail --test-threads 8
 env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --lib track_activity:: --no-tests=fail --test-threads 8
npm --prefix fe run gen:api
cd fe
npx vitest run --project web-dom web/src/features/settings/planners.test.tsx web/src/app/shell/settings-planners.test.tsx web/src/app/providers/settings-write.contract.test.tsx web/src/app/router/track-conversation.test.tsx
npx vitest run --project browser --project browser-coarse web/src/features/settings/planners.browser.test.tsx web/src/features/track/page/header-status.browser.test.tsx
npx playwright test e2e/settings-roundtrip.spec.ts
```

core codec 新测试注册后用 platform-independent 精确路径；Track 单测及 Playwright 增补通知 reload/open/dismiss。
保存/重载/reset/非法 PUT 保证原 key 恢复，不清空全局 bag。实施 gate 分工：text/contract、Rust quick
`--base origin/main` 与上述聚焦 replay；FE `npm ci && npm run lint && npm run build && npm test` 和 browser；
worker 做红绿、lint 修复、真实生成器与聚焦检查；CI broad Rust，不跑全 workspace nextest/full rust gate。
关键 mutation 独占 worktree：去掉身份 fence、去掉原子 notice insert、移除 held 暂停，各单因子先按实际 list
预测完整红集，跑整个观察模块，对比并安全恢复绿。不添加永久测试名清单；新增普通测试只改自身模块，
注册新 target 才改 suite；gate 聚焦 replay 每实施任务一次，广泛 suite 每 CI workflow 一次。

真实预览遵循 [development-preview.md](../development-preview.md)：terminal 中 loopback `make dev-fresh`
与 FE dev proxy、preview_add 的 `/next/` Report 链接，由验证任务浏览设置和 Track（桌面/移动）后停服务移除预览。
集成运行 `./e2e/run.sh`（tier1）；不在共享主机跑真实 Codex Tier2。本文不生成 API、不启动预览、不运行代码测试。
