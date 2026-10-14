# #2618 Planner 单轮预算：第一阶段

CR-2618-budget 已批准并广播。实施为 L2：跨生命周期、持久化与公开设置契约，
由 Planner 安排两个独立评审渠道。本阶段只有后端设置、冻结预算及预警；
下一轮通知、Track 活动和 UI 留待下一阶段。

## 设置与采样

唯一 owner 为 calm-server 的 `planner_turn_budget`，key
`planner_max_turn_duration_minutes`。只接受 ASCII 十进制 1..240，默认30分钟，
合法值规范化；null/空串删除并恢复默认。PUT 在任何 key 写入前验证完整 incoming
patch，400 给出具体原因，未知 key 与 proxy 行为保留。GET 原样展示损坏存储，
PUT reset 可清除损坏值；生产发轮读到损坏值必须明确失败，不能默认回退。

通用真实发轮入口读取 DB 中当前 card role/profile 和设置，不依赖 harness 构造参数或广播。
只有真实 Planner 适用，既存 harness 下一轮读取新设置；当前轮冻结。
Assistant、plain chat 与 compaction 保持原计时策略。预算在发送 start 前随
snapshot 的 start attempt identity 持久化；恢复保留该冻结值，不重新采样活轮。
旧 snapshot 明确为 legacy，不推测其预算。

## 输入、计时及预警

每次真实 Planner start 通过既有生产输入 segment/projection 交付可信系统预算与
收尾阈值，不能伪装人类消息。软阈值剩余 `min(5分钟, limit/5)`；1分钟预算为12秒。
硬停止与预警共享既有 watchdog 基准；hold 等待不计执行时间。
支持 steer 的后端仅尝试一次有界异步当前轮提醒；不得在 watchdog 上等待 steer。
检查 runtime/turn identity、hold 及当前相位；RPC 使用 expected turn identity，
迟到、失败或停止不产生新轮输入。Claude 仅接收初始预警，不假设其 stdin 支持 steer。
提醒在 RPC 前写既有 System input projection，echo 原位升级，避免异步回包早于投影造成
重复或用户归属；失败保留未确认投影供迟到 echo 升级，可信终态仅删除仍未确认的
投影，不排入下一轮。
恢复保留既有持久投影供迟到 echo 升级，不以重启推断终态。
保留既有硬 interrupt、有限确认与 Wedged 策略。

## 恢复与下一阶段

不新增 outbox/receipt 两表，不实施 blanket `recovery_stop`，不新增 failed attention。
现有活轮恢复会进入 Resumed，reconcile 后 Idle，不恢复 monotonic elapsed；
这个既有停止/耗时证据缺口作为风险保留，本阶段不扩大恢复停止策略。
持久冻结预算不能被误述为已修复重启后的执行计时。

下一阶段通知须从可信终态构造下一次真实唤醒的系统输入，携带原因与时间，
使用稳定 ID 至少一次重投；不能自唤醒，也不能把未确认停止表述为确认超时。
届时单独申请持久化和 UI owner 契约。

## 验证与维护成本

worker 先在 REST/start/watchdog 生产入口确认红，运行聚焦绿测及单因子生产 mutation，
真实 API 生成器为 `npm --prefix fe run gen:api`。设置目标实际注册为 `api_suite`，
嵌套模块 `domain_api_suite::settings::`；预算新测试注册到 `planner_harness_suite`，
名称包含 `planner_turn_budget`。先 list 确認非空，再运行：

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --test api_suite 'domain_api_suite::settings::' --no-tests=fail --test-threads 8
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --test planner_harness_suite -E 'test(/planner_turn_budget/) | test(/watchdog/) | test(/^planner_hold_ask::/) | test(/^planner_codex_approvals::/)' --no-tests=fail --test-threads 8
```

kernel gate 拥有 text、contract、Rust quick `--base origin/main` 与上述一次聚焦 replay；
CI 拥有 broad suite。不跑 broad 本地 suite 或真实 Codex E2E。
mutation 独占 worktree，先预测完整红集，单因子修改生产采样，跑全部 budget filter，
安全恢复并确认绿，不永久维护测试名列表。普通新增测试只改自身模块，新增模块再登记 suite；
无新增每 CI 重复扫描规则或应用身份白名单。生成器实际改动的 frozen 路径必须附
exact-path `OWNERSHIP-CHANGE` trailer (#2618)，不手写生成物或修改 allowlist。
