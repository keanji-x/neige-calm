# 活动指示器 v2：任务为单位、track 阶段为框架；done 收尾；退役 Card FSM（#1743）— 设计 v2（2026-09-20）

基线：`origin/main` = `9a9bcd0c2`（工作树 `1743-design`，含 #1722 五个 PR）。所有 `path:line` 都在该基线上读取核实；4140 的数字都由 `sqlite3 -readonly ~/.local/share/neige-next/data/calm.db "<sql>"` 只读取得，命令随数字给出（§2.3）。前作：`docs/architecture/1722-track-activity-indicators.md`（v8；本文引用其 F/G 编号时原样沿用）。v1 → v2 的逐项处置在 §10。

**owner 的两条规则（每一节都受其约束）**：

1. **不要无限扩张**：抓重点和痛点；复杂问题不求完善，简单优先。只用覆盖**已观测**症状的最少机制；假设性情形写进 §8 一行「已知缺口」，不变成机制。
2. **兼容只看 4140**：不做一般兼容叙事（旧客户端、迁移矩阵、兼容窗）。兼容 = 4140 库里实际存在的行在升级后继续工作 / 被清理。

## 1. 问题与痛点

issue #1743（4140，`9a9bcd0c2`）：rail「Waiting on you」6 条、Today「6 waiting on you · 2 in progress」、一条 done track 侧条「7 items need attention」。逐表读库：6 条红点全在 `lifecycle=done` 的 track 上；13 个 `task/failed` 是 9/5–9/18 的旧 attempt，planner 早已处理；2 个 `session/failed` 与其中两个任务是同一次失败；2 条「Requires input」是升级前 FSM 写的 `AwaitingInput` 旧行（最后 hook 是 `idle_prompt`）；25 个 `state='running'` 的 PTY 进程全活在 done track 上（最老 9/14）；Today「2 in progress」是两条 planner 空闲的 `planning` track——阶段被读成活动。全库 `Notification` 18 次全是 `idle_prompt`，codex 线程状态只出现过 `idle/active`：**「等待输入」这个卡级状态在这台机器上从未发生过**。四条根因（判定缺 track 阶段维；失败无「已处理」载体；done 不收尾；卡级状态是多写者存量快照）与结论（屏幕判定模型否决、planner 当判定器否决、交互卡**没有**「等输入」状态）都在 issue 里定案，本文不重开。编排方已定：**S5（`calm.task.ask`）砍掉**；**S2 做**；**S1 老化按 planner 最后一轮**；PR 分组见 §6。

## 2. 事实表

### 2.1 内核

| # | 事实 | 位置 |
|---|---|---|
| K1 | 投影器每次唤醒/tick 从持久行**整算**：W 任务子句、S0 有资格会话、`kernel/card/status` 行、lifecycle；bus 事件只是唤醒；30 s tick 扫全部未归档 track | `track_activity.rs:233-271`（W）、`:274-378`（按后端 S 规则）、`:381-392`（lifecycle）、`:630-643`（`reconcile_all`）、`:65`（`RECONCILE_INTERVAL`） |
| K2 | `TrackRow` 只读 `lifecycle, updated_at`，**不读 `archived_at`**；`done` 只在 lifecycle 项里被 `_ => {}` 忽略，从未当过滤器 | `track_activity/sql.rs:46-50`（`TrackRow`）、`:127-135`（`track_row` 的 SELECT）；`track_activity.rs:389-392` |
| K3 | 失败项：`current_tasks.status='failed'` → `items += {failed, source:task}`、`cards[worker]=failed`；`ws.state='failed'` → `{failed, source:session}`（† 例外 `failed_session_is_finished_work` 只压掉「任务已 done 且会话铸于完成前」的会话）；两者都**没有**「已处理」判据 | `track_activity.rs:253-268,206-218,278-279` |
| K4 | S0 SELECT 列：`id, card_id, provider, state, last_thread_status, last_activity_ms, updated_at_ms, created_at_ms, mode, isolated, task_bound`——**无 `terminal_run_id`、无 `cards.role`、无 `last_turn_completed_ms`** | `track_activity/sql.rs:175-212`（SELECT 文本 `:179-194`） |
| K5 | (ii) 共享线程交互卡：`working` = `state∈{running,turn_pending} ∧ last_thread_status='active'`；`waitingOn*` → input；`systemError` → failed。(iv) claude 交互卡：FSM 行 `Working/AwaitingInput/Errored` 经活会话门。(v) 终端：永不来自会话信号 | `track_activity.rs:313-340,341-372,375-377` |
| K6 | E5（交互卡 stop hook 行）、E6（feeder 写的 `last_turn_completed_ms`）是交互卡的 unread 证据；E1 是 harness 的 | `track_activity/sql.rs:280-289`（E5）、`:290-299`（E6）、`:24-28`（E1） |
| K7 | 唤醒表含 `overlay.set kind=status`（FSM 提交后）；`WorkerSessionStatusChanged` 也是唤醒 | `track_activity.rs:648-690` |
| K8 | `card_fsm.rs`（2005 行）：消费 `Event::CodexHook/ClaudeHook`，写 `kernel/card/status`（`commit`）与 `kernel/track/any_card_needs_input`（`recompute_track_needs_input`）；内存 map，重启即空 | `card_fsm.rs:436-461,590-661,684-784,52-59` |
| K9 | `card_fsm` 的生产调用者只有 spawn 一处；另外两处只借它的**hook 名词表**：`terminal_hooks.rs:17`（`CLAUDE_WORKER_HOOKS`, `ClaudeWorkerHook`）、`routes/claude_cards.rs:247,255,311,321`（生成 claude settings）；`CODEX_WORKER_HOOKS` 的读者全在本文件内：生产映射 `codex_kind_to_state`（`:181-186`，随模块一起删）与测试（`:841,910,1034`，对照 `docker/codex-requirements.toml`） | `state.rs:1422`；`card_fsm.rs:141-176,181-186,208-317,909` |
| K10 | 测试侧 spawn FSM 的文件：`codex_permission_request_overlay.rs:140`、`claude_fsm_overlay.rs:98`、`terminal_signals.rs:2234,2314,2406`、`auth.rs:193`、`track_activity_projection.rs:521`；后三者拿 `status` overlay 当「hook 被接受/拒绝」的观测点 | `auth.rs:208-224`；`terminal_signals.rs:2430-2438,2443-2461` |
| K11 | overlay kind 注册表挡**外部写**（REST `routes/overlays.rs:191`、插件回调 `plugin_host/callbacks.rs:287`），**也驱动读侧版本过滤**：`should_skip_overlay` / `should_skip_event_for_overlay_version` 经 `max_supported_overlay_schema_version` 查表，未注册 kind 视为插件所有、原样放行；`status` 与 `any_card_needs_input` 各一项，校验器 `:255,317`，既有注册表测试 `:964,1044,1092,1875` | `calm-truth/src/validation.rs:426-465,167-168,188-191,603,645,683` |
| K12 | feeder 只写 `last_activity_ms, last_thread_status, last_turn_completed_ms`；reaper 的 busy 预门读 `last_thread_status ∈ {active, waitingOn*}`——这是它除投影器外唯一的读者 | `liveness_feeder.rs:1-17`；`reaper/mod.rs:212-216`；`session_row.rs:404-432` |
| K13 | DELETE-card 拆除：`interrupt_shared_card_active_turn`（codex 线程，best-effort）→ `reap_terminal_artifacts_with_renderer`（renderer 优雅 TERM → drop entry → 删 hook settings → pid 兜底 SIGTERM）→ 一个 tx 删 terminal 行 + card 行（`card_delete_tx` 顺带 `DELETE FROM worker_sessions WHERE card_id`） | `routes/cards.rs:2066,2082-2087,2091-2130`；`terminal_sweeper.rs:288-337`；`card.rs:569` |
| K14 | DELETE-track 拆除是删除专用 saga：`quiesce_shared_card_active_turn`（seal 线程）、`quiesce_terminal_artifacts_for_deletion`、`harness.shutdown_track`、workspace recycle、post-commit `forget_threads_for_deleted_cards`；**没有 isolated 臂**（`grep -n isolated routes/tracks.rs` = 0） | `routes/tracks.rs:3997-4033,4200-4330`；`shared_codex_appserver.rs:759-803,1663-1697` |
| K15 | PTY 退出的持久化只在 attach reader 的 `Exited` 臂：写 `terminals.exit_code/signal_killed/pty_output`（signalled ⇒ `exit_code=None, signal_killed=1`），**ephemeral** 会话经 `complete_ephemeral_session_from_terminal_exit` → `Failed`（signalled）/`Exited`；**resumable（codex）会话不动**——它的生死由 reaper 仲裁：`confirm_durable_death` 对「最后一轮已完成」的线程判 `Alive`，永不 reap；reader 在 `Exited` 臂末 `break`，**注册表项不删** | `attach_reader.rs:64-165,112-146,172`；`terminal_sweeper.rs:89-119`；`calm-provider/src/provider/codex.rs:132-150,217-236` |
| K16 | attach reader 每收一帧 `ControlReply::Output{bytes}` 就 `on_pty_chunk` + `output_capture.push`——**PTY 输出流已经经过内核**，只是没人记时间 | `attach_reader.rs:55-63` |
| K17 | renderer 注册表是进程内 map；内核重启后为空，存活的 PTY 只在 WS 打开那张卡时**惰性重挂**（probe supervisor → `spawn_terminal_for` → `EnsureProc` 幂等快路径）；注册表在投影器 spawn 之前就建好 | `terminal_renderer/mod.rs:329-344,413-418,486-491`；`ws/terminal.rs:123-215`；`state.rs:1522,1538` |
| K18 | 终端 sweeper：纯时间驱动 30 s，跳过启动首 tick；只处理「卡无活会话」的孤儿终端，拆 + **删行** + `terminal.deleted`；#1701 让有退出记录的 Terminal 卡终端跟卡走；它持有整个 `AppState` | `terminal_sweeper.rs:132,148-181,186-275`；`read.rs:952-985`；`state.rs:1727` |
| K19 | 启动对账 `reconcile_supervisor_on_boot` 只处理「行 running 但 supervisor 不认识」→ 标 `-1` exited；反向（进程活、行终态）不管；boot 顺序被 `boot_order_tests` 钉死 | `lib.rs:61-127,778-820`；`main.rs:50-59` |
| K20 | lifecycle 写者只有 `track_update_tx`（`tracks.lifecycle, terminal_at, archived_at, updated_at` 同一条 UPDATE；进终态盖 `terminal_at=now`，重开清 NULL）；`done` 只能由 planner 从 `reviewing` 推到；user 可 `done → planning/working` 重开 | `calm-truth/src/db/sqlite/track.rs:357-383,417-431`；`track_lifecycle.rs:49-91,117-160`；`calm-types/src/track_lifecycle.rs:422,434,445` |
| K21 | `task.completed/failed/gate_result` 由 dispatcher 推给**planner 卡**（`cards.role='planner'`，经 role cache）；assistant 卡不收 | `dispatcher/mod.rs:1173-1186,1394-1411,1541-1560` |
| K22 | `session_complete_tx(id, Exited|Failed)` 写 `state, updated_at_ms, completed_at_ms`（不写 `exit_interpretation`）；`running → exited` 合法；`claude/restart` 端点存在但 FE 无调用者（`grep -rln "claude/restart" fe/` 只命中 `openapi.json`） | `session_projection.rs:652-673`；`session_row.rs:212-244,579-625`；`routes/claude_cards.rs:41` |
| K23 | codex worker 的 PTY 是 `codex resume <thread> --remote <uri>` 的 TUI 观察窗；线程在共享 daemon 上 | `operation/codex_adapter/mod.rs:552-563` |
| K24 | isolated 会话无 PTY；由 executor 自己 `controller.stop` 后写 `Exited` + `WorkerSessionStatusChanged` | `isolated_codex/observe.rs:80-140` |
| K25 | `activity_window.rs` 是 Today 摘要的事件窗投影，不读 overlay（`grep -c overlay` = 0），本设计不碰 | `activity_window.rs:1-30` |

