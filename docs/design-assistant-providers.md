# Assistant 多 provider 设计

状态：planner 已裁决 provider-neutral managed Assistant 身份；后端实现接续失败事件 319505。
本文记录最终结构与验收计划，实际测试结果由 worker receipt 和 kernel gate 报告。
依据调查 attempt `ab5a47b5424742e4a548f04f6f781e7d:inspect-assistant-providers`
（receipt 314737）及源码复核。评审级别 **L2**：跨 session 持久化、凭据和权限边界；
实现由 planner 安排两个独立 review 通道，本设计任务不安排 review。

## 目标与非目标

用户创建 Assistant 时可选 Codex、Claude、OpenCode 及其 available catalog；创建后
provider 固定，同 provider 的 model/effort 可改。跨 provider 必须新建 Assistant；
side discussion 只携带冻结文本，不移植 native history。三个 provider 都是完整目标，
不能以 Planner 身份冒充支持，也不能悄悄只交付其中两个。

保留 `kind=codex`、`harness_profile=assistant`、`CardRole::Assistant` 和
track-assistant summary 身份。Assistant 可读写 report prose、使用同 Track 的
task-worker terminal；不能写 plan/task blocks、作 task verdict、关闭 Track、开启
terminal、获得 Planner 权限或成为 root。permission mode 保持 `Never`。
不改普通 worker、PlainChat、Planner 策略；不新增菜单、不复制 provider policy，
不因身份扩展 OpenCode 的 image、manual compact、history replacement 能力。

| 身份 | provider | card kind / role / marker | session contract / kind / mode | root / MCP |
| --- | --- | --- | --- | --- |
| Assistant（三者相同） | Codex / Claude / OpenCode | codex / Assistant / assistant | **Assistant / ManagedAssistant / resumable** | 否 / Assistant |
| Planner（不变） | Codex / Claude / OpenCode | codex / Planner / 既有 Planner 标记 | Planner / SharedPlanner / resumable | 是 / Planner |
| PlainChat（不变） | Codex | codex / Worker / plain_chat | Executor / CodexCard / resumable | 否 / 无 |

新增 `WorkerContract::Assistant`（DB `assistant`）和
`WorkerSessionKind::ManagedAssistant`（wire `managed-assistant`）。
[现有 contract](../crates/calm-types/src/worker.rs) 与
[kind](../crates/calm-types/src/runtime.rs) 没有同名冲突；不要用 `SharedPlanner`
或 `ClaudeCard` 替代。发现实质冲突应交 planner 决策。

## 当前调用链与 owner 变更

创建链为 [app draft](../fe/web/src/app/conversations/public.tsx) →
[query mutation](../fe/web/src/app/providers/queries.ts) →
[core create operation](../fe/core/domain/conversation.ts) →
[POST track conversations](../crates/calm-server/src/routes/track_conversations.rs) →
[start operation adapter](../crates/calm-server/src/operation/planner_harness_start_adapter.rs)
（hash、mint、queue、session init）→
[binding](../crates/calm-server/src/harness/profile.rs) →
[backend issuance](../crates/calm-server/src/harness/backend.rs)。
现在 draft/route/seed 都不携带 provider，binding 把 Assistant 固定 Codex；
仅改 binding 会留下错误 session 身份和 prompt。

