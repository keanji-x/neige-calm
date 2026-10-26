# WA0：Claude Code CLI 2.1.280 本机实测证据

本文是 [task-worker-execution.md](task-worker-execution.md) §5.3 实测清单（E1–E9）的证据文档，设计 §5.3、§5.4 链接到这里。
它只记录实测事实与对设计的含义，不改已定结论；设计中凡与本文不一致的〔待实测〕表述，以本文的实测边界为准。

- 实测 attempt：`33612b9baae14ea18b0ca8476a09e551:wa0-measure`（只读实测，Planner 已验收）；归档 attempt：`33612b9baae14ea18b0ca8476a09e551:wa0-doc`。
- 时间：2026-10-26（Asia/Shanghai；原始 transcript 时间为 2026-10-25 UTC）。
- 依据：根 `AGENTS.md`、`CONTRIBUTING.md`，以及设计 §3.1.1、§3.4.1、§5.2–§5.4；E1–E9 以 §5.3 为准。用户明确授权本机实测，未运行 Codex E2E。
- 二进制：`~/.local/bin/claude`；`--version` 输出 `2.1.280 (Claude Code)`。全部 E 项均为此版本。
- 临时根目录：`/tmp/wa0-1hb2ck_p`（下文附录记作 `$R`），权限 0700。实测时无仓库文件变更。
- 路径写法：`~` 指实测用户的 home 目录。原始记录是绝对路径，归档时略去用户名；路径的相对结构与存在性不变。

## 实验隔离、命令与证据边界

CLI 会自动写 session、配置缓存等。因此用 bubblewrap 将 `/` 设为只读，将用户配置副本映射到原路径：`~/.claude` → 临时 `config/`，
`~/.claude.json` → 临时 `claude.json`。副本来自本机用户现有文件（含凭据，仅本地复制，从未输出其值），skills/plugins 完整复制；
未登录、登出或修改原始用户 settings。E9 的 `CLAUDE_CONFIG_DIR` 只指向临时目录。CLI 看到的源路径是原路径，实际可写内容均为副本；
结论不冒充未经隔离的原生运行。

修正后的工具实验另用 namespace 内的临时 `/tmp`，再挂载实验根目录，以允许 CLI 自身创建临时 shell 文件；该 tmpfs 随进程退出释放。
最初一次工具实验出现 `EROFS`，并非权限拒绝；它与最初插件软链接循环的结果均不用于最终结论。此后改为完整复制 plugins/skills，并重测。
用于结论的 argv 在附录，输入与投递时序在各节。

所有模型实验使用 `--max-turns 3 --max-budget-usd 0.12`，E2 子代理实验为 6 turns / USD 0.18；这些是单次上限，并非总预算或精确扣费保证。
默认用 Haiku，仅 auto 确认用 Sonnet。设置 `--strict-mcp-config` 和空 `mcpServers`，避免加载用户外部 MCP；
因此 E6/E9 是用户 settings/skills/plugins 继承测试，不是用户 MCP 完整继承测试。

`claude --help` 已确认原名存在：`--permission-mode auto`、`--permission-prompts none`、`--settings`、`--input-format stream-json`、
`--output-format stream-json`、`--session-id`、`--resume`、`--replay-user-messages`。`--max-turns` 未列在 help 中，但实际接受并可运行；
`--autocompact` 的帮助值域为 auto 或 100k–1M tokens，未用长上下文堆量触发自动压缩。无须替代权限选项。

## 结论一览