### 2.2 前端

| # | 事实 | 位置 |
|---|---|---|
| F1 | `TrackActivity.anyCardNeedsInput` 仍被解码（`trackActivityFrom` 的 `any_card_needs_input` 臂），但**无谓词读它**（#1740）；`overlayWireSchema.kind` 是 `z.string()` | `fe/core/domain/track.ts:52-57,76-80,248-250,779-806,113-121` |
| F2 | 三条谓词只读 overlay：`isWorking/needsUserAttention/hasFailed`；`activityStateOf` 优先级 `failed > attention > working > unread > quiet` | `track.ts:779-806`；`fe/core/domain/activity.ts:40-48` |
| F3 | 侧条 = `attentionNotifications(track.attentionItems, cards)`：**一 item 一行**，同卡两项两行；`source` 对无卡的 task 项是 `Task ${item.id}`（key）；worker 卡的 `title` 本来就是 key（4140：`review-r6-a`、`dispatch-e015…`） | `fe/web/src/app/router/public.tsx:2752-2808,3083-3086`；`features/track/page/public.tsx:79-96,828-893` |
| F4 | Today：`waiting` = `needsUserAttention ∨ hasFailed`；第二个数 `inProgress` = `isRunning(lifecycle)` 的行数（阶段）；分组标题「In progress」 | `features/today/public.tsx:237-240,254,408-417` |
| F5 | rail「Waiting on you」= 同一对谓词 | `app/shell/sidebar.tsx:102-104` |
| F6 | 任务的人读题目在 worker 卡自己的 `payload.goal`（D14；`attentionNotifications` 已拿着 `cards` 含 `payload`，`notificationCardLabel` 就在嗅 `card.payload`）；report task block 也有一份 `goal`/`command`，但 worker 卡的 `payload.idempotency_key` 是 `<track_id>:<key>`，不是 block 的 `key`，FE 也没有它的读者（`grep -rn idempotency_key fe/core fe/web/src` 只命中 create 头与 task-recovery） | `router/public.tsx:2753-2778,3084`；`fe/core/api/schemas.ts:772` |
| F7 | 变异清单条目形状：`mutation_id / defends / target / patch / expected_red / selection_paths / why_more_than_one` | `fe/tools/mutation/manifest.json:1241-1266` |
| F8 | oracle：`INV-APP-118`（判定只从 overlay 推导）；`capabilities-e2e.yaml:394` 的锚点 `router/public.tsx:2773-2798` 已漂到 `attentionNotifications` 身上（#1741 的既有偏差，不在本文范围） | `docs/oracle/app-dataflow.yaml:635-660` |

### 2.3 4140 库（`sqlite3 -readonly ~/.local/share/neige-next/data/calm.db "<sql>"`；2026-09-20 **11:30** 重取——v1 为 10:25，此后多了 1 条 draft track、`06ff541a` 上 1 条 done 之后的 zsh、`24dc392d` 的 planner 完成了一轮；数字会继续漂，§7 的期望一律按 sql 当场取）