| owner / 入口 | 必须改变的合同 |
| --- | --- |
| calm-types；truth 的 [session init/mirror](../crates/calm-truth/src/db/sqlite/session_mirror.rs)、[identity](../crates/calm-truth/src/db/sqlite/session_row.rs)、[projection](../crates/calm-truth/src/session_projection_row.rs) | 新 contract/kind；managed Assistant 构造接收必填 typed provider，三者 resumable；不得用 Option、默认值、fixture backfill 隐藏缺失。generic kernel 不从 card kind 猜 backend。扫全 enum match、序列化、caller、fixture、日志。 |
| harness profile/binding；truth [validation](../crates/calm-truth/src/validation.rs) | mint 写 server-owned `assistant_provider: AgentProvider`；集中解析 profile + provider，缺失/未知/row-card 不一致 fail closed。加入既有 protected keys 和 sticky 规则，损坏值也不能被 REST/plugin replacement 删除或“修复”。模型 GET/PUT、issuance、恢复使用这一入口，Assistant 绑定失败不得退回 Codex catalog。 |
| harness [wiring](../crates/calm-server/src/harness/wiring.rs) 与 [instructions owner](../crates/calm-server/src/planner_card.rs) | row 携带 required binding；一个 profile-aware instructions 入口供创建、Codex、[Claude wiring](../crates/calm-server/src/claude_planner/wiring.rs)、[ACP wiring](../crates/calm-server/src/acp_planner/wiring.rs) 及恢复共用。保留普通/launchpad prompt，再按 provider 渲染 tool discovery；Claude/ACP 不调用 Planner instructions，不附加 Planner template/plugin 权限片段。 |
| core（已批准接口变更） | create 输入 required typed provider，API schema、Conversation/provider 与 dormant projection 使用服务端声明；不依赖 runtime 是否存在。依赖方向仍遵循 [core 指南](../fe/core/AGENTS.md)。 |
| app draft/router（已批准接口变更） | 保存 provider + model/effort；首发后与 key/text/side 一起冻结，模糊失败 retry 原请求。消费 available catalog、认证 notice 及 backend capabilities；复用 [ModelPill](../fe/web/src/features/chat/thread/model-pill.tsx) 和 [新 Track 组装](../fe/web/src/app/router/new-track-route.tsx)，创建留在 core/query，router 不复制策略。 |

`kind=codex` 是历史壳，不是 backend。模型读写沿
[models](../crates/calm-server/src/routes/models.rs) / [model setter](../crates/calm-server/src/routes/planner_model.rs)
进入 binding；只接受本 provider 的模型/effort。Today 系统入口
[summary](../crates/calm-server/src/routes/today_summary.rs) 与
[watcher](../crates/calm-server/src/worker_watch/watcher.rs) 显式传 Codex；
扫普通 Assistant、launchpad、side、area-chat caller，PlainChat 不新增 provider 选择。

## API、幂等与迁移

旧 POST 省略 provider → Codex 是**显式 API 兼容分支**，只在边界解析，随后进入
必填 typed 构造。新客户端显式发送 provider。为 start operation 保留冻结 legacy wire
形态与原 hash 路径；新增版本化 Assistant wire 形态携带必填 provider，解码后得到
typed execution input。旧 serialized operation 的字段省略、序列化形态、stable hash、
key/card 推导及重放必须原样兼容，包括 PlainChat；不重写历史 operations。
不要给所有 LazyMintCardSeed caller 加一个默认/可选 provider。新请求 hash 包含
provider，同 key 改 provider/model/text/side 必须冲突，retry 不 mint 第二张卡。
历史与新版重放均通过 production adapter 验证。

在 [compatibility](../crates/calm-types/src/compatibility.rs) / [version route](../crates/calm-server/src/routes/version.rs)
声明 Assistant provider 能力并提升相关 REST/web compatibility 合同；更新维护 bundle
的协商和 schema。新客户端对旧 server 缺少能力或不满足版本时阻止提交并提示升级，
不能发送一个会被旧 server 静默忽略的 provider，也不能静默改选 Codex。
旧客户端向新 server 的省略请求仍走显式 Codex 兼容路径。

新增专用迁移，**所有已发布迁移字节冻结**，包括
[0156](../crates/calm-truth/migrations/0156_acp_planner_sessions.sql)。扩展 contract CHECK；
OpenCode 仅允许 `planner|assistant` + resumable，不放开普通 executor。
精确识别旧 `kind=codex + role=assistant + harness_profile=assistant` 卡，
验证其关联历史/current sessions 均为旧 Codex executor/resumable 身份，再回填
`assistant_provider=codex` 与 `contract=assistant`。存在非法 marker、冲突 provider、
错误 session/card/track 关系或 Assistant root 等损坏时中止迁移事务并报告诊断，
不能猜测、跳过或把非 Assistant worker 转换过来；合法无 session 的 dormant 卡也回填。

