# #2618 Planner 单轮预算与可信超时通知

第一阶段 CR-2618-budget 已批准并广播，包含后端设置、冻结预算及预警。
第二阶段 CR-2618-timeout 批准可信终态、下一真实输入通知及 Track 活动投影，
见后文实际契约；第三阶段提供设置 UI 与独立 timeout 通知。实施为 L2：跨生命周期、持久化与公开设置
契约，由 Planner 安排两个独立评审渠道。

## 设置与采样

唯一 owner 为 calm-server 的 `planner_turn_budget`，key
`planner_max_turn_duration_minutes`。只接受 ASCII 十进制 1..240，默认30分钟，
合法值规范化；null/空串删除并恢复默认。PUT 在任何 key 写入前验证完整 incoming
patch，400 给出具体原因，未知 key 与 proxy 行为保留。GET 原样展示损坏存储，
PUT reset 可清除损坏值；生产发轮读到损坏值必须明确失败，不能默认回退。

通用真实发轮入口读取 DB 中当前 card role/profile 和设置，不依赖 harness 构造参数或广播。
只有真实 Planner 适用，既存 harness 下一轮读取新设置；当前轮冻结；未知 start-attempt 的重试沿用原冻结预算，仅新 attempt 采样当前设置。
Assistant、plain chat 与 compaction 保持原计时策略。预算在发送 start 前随
snapshot 的 start attempt identity 持久化；恢复保留该冻结值，不重新采样活轮。
旧 snapshot 明确为 legacy，不推测其预算。

## 输入、计时及预警

每次真实 Planner start 通过既有生产输入 segment/projection 交付可信系统预算与
收尾阈值，不能伪装人类消息。软阈值剩余 `min(5分钟, limit/5)`；1分钟预算为12秒。
硬停止与预警共享既有 watchdog 基准；hold 等待不计执行时间。
支持 steer 的后端仅尝试一次有界异步当前轮提醒；不得在 watchdog 上等待 steer。
检查 runtime/turn identity、hold 及当前相位；RPC 使用 expected turn identity，
迟到、失败或停止不产生新轮输入。不支持 steer 的后端仅接收初始预警，能力由 backend.supports_steer 声明。
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

## 第二阶段：可信 timeout 终态与可继续通知

CR-2618-timeout 批准沿用 L2；正式两个独立渠道的评审由 Planner 安排。
不修改设置 UI、FE 手写契约或 attention 的闭集，不新增迁移、outbox、receipt 或
blanket recovery_stop。

### 证据与确认边界

`HarnessInterruptionIntent` 明确区分旧 `{turn_id, reason}` 与 `version: v1`。
旧 shape 只证明原因，不推测请求时间、耗时或预算，不生成第二阶段通知。
v1 的 session/card/track/thread/turn、reason、requested_at_ms、effective_elapsed_ms、
budget_ms 都为 required 类型；watchdog 在 interrupt 前采样并持久化。
有效耗时使用既有 `watchdog_from`，与冻结预算/hold 暂停规则共享同一基准。

唯一 `turn_outcome::record` 删除 provider 的 `harness_interruption_reason` 和
`harness_timeout`，仅在明确 interrupted、显式 turn id、持久 snapshot 的 thread/turn、
当前 session/card/track 与 intent 全身份一致且停止已确认时，写入
`harness_interruption_reason: {version: v1, request: {...}, ended_at_ms}`。
ended_at_ms 是确认终态写入时的内核墙钟，不是推测 provider 的结束时间。
UTC 墙钟可向后校正；reader 不用它与请求时间的大小比较拒绝真实证明，执行超限
只由 monotonic effective_elapsed_ms/budget_ms 判断，不人工修补或假造 UTC 时间。
既有 first-insert-wins 保留；重复 live/recovery 记录不升级、覆盖或改写首次终态。