| # | 数字 | sql |
|---|---|---|
| D1 | track 19 = done 14 / draft 3 / planning 2；归档 0 | `SELECT lifecycle, archived_at IS NOT NULL, COUNT(*) FROM tracks GROUP BY 1,2` |
| D2 | done/归档 track 上 `running` 会话 26 = claude 9 / codex 12 / terminal 5 | `SELECT ws.provider, COUNT(*) FROM worker_sessions ws JOIN tracks t ON t.id=ws.track_id WHERE ws.state='running' AND (t.lifecycle='done' OR t.archived_at IS NOT NULL) GROUP BY 1` |
| D3 | 26 条都经 `terminal_run_id` 挂着一条未记录退出的终端行（`ps -o pid=,stat= -p <pids>` 26 个全活：21 `Ss+` / 5 `Ss`） | `SELECT COUNT(*) FROM worker_sessions ws JOIN terminals te ON te.id=ws.terminal_run_id WHERE ws.state='running' AND te.exit_code IS NULL AND te.signal_killed=0` |
| D4 | 其中铸于 track 完成**之前**的 22 条；另 4 条是 done 之后用户在 `06ff541a` 上开的 zsh（9/14、9/18×2、9/20 11:01；都不绑任务） | `SELECT COUNT(*) FROM worker_sessions ws JOIN tracks t ON t.id=ws.track_id WHERE ws.state='running' AND t.lifecycle='done' AND ws.created_at_ms <= t.terminal_at`；后者把 `<=` 换成 `>` |
| D5 | 26 条里任务绑定 21（9 claude + 12 codex），5 条 terminal 从未绑任务 | `SELECT COUNT(*), SUM(EXISTS(SELECT 1 FROM tasks tk WHERE tk.worker_card_id=ws.card_id)) FROM worker_sessions ws WHERE ws.state='running'` |
| D6 | `current_tasks.status='failed'` 13，全在 done track | `SELECT t.lifecycle, COUNT(*) FROM current_tasks ct JOIN tracks t ON t.id=ct.track_id WHERE ct.status='failed' GROUP BY 1` |
| D7 | planner 卡会话 36 条里 `last_turn_completed_ms` 非空 **2**（`61740eec` 9/19、`24dc392d` 9/20 11:01）；按「晚于 planner 最后一轮」能老化的失败 **0/13** | `SELECT COUNT(*) FROM worker_sessions ws JOIN cards c ON c.id=ws.card_id WHERE c.role='planner' AND ws.last_turn_completed_ms IS NOT NULL`；`SELECT COUNT(*) FROM current_tasks ct WHERE ct.status='failed' AND COALESCE(ct.finished_at_ms,ct.updated_at_ms) <= (SELECT MAX(ws.last_turn_completed_ms) FROM worker_sessions ws JOIN cards c ON c.id=ws.card_id WHERE ws.track_id=ct.track_id AND c.role='planner')` |
| D8 | `kernel/card/status` 9 = AwaitingInput 2 / Idle 2 / Working 5（全是 done track 上 claude worker 卡，会话 `running`）；9 张卡的当前任务终态 = done 7 / failed 2（`a415e76d` Idle、`32acbdf9` Working）；9 行都是显式 `schemaVersion` 1 | `SELECT json_extract(payload,'$.state'), COUNT(*) FROM overlays WHERE plugin_id='kernel' AND entity_kind='card' AND kind='status' GROUP BY 1`；`SELECT json_extract(o.payload,'$.state'), (SELECT group_concat(status) FROM current_tasks tk WHERE tk.worker_card_id=c.id) FROM overlays o JOIN cards c ON c.id=o.entity_id WHERE o.kind='status'`；`SELECT kind, json_extract(payload,'$.schemaVersion'), COUNT(*) FROM overlays WHERE plugin_id='kernel' AND kind IN ('status','any_card_needs_input') GROUP BY 1,2` |
| D9 | `any_card_needs_input` 3 = value 0 ×2 / 1 ×1（`32acbdf9`）；3 行都是显式 `schemaVersion` 1（D8 第三条 sql） | `SELECT json_extract(payload,'$.value'), COUNT(*) FROM overlays WHERE plugin_id='kernel' AND kind='any_card_needs_input' GROUP BY 1` |
| D10 | `activity` 19 = attention failed 6 / none 13；6 条 failed 全在 done track | `SELECT json_extract(payload,'$.attention'), COUNT(*) FROM overlays WHERE plugin_id='kernel' AND kind='activity' GROUP BY 1`；`SELECT COUNT(*) FROM overlays o JOIN tracks t ON t.id=o.entity_id WHERE o.kind='activity' AND json_extract(o.payload,'$.attention')<>'none' AND t.lifecycle='done'` |
| D11 | harness 会话 42 = idle 24 / superseded 18，**没有一条挂 PTY**；`state='failed'` 会话 2（`32acbdf9`，codex worker-timeout） | `SELECT ws.state, COUNT(*) FROM worker_sessions ws WHERE json_extract(ws.handle_state_json,'$.mode')='harness' GROUP BY 1`；`SELECT COUNT(*) FROM worker_sessions WHERE json_extract(handle_state_json,'$.mode')='harness' AND terminal_run_id IS NOT NULL`；`SELECT COUNT(*) FROM worker_sessions WHERE state='failed'` |
| D12 | isolated 会话 4 条全 `exited`；done track 上在飞任务 0；`canceled/failed` track 0；done track `terminal_at` 无 NULL | `SELECT ws.state, COUNT(*) FROM worker_sessions ws WHERE EXISTS(SELECT 1 FROM operations o WHERE o.kind='codex-isolated-worker' AND o.target_type='card' AND o.target_id=ws.card_id) GROUP BY 1`；`SELECT COUNT(*) FROM current_tasks ct JOIN tracks t ON t.id=ct.track_id WHERE t.lifecycle='done' AND ct.status IN ('dispatched','running','verifying')`；`SELECT COUNT(*) FROM tracks WHERE lifecycle IN ('canceled','failed')`；`SELECT COUNT(*) FROM tracks WHERE lifecycle='done' AND terminal_at IS NULL` |
| D14 | worker 卡 `payload.goal`：claude 9/9、codex 19/19、terminal 0/7；非终端 worker 卡 28 张里 `payload.idempotency_key` = `track_id\|\|':'\|\|title` 的 27（= 任务绑定的 27 张），命中任何任务 `key` 的 **0** | `SELECT kind, COUNT(*), SUM(json_extract(payload,'$.goal') IS NOT NULL) FROM cards WHERE role='worker' GROUP BY 1`；`SELECT COUNT(*), SUM(json_extract(payload,'$.idempotency_key')=(track_id\|\|':'\|\|title)), SUM(EXISTS(SELECT 1 FROM tasks tk WHERE tk.worker_card_id=c.id AND tk.key=json_extract(c.payload,'$.idempotency_key'))) FROM cards c WHERE role='worker' AND kind<>'terminal'` → `28\|27\|0` |
| D13 | `Notification` hook 18 次全 `idle_prompt`；`last_thread_status` 只有 NULL 37 / active 18 / idle 22 | `SELECT json_extract(payload,'$.payload.notification_type'), COUNT(*) FROM events WHERE kind='claude.hook' AND json_extract(payload,'$.kind')='hook.claude.notification' GROUP BY 1`；`SELECT last_thread_status, COUNT(*) FROM worker_sessions GROUP BY 1` |

### 2.4 源码不变量门禁

| 门禁 | 位置 | 约束 |
|---|---|---|
| deferred-tx | `tests/cases/deferred_write_tx_invariant.rs` | **约束**：S1/S3 的新 SELECT 仍是自动提交单语句；S2 的会话写是 `write_in_tx_typed`（IMMEDIATE），进程信号在 tx 外（§4.4） |
| boot_invariants / `boot_order_tests` | `tests/cases/boot_invariants.rs`；`lib.rs:778-820` | 不约束：S2 不在 `main.rs` 加启动步骤（扫描落在 sweeper 首 tick，K18） |
| harness_turn_start / fork_guard_exemption | `tests/cases/{harness_turn_start,fork_guard_exemption}_invariant.rs` | 不约束：不发 turn、不 fork |
| terminology ratchet | `scripts/gate-1316-terminology-ratchet.sh` | **约束**：扫 `docs/`；本文避开退役词 |
| prose ratchet | `scripts/gate-prose-ratchet.sh` | 新 Rust 文件不含 ≥4 字 CJK 串或 ≥120 字符字面量 |
| sync-event lockstep | `scripts/gate-sync-event-version-lockstep.sh` | 不 bump：不新增/改名事件 kind（S3 不发新事件，边沿走进程内通道） |
| web-compat lockstep | `scripts/gate-web-compat-version-lockstep.sh:7-8,21-27`；`routes/version.rs:154`（30）、`providers/public.tsx:40`（30） | **不 bump**（§4.5 按 `version.rs:138-153` 的规则判） |
| overlay 注册表测试 | `track_activity_projection.rs:2292`（`activity_payload_passes_the_overlay_registry`）、`replay_fixtures.rs:872-897`（以 `status` 为例的 v999 重放，读 `tests/fixtures/events/schema_forward_compat.events.json:39,52`）、`validation.rs:964,1044,1092,1875`（注册表枚举测试） | S3 删两项注册后：重放测试与 JSON fixture **一致地**换一个仍注册的 kind（只改 `.rs` 不改 fixture，v999 会作为未注册 kind 被放行、断言反转）；枚举测试去掉两行 |
| oracle 锚点 | `docs/oracle/capabilities-e2e.yaml:394`（`router/public.tsx:2773-2798`） | S4 改 `attentionNotifications` 会移动这段行号；按 `fe/tools/oracle/README.md` 更新锚点，既有偏差留在 #1741 |
| `no-module-runtime-state` / dependency-cruiser | `fe/eslint.config.js:60` | `core/domain` 纯函数；S4 的折叠函数放 `core/domain`，无模块态 |

## 3. 原则

- **任务是工作单位，planner 是结果的消费者**：worker 卡的 working/failed/done 只来自 `current_tasks`（W，K1）；失败的「已处理」= planner 自那以后完成过一轮（K21 决定「planner」= `cards.role='planner'` 的会话）。
- **track 阶段是框架**：`done`/归档 ⇒ 没有东西在等人（S1），且它完成时还活着的工作区可以停（S2）。
- **状态尽量推导，不存快照**：交互卡的「在动」= 注册表里那条 PTY 最近 N 秒有输出且终端行无退出记录（K16/K17，§4.3 退出门），退出即压掉；S3 的 `last_output` **不落库**（§4.3），只有折进 overlay 的高水位是持久的。
- **不猜**：交互卡没有「等输入」状态；hooks/线程状态不再是状态来源。

## 4. 内核设计

### 4.1 S1 终态过滤 + 失败老化

**读的行**（在 K1 的整算里多两样）：`TrackRow` 加 `archived_at`（`track_row`，`sql.rs:127-135` 的 SELECT 加一列）；新增一条自动提交 SELECT **P**（planner 最后一轮）：

```sql
SELECT MAX(ws.last_turn_completed_ms) FROM worker_sessions ws JOIN cards c ON c.id = ws.card_id
 WHERE ws.track_id = ?1 AND c.role = 'planner'
```

P 取该 track **全部** planner 角色会话（含 superseded）的最大值：planner 重启换会话不会把 P 归零；列由 feeder 在 `turn/completed{completed}` 时单调写入（K12）。选它而不是 E1 的转录子查询，只因简报已定此列且 E1 是全卡（含 assistant）的 MAX，而观察只推给 planner 卡（K21）。

**规则**（在 `fold` 里，`track_activity.rs:220` 的纯函数，顺序不变）：

1. **终态过滤**：`track.lifecycle = 'done' ∨ track.archived_at IS NOT NULL` ⇒ `items = []`、`attention = 'none'`，`cards[]` 只保留 `working` 结论（`input`/`failed` 是 items 的按卡形式，一起掉）。`working` **不过滤**：W 与会话规则照算——done 上真在跑的东西不藏（S2 负责收）。Tasks 面板的 `failed` 是状态 token（`data-nc-status`），不经 overlay，照旧可见。
2. **失败老化**：`kind = failed` 且 `source ∈ {task, session}` 的项只在 `P.map_or(true, |p| item.at_ms > p)` 时计入（P 为 NULL ⇒ 没处理过 ⇒ 计入）；被老化的项连同它给出的 `cards[card] = failed` 一起不计。`at_ms` 就是今天的取列（任务项 `track_activity.rs:259`；会话项闭包 `:280-286`，`stamp_at` `:290`）。不老化的：`source = lifecycle`（track 自己的阶段）、`kind = input`（活状态）。planner 自己 wedged（`session/failed`）时 P 不会前进 ⇒ 保持红，符合直觉。
3. unread 不变：被老化的失败 `finished_at_ms` 仍进 E3 高水位；planner 那一轮本身经 E1 亮一次 unread——「planner 处理之后转为对话的 unread」由现有证据成立，不加机制。