table rebuild 显式保存并恢复所有关系（cards.session_id、Track root、parent/requester、
worker-flow、terminal、spawn operation）、全部 index/trigger 和 foreign keys；以实现时
最新 schema 为准，不复制 0156 后漏掉后续对象。session/native IDs、transcript、
handle_state_json、queue、watermark/harvest 状态原样保留，禁止重新排队。
迁移回归使用真实旧数据库，比较数据与 schema 对象并执行 foreign_key_check，
包含 dormant、superseded/failed 历史及损坏数据的原子失败。

## 生命周期、凭据和 managed actor 授权

恢复链是 [truth boot selector](../crates/calm-truth/src/db/sqlite/session_projection.rs) →
[harness boot/deferred/recover](../crates/calm-server/src/harness/mod.rs)；
[lazy send](../crates/calm-server/src/routes/planner_session.rs) 与
[reset/restart](../crates/calm-server/src/routes/planner_cards.rs) 同用 binding/start adapter。
统一验证 live card + persisted role/marker + provider + session contract/mode。
Claude/OpenCode Assistant 不等 Codex daemon，Codex 按 readiness deferred；领取一次，
保留 claim abandonment/rearm、thread seals、delete rollback 的现有补偿路径。
`root_session_id` repoint 和 Planner backlog catch-up **仍仅 Planner**。
同步 [failed carrier](../crates/calm-truth/src/db/sqlite/session_system_error_recovery.rs)
的旧 Assistant contract 判断，保留 queue harvest 幂等，不把 Codex system-error 策略
套给 Claude/ACP。force-new-thread、phase-2 crash、重启、删除失败回给队列均需覆盖。

managed Assistant token 不继承前任 card hash：首 turn 通过
[mint owner](../crates/calm-server/src/mcp_server/wiring.rs) 原子写 card/current active
session；supersede/fail、boot、track revoke、delete/rollback 按 live authority 清理或
重新 mint，旧 token 不得认证新 runtime。Codex loaded thread 的热 resume 不更新 MCP
配置：失败 start 补偿由 MCP credential owner 使用 retiring 事务捕获的 hash/thread
checkpoint，原子恢复同一 predecessor 的 active/current 与原 native token。仅本次
failed successor 仍占 current slot 时允许 undo；已恢复且 hash 相同可幂等重放，
其后被撤销或轮换、其他 current runtime、failed carrier 或任意 stale/superseded
历史均不能借 checkpoint 获权。成功 successor 继续取得新 token，不继承旧 hash。
reset/restart/lazy start 同用此补偿；delete rollback 不 supersede Codex row，也不使用
此 checkpoint，Claude/ACP 保持 revoke 后首 turn mint。扩展 session_mirror 与 session_row 的
Planner-only managed 条件，不另造凭据路径；Codex 也验证这一 managed Assistant
不继承不变量。进程继续显式 allowlisted env / strict MCP config，不引入隐式配置。
共享角色规则、各 provider 的 discovery 语法来自其公开/返回合同；
[Assistant mechanics](../crates/calm-server/prompts/assistant/mechanics.md) 中 Codex
deferred search 不能原样当作 Claude/ACP 合同，prompt 本身不赋权。

所有新 managed Assistant 使用 provider-neutral `AiManagedSession`，包括 Codex、Claude、
OpenCode。`AiManagedCard` 只作为内部 role-gate surrogate：serde/TS 跳过，不能从事件
wire 读入或写出；不是新增公开 card actor。原 `AiCodex*`、`AiClaude*`、`AiPlanner*`
wire 和历史语义保持。MCP transport 与 report attribution 共同使用 typed actor constructor，
Assistant 分支不匹配 provider，不借 Codex 或 Planner actor。新的 Assistant contract
在 live gate 拒绝所有历史 session actor；历史 executor/Planner actor 语义不变。