| 项 | 版本 | 结论 | 主要限制 |
|---|---|---|---|
| [E1](#e1auto-与无批准界面) | 2.1.280 | 支持；有效模式须检查 init | Haiku 下 init 报 `permissionMode=default`；只验证显式 ask 路径 |
| [E2](#e216-个-hook-与-transcript_path) | 2.1.280 | 部分支持：16 个中 12 种实际触发；SessionStart 带 `transcript_path` | PermissionDenied、Notification、Elicitation、StopFailure 未触发 |
| [E3](#e3未信任目录) | 2.1.280 | 支持：`-p` 跳过目录信任对话框 | 只测新建的未信任 git 目录；不等于隔离不可信项目配置 |
| [E4](#e4压缩后恢复) | 2.1.280 | 部分支持：手动 `/compact` 后 resume 成功 | 长 turn 自动压缩未验证 |
| [E5](#e5turn-中途停止后恢复) | 2.1.280 | 支持 control interrupt 与 SIGTERM 后新进程 resume | 未测 SIGKILL/断电下悬空的 tool_use |
| [E6](#e6用户-settings-下-init-的-skillsplugins) | 2.1.280 | 支持：init 报 79 个 skills、7 个 plugins | MCP 为空，不是 MCP 继承测试 |
| [E7](#e7常驻输入与-turn-中途消息) | 2.1.280 | 支持跨 result 等待；工具期间的消息在工具边界被当前 turn 吸收 | 单次工具边界实测，不推广到所有工具与时点 |
| [E8](#e8没有输入的预分配-session) | 2.1.280 | 同 ID 可再次 `--session-id`；首次 `--resume` 失败 | 只测 SIGTERM 停止的空会话 |
| [E9](#e9配置目录登录与-tui) | 2.1.280 | 配置/登录随 `CLAUDE_CONFIG_DIR` 切换；同用户 TUI 与 `-p` 凭据同源 | 仅本机 Linux 文件凭据后端 |

## E1：auto 与无批准界面

**版本：2.1.280。结论：支持，但有效模式须检查 init。**

命令见附录 `e1-auto-haiku-final`、`e1-auto-sonnet`、`e1-auto-ask`，另有 manual 对照 `e1-denial-ask`。前两条输入分别要求回复
`HAIKU_AUTO` / `WA0_SONNET`。权限测试输入：`Use Bash exactly once to run: printf WA0_PERMISSION_TEST. If denied reply DENIED and stop.`。
临时 ask.json 保留全部 hook，并加 `permissions.ask=["Bash(printf WA0_PERMISSION_TEST)"]`，保证这是需要批准的无害动作，
而非碰运气等待 auto classifier 拒绝。

Sonnet 的 init：`model=claude-sonnet-5, permissionMode=auto`；显式 ask + auto + none 时，stream 中 Bash 返回 `tool_result.is_error=true`，
说明需要 approval、当前无 approval surface，自动拒绝，动作未执行。最终 `result.subtype=success, is_error=false, result=DENIED`，
但 `permission_denials` 非空，列出 Bash 的 tool_use_id 与原输入。manual 对照同样拒绝并继续让模型答复。权限拒绝不必然让进程失败。

Haiku 有效模式见最后复测结果：CLI 接受 auto 参数，但 init 报告 `permissionMode=default`。不能仅凭 argv 认定 auto 已启用。

**设计含义：OQ3 可行，无须改已定方向；补充模型能力/有效模式检查，以及 permission_denials 与 result 成功独立的说明。**
这里只验证显式 ask 路径，未穷举 auto classifier 的全部风险分类。

## E2：16 个 hook 与 transcript_path

**版本：2.1.280。结论：部分支持，12 种实际触发；不能声称 16/16。**

命令见 `e2-tools-fixed`、`e2-extra`，并复用 E1 权限请求、E5 中断、E7 多消息实验。`hooks.json` 用设计源码 `CLAUDE_WORKER_HOOKS` 的
16 个名称注册 command hook；PreToolUse、PostToolUse、PostToolUseFailure、PermissionRequest、PermissionDenied 使用 matcher `*`，
其余不设 matcher。每个 hook 命令都是 `python3 /tmp/wa0-1hb2ck_p/hook.py`，脚本读取 stdin JSON，逐行写入临时 hooks.jsonl，不改变工具决策。

工具输入：Bash `printf WA0_TOOL_OK`（成功），Read 不存在的临时文件（失败）。额外输入：TaskCreate 临时任务、TaskUpdate completed，
调用一个只回复 SUB_OK 的 Haiku general-purpose Agent。模型实际执行了这些生产工具，TaskCreated/TaskCompleted/SubagentStart/SubagentStop 都有记录。

实际观察到：SessionStart、UserPromptSubmit、PreToolUse、PostToolUse、PostToolUseFailure、SubagentStart、SubagentStop、TaskCreated、
TaskCompleted、PermissionRequest、Stop、SessionEnd。

未观察到：

| hook | 实测边界 |
|---|---|
| PermissionDenied | E1 已真实拒绝并观察 PermissionRequest，但该拒绝路径未记录 PermissionDenied；不能依赖它必然出现，也不能推广为该 hook 全面不支持。 |
| Notification | 未构造触发通知的特定条件；stream 中事件不自动等于同名 hook。 |
| Elicitation | 为避免调用用户外部服务，MCP 为空，未构造 elicitation 服务；未验证。 |
| StopFailure | 未构造稳定的模型/API 失败；E5 control interrupt 本次也未记录此 hook；未验证一般失败路径。 |

SessionStart 摘录：`hook_event_name=SessionStart, source=startup, session_id=b1c68542-ff53-4141-8eb6-7b09d4460d74,
transcript_path=~/.claude/projects/-tmp-wa0-1hb2ck-p-work/b1c68542-ff53-4141-8eb6-7b09d4460d74.jsonl`。此路径由 hook 报告，
未以 CLI 私有命名规则推导；对应文件在临时映射中存在。resume 的 SessionStart 也记录 `source=resume`。

**设计含义：OQ4 的 hooks 继承和 transcript 定位有正证据；§3.4/§5.2 应写成“注册 16 个、按条件触发”，保留以上未验证项。**
不把 StopFailure/PermissionDenied 当成唯一结算或拒绝信号。统计中的两个 SubagentStop 来自不同 session（包含压缩过程）；
这些次数不构成 hooks 恰好一次投递的证明。

## E3：未信任目录

**版本：2.1.280。结论：支持（新建未信任 git 目录）。**

在临时根下创建全新的 git 目录 `untrusted`：`git init -q /tmp/wa0-1hb2ck_p/untrusted`。它不是 checkout 的 worktree，
未改 checkout 的 Git 元数据；用于验证相同的目录信任入口。命令 `e3-untrusted`，输入 `Reply exactly UNTRUSTED_OK.`，stdin 无交互回答。

输出 init → assistant `UNTRUSTED_OK` → result `success, is_error=false`，进程退出 0，用时约 5.52 秒。无 trust 确认输入或等待。
`--help` 也明确说明 -p / 非 TTY 会跳过 workspace trust dialog。

**设计含义：支持 §5.2 移除 TUI 信任 watcher；不改变 OQ4/OQ10。** 该结论不等于 -p 会隔离不可信项目配置；必须由宿主决定允许哪些目录和配置。

## E4：压缩后恢复

**版本：2.1.280。结论：部分支持。自动压缩未验证；手动压缩后 resume 已支持。**

使用 E7 的 session `f6a00746-e67e-41b4-a2c0-0581d01508d1`。命令 `e4-compact`，输入 `/compact Preserve the marker STEERED_DONE.`。
stream 返回压缩总结 user 消息、`<local-command-stdout>Compacted </local-command-stdout>`，最终 result `success, is_error=false, num_turns=0`。
transcript 记录 `compact_boundary, trigger=manual, preTokens=10872, postTokens=900`。

随后新进程命令 `e4-resume`，同一 session，输入 `What marker did you return at the end of the Bash task? Reply with only that marker.`，
答复与 result 均为 `STEERED_DONE`，退出 0。

为控制成本，未堆到至少 100k tokens 的自动压缩阈值；未证明长 turn 自动压缩的触发时机或其 hook 行为。按任务许可改用 /compact 验证恢复链路。

**设计含义：OQ1 的持久会话恢复可跨手动压缩；自动压缩部分仍须保留待测，不能把 E4 标成全部完成。** 不改变每 turn 一 execution 的选择。

## E5：turn 中途停止后恢复

**版本：2.1.280。结论：支持 control interrupt 与 SIGTERM 后的新进程 resume；未测 SIGKILL/断电。**

命令与输入见 `e5-stop` / `e5-resume`、`e5-terminate` / `e5-terminate-resume`。两次初始 user 输入相同：
`Use Bash to run exactly sleep 8; printf WA0_TOOL_END . After the tool returns reply ORIGINAL_DONE.`。见到 assistant 的 Bash tool_use 后约 1 秒发停止。

control 输入完整为：

```json
{"type":"control_request","request_id":"wa0-stop","request":{"subtype":"interrupt"}}
```

输出 `control_response.response.subtype=success`。未完成 tool_use 被同 tool_use_id 的错误 tool_result 配对，内容说明用户不想继续该工具；
另有 `[Request interrupted by user for tool use]`。result 为 `error_during_execution, is_error=true, terminal_reason=aborted_tools, stop_reason=tool_use`，
随后关 stdin 后进程退出 1。transcript 保存了 tool_use、错误 tool_result 与中断消息。

新的 `--resume 41c2116f-41d0-44d1-a35e-40e327e231ee` 输入要求继续并 Bash `printf WA0_RESUMED`，实际工具返回 WA0_RESUMED，
模型回复 RESUMED，result success，退出 0。

第二次对整个实验进程组发送 SIGTERM，进程退出 -15，停止前 stream 仍记录对应工具的 `tool_result.is_error=true, content="Exit code 137"`；
没有 result。新进程 resume 同 session `7e92d9d0-0bdc-4a22-bcdb-00c9df766591`，模型根据已记录的失败结果明确说前一次 Bash 未成功，
然后回复 RECOVERED，退出 0。

**设计含义：支持 OQ1(a) 停止后以新 execution 续跑；完成消息与进程退出必须分别处理。** resume 不保证原工具恰好执行一次或自动补做；
本次停止均留下配对错误结果，不能外推为强杀后悬空 tool_use 的修复合同。

## E6：用户 settings 下 init 的 skills/plugins

**版本：2.1.280。结论：支持用户 skills/plugins 被加载。**

命令 `e6-user`：不设 CLAUDE_CONFIG_DIR、不加 setting-sources、不禁用 slash commands；明确工具集为空避免任意工具操作。
输入 `Reply exactly WA0_USER.`，退出 0，init 报告 79 个 skills、7 个 plugins。MCP 因安全隔离为空，见前述边界。

plugins（init 原值中的 name/source；省略用户/组织标识）：

| name | source | version |
|---|---|---|
| rust-analyzer-lsp | rust-analyzer-lsp@claude-plugins-official | 1.0.0 |
| equity-research | equity-research@claude-for-financial-services | 0.1.2 |
| market-researcher | market-researcher@claude-for-financial-services | 0.1.1 |
| frontend-design | frontend-design@claude-plugins-official | 未报告 |
| finance | finance@synced | 1.3.0 |
| agents-md | agents-md@builtin | 未报告 |
| telemetry | telemetry@builtin | 未报告 |

前四者路径在 `~/.claude/plugins/cache/`；finance 在 `~/.claude/plugins/synced/`；后两者 path 为 builtin。79 个 skills 的完整 init 列表：

```json
[
  "ghidra-arm-reverse",
  "lark-approval", "lark-attendance", "lark-base", "lark-calendar", "lark-contact", "lark-doc",
  "lark-drive", "lark-event", "lark-im", "lark-mail", "lark-markdown", "lark-minutes", "lark-okr",
  "lark-openapi-explorer", "lark-shared", "lark-sheets", "lark-skill-maker", "lark-slides",
  "lark-task", "lark-vc", "lark-vc-agent", "lark-whiteboard", "lark-wiki",
  "lark-workflow-meeting-summary", "lark-workflow-standup-report",
  "longbridge", "navi", "deep-research",
  "equity-research:catalyst-calendar", "equity-research:earnings-analysis",
  "equity-research:earnings-preview", "equity-research:idea-generation",
  "equity-research:initiating-coverage", "equity-research:model-update",
  "equity-research:morning-note", "equity-research:sector-overview", "equity-research:thesis-tracker",
  "market-researcher:competitive-analysis", "market-researcher:comps-analysis",
  "market-researcher:idea-generation", "market-researcher:pptx-author",
  "market-researcher:sector-overview",
  "frontend-design:frontend-design",
  "finance:financial-statements", "finance:journal-entry", "finance:reconciliation",
  "finance:sox-testing", "finance:variance-analysis",
  "design", "design-sync", "dataviz", "update-config", "verify", "debug", "code-review", "simplify",
  "batch", "fewer-permission-prompts", "doctor", "loop", "schedule", "claude-api",
  "workflow-authoring", "run", "run-skill-generator",
  "anthropic-skills:built-in-browser", "anthropic-skills:chrome-browser",
  "anthropic-skills:computer-use", "anthropic-skills:deep-research", "anthropic-skills:docs",
  "anthropic-skills:docx", "anthropic-skills:google-workspace", "anthropic-skills:import-memory",
  "anthropic-skills:morning", "anthropic-skills:pdf", "anthropic-skills:pptx",
  "anthropic-skills:skill-creator", "anthropic-skills:xlsx"
]
```

**设计含义：支持 OQ4/OQ10 的 UserHome；worker init_check 不能沿用 Planner 对非内置 plugins/skills 的拒绝规则。**
列表含用户、插件和内置能力，不能把 79 项都归类为用户安装项。

## E7：常驻输入与 turn 中途消息

**版本：2.1.280。结论：支持跨 result 等下一输入；工具期间消息在工具边界吸收进当前 turn。**

命令 `e7-multi` / `e7-inject`，stdin 是逐行 JSON，保持打开；均启用 `--replay-user-messages`。输入 envelope：

```json
{"type":"user","message":{"role":"user","content":"<本节列出的输入文本>"}}
```

多 turn：先输入 `Reply exactly FIRST.`。5.605 秒收到第一 result，7.607 秒确认 CLI 仍存活，7.608 秒输入 `Reply exactly SECOND.`，
11.067 秒收到第二 result，同 session。最后关闭 stdin，退出 0。该次模型没有遵照 FIRST/SECOND 原样回答，而是询问任务；
不以回答文字证明内容正确，仅以两条回显、两次 result 与存活检查证明协议行为。

工具期间插话：初始输入与 E5 相同；4.164 秒见 Bash tool_use，5.164 秒注入
`Steering message: after the pending Bash completes reply STEERED_DONE instead of ORIGINAL_DONE.`；13.025 秒收到 Bash 正常 tool_result
`WA0_TOOL_END`；13.094 秒 stream 回显插话；15.092 秒 assistant 回复 STEERED_DONE；15.150 秒 result success、num_turns=2。
该测试仅一个 result，而非为插话另起一个独立 result。

transcript 中插话不是普通独立 user turn，而是：

```json
{"type":"queue-operation","operation":"enqueue","content":"Steering message: ..."}
{"type":"attachment","attachment":{"type":"queued_command","prompt":"Steering message: ..."}}
{"type":"queue-operation","operation":"remove","reason":"absorbed_mid_turn"}
```

attachment 的 rendered 文本以 system-reminder 形式说明用户在当前工作期间发来消息。`num_turns` 不等于外部 user line 数，不能据它拆分业务 turn。

**设计含义：OQ11 的常驻形状技术上可行且有 steer 语义；更新 §3.1.1 的待测句。仍保留已定“每 turn 一 execution”，
因为本实验没有解决逐 turn 授权/C5 空闲占用/重启接管的设计问题。** 单次工具边界实测不能推广成所有工具、所有时点都相同。

## E8：没有输入的预分配 session

**版本：2.1.280。结论：同 ID 可重新 --session-id；首次 --resume 不支持。**

命令 `e8-empty`：session-id `4b6eede8-0c2a-4fd4-ac43-d26226059bfb`，stream-json stdin 保持打开且不写任何字节；等待约 5 秒再 SIGTERM，
退出 -15，无 result。随后先运行 `e8-resume-before-reuse`（顺序重要，避免后一步创建会话污染结果），输入 `Reply exactly EMPTY_RESUME.`：
退出 1、result `error_during_execution, is_error=true, num_turns=0`，错误 `No conversation found with session ID: ...`。

再用 `e8-reuse` 以同 ID `--session-id` 输入 `Reply exactly EMPTY_REUSED.`：init 保持该 ID，答复 EMPTY_REUSED、result success，退出 0。

**设计含义：OQ1 首 turn 尚未交付的恢复不能无条件用 resume；须区分“仅预分配 ID”和“已持久化会话”，由发行状态及 CLI 外部合同决定启动方式。**
与设计 §3.6 首 turn 未 stamp 的窗口一起核对；需要补充此恢复分支，不能仅以 ID 已分配认定存在可恢复 transcript。

## E9：配置目录、登录与 TUI

**版本：2.1.280。结论：在本机 Linux 文件凭据后端支持配置/登录随 CLAUDE_CONFIG_DIR 切换；同用户 TUI 与 -p 凭据同源。**

命令 `e9-unset`、`e9-empty`、`e9-auth` 是 auth status，只读观察；三者用 strace 记录 openat/newfstatat/readlink（不追踪 read 内容）。
`e9-empty-print` / `e9-auth-print` 是实际 -p 模型验证。`e9-tui` 在 PTY 启动，无模型任务输入，约 10 秒后 SIGTERM 清理；
只记录其读取路径和登录 UI，不登录/登出。

| 条件 | 凭据/配置读取路径（路径存在性，不含值） | 观察 |
|---|---|---|
| 不设 CLAUDE_CONFIG_DIR | `~/.claude/.credentials.json`、`~/.claude/settings.json` 存在；全局 `~/.claude.json` 存在 | auth status: loggedIn=true, authMethod=claude.ai, configDirectory=`~/.claude`；-p 成功；E6 79 skills/7 plugins |
| 指向空 dedicated-empty | 该目录的 .credentials.json、settings.json、.claude.json 起初不存在；trace 明确尝试临时路径 | auth status: loggedIn=false, authMethod=none；-p 退出 1、Not logged in，请 /login；不读取用户原凭据作为回退；init 仍有 17 个 skills、telemetry plugin |
| dedicated-auth，仅复制现有凭据 | 临时目录 `.credentials.json` 存在，未复制用户 settings/skills/plugins；CLI 在临时目录创建自己的状态 | auth status: loggedIn=true, authMethod=claude.ai；-p 输入 Reply exactly CONFIG_AUTH.，返回 CONFIG_AUTH、退出 0；init 18 skills/telemetry plugin，用户插件未加载 |
| 同用户 TUI，配置仍为保护副本 | strace 观察读取同一 `~/.claude/.credentials.json` | UI 显示 Claude Code v2.1.280、Haiku 4.5、Claude Max，与 auth status 的 subscriptionType=max 相符；未要求登录；退出 -15 |

不设目录时 skills 来自所复制用户 skills 目录、插件及内置能力；插件源路径见 E6。指定 dedicated 后用户 settings/skills/plugins
不再从默认配置根加载，内置/动态能力仍存在，因此不能写“Dedicated 下没有任何 skills/plugins”。credentials-only 对照证明：
加载原账户凭据不等于加载其本机用户 settings/plugins。本次没有证明其他操作系统的 keychain、API key/代理身份或组织托管配置也被该变量隔离。

**设计含义：支持 OQ4/OQ10 的 UserHome，以及 §3.4.1 不共用 Planner Dedicated 配置的要求。** Dedicated 通常需独立配置认证；
本次临时复制凭据只是定位来源的实验，不能变成产品自动复制凭据的设计建议。

## 最终状态与设计处置

- OQ1：恢复方向获得 E4/E5 正证据；补充 E8 空会话首次启动分支。未验证 SIGKILL、断电或自动压缩。
- OQ3：无批准界面会将需要批准的工具拒绝回模型，方向可行；检查实际 permissionMode。
- OQ4/OQ10：UserHome 继承 settings/skills/plugins 和当前登录，方向可行；不能套 Planner init 的拒绝插件规则。16 hook 并非都已验证。
- OQ11：常驻与工具边界 steer 均得到实测；保留每 turn 一 execution 的决定，只更新证据与语义。
- 实测 attempt 未改设计或任何仓库文件；本文由归档 attempt 写入，设计 §5.3/§5.4 只补证据链接与说明，不改已定结论。
  原始用户配置的 5 个关键文件前后 SHA-256 一致（不公开 hash 或秘密内容）；只读根挂载保护了其他原始用户文件。
  实测时 git status --short、git diff --stat 均为空。所有本实验 subprocess 已回收，最终进程检查无带本临时路径的 CLI/Bash 遗留。
  实测无代码变更，因此未运行仓库 lint/测试 gate。

## 附录：实际进程命令

以下由实际 argv 的 shell 转义表示精简而来：公共前缀只写一次，每条命令列出其余参数，参数顺序与原 argv 一致。
hook/输入驱动脚本均仅位于上述临时根；stdin 内容和投递时点见对应 E 节。命令没有 token/key 值。进程启停、EOF 和信号由 Python harness 驱动。

```sh
~/.local/bin/claude --version
~/.local/bin/claude --help
```

公共前缀（`R=/tmp/wa0-1hb2ck_p`）：

```sh
# BWRAP：隔离外层。标“无 tmpfs”的命令不含 --tmpfs /tmp；DIR 默认 work；
# 标 dedicated-* 的命令在 --unsetenv 之后、-- 之前另加 --setenv CLAUDE_CONFIG_DIR $R/dedicated-*。
bwrap --ro-bind / / --dev-bind /dev /dev --proc /proc --tmpfs /tmp --bind $R $R \
  --bind $R/config ~/.claude --bind $R/claude.json ~/.claude.json \
  --chdir $R/$DIR --unsetenv CLAUDE_CONFIG_DIR --

# CLAUDE_P：模型实验的 claude 前缀，接在 BWRAP 之后。
~/.local/bin/claude -p --verbose --output-format stream-json --model haiku --max-turns 3 --max-budget-usd 0.12 \
  --strict-mcp-config --mcp-config $R/empty-mcp.json --settings $R/hooks.json --permission-prompts none

# STREAM <id>：常驻 stdin 实验在 CLAUDE_P 之后的参数（无位置 prompt，输入走 stdin）。
--permission-mode manual --allowedTools Bash --tools Bash --input-format stream-json --replay-user-messages --session-id <id>
```

`PERM` 指 E1 的权限测试输入 `'Use Bash exactly once to run: printf WA0_PERMISSION_TEST. If denied reply DENIED and stop.'`。

| 名称 | 外层 | CLAUDE_P 之后的参数 |
|---|---|---|
| e1-auto-haiku-final | BWRAP | `--permission-mode auto --tools '' -- 'Reply exactly HAIKU_AUTO.'` |
| e1-auto-sonnet | BWRAP（无 tmpfs） | `--model sonnet --permission-mode auto --tools Bash,Read -- 'Reply exactly WA0_SONNET without tools.'` |
| e1-denial-ask | BWRAP | `--settings $R/ask.json --permission-mode manual --tools Bash -- PERM` |
| e1-auto-ask | BWRAP | `--settings $R/ask.json --model sonnet --permission-mode auto --tools Bash -- PERM` |
| e2-tools-fixed | BWRAP | `--permission-mode manual --allowedTools Bash --tools Bash,Read -- 'Use Bash to run printf WA0_TOOL_OK; then use Read on /tmp/wa0-1hb2ck_p/work/missing.txt which does not exist; then reply DONE.'` |
| e2-extra | BWRAP；CLAUDE_P 中为 `--max-turns 6 --max-budget-usd 0.18` | `--permission-mode manual --allowedTools Agent,TaskCreate,TaskUpdate --tools Agent,TaskCreate,TaskUpdate -- 'First TaskCreate a task with subject WA0 probe and description temporary hook test. Mark that task completed with TaskUpdate. Then use Agent once, subagent_type general-purpose and model haiku, prompt Reply exactly SUB_OK without tools. Finally reply DONE. Do not do any other work.'` |
| e3-untrusted | BWRAP（DIR=untrusted） | `--tools '' -- 'Reply exactly UNTRUSTED_OK.'` |
| e4-compact | BWRAP | `--resume f6a00746-e67e-41b4-a2c0-0581d01508d1 --tools '' -- '/compact Preserve the marker STEERED_DONE.'` |
| e4-resume | BWRAP | `--resume f6a00746-e67e-41b4-a2c0-0581d01508d1 --tools '' -- 'What marker did you return at the end of the Bash task? Reply with only that marker.'` |
| e5-stop | BWRAP | `STREAM 41c2116f-41d0-44d1-a35e-40e327e231ee` |
| e5-resume | BWRAP | `--resume 41c2116f-41d0-44d1-a35e-40e327e231ee --allowedTools Bash --tools Bash -- 'Continue the interrupted task. Run printf WA0_RESUMED with Bash, then reply RESUMED.'` |
| e5-terminate | BWRAP | `STREAM 7e92d9d0-0bdc-4a22-bcdb-00c9df766591` |
| e5-terminate-resume | BWRAP | `--resume 7e92d9d0-0bdc-4a22-bcdb-00c9df766591 --tools '' -- 'State whether the previous Bash tool succeeded, based only on its recorded result. Then reply RECOVERED.'` |
| e6-user | BWRAP（无 tmpfs） | `--permission-mode manual --tools '' -- 'Reply exactly WA0_USER.'` |
| e7-multi | BWRAP | `STREAM 2ffca91c-b2d6-43e2-84e0-e0377dcf8343` |
| e7-inject | BWRAP | `STREAM f6a00746-e67e-41b4-a2c0-0581d01508d1` |
| e8-empty | BWRAP | `STREAM 4b6eede8-0c2a-4fd4-ac43-d26226059bfb` |
| e8-resume-before-reuse | BWRAP | `--resume 4b6eede8-0c2a-4fd4-ac43-d26226059bfb --tools '' -- 'Reply exactly EMPTY_RESUME.'` |
| e8-reuse | BWRAP | `--session-id 4b6eede8-0c2a-4fd4-ac43-d26226059bfb --tools '' -- 'Reply exactly EMPTY_REUSED.'` |
| e9-empty-print | BWRAP（dedicated-empty） | `--tools '' -- 'Reply exactly CONFIG_EMPTY.'` |
| e9-auth-print | BWRAP（dedicated-auth） | `--tools '' -- 'Reply exactly CONFIG_AUTH.'` |

不经 CLAUDE_P 的 E9 命令：

```sh
# e9-unset / e9-empty / e9-auth：auth status，只读观察。
# e9-empty、e9-auth 的 BWRAP 分别为 dedicated-empty、dedicated-auth。
strace -f -e trace=openat,newfstatat,readlink -o $R/<名称>.trace BWRAP ~/.local/bin/claude auth status --json

# e9-tui：PTY 中启动交互 TUI。
strace -f -e trace=openat -o $R/e9-tui.trace BWRAP \
  ~/.local/bin/claude --model haiku --permission-mode manual --tools '' --strict-mcp-config --mcp-config $R/empty-mcp.json
```

Hook 记录统计（含探索/对照实验，只表示观察次数，不表示恰好一次合同）：

```json
{
  "SessionStart": 25,
  "SessionEnd": 25,
  "UserPromptSubmit": 24,
  "Stop": 20,
  "PreToolUse": 14,
  "PostToolUseFailure": 4,
  "PermissionRequest": 2,
  "PostToolUse": 6,
  "TaskCreated": 1,
  "TaskCompleted": 1,
  "SubagentStart": 1,
  "SubagentStop": 2
}
```

## 临时敏感副本清理

完成摘要提取后删除了临时 config、dedicated-auth、dedicated-empty 与全局配置副本，包含其中凭据和备份。保留实测报告（本文的来源）、实验脚本、
命令、hook/脱敏事件及相关 transcript 摘录；重跑附录需重新按本文隔离步骤准备副本。没有把任何凭据值放入证据正文。