payload 形状不变（`schemaVersion: 1`）。4140 效果：D6 的 13 条失败与 D8 的 2 条 `AwaitingInput` 全在 done track，规则 1 一次整算全部清掉（D10 的 6 条 failed overlay → none）；规则 2 在 4140 今天**不改变任何行**（D7：能老化的 0/13）——它是给 planning track 上下一次失败用的。

### 4.2 S2 done 收尾

**机制**：`terminal_sweeper::sweep`（K18）加第二臂「已完成 track 的存活工作区」；不订阅事件、不加 boot 步骤：sweeper 30 s 一跳，启动后首个真实 tick（+30 s）就是一次性启动扫，此后每 30 s 同一函数同一谓词。选它而不是 `track.lifecycle_changed` 订阅者：少一个任务、少一张唤醒表，丢事件不留漏网，代价是 done 之后 ≤30 s 才收——没人在等这件事。

**集合**（一条自动提交 SELECT）：

```sql
SELECT ws.id, ws.provider, ws.card_id, te.id AS terminal_id
  FROM worker_sessions ws JOIN tracks t ON t.id = ws.track_id
  JOIN terminals te ON te.id = ws.terminal_run_id AND te.exit_code IS NULL AND te.signal_killed = 0
 WHERE ws.state = 'running'
   AND ( (t.lifecycle = 'done' AND ws.created_at_ms <= t.terminal_at)
      OR (t.archived_at IS NOT NULL AND ws.created_at_ms <= t.archived_at) )
   AND NOT EXISTS (SELECT 1 FROM current_tasks ct WHERE ct.track_id = t.id AND ct.worker_card_id = ws.card_id
                     AND ct.status IN ('dispatched','running','verifying'))
```

- **只收「track 完成时已在跑」的会话**（`created_at_ms <= terminal_at/archived_at`，K20 的列）：done 之后用户新开的终端、重开卡、planner 新一轮起的会话不在集合里——「重开能起」由这一行谓词保证，不需要事件区分。`terminal_at` 为 NULL 的 done track（4140 无，D12）不收（fail-closed 到「不动」）。
- **不抢在飞任务**（`NOT EXISTS` 臂）：与 planner 轮/scheduler 的竞争按最简单的顺序处理——S2 不结束任何仍 `dispatched/running/verifying` 的任务的 worker；任务落定（worker 报告、gate、或 reaper 超时判 failed）后下一 tick 再收。4140 今天 0 条（D12）。
- **只收挂 PTY 的**：harness（planner/assistant）行没有 `terminal_run_id`（D11），结构上不进集合，planner 在 done track 上仍可被追问；isolated 无 PTY（K24），不在 S2 里——DELETE 路径也没有 isolated 臂（K14），4140 的 4 条 isolated 会话都已 `exited`（D12）。

**每条会话的动作**（复用 K13 的 DELETE-card 底半部，顺序固定）：

1. 与 `cleanup_terminal` 同一个 `operation_runtime.lock_for_track_delete()` 守卫 + `terminal_disposal::require_safe(Scope::Terminal)`（`terminal_sweeper.rs:187-194` 同一调用）；不安全则跳过、下 tick 再来。
2. codex：`interrupt_shared_card_active_turn`（`routes/cards.rs:156-193`，best-effort，不 seal——seal 是删除 saga 的东西）。
3. **先写后杀**：一个 IMMEDIATE tx `session_complete_tx(ws.id, Exited)`（K22；写 `state='exited', completed_at_ms, updated_at_ms`；不发事件——与 attach reader 的退出路径一致（isolated 例外：K24 发 `WorkerSessionStatusChanged`）；投影器 ≤30 s tick 读到）。
4. 有 renderer entry 则直接 `reap_terminal_artifacts_with_renderer`；没有（内核重启后，K17）先走 `ws/terminal.rs:123-215` 的惰性重挂（把 `resolve_live_renderer_from_terminal` 提为 `pub(crate)`），`Alive` 再 reap；`ChildExited` 只说明**此次未取得 renderer**（重挂失败、probe 出错也返回它，`ws/terminal.rs:194,211`），不是进程已死的证明：本 tick 不再动，交给既有孤儿臂（下段）；终端行的退出记录由 K19 的下一次 boot 对账补 `-1`。

**每提供者的效果**：claude PTY worker / 终端 PTY：进程 TERM→KILL，attach reader 写 `terminals.exit_code=NULL, signal_killed=1, pty_output`（K15），它的 ephemeral 补写找不到活会话（第 3 步已 `exited`）→ 无操作；task hook 见任务已终态 → 返回。codex 共享线程 worker：`codex resume` 观察窗被杀，线程留在 daemon 上空闲（与 DELETE 后一样，K14 只丢归属不卸线程），会话行由第 3 步写成 `exited`——不再依赖 reaper 仲裁（K15 说它永远判 `Alive`）。isolated：不涉及。**写的行**：`worker_sessions.state/completed_at_ms/updated_at_ms`（第 3 步）、`terminals.exit_code/signal_killed/pty_output(_truncated)`（reader）；`exit_interpretation` 不写（`session_complete_tx` 不写它，K22）；`terminals` 行不删——Terminal 卡的行有退出记录后由 #1701 规则跟卡走；worker 卡的终端行交给既有孤儿臂：它的 60 s 是**创建**宽限（`read.rs:966,980`：`t.created_at < now − 60 s`），不是退出宽限，4140 的行早已超过 ⇒ 第 3 步写完 `exited` 后的**下一次孤儿扫描**就删行、并经同一个 helper 再发一次 SIGTERM（今天 worker 正常退出后就是这样，K18）。

**先写后杀的理由**：杀了再写，attach reader 会把 ephemeral 会话写成 `failed`（signalled）、codex 的留 `running`——同一动作两种结局；先写则一律 `exited`，reader 的补写因找不到活会话而空过。若杀失败（进程还在、行已 `exited`）：卡已无活会话、终端行无退出记录、创建早过宽限 ⇒ 下一次孤儿扫描 SIGTERM + 删行——同一个 sweeper 就是重试。

**重开**：done 之后开新终端卡（`POST /api/tracks/{id}/terminal-cards`）、planner 新一轮派任务、`claude/restart`——铸出的会话 `created_at_ms > terminal_at`，不在集合里。今天的 FE 对已退出的 claude/terminal 卡没有「重开」按钮（K22），S2 不改变这一点。

**4140**：首个 tick 收 22 条（D4），`06ff541a` 上 done 之后开的 4 条 zsh 留着（用户自己的；这个数随用户开新终端而涨，验收按 D4 的 sql 当场取）。

### 4.3 S3 Card FSM 退役，交互卡按输出判

**删除清单**：`card_fsm.rs` 整个文件 + `lib.rs:601` 的 `pub mod` + `state.rs:1418-1422` 的 spawn；注册表里 `status`、`any_card_needs_input` 两项、两个 `OVERLAY_*_SCHEMA_VERSION` 常量、两个校验器（`validation.rs:255,317`）及其单元测试（`:1302-1331`），枚举测试 `:964,1044,1092,1875` 去掉这两行（K11）；投影器规则 (iv)、(ii) 里读 `last_thread_status` 的三支（K5）、`card_status_overlays` 读（`sql.rs:217-243`）、`TrackRows.card_status`、唤醒表的 `overlay.set kind=status` 行（K7）；E5、E6 两条证据（K6，由下文 E8 取代）。**保留**：hook 名词表（`ClaudeWorkerHook{event_name, matcher}` + `CLAUDE_WORKER_HOOKS` 去掉 `state` 字段搬到 `routes/claude_cards.rs`，它就是生成 settings 的地方；`CODEX_WORKER_HOOKS` 搬到 `routes/codex.rs`，带着 requirements.toml 的对照测试）；`/internal/{claude,codex}/hook` 路由与 `Event::ClaudeHook/CodexHook` 的落库/广播不变——`terminal_hooks`（#1704）继续从同一事件流取 planner 终端信号；feeder 全部保留（reaper 的 busy 预门还读 `last_thread_status`，K12；`last_turn_completed_ms` 是 §4.1 的 P）。`ItemSource::Card`、`CardState::Input` 与 FE `ActivityOrigin 'card'`/`CardActivity 'input'` 在 S3 后没有生产者，**留作线上词汇**（删了要动 validation/zod，得不到任何东西）。