事件写事务 live 读取 session active 状态、card 当前 session 指针、card/session track、
Assistant role/marker、Assistant contract、resumable mode；payload provider 经 typed
`AgentProvider` 解码并与 typed `WorkerProviderKind` 比较。缺失、未知、stale、cardless、
不一致、跨 Track 均拒绝。通用 surrogate 复用既有 Assistant 授权且拒绝非 Assistant
role；Planner gate 与 root 规则不放宽。

能力由既有 `harness::backend::PlannerBackend` owner 的 typed metadata 声明，
实现位于 [backend metadata](../crates/calm-server/src/harness/backend_metadata.rs)。
注册列表同供已有 availability cache，不能维护另一份 Assistant provider allowlist。
创建/恢复 binding 消费 managed-conversation 能力；catalog 消费 CatalogProtocol；prompt
消费 ToolDiscovery，具体 discovery 文案和语法留 backend owner。既有 compact 能力入口
也消费同一 metadata。

`GET /api/agent-providers/managed?refresh=true` 返回动态注册 provider 的
`{ availability, capabilities }[]`；availability 保持已有 status/reason/authentication_notice，
capabilities 必填 `managed_conversation/images/manual_compact/history_replacement/requires_ready_availability`。
`requires_ready_availability` 由 backend 声明初次创建准入：Codex 保留 daemon-ready 但
account/auth probe unavailable 时仍可创建的既有语义；catalog 失读不阻断合法默认创建。
冻结请求先核对已有派生卡，再原 body/key 重试；刷新 metadata 不撤销已派发请求的
准入，版本协商与失联保护仍生效。managed 菜单展示通用 provider 标识，无必填 UI
专属表项。backend 另声明 `requires_codex_daemon`，供 boot/deferred 选择共享消费。
图片输入由 typed provider 的 `images` 和 workspace fence 共同准入，run/upload/input
使用同一 owner 判据，在上传、恢复、永久绑定和队列副作用前拒绝；历史附件读取继续
遵循既有 workspace fence。UI 将声明的 images 与 run capability 取交集。
前端只同步生成 schema 和 core typed operation；menu/draft/router 由下一任务消费。
server 的 `assistantProviders` 能力协商仍是新客户端提交显式 provider 的前提。

新 managed SQL 以 contract 和 required typed provider 选择，不列举 Assistant providers。
0163 尚未发布，可修订：provider 列只约束非空，已知值由 typed reader/writer owner
验证；Assistant CHECK 只排除 terminal 并要求 resumable。旧迁移内历史封闭值不修改；
OpenCode 的专属 protocol constraint 仍在其 backend contract 范围。

同时修改 [ACP turn admission](../crates/calm-server/src/acp_planner/session.rs) 与
[durable submission SQL](../crates/calm-truth/src/db/sqlite/acp_submission.rs)，允许验证过的
Assistant contract；继续使用 [ACP journal recovery](../crates/calm-server/src/acp_planner/recovery.rs)。
`sending/unknown` receipt 永不授权重发 native prompt；resume 不推导外部 session
路径/命名，使用 provider 返回 ID。新 actor 的历史事件读取兼容须随 wire/golden 扫描，
需要时提升 sync event 合同，不放宽 Planner fence。

## 实施阶段与验证矩阵

1. 在真实 POST 加非 Codex 创建复现并确认行为红（不能拿编译失败充数）；冻结旧
   operation/hash 的 production replay 样本。落实 typed identity/API 协商、新迁移及
   init/projection/binding；扫 caller，迁移与兼容先绿。
2. 共享 instructions、三 backend issuance、凭据/恢复/补偿、通用 managed actor 与 ACP
   admission/journal 一起实现。先 fake CLI/process 经 production 入口验证；未通过
   权限与 unknown journal 检查前不向 UI 宣告三 provider 可用。