Codex 的真实终态通知/恢复读出的终态证明 provider 停止。Claude 的本地 driver
在 stop 确认之后才记录证据；stop 不确认时只写不带 timeout 证明的终态。
ACP 只有 prompt 已返回且 teardown/revoke 已成功的终态/持久 receipt 才能证明结束。
通用 loop 消费 backend 声明的确认契约，不能将本地 adapter 的事件当作缺失的
teardown 证明。Claude 重启 sweep 合成的 interrupted 行显式为 Unconfirmed，
不能仅因重启后进程已消失就宣称此前预算超时已确认。

活轮重启仍沿用既有 Resumed/reconcile 策略，没有恢复 monotonic 执行计时；
持久预算和请求证据不修复该既有缺口，也不会主动再次停止所有恢复轮。

### 下一真实输入的交付与恢复

真实 issue 入口在修改 Issuing 状态/清 intent/排空队列之前，从 Repo 读取当前
card/track 的最新 `turn/completed`。即使 session 或 thread 更换，仍以保留的对话
历史为准；reset 清理历史后自然没有上一轮证据。最新终态为可信 timeout 时，
将 `planner_turn_timeout:<terminal row id>` 放入 System segment，携带原因、请求与
确认记录时间、有效耗时和预算，并要求核查已保存成果及更新 Report 的实际进度/
验证/剩余工作。不推测被截断步骤或未回执操作的结果。

通知与现有真实队列一起经过 input projection、turn/start 和 rebuffer；没有独立
wake，没有临时 observe，没有另建消费 receipt。准备 projection 不是 accepted
证明。start 失败保留上一终态，恢复后重读存储并用同一 terminal row 标识至少一次
投递；start 回执丢失也不能据此消耗通知。新轮产生任意终态后，旧 timeout 不再是
“上一轮”，后续 start 不重复该 notice。Assistant 同样收到自己保留的上一轮证据，
与只有真实 Planner 才采样设置预算的角色判断相互独立。

### Track 投影和查询

新增 `planner_turn_timeout` source，dismiss key 为 `planner_turn_timeout:<row id>`。
每个可信 timeout 都保留至该 key 被用户 dismiss，或它之后有成功 completed；
发消息、failed 或人工 interrupted 都不能清除。后来的 timeout 不隐式清掉此前
尚未处理的 key。attention 保持既有 None/Input/Failed 的折叠：timeout 单独为 None，
不复用 PlannerDown，不把 timeout 生命周期变为 Failed。UI decode/mapping 在下阶段处理。

Repo 提供 latest terminal 和 pending timeouts 两个窄读取，以既有
`idx_transcript_card_method_created_at` 的 card/method 范围读取，按 owner 分配的持久 row ID
确定因果顺序，UTC 只保留为原始时间事实。下一输入取最新一行（全状态）；Track 只读取成功完成之后的 timeout
候选并核查每行完整 identity，不全量分页 transcript、不设硬截断、无新索引迁移。
永久维护成本只有 owner 类型、真实 writer/reader 和新增测试模块的 suite 注册；
增加普通测试只修改自身模块。kernel gate 各跑一次文本/合同/quick 与三组聚焦
replay，CI 保留 broad suite。

API evidence 类型由 calm-types 导出，并注册到 OpenAPI；唯一生成命令为
`env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 npm --prefix fe run gen:api`。
生成物实际变化附 exact-path ownership trailers，不手写生成物或 FE 枚举。

通知文案实际 owner 为 [下一输入模板](../../crates/calm-server/prompts/previous-turn-timeout.md)
与 [Track 通知模板](../../crates/calm-server/prompts/planner-timeout-activity.md)。固定表名的
四个新 SQL 引用（三个 storage SELECT 引用及一个旧数据兼容测试更新）按既有 terminology ratchet 记录有界 baseline 例外，不改变门禁规则；
以后增加该物理表的 SQL 引用仍需更新 baseline 与对应理由，门禁在 CI lint 中运行一次。