**交互卡 `working` 的载体——进程内，不加列**：`RendererEntry` 加 `last_output_ms: AtomicI64`，attach reader 在每帧 `Output` 时写 `now_ms()`（K16 的那一处，一行）；投影器持有 `Arc<TerminalRendererRegistry>`（K17：spawn 点之前已建好，与 `HarnessRegistry` 同样以 `Arc` 克隆传入），S0 多读 `ws.terminal_run_id` 与终端行的退出记录（`LEFT JOIN terminals te ON te.id = ws.terminal_run_id`，`te.exit_code IS NULL AND te.signal_killed = 0` 读作 `pty_open`），`read_rows` 对每条从未绑任务的交互会话取 `registry.last_output_ms(terminal_run_id)` 填进 `TrackRows.output: HashMap<card_id, i64>`——与规则 (i) 读 `live_harness_sessions` 同一形状。规则：**交互 PTY 卡（`provider ∈ {claude, codex, terminal}`、非 harness、非 isolated、从未绑任务）`working` ⇔ `now − last_output_ms < N`（N = 5 s）∧ `pty_open`**。退出门是必要的：reader 在 `Exited` 臂末 `break`，注册表项与 `last_output_ms` 都留着（K15，`attach_reader.rs:172`），没有它退出后还会 working 到 N s。`attention` 无（声明）。`failed` ⇔ `ws.state = 'failed'` **只此一条**（ephemeral 会话被信号杀时 reader 写的，K15；S1 老化）。codex `codex-create` 会话是 resumable：TUI 被信号杀只留终端行 `signal_killed=1`，`ws.state` 仍 `running`、daemon 线程仍活（死亡仲裁判 `Alive`，`codex.rs:228-233`）⇒ 卡不 `failed`，也不 `working`（退出门）——**不加第二个失败族**，登记 §8 G-11。任务绑定的卡输出不参与（W 是唯一来源）。选内存而不是列：列要每帧写库（或再造一个节流写者），而值本来就只在进程活着时有意义。**内核重启后**：注册表为空，存活 PTY 的交互卡读作 `working=false`、无新 mark，直到 WS 重挂那张卡（K17）——与 FSM 时代「map 为空直到下一个 hook」（G10）同一形状，登记 §8。

**唤醒**：注册表加一个 `mpsc::UnboundedSender<String>`（terminal_id）槽，由投影器 spawn 时装入（同 `set_task_hook` 的形状，`terminal_renderer/mod.rs:368`）；attach reader 每帧 `Output` 一次原子 `swap(now)` 取回旧值，旧值早于 `now − N`（含首帧）才 `try_send(terminal_id)`（**前沿**：安静→有输出，一次）；投影器 `select!` 第三臂收 terminal_id → `terminal_get → card_get` → 整算该 track。**没有后沿臂**：有输出→安静由 30 s tick 收（K1；与 1722 G5 同一收敛上界，登记 §8 G-12），不在 reader 里包 timeout（`read_frame` 不可取消，半帧会丢），也不给投影器加截止表——v1 的 `sleep_until` 臂按规则 1 砍掉。事件仍只是唤醒：值一律从注册表当场读。

**unread（E8，取代 E5/E6）**：交互卡的 `last_output_ms` 折进 `activity_at_ms` 高水位（`recompute_track`，`track_activity.rs:537-540` 的 max 多一项）。不做显式「安静边沿一次」触发：输出持续期间 mark 持续前进但被 `working` 压住（F2 的优先级），安静后的下一次 tick（≤30 s）`working=false`、mark = 最后一次输出时刻 ⇒ 可见效果就是安静后一次 unread，打开即消；正常退出时退出门立即压掉 `working`，mark 同样已折入。内核崩溃丢的只是最后 ≤N s 未折入的 mark（§8）。

**生产者 × 状态矩阵**（每格 = working / attention / failed / unread）：

| 生产者 | 最近 N s 有输出 | 安静 | 正常退出（exit/Ctrl-D） | 被信号杀 |
|---|---|---|---|---|
| 任务绑定 worker（codex 共享线程 / claude PTY / isolated / terminal 任务） | 只看 W：任务 `dispatched/running/verifying` ⇒ working；输出不参与 / 无 / 任务 `failed` ⇒ failed（S1 老化） / 任务 `done` ⇒ E3 一次 | 同左 | 同左；会话 `exited` 无结论 | 任务已 `done` 且会话铸于完成前 ⇒ 无（† 例外不变）；否则 ephemeral 的 `session/failed`（S1 老化）；resumable codex 的 `ws.state` 不变（K15），只有 reaper 超时才 failed |
| 交互 PTY 卡·ephemeral（claude / 终端，从未绑任务） | working；`cards[c]=working` / 无 / 无 / mark 前进（被 working 压住） | 无（下一 tick） / 无 / 无 / mark 已折入 ⇒ 一次 unread | **无——立即**（退出记录 ⇒ `pty_open=false`，不等 N s） / 无 / 无（`exited`） / 已折入的 mark 照常 | 无（退出记录） / 无 / failed（`state='failed'`，`source:session`，S1 老化；S2 自己的收尾先写 `exited`，不走这里） / 已折入的 mark |
| 交互 PTY 卡·resumable（codex `codex-create`，从未绑任务） | 同上 | 同上 | 同上（TUI 退出；`ws.state` 仍 `running`，线程留在 daemon，K15） | 无（退出记录） / 无 / **无**（`ws.state` 仍 `running`，不铸 failed；G-11） / 已折入的 mark |
| planner / assistant harness | (i) 不变：`turn_pending ∧ 注册表` / — / `Wedged` ⇒ failed（P 不前进 ⇒ 保持） / E1、E2 | 同左 | — | — |
| track | 任一卡 working / items 折叠（done/归档 ⇒ none） / 同左 / `activity_at_ms` 高水位 | 其他卡仍可 working / 同左 / 同左 / 高水位不降，最后输出时刻已折入 | 同安静 / 同左 / 同左 / 同左 | 同安静 / 同左 / ephemeral 的 `session/failed` 进 items（S1 老化）；resumable 无 / 同左 |

`codex-create` 交互卡从此与终端同规则：K5 里 (ii) 的 `active`/`waitingOn*`/`systemError` 三支删除；1722 §7 B′ 的真栈行改成 §7 第四行的形状。

**4140 的 12 条旧行（D8、D9）**：**不做迁移，读者停止读取**。S3 之后没有任何代码路径按 `kind='status'` 或 `kind='any_card_needs_input'` 读 overlay（投影器读删除；FE 解码臂在 S4 删；注册表项删除后外部也写不进 `kernel` 命名空间——它本来就写不进）；读侧版本过滤在退役后把这两个 kind 当插件所有 kind **原样放行**（K11：未注册 ⇒ `max_supported_overlay_schema_version` 为 `None` ⇒ `should_skip_overlay` 不过滤），而 4140 的 12 行都是显式 `schemaVersion` 1（D8/D9），所以它们只作为不透明行出现在通用 overlay 列表接口里，`trackActivityFrom` 对未知 kind 跳过（F1）——「不透明」的全部含义就是这一句。owner 想清就一条 `DELETE FROM overlays WHERE plugin_id='kernel' AND kind IN ('status','any_card_needs_input')`，不进迁移。

### 4.4 事务形状

S1：与今天同形——全部 SELECT（含新增的 P 与 `archived_at` 列）是自动提交单语句，无共享快照，两条之间的写由下一 tick 修正（G12）；只有变化才 `write_with_events_typed` 一个 IMMEDIATE 写。S2：每条会话一个 IMMEDIATE tx（`write_in_tx_typed(session_complete_tx)`，无事件），进程信号与 renderer 重挂在任何 tx **之外**（与 `delete_card` 把 reap 放在 tx 之前同一顺序，K13）。S3：输出时间不写库；overlay 写不变。三片都不开 deferred 事务（§2.4）。

### 4.5 兼容（只看 4140）

| 4140 现有行 | 升级后第一次经过时 |
|---|---|
| 14 条 done track（D1） | 投影器启动扫（`reconcile_all`）整算：6 条 `attention=failed` 的 overlay 改写为 `none`、`items=[]`（D10 → 0/19）；13 条本就 none 的无变化（比较后不写） |
| 26 条 `running` PTY 会话（D2–D5） | sweeper 首 tick（启动 +30 s）：22 条按 §4.2 收——9 claude worker、12 codex `resume` 观察窗、1 zsh——行写 `exited`，进程收到 TERM，终端行记 `signal_killed=1`；`06ff541a` 上 done 之后开的 4 条 zsh **不收**。codex 的 12 条线程留在 daemon 上空闲 |
| 13 条 failed attempt（D6） | 全在 done track ⇒ 侧条/rail 不再计入；Tasks 面板 token 照旧；老化规则对它们无作用（D7） |
| 2 条 `AwaitingInput` overlay（D8） | S1 起被终态过滤挡住（都在 `32acbdf9`，done）；S3 起无人读，读侧过滤按未注册 kind 放行（§4.3）；行不动 |
| 5 `Working` + 2 `Idle` overlay（D8） | 同上：S3 前只对从未绑任务的活会话有意义（这 7 张卡都绑任务、任务已终态——5 done / 2 failed，D8 第二条 sql ⇒ Working 要 `!task_bound`、Idle 是 `_ => {}`，今天就不起作用），S3 后无人读 |
| 3 条 `any_card_needs_input`（D9） | S3 起无写者；S4 起 FE 不解码；行不动 |
| 2 条 `failed` 会话（D11） | done track ⇒ 过滤；它们是 K3 的 `session` 项，和同一失败的 `task` 项一起消失 |
| 19 条 `activity` overlay | 原地改写（同一 upsert），无新行 |
| `apiVersion` 10 / `webCompatVersion` 30 | **不 bump**。`version.rs:138-153` 的规则只在响应新增必填字段、路由删/改名、事件枚举值让 zod union 拒绝时 bump；S1–S4 都不做这些：overlay payload 形状不变，`kind` 在 FE 是 `z.string()`（F1），删掉的只是两个不再有写者的 kind 和一个 FE 内部字段。旧 bundle 对新内核、新 bundle 对旧内核都能解析 |