3. core typed create/dormant projection、app draft/catalog 组装及 Today 显式 Codex；
   生成 artifacts，浏览器与集成验证。每次修复重跑失效检查；L2 两通道检查 owner
   边界、重复策略、硬编码、外部合同和维护成本，修复后按 AGENTS 重查。

Rust 命令统一前缀（在仓库根执行）：

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked
```

下表后缀均附 `--no-tests fail --test-threads 8`，禁止空选择。
“新增”是实施时必须注册的测试，不是当前存在的 gate target；注册后先用 nextest list 确认。
调查中的 domain_api_suite/migration_suite/terminal_ws_suite 是模块；按
[Cargo 注册](../crates/calm-server/Cargo.toml) 使用 api_suite/runtime_suite 实际 target。

| 阶段 / 责任 | 精确 target/filter 后缀 | 关键断言 |
| --- | --- | --- |
| worker 红绿（新增） | `-p calm-server --test api_suite -E 'test(domain_api_suite::track_conversations::assistant_provider_)'` | 三 provider 首 turn/model/prompt/token、旧省略 Codex、hash/retry 冲突、固定 provider |
| worker 红绿（新增） | `-p calm-truth --lib -E 'test(assistant_provider_persistence::)'` | typed identity roundtrip、root 不变、损坏拒绝、无 token 继承 |
| worker 红绿（新增） | `-p calm-server --test runtime_suite -E 'test(migration_suite::assistant_provider_migration::)'` | 真旧 DB 升级、历史/关系/queue/index/trigger 无损、原子失败 |
| worker 红绿（新增） | `-p calm-server --test planner_harness_suite -E 'test(assistant_provider_recovery::)'` | boot/deferred/restart/lazy/delete rollback/failed carrier、root、旧 token、ACP unknown 不重发 |
| 失败 native start 凭据回归 | `-p calm-server --test planner_harness_adapters -E 'test(assistant_provider_)'` | 正常 required ManagedAssistant/Codex thread/start 捕获 native token；协议失败后原 session/thread/current/队列 ID 内容恢复，旧 token 重新 MCP initialize + prose write；成功 successor 不继承且旧 token 拒绝 |
| 现有回归 | `-p calm-server --test planner_harness_adapters assistant_thread_start_carries_mcp_config_and_the_assistant_prompt` | 原 Codex prompt/MCP |
| 现有回归 | `-p calm-server --test api_suite domain_api_suite::track_conversations::`；`-p calm-server --test planner_harness_suite planner_harness_boot_recovery::` | 创建/恢复旧路径与 Planner/PlainChat 隔离 |
| 扩展现有权限用例（三 provider 参数化） | `-p calm-server --test mcp_core_suite mcp_assistant_tool_gate::`；`-p calm-server --test mcp_integration_suite mcp_assistant_report_channel::`；`-p calm-server --test api_suite terminal_ws_suite::assistant_terminal::` | 真实 minted token + event gate：允许 prose/task-worker terminal，拒绝 plan/verdict/close/open terminal/跨 Track；managed actor 验证 |
| 现有 schema inventory | `-p calm-server --test api_suite domain_api_suite::head_schema_fixture::` | head 迁移完整；模块在 domain_api_suite，实际 target 为 api_suite |

只 mutation-verify load-bearing invariant：生产 root repoint 加 Assistant、managed token
继承保护删 Assistant、通用 Assistant actor 改回 Planner、恢复 row-card equality 删除、
事件事务 typed provider equality 删除、新 contract 旧 actor 拒绝删除，
分别单因素执行。实施前按最终调用链预测完整 expected-red 名字集合，跑所有可观察
目标/filter，与完整实际红集比较；缺失/额外红必须解释。独占可恢复 worktree，安全
恢复后复绿并查 residue，不建立永久测试名字名单。

| 检查 | owner 与准确命令 / 范围 |
| --- | --- |
| 原设计任务（历史） | kernel gate 仅 `scripts/local-ratchet-gates.sh`；链接存在性与最终 diff 人工/脚本检查，不跑 Rust |
| 实现生成器 | worker：`env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 npm --prefix fe run gen:api`；审阅 wire.ts/openapi.json 全部产物。head_schema_fixture 是手写既有 inventory，无生成器，只追加真实迁移名 |
| prompt 内容改变时 | worker：上述 nextest 前缀增加 `REGEN_PROMPT_GOLDENS=1`，后缀 `-p calm-server --lib -E 'test(planner_card::tests::the_ordinary_assistant_prompt_matches_its_reviewed_golden) or test(planner_card::tests::the_launchpad_assistant_prompt_owns_the_report_and_keeps_the_mechanics)'`；附禁止空选择参数；生成器写入后有意 panic 为红，审阅 goldens 后去掉 regen 再绿 |
| frontend focused | 后端 worker 类型/schema 检查；下一 frontend task，在 fe：`npx vitest run --project platform-independent core/domain/conversation.test.ts core/api/schemas.contract.test.ts`；`npx vitest run --project web-dom web/src/app/router/new-conversation-composer.test.tsx web/src/app/router/track-conversation.test.tsx` |
| browser focused | 下一 frontend task，在 fe：扩展现有 `web/src/app/router/side-conversation.browser.test.tsx` 与 `web/src/app/router/conversation-restart.browser.test.tsx`，用 `npx vitest run --project browser` 加这两个路径；验证 provider 选择/冻结 retry/dormant/恢复与 capability 隐藏，默认空选择失败 |
| 实现 kernel gate | 一次声明：`scripts/local-rust-gates.sh --quick --base origin/main`、`scripts/local-ratchet-gates.sh`、`scripts/local-contract-gates.sh`，及上述新增 focused 测试 replay（拒绝空选择）；worker 只 focused 红绿与 fmt/lint 修正，不重复全套 |
| frontend 全面检查 | 下一 frontend task gate，在 fe：`npm run lint && npm run build && npm test && npm run test:browser`；仅缺依赖时 `npm ci` / `npx playwright install --with-deps chromium` |
| integrated / preview | worker：`./e2e/run.sh` 默认 tier 1；另用 fake backend 的真实 dev stack 覆盖三 provider 创建、首回复、report 写、restart/boot，现有 tier 1 不替代新流程。按 [preview workflow](development-preview.md) 用 terminal 卡启动 loopback/autologin backend 与 Vite，发布 `/next/`；真实浏览器检查键盘/手机、catalog、旧会话与恢复 notice，block_hint 交 planner |
| CI / 专用环境 | CI 广域 nextest/default-feature lib/release、runner setup 和既有 frontend gates；不把广域 suite 放本地 gate。专用环境验证实际 Claude/OpenCode CLI discovery/prompt/MCP 合同；共享 host 不跑真实 Codex E2E、`--tier 2` 或 `--all`，不能把 fake backend 结果声称为真实 CLI 通过 |

永久约束只保留 typed identity、权限与生命周期不变量，不新增测试/文件/provider 名字
pin。既有 protected-key registry 加一个键（新增 server-owned 字段才需修改）；
新增迁移维护迁移文件与既有 head fixture 两处；gen:api 更新真实生成物。
未来 provider 接入至少修改 `calm-types/src/runtime.rs` 的 AgentProvider、
`calm-types/src/worker.rs` 的 WorkerProviderKind/typed conversion，backend metadata 与
backend registration/open，以及相应 protocol owner 的 readiness/catalog/turn/MCP wiring。
现有 readiness owner 是 `agent_providers.rs`，协议实现位于 `provider/` 和 server 的
Codex/Claude/ACP 模块；start adapter 的 readiness/native session 初始化与恢复安装也是
owner 协议工作，不能声称只增加 enum 就支持新 CLI。truth 的
`session_projection_lookup.rs::resolve_session_for_thread` 是 native thread/session ID
协议路由，也需按新 backend 的公开 ID 合同接入；这不是 Assistant 专属权限路径。通用 Assistant actor、role gate、
binding、创建/恢复 SQL、catalog routing 与 prompt 消费路径无需新增 provider 分支；
生成 API 仍需运行真实 generator，新增 wire enum 要按发布兼容规则升级版本。
若新协议具有既有 metadata 无法表达的语义，先扩展 owner typed contract，而非加
Assistant identity 专例。UI 消费动态能力/catalog，不维护 provider policy 名单。
text/contract gate 各在实现 kernel gate 一次并由 CI 再查，focused replay 有意重复；
不额外建立永久名单或门禁。被修复、迁移/golden/generated 变更失效的检查重新运行。

## 发布与回滚限制

先交付完整 schema/reader/actor/能力合同，再启用新创建；数据库备份与旧数据升级验证
是发布前条件。新 contract/actor/kind 落盘后旧 binary 不能安全读取，优先 roll-forward。
应急可关闭新创建并用仍支持新 schema 的 binary 修复；不得宣称直接 binary 回退安全。
真正 downgrade 需专门停机方案、凭据撤销、所有三 provider 数据/历史的显式处理与
验证，不能把 Claude/OpenCode sessions 改写为 Codex 或删除历史/queue；未验证前禁止。

## Phase-2 committed state delivery (#2616)

L2: the phase-2 transaction owns the complete `TxOutput` checkpoint, including the
raced predecessor's runtime/status/native credential and the accumulated harvest
journal. It returns that complete output, which the start adapter adopts before
any fallible post-commit stop or transcript event. The normal driver write and
error compensation therefore receive the same committed state as boot replay.
There is no second per-key post-commit merge, generic driver policy change, or
credential authorization change. The local return tuple has no external callers;
legacy wire/hash and provider-reported native IDs remain unchanged.

The adapter test target uses one-shot operation-boundary notifications to stage
prepare with and without a predecessor, then a second production start supplies
the real native credential. Spawn failure and a post-commit event failure exercise
the driver; cancellation before/after checkpoint exercises boot replay. Assertions
cover durable undo state, queue identity/content and fresh native MCP prose writes.
Existing stale/revoked/successor checks retain the credential owner's restrictions.
New tests use the existing `assistant_provider_` prefix and one adapter module
registration; later tests need no permanent name list or new gate. Worker focused
red/green and one production output-delivery mutation precede the existing kernel
18-step gate, with no repeated worker text/contract/quick runs or schema generator.
The same raced fixture also checks the mint owner's current-carrier guard directly
after displacement (N1), without changing credential policy. A successful successor
and its idempotent replay must deliver the restored sentence exactly once.

## 最新 upstream 合同合并（#2616）

本次承接 main 的 Planner turn budget/timeout 与 closed-track hibernation 合同，
完整重放原 base 到最后完整 feature 的净 diff。hibernation owner 仍以 Planner
role/contract 判据选择，Assistant 不被自动休眠；自动恢复的 ReplaceIfOpen 与
显式 wake 的 Replace 语义保留，Assistant live binding 检查同样保留。
web compatibility 从 main 的 52 提升到 53，保留 timeout/budget 的全部 decoder
与生成 schema；sync event 仍为 29。main 最新 migration 为 0162，feature 0163
没有序号冲突，所有已发布 migration 字节冻结。

fake daemon 的一次性 thread/start failure marker 与 main 的 scripted start
响应共存，前者只在显式设置 marker 时先消费一次失败，未设置时保留 scripted
响应控制。真实 API generator 负责合并后的产物，不手写替代 schema。
phase2 完整 checkpoint、credential compensation owner 与五个真实 driver 竞争
测试保持原候选字节；原单因素 output mutation 的生产注入点、driver persistence、
补偿与 boot 观察路径未变，既有预测五红/全 scope 恢复绿证据继续适用。
验证结果以本次 worker receipt 和 kernel 原 18 步 gate 为准；此前 mobile
Settings 边缘点击间歇失败仍是未确诊记录，不改 timeout、retry、skip 或断言。