可信 v1 outcome 使用既有 protected `harness_interruption_reason` 命名空间：旧版本已
删除 provider 的该字段并且最多写 cause-only 字符串，因此历史行不能经旧 provider
任意 payload 获得新时间 authority。未保护过的 `harness_timeout` 即使完整匹配 v1
shape 也不被 reader 信任；当前 writer 清除它。transcript error 文本明确兼容旧 reason
字符串与 v1 证据对象，不以 provider 文本或新字段外观推断可信 timeout。

## 第三阶段：Settings 与 Track UI

CR-2618-settings/track 已批准 core/domain 的 settings/activity/track/conversation
窄 codec、features/settings 的 Planners props、features/track/page 的通知 kind，
以及 app shell/query/router 的注入映射。沿用 L2（消费持久设置与可信终态边界），
正式独立评审和真实 stack preview 由 Planner 的后续任务执行。

[设置 codec](../../fe/core/domain/settings.ts) 仅处理
`planner_max_turn_duration_minutes`：ASCII 整数 1..240，缺失明确为30，非法存储为
错误，写入规范化，reset 只发送该 key 的 null。400 是整份 patch 写入前的拒绝，
不能再把 handler 描述成只返回500。它不采样预算、不推断后端能力，也不修补坏值。

[Planners pane](../../fe/web/src/features/settings/planners.tsx) 复用 SettingRow，接收
单值、读取状态/成功读取 revision 与 save/retry 能力；[shell host](../../fe/web/src/app/shell/settings-overlay.tsx)
复用既有 settings query/mutation/invalidation，不能横向导入其他 feature。
帮助明确默认30分钟、下一轮生效、hold等待不计，以及 不支持 steer 的后端只有轮初预算提醒。
控件保留数值原文，并用 numeric inputMode 提供手机数字键盘。这里有意复用 TextInput
而不用 NumberInput：后者会将越界值在 blur/Enter 自动钳位、解析 locale 数字；这会
掩盖 ASCII/范围错误与损坏的原始存储。范围由唯一 core codec 验证，未引入新 UI primitive。

Enter/blur 发出一次单 key 写入；一个请求未结束时仍可编辑，后续草稿保持未保存并
要求完成后再 Enter。没有串行队列、自动重试或卸载时隐式保存，也没有复制 Network
两字段的 sequence/status 表。服务端刷新同时比较值与成功读取计数（不能用可能相同的毫秒时间）：
即使重读最初的值也撤销旧 Saved，干净草稿随读取更新，脏草稿保留
并提示 Changed elsewhere。成功只确认所提交的值；新草稿不能继承 Saved。失败明确
区分拒绝与 unconfirmed，blur 不重放失败值，Enter 或 Restore default 是显式恢复。
PUT 响应不写入 query cache；settings invalidation 重新读取，不能倒灌旧响应。

[activity 模型](../../fe/core/domain/activity.ts) 与
[Track decoder](../../fe/core/domain/track.ts) 明确接收 `planner_turn_timeout`，unknown
source 保持跳过语义，不能落入 ask/down。attention 闭集及 fold 不变；
[router](../../fe/web/src/app/router/public.tsx) 穷尽映射独立
`planner-turn-timeout` kind，[Track page](../../fe/web/src/features/track/page/public.tsx)
标题为 “Planner turn timed out”，保留 kernel 原文并显示 atMs 的 UTC 记录时间。
可 Open Planner 和 dismiss 原始 key，无乐观删除；不要求 attention Input/Failed 或
Planner Failed 生命周期。手机只有包含 timeout 的通知容器新增可见分支；打开对话或
被前景 mobile panel 遮蔽时恢复旧隐藏行为，旧 ask/down 独立展示不变。

[conversation decoder](../../fe/core/domain/conversation.ts) 将 protected
`harness_interruption_reason` 的 legacy string 与完整 v1 object 作为独立 kernel
metadata 保留。durationMs 与 created_at_ms 继续来自原始 row，不以预算代替 duration，
不比较请求与结束 UTC 大小，不把 metadata 填到 provider message/code。
未知 shape 仅丢该 detail，既有 outcome/status/error 行继续展示；即使 unprotected
`harness_timeout` 完全符合 v1 shape 也不能接受。FE 不复制后端可信 record 验证规则。