## 5. 前端设计（S4）

- **侧条按卡折叠**：`core/domain` 新增纯函数 `foldAttentionByCard(items) → ActivityItem[]`——按 `cardId` 分组，一组一行，`at_ms` 取最大；`cardId=null` 的项按 `(origin,id)` 各自一行。不需要 `failed > input` 的优先序：S3 之后没有生产者发卡级 `input` 项（K5 (ii)/(iv) 删除；lifecycle 项 `cardId=null`），同卡只会是 `failed`。`attentionNotifications`（F3）改吃折叠后的列表，`key` 变 `cardId ?? origin:id`。**标题**：有卡的项用 `card.payload.goal` 首行（≤60 字；`attentionNotifications` 已拿着 `cards` 含 `payload`，F6；4140 claude 9/9、codex 19/19 有 `goal`，D14），没有 `goal` 的（终端卡 0/7）回落 `notificationCardLabel`（Planner / 卡题 / kind）；`cardId=null` 的 task 项保持 `item.id`（key）。**不**按 `payload.idempotency_key` 联 report block：它是 `<track_id>:<key>`，命中任务 key 0/28（D14），联了永远回落到 key。消息文案按折叠后的 `kind` 一句。done track 上侧条空由 S1 保证（`items=[]`），FE 不加判断。
- **Today**：第二个数改为 `shownTracks.filter(isWorking).length`，词改「working」（`activityNameBit('working')` 的同一个词）；「In progress」分组改名「Open」，成员仍按阶段（`isRunning(lifecycle) ∧ ¬needsPerson`）——分组是阶段，数字是活动，两者不再共用一个词。读这两个词的地方全改：`e2e/draft-recovery.spec.ts:33,41`（`/\d\s*in progress/`，fe-e2e job 跑）、`router/read-fallbacks.contract.test.tsx:68,73`（`'1in progress'`；夹具是 `planning` track + `overlays ok([])` ⇒ 改后数字 0、词 `working`）、`features/today/public.test.tsx:56-70`（`'2in progress'` + 标题）、`router/today-activity.test.tsx:77`（标题）、`today/public.tsx:12,232,370,409` 与 `core/domain/track.ts:773` 的注释、`today/README.md:129`。**不碰** `core/view/panel-groups.ts:6`、`router/task-recovery.test.tsx:243`、`track/page/panel-sticky.browser.test.tsx:125`——那个「In progress」是 Tasks 面板的分组，不是 Today。
- **rail「Waiting on you」**：不改代码（F5 读同一对谓词），随 S1 收缩。
- **`anyCardNeedsInput` 死代码删除**：`TrackActivity.anyCardNeedsInput` 与 `NEUTRAL_ACTIVITY` 里的键、`trackActivityFrom` 的 `any_card_needs_input` 臂（F1）、`track.test.ts:199-260` 的两条「忽略 retired flag」测试、`row/public.test.tsx:175`、`page/public.test.tsx:189`、`read-fallbacks.contract.test.tsx:371` 的夹具键、`features/today/README.md:349` 的一句、`manifest.json:1251,1253` 两条 `expected_red` 标题；`manifest.json:1270`——`s2a-activity-overlay-plugin-gate-dropped` 的 patch 以 `any_card_needs_input` 臂为上下文行，臂删掉后 `git apply --check`（`tools/mutation/fixture-e2e.mjs:28`）不过 ⇒ 该 patch **重新生成**，不是改标题；注释里的 `anyCardNeedsInput`：`app/providers/queries.ts:990`、`core/domain/track.ts:52,236,765`。内核写者/注册在 S3。
- **`data-nc-*` 载体**：`data-nc-needs-input-notice` 的 `<li>` 从「一 item 一行」变「一卡一行」，`data-nc-notification-state` 取折叠值；Today 页头无 `data-nc-*`，改的是文案与计数；`data-nc-activity` 不变。
- **oracle**：`INV-APP-118` 的 statement 去掉 `anyCardNeedsInput` 一词并加一句「侧条按卡折叠、Today 第二个数是 working 计数」，`source/authoritative_test` 行号随改；`capabilities-e2e.yaml:394` 的锚点随 `router/public.tsx` 行号移动而更新（§2.4）。

## 6. 切片表