WEB_COMPAT_VERSION 两声明同步 50→51：旧 bundle 会静默跳过新 timeout item，
造成已确认终态通知不可见。没有修改 activity schemaVersion、sync event、REST版本、
迁移或预算规则。实际 API generator 已执行，产物与 c9a29a0 相同，无手改 generated。

### 验证归属与维护成本

worker 先确认 core codec/track/conversation、Planners、shell host、router 与 desktop/
mobile/coarse browser 最小红测，再跑这些聚焦选择；protected namespace 的 load-bearing
断言用单因子 production mutation 验证：改读 unprotected namespace 恰好两个
conversation 测试红；让 decoder 不认识 timeout source 恰好 core/route 两个测试红，
旧通知测试仍绿，恢复后全部重绿。仅因 compatibility floor 的 Rust 声明更新，
worker 另跑要求的 contract 选择与 Rust quick（非 broad suite）。kernel gate 各运行一次文本、前端lint/build/
unit及指定browser replay，CI 拥有 broad suite，worker 不跑 broad suite。
指定 gate 的普通 browser 文件不被 browser-coarse 的 suffix 分区收集，因此另在现有
模块目录中以 `.coarse.browser.test.tsx` suffix 放置两个 coarse 文件，实际运行 coarse 用例；inventory 的 basis 只是说明，不控制发现或收集。settings codec 测试同样由既有 suffix 规则发现，不扩大 Vitest include 或永久测试列表。
普通以后增改测试仅改所属测试文件；新增模块才登记 inventory。没有新增 styles token、
通用样式/allowlist、扫描规则或每 CI 重复检查。既有大型 conversation/router/page 文件
只扩展窄 codec/映射/通知展示，未引入新的大型手写文件。

真实 preview 按 [开发预览流程](../development-preview.md) 使用两个空闲 loopback 端口：
backend CALM_PORT 与 frontend FE_DEV_PORT，后者 proxy 前者。检查
`/next/settings/planners` 与 `/next/track/<track-id>`，包括坏值reset、保存失败、下一轮
新预算、none attention timeout 的打开/dismiss，桌面与手机均需检查。
后续验证运行 tier 1 `./e2e/run.sh`；真实 Codex/Tier 2 只在 dedicated host 执行。

## Review correction: causal order and replaceable context

Terminal reads use the owner-assigned harness row ID as durable causal order;
UTC timestamps remain unchanged facts, including rollback and ties. Receipt
enrichment runs while segments still match queue entry indexes, before either
kernel prefix is inserted.

Only `system_planner_turn_budget` identifies the initial, kernel-generated
context that a replacement turn rebuilds. It has no observation origin or
attachments and never counts as a user message. Legacy `system`, mid-turn
warnings, previous-timeout notices and task observations remain nonreplaceable;
missing input still refuses replacement. The frontend decodes the new variant
as a system entry; compatibility 52 prevents older bundles dropping its row.
No migration or backfill confers this property on old rows.

The frontend classifies a background sync by its observations: ignore only the
initial budget variant, require at least one remaining segment, and require all
remaining segments to be report edits. The opening budget belongs inside that
fold, whose author comes from the report edit. A budget alone never folds. A
confirmed replacement removes that regenerable prefix from the shown turn;
composer refill and marked message IDs still contain only user speech/images.

Corrupt budget sampling precedes expensive refresh/diff and uses the existing
visible refusal/backoff path only for parsed invalid settings; resetting storage
resumes at the existing retry. DB/card read failures retain their original error
and existing Retryable pacing, with no advice to reset storage.
No new recovery or stop policy is introduced. Ordinary new tests need no
inventory edit; the enum requires updating its FE decoder/label and running
`npm run gen:api` once, with generated drift checked by CI.

The existing terminology ratchet is regenerated for eight test-only references
to the frozen transcript table (282 → 290 in the crates scope). Its scan rule
and CI frequency stay unchanged; future SQL naming that table still updates the
existing baseline and rationale, rather than adding another check or inventory.