| 切片 | 文件 | 测试（单元 + 不变量） | 必红变异 → 指名的红测试 | 必红 / 必绿 对 |
|---|---|---|---|---|
| **S1** 终态过滤 + 老化 | `track_activity.rs`、`track_activity/sql.rs`（`TrackRow.archived_at`、`planner_last_turn` SELECT） | `tests/cases/track_activity_projection.rs` 新增：`done_track_failed_attempt_is_quiet`、`archived_track_failed_attempt_is_quiet`、`reopened_track_failed_attempt_is_red_again`（`done → planning` 经 `track_update_tx`）、`done_track_running_task_is_still_working`、`failed_after_planner_turn_is_red`、`failed_before_planner_turn_is_quiet`（planner 会话由 fixture 直接 `UPDATE last_turn_completed_ms`）、`null_planner_turn_keeps_failed_red`、`superseded_planner_turn_still_ages`（旧 planner 会话的 P 生效）、`lifecycle_failed_item_is_not_aged`、`aged_failure_drops_its_card_verdict`；改：`done_task_worker_signal_killed_is_quiet` 等现有用例的 track 保持非终态 | 去掉终态过滤 → `done_track_failed_attempt_is_quiet`；把 `archived_at` 从过滤里去掉 → `archived_track_…`；把过滤写宽到 `working` → `done_track_running_task_is_still_working`；P 为 NULL 时不计入（`map_or(false, …)`；「当作 0」不是变异——`at_ms > 0` 对每个真实戳都成立，测试照绿）→ `null_planner_turn_keeps_failed_red`；P 只取当前会话 → `superseded_planner_turn_still_ages`；老化写宽到 lifecycle → `lifecycle_failed_item_is_not_aged` | 必红：done + failed attempt → `attention='none'`；必绿：同一 track `done → planning` → `failed` 回来。必红：`at_ms ≤ P` → 无项；必绿：`at_ms > P` → 红。必红：done track 上任务 `running` → `working=false`；必绿：→ `working=true` |
| **S2** done 收尾 | `terminal_sweeper.rs`（第二臂 + 集合 SELECT）、`ws/terminal.rs`（`resolve_live_renderer_from_terminal` → `pub(crate)`；`interrupt_shared_card_active_turn` 已是 `pub(crate)`，`routes/cards.rs:156`） | `tests/cases/terminal_signals.rs` 的 Harness（真 PTY）新增：`done_track_running_pty_session_is_torn_down`（zsh 终端卡，`track_update_tx` 推到 done，`sweep()` 一次 → 会话 `exited`、终端行 `signal_killed=1`、pid 不在）、`session_started_after_done_survives_sweep`、`in_flight_task_worker_survives_sweep`、`harness_session_is_never_in_the_sweep_set`（idle harness 行；**结构断言**，不配必红：harness 行被 `state='running'` 和 `terminal_run_id IS NULL` 两个条件各自排除，没有单因素补丁能让它进集合）、`archived_track_session_is_torn_down`、`sweep_is_idempotent`（第二次 `sweep()` 无写）；`reattach_then_reap_after_registry_reset`（清空 renderer 注册表模拟重启） | 去掉 `created_at_ms <= terminal_at` → `session_started_after_done_survives_sweep`；去掉 `NOT EXISTS` 臂 → `in_flight_task_worker_survives_sweep`；先杀后写 → `done_track_running_pty_session_is_torn_down` 的 `state='exited'` 断言 | 必红：done track 上铸于 `terminal_at` 之前的 `running` PTY 会话，一次 sweep 后仍 `running` 或进程仍在；必绿：铸于之后的会话 sweep 后仍 `running`、进程在。必红：任务 `running` 的 worker 被收；必绿：任务 `done` 的 worker 被收 |
| **S3** FSM 退役 + 输出判 | 删 `card_fsm.rs`、`tests/cases/{claude_fsm_overlay,codex_permission_request_overlay}.rs`；改 `lib.rs`、`state.rs`、`validation.rs`（两项注册 + 两常量 + 校验器 `:255,317` + 单元 `:1302-1331` + 枚举测试 `:964,1044,1092,1875`）、`tests/fixtures/events/schema_forward_compat.events.json:39,52`（与 `replay_fixtures.rs:872-897` 一致换 kind）、`track_activity.rs`、`track_activity/sql.rs`、`terminal_renderer/{mod,attach_reader}.rs`、`routes/claude_cards.rs`、`routes/codex.rs`、`terminal_hooks.rs:17` 的 `use`；`auth.rs`/`terminal_signals.rs` 的 hook 观测点从 `status` overlay 改为持久的 `claude.hook`/`codex.hook` 事件行（`h.persisted_hook_events()` 已有）；`replay_fixtures.rs:872-897` 换 kind；`track_activity_projection.rs` 删 K5/K6 相关 10 条用例（`interactive_codex_card_active_is_working`…`interactive_card_turn_end_lights_unread`），`wakeup_table_resolves_every_row_of_the_design` 去掉 `overlay.set` 行；注释残留一并删：`actor.rs:23`、`routes/overlays.rs:67,81`、`plugin_host/manifest.rs:987,3191`、`dispatcher/mod.rs:529`、`terminal_sweeper.rs:124`、`calm-truth/src/event_bus.rs:140`、`calm-truth/src/db/mod.rs:24`、`validation.rs:25,178,191`（全部核过是注释） | 新增（真 PTY Harness）：`interactive_terminal_output_is_working`（`printf` 循环 → 前沿唤醒后 `working=true`、`cards[c]=working`）、`quiet_then_tick_is_unread_once`（安静后调一次 `reconcile_all` → `working=false`、`activity_at_ms` = 最后输出时刻）、`exited_interactive_card_is_not_working`（`exit` 后 `last_output_ms` 仍新鲜 → `working=false`）、`task_bound_worker_output_is_ignored`、`interactive_codex_card_uses_output_not_thread_status`（fixture `UPDATE last_thread_status='active'` 不再 working）、`signal_killed_ephemeral_card_is_failed`、`signal_killed_codex_interactive_card_is_not_failed`（resumable：`ws.state` 仍 `running` ⇒ 无 failed、无 working）、`registry_empty_after_restart_reads_quiet`；`validation.rs` 单元：`status_and_any_card_needs_input_are_not_registered`（证注册表，不证全称否定）；**门禁**（全称否定要扫）：`grep -rn "any_card_needs_input\|kind == \"status\"\|kind='status'\|kind: \"status\"" crates/*/src fe/core fe/web/src` 在 S3+S4 之后**只剩**四处以 `status` 为任意插件 kind 的测试夹具（`dispatcher/tests.rs:2411,2424`、`calm-truth/src/db/sqlite/track_detail_order_tests.rs:186`、`calm-types/src/event.rs:3004`，`plugin_id` 都不是 kernel），多一处即残留；`routes/claude_cards.rs` 的 `settings_registers_exactly_the_hook_table` 随表搬家 | reader 不写 `last_output_ms` → `interactive_terminal_output_is_working`；去掉退出门 → `exited_interactive_card_is_not_working`；输出规则不排除任务绑定卡 → `task_bound_worker_output_is_ignored`；留下 (ii) 的 `active` 支 → `interactive_codex_card_uses_output_not_thread_status`；给 resumable 会话按 `signal_killed` 铸 failed → `signal_killed_codex_interactive_card_is_not_failed` | 必红：交互终端 N s 内有输出 → `working=false`；必绿：安静后一次 tick → `working=false` 且 mark = 最后输出。必红：`exit` 后 N s 内仍 `working=true`；必绿：退出前一刻 `working=true`。必红：任务绑定 worker 的 PTY 输出让它 `working`（任务已 done）；必绿：任务 `running` 时 `working=true`（W）。必红：`kernel/card/status` 有新行写入；必绿：hook 事件行仍落库 |
| **S4** FE | `core/domain/activity.ts`（`foldAttentionByCard`）、`track.ts`、`router/public.tsx`、`features/track/page/public.tsx`、`features/today/public.tsx`、上列测试与夹具、`docs/oracle/app-dataflow.yaml`、`capabilities-e2e.yaml`、`manifest.json` | `activity.test.ts`：折叠全序（同卡 task+session 两 failed → 一行、`at_ms` 取大；无卡项各自保留；两卡两行）；`router/track-conversation.test.tsx:446-470`（同卡 task+session 两 failed + 两条无卡项）改为「Worker 一行、计数 3」，`:414-436`（planner input + worker failed，两卡）保持两行当孪生；标题测试：worker 卡项显示 `payload.goal` 首行而非 key，终端卡（无 `goal`）仍是卡题；`today-activity.test.tsx`：planning + idle planner → 第二个数 0，dispatched 任务 → 1，分组标题「Open」 | 登记 `manifest.json`：`s4-aside-not-folded-per-card`（patch：`foldAttentionByCard` 直接返回输入 → red：折叠测试 + 侧条计数测试）、`s4-today-second-count-from-phase`（patch：改回 `isRunning` → red：today 计数测试）、`s4-aside-title-falls-back-to-key`（patch：去掉 `payload.goal` 读取 → red：标题测试）；`manifest.json:1270` 的 patch 重新生成（§5） | 必红：同卡两项两行；必绿：两卡两行。必红：Today 第二个数计入 planner 空闲的 planning track；必绿：计入有 `dispatched` 任务的 track |

**PR 分组**：S1+S2 一个 PR（估 S1 ≈ 150 行实现 + 300 行测试，S2 ≈ 120 + 250，合计 ≲ 850 < 1k）；S3 一个 PR（净减行，但触及 ~15 个文件，单独评审）；S4 一个 PR。顺序 S1+S2 → S3 → S4；S3 先于 S4 是因为 FE 删解码臂前内核先停写。每片门禁沿用 1722 §6 的命令（Rust：fmt / clippy `-D warnings` / nextest `--features calm-server/codex-e2e --test-threads 8` / 三个 gate 脚本；FE：`npm run lint && build && test && test:browser && e2e` + oracle + `test:mutation:{fixtures,plan,run}` 在脱离的私有工作树里跑）；S3 的真 PTY 用例走 `terminal_signals.rs` 的 Harness，不需要 codex-e2e。

## 7. 4140 验收

| 行 | 动作 | 观测 | 期望 |
|---|---|---|---|
| 1（S1） | 升级、内核启动扫完成（≤30 s） | 新浏览器 rail：`document.querySelectorAll('[data-nc-activity="attention"],[data-nc-activity="failed"]').length`；Today 页头第一个数；打开 `32acbdf9` 页面 `document.querySelector('[data-nc-needs-input-notice]')`；库：`SELECT COUNT(*) FROM overlays WHERE kind='activity' AND json_extract(payload,'$.attention')<>'none'` | 0；0；`null`；0（升级前 6） |
| 2（S1） | 在「股票持仓表」（planning）派一个真任务；再派一个让 worker 故意 `calm.task.fail` 的任务；然后给 planner 发一句让它回一轮 | rail 行与 TASKS 行的 `data-nc-activity`；库：`SELECT MAX(ws.last_turn_completed_ms) FROM worker_sessions ws JOIN cards c ON c.id=ws.card_id WHERE ws.track_id='<id>' AND c.role='planner'` 与该失败行的 `finished_at_ms` | 派发期 `working` → 完成后一次 `unread`（打开即消）；失败 → `failed`；planner 回轮后 P > `finished_at_ms` → 红消失，rail 一次 `unread` |
| 3（S2） | 升级前按 D4 的两条 sql 取 A = 铸于完成前的集合（含 pid，`… AND ws.created_at_ms <= t.terminal_at`）、B = `SELECT COUNT(*) … WHERE ws.state='running' AND t.lifecycle='done' AND ws.created_at_ms > t.terminal_at`；升级后 ≤60 s 再取 | `SELECT COUNT(*) FROM worker_sessions ws JOIN tracks t ON t.id=ws.track_id WHERE ws.state='running' AND t.lifecycle='done'`；对 A 的 pid `ps -o pid= -p <pid>`；然后在一条 done track 上 `POST /api/tracks/{id}/terminal-cards` 开一个 zsh，等两个 tick | = B（验收时刻的值；11:30 为 4，A 为 22）；A 的 pid 全部消失、B 的仍在；新开的 zsh 60 s 后仍 `running`、进程在 |
| 4（S3） | 在一条 planning track 手开 zsh 终端卡，跑 `for i in $(seq 1 40); do echo $i; sleep 1; done`（40 s 有输出，跨过一个 30 s tick；`sleep 5; ls` 的毫秒级输出只能看到事后那一次 unread） | rail 行 `data-nc-activity`；结束后 `SELECT COUNT(*) FROM overlays WHERE plugin_id='kernel' AND entity_kind='card' AND kind='status'` | 输出期间 `working`（前沿唤醒 → overlay.set → 重取，≤ 数秒出现）；安静后 ≤30 s 灰环消失、`unread` 一次、打开即消；`status` 行数仍为 9（零新增） |

## 8. 已知缺口（登记，不加固）

- **G-1 重启后交互卡失明**：注册表空到 WS 重挂为止（K17），期间交互卡 `working=false`、无新 mark；打开那张卡即恢复。4140 上受影响的存活 PTY 都在 done track（S2 收掉）。
- **G-2 崩溃丢最后 ≤N s 的 mark**：`last_output_ms` 不落库，只有整算时折进 overlay；进程内 crash 前最后一段输出不亮 unread。
- **G-3 codex 交互 TUI 的空闲重绘**：若某个 TUI 空闲时周期性重绘（本文未实测 claude/codex 空闲帧），它会一直 `working`——那确实是输出；不做「屏幕判定」。
- **G-4 S2 的 ≤30 s 与 tick 级 unread**：done 之后最多 30 s 才收；G5 的收敛上界不变。
- **G-5 `canceled` track 不在终态过滤/收尾里**（issue 只说 done/归档；4140 为 0，D12）；`lifecycle=failed` 的 track 也不在（它自己就是 failed 项）。
- **G-6 老化的载体是 feeder 列**：`NEIGE_REAPER_DISABLED` 下 feeder 不起（G9）⇒ P 不前进 ⇒ 失败不老化（方向：红多）；G15 的 stale `TurnCompleted` 也会盖 P（daemon 上确实完成了一轮）。老化的两个方向各一条：G-6 红多，G-13 红少。
- **G-7 S2 不抢在飞任务**：done track 上卡住的 worker 由 reaper 超时（900 s）判 failed 后下一 tick 才收；4140 为 0（D12）。
- **G-8 S2 后 `06ff541a` 的 4 条 zsh 留着**（11:30 计）：done 之后用户自己开的，按规则不属于「完成时的工作区」。
- **G-9 planner 在 done track 上开的终端**：铸于 `terminal_at` 之后 ⇒ 不收（同 G-8 的规则），planner 用完要自己 `calm.terminal.release`。
- **G-10 1722 的 G1、G4、G5、G12、G13、G15、G16、G18、G20、G21 原样成立**；G7、G10、G14、G17 随 FSM/线程状态读取一起消失。
- **G-11 codex 交互 TUI 被信号杀不铸 failed**：resumable 会话的 `ws.state` 留 `running`（K15），卡只是不再 working（退出门）；线程在 daemon 上仍可 `codex resume`。真要一个「TUI 没了」的红点需要第二个失败族（按终端行的 `signal_killed` 铸），规则 1 不加。
- **G-12 安静后 ≤30 s 的 working 尾巴**：没有后沿臂（§4.3），有输出→安静由 tick 收；与 1722 G5 同类。
- **G-13 落在 planner 轮中间的失败被那一轮老化**（红少）：任务在 t0 失败时 planner 轮 T1 在飞，`observe_envelope` 只入队（`harness/run_loop.rs:687-716`），`maybe_issue_turn` 要 `can_issue_turn()` 才排空（`:3688-3800`）⇒ 观察等到 T2；T1 在 t1 > t0 完成，feeder 写 P = t1 ⇒ `at_ms ≤ P` ⇒ 红只亮 t0..t1 就消失，T2 的 E1 之后亮一次 unread。4140：有转录覆盖的 4 条失败里 0 条落在轮内（`WITH f AS (failed, fa) , tc AS (planner turn/completed), ts AS (MIN item/started per turn) … ts.first_item_ms < f.fa AND tc.end_ms > f.fa` → 0/4），不是已观测缺陷。

## 9. 不在范围

S5 `calm.task.ask`（砍掉：该状态在 4140 从未发生）；屏幕判定模型；服务端回执；planner 阶段推进 nudge；main 上本来就偏的 oracle 锚点（#1741）；`kernel/card/status` 与 `any_card_needs_input` 旧行的删除迁移（§4.3：无人读即可）；`activity_window.rs`（K25）；FE 的「重开已退出卡」按钮（K22，今天没有，本设计不加）。

## 10. 处置历史（第一轮 v1 → v2）

两个通道（A、B）第一轮都判 REVISE；每条：发现 → 结论 → 改了什么。所有引用与 sql 在折入前都重新读过（§2.3 为 11:30 重取）。

| # | 发现（通道/等级/一句） | 结论 | 改了什么 |
|---|---|---|---|
| 1 | B MAJOR-1：S3 把 PTY 退出与 resumable 会话终态混为一谈——codex TUI 被杀 `ws.state` 仍 running，矩阵承诺的 failed 不会出现；正常退出后 reader 不删注册表项，会 working 到 N s | 接受 | §4.3 规则加退出门 `pty_open`（S0 `LEFT JOIN terminals`），`failed` 只剩 `ws.state='failed'`；矩阵拆 ephemeral / resumable 两行、补 track 行三格、正常退出列改「立即」；§8 G-11；§6 S3 加 `exited_interactive_card_is_not_working`、`signal_killed_codex_interactive_card_is_not_failed` |
| 2 | A MINOR-5 / B MINOR-4：后沿 `sleep_until` 臂与截止表是过度扩张 | 接受（规则 1） | §4.3 砍后沿臂，前沿改成一次原子 swap + `try_send`；§7 行 4 改「安静后 ≤30 s」；§8 G-12；§6 S3 的后沿用例与变异换成 tick 用例 |
| 3 | A BLOCKER-1：S4 标题按 `payload.idempotency_key` 联 report block，4140 上 27/27 是 `<track_id>:<key>`、命中任务 key 0/28，永远回落到 key | 接受（sql 重跑 `28\|27\|0`） | §5 标题改读 `card.payload.goal` 首行（D14：claude 9/9、codex 19/19、terminal 0/7 回落）；F6 改写；§6 S4 标题测试与 `s4-aside-title-falls-back-to-key` 改指 `payload.goal` |
| 4 | A MAJOR-1：S4 清单漏 `manifest.json:1270` 的 patch 上下文、`draft-recovery.spec.ts:33,41`、`read-fallbacks.contract.test.tsx:68,73`、`today/public.test.tsx:56-70`；折叠用例应是 `:446-470` | 接受（逐条核过；另核出 `today-activity.test.tsx:77`、`track.ts:773`） | §5 删除清单与「In progress」读者清单，含三处「不碰」（Tasks 面板分组）；§6 S4 用例行号 `:446-470`，patch 重新生成 |
| 5 | A MAJOR-2 = B MAJOR-2：S1「P 当作 0」不老化任何行，指名测试照绿；S2「集合加上 harness」没有单因素补丁 | 接受 | §6 S1 变异改 `map_or(false, …)`；§6 S2 删该必红承诺，测试标为结构断言并说明两个排除条件 |
| 6 | B MINOR-1：注册表退役漏校验器、既有注册表测试、JSON fixture；K11 漏读侧版本过滤 | 接受（`validation.rs:603,645,683` 核过；12 行显式 `schemaVersion` 1） | K11 改写；§2.4、§4.3 删除清单、§6 S3 文件列加 `validation.rs:255,317,964,1044,1092,1302-1331,1875` 与 fixture `:39,52`；§4.3/§4.5 写明退役后按未注册 kind 放行 |
| 7 | A MINOR-6：「无人读」是全称否定，需要 sweep；注释残留未列 | 接受 | §6 S3 加 grep 门禁并写出**期望残留集**（四处插件 kind 测试夹具，非「空」）；注释残留 12 处列入 S3 删除（A 写的 `calm-server/src/db/mod.rs:24` 实为 `calm-truth/src/db/mod.rs:24`，已改） |
| 8 | A MINOR-4：落在 planner 轮中间的失败被该轮老化（红少），G-6 的方向声称错 | 接受（`run_loop.rs:687-716,3688-3800` 核过；4140 0/4 重跑） | §8 G-13；G-6 改「两个方向各一条」 |
| 9 | A MINOR-5 折叠优先序：S3 后无卡级 `input` 生产者，`failed > input` 空转 | 接受 | §5 折叠简化为「按 `cardId` 分组、一行、max `at_ms`」并说明为何不需要优先序 |
| 10 | A MINOR-1 + B 引用表：`sql.rs`/`track_activity.rs` 行号漂移 ~10 行 | 更正（每处重读） | K1/K2/K4/K6/K7、§4.1、§4.3 的行号 |
| 11 | B：K9 漏 `CODEX_WORKER_HOOKS` 的生产读者 `card_fsm.rs:181` | 更正（结论不变：读者在待删模块内） | K9 |
| 12 | A fact 4 / fact 7：§4.5 七张卡「任务已 done」实为 5 done / 2 failed；§4.2「与今天所有会话退出一致」漏 isolated 发事件 | 更正（sql 重跑；`observe.rs:113,121` 核过） | §4.5 行、§4.2 第 3 步、D8 |
| 13 | B MINOR-2：孤儿臂 60 s 是创建宽限不是退出宽限 | 更正（`read.rs:966,980`） | §4.2 效果段与重试段 |
| 14 | B MINOR-3：`ChildExited` 不是进程死亡证明 | 更正（`ws/terminal.rs:194,211`） | §4.2 第 4 步 |
| 15 | A MINOR-3：4140 数字漂移；§7 行 3 写死「3」 | 更正（11:30 全量重取） | §2.3 全表 + D14；§4.2/§4.5/§8 G-8 的「3 条」→ 4；§7 行 3 改为验收时刻按 sql 取 A/B 集合 |
| — | 两通道都未找到构造、本文据以不加机制的两点 | 记录 | attach reader 在 renderer 建立时由内核起、不依赖浏览器（`terminal_renderer/mod.rs:882,918`）⇒ S3 的输出时间在没有人看那张卡时也在记；先写后杀是空操作：`session_projection_active_for_terminal_tx` 只找活态（`calm-truth/src/db/sqlite/session_projection.rs:693`），`complete_ephemeral_session_from_terminal_exit` 找不到就返回（`terminal_sweeper.rs:104-107`）⇒ S2 第 3 步之后 reader 的补写不会把 `exited` 改成 `failed` |

驳回：无。
