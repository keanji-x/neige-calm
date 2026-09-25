# `neige` 命令语义进内核，二进制只做转发（#1801）— 设计 v1

> **owner 规则（本设计的约束，优先于其余一切）**
> 1. 只解决观察到的痛点，先选最简单的做法：用最少的机制消除版本分叉；假想情形写成一行 KNOWN GAP。
> 2. 兼容只看 4140 部署（`~/.local/share/neige-next`），不做通用兼容方案。
> 3. **避免双重语义**：每条 CLI 命令只有一个语义来源，内核 CLI 层只把 argv 映射到现有 MCP 工具处理函数，不重新实现；
>    客户端不做解析、校验、渲染或兜底；旧的胖客户端 `neige` 按 initialize `clientInfo` 识别并拒绝，拒绝信息给出正确路径；
>    冻结的只有转发协议本身。
>
> 基线 `origin/main` = `dd6773667`（工作树 `design/1801-kernel-cli`）。file:line 均在该基线实测；
> `cli/` 指 `crates/neige-cli/src/`，`srv/` 指 `crates/calm-server/src/`，`mcp/` 指 `srv/mcp_server/`。

## 0. 结论先行

- 新增一个 JSON-RPC 方法 `neige/cli`，与 `tools/call` 同在内核唯一的 MCP UDS `<data_dir>/mcp/kernel.sock`（`mcp/transport.rs:1645-1647`）上；卡片身份来自连接时出示的每卡 token，分发入口是 `mcp/transport.rs:329` 的 `dispatch_request`。
  内核拿到 argv 后，按一张命令表解析，再经 `tools/call` 用的**同一个**内部函数（身份解析、worker grants、处理函数）执行，最后渲染出
  `{stdout, stderr, exit}`。`neige` 二进制缩到约 120 行：读两个环境变量，连接，发 argv，原样打印，按给出的退出码退出；唯一本地回答的是 `--version`（打印转发器自身版本，供发布打包探测，§3.4）。
- 围栏：在 `handle_initialize` 里判断，`clientInfo.name == "neige"` 就是旧胖客户端（4140 盘上 46 个副本全部如此，§4.3）。
  这类连接以 `-32426` 拒绝，消息里带上 `<kernel current_exe 目录>/neige`。新转发器改用 `clientInfo {name:"neige-forward", version:"1"}`，
  其中 `version` 即转发协议版本。
- 保留 #1784 的 PATH 前置（§6）：正确性由围栏保证，PATH 前置只是让用户默认就找到正确的转发器，一般碰不到拒绝。
- 一个 PR：新增生产代码约 0.95k 行（其中约 0.45k 行由 `cli/` 搬入），删除约 2.5k 行（`cli/main.rs` 胖逻辑、`help.rs`、客户端单测和假服务器测试）。
- **硬前提**：4140 只通过 `deploy/apply.py` 的整目录切换发布（§7、§8 R2）；#1801 之后禁止单个二进制的临时替换（例如预览换装）。
- 与 issue 相左或补充的发现见 §9，要点：`--help` 今后也需要 socket，因此 gate guard 的 `--help` 豁免要删掉；
  隔离 worker 的 grants 必须在 CLI 路径上同样生效，否则会扩大权限。

## 1. 现状清单（`cli/main.rs`，1570 行 + `cli/help.rs` 235 行）

每个连接的流程：initialize（`main.rs:115-141`）→ 一次 `tools/call`（`:225-238`）→ 取 `structuredContent`，取不到就回退去解析 `content[0].text`
（`:259-270`，属于兜底）→ 渲染（`:78-100`）。所有命令都不流式输出，也不读 stdin。

| 命令 | 调用的工具（处理函数） | 客户端额外逻辑（main.rs 行） | 输出 | 内核新归宿 |
|---|---|---|---|---|
| `ls [path]` | `calm.track.ls`，`mcp/tools/track_file.rs:72-87`（Planner/Worker） | 默认 path `"/"`（`:658`），与工具重复：工具缺 `path` 时本就列根（`track_file.rs:112-118`）；最多一个 path（`:652`）；文本渲染 `d name`/`- name`，`kind` 缺失时当 `"file"`（`:344-390`，:378 是兜底） | 短 | 不传 path 时 CLI 表**不填默认值**，由工具决定；`mcp/cli/render.rs::ls`（`kind` 缺失 = 渲染错误，不回退） |
| `cat <path>` | `calm.track.cat`，`track_file.rs:89-106` | 必须恰好一个 path（`:663-681`）；`content_type == application/json` 时 pretty 打印，解析失败则打原文（`:392-433`）；`content_type` 缺失当空串（`:406`，渲染规则：没有类型就打原文）；`--json` 只影响错误格式，不影响输出（`:80`） | **可能很长**（整个视图文件） | `render::cat_like` |
| `state` | `calm.track.state`，`mcp/tools/track_state.rs:64-115` | 不接受参数（`:683-696`）；默认 pretty，`--json` 输出紧凑 JSON（`:435-459`） | 中等 | `render::state` |
| `diff <from> [to] [path]`，`--to`/`--path` | `calm.track.diff`，`mcp/tools/track_history.rs:104-137` | 位置参数与选项二选一、每项只能一次、拒绝空值（`:697-770`）；文本 `path new/deleted/edited` + patch（`:461-531`）；**`added→new`、`modified→edited` 重命名**（`:604-611`），`status` 缺失时当 `modified`（`:500`，兜底） | **可能很长**（每文件 patch ≤200 行，`calm-truth/src/track_vcs/mod.rs:6`，文件数无上限） | argv 形状 → `commands.rs`；重命名改用已有的 `DiffStatus::observation_label`（`calm-truth/src/track_vcs/types.rs:65`，§5 H3） |
| `cat-at <commit> <path>` | `calm.track.cat_at`，`track_history.rs:139-158` | 恰好两个位置参数（`:771-792`）；渲染同 `cat`，`--json` 同样只影响错误（`:83`） | **可能很长** | `render::cat_like` |
| `log [path] --limit N --include-empty` | `calm.track.log`，`track_history.rs:160-180` | `--limit` 必须是正整数，拒绝 0（`:801-817`）；文本 `hash8 event=N lifecycle msg`，`lifecycle` 缺失时当 `unknown`（`:533-602`，:572 是兜底）；`message` 为 null 打空串（`:574`）、`event_id` 为 null 打 `event=-`（`:584-590`）——这两条是**渲染规则**（`calm-truth/src/track_vcs/types.rs:99,101` 均为 `Option`），保留 | ≤200 条（工具内 clamp，`track_history.rs:244-259`） | 整数解析 → `commands.rs`；**范围只由工具决定**（H4）；`render::log` |
| `task-completed --idempotency-key K [--result R] [--artifact P]...` | `calm.task.complete`，`mcp/tools/emit.rs:121-168`（仅 Worker） | key 非空、只能一次（`:844-863`）；**`--result` 先按 JSON 解析，失败就当字符串**（`:871-873`）；输出原始 JSON（`:85-99`） | 短 | `--result` 的 JSON-或-文本转换 → `commands.rs`（这是 argv 层的语义，工具只接收 `Value`）；空 key 由工具拒绝（`emit.rs:128-135`） |
| `task-failed --idempotency-key K --reason T` | `calm.task.fail`，`emit.rs:367-396`（仅 Worker） | key 和 reason 都拒绝空值（`:926-954`），但**工具接受空 reason**（`emit.rs:382-386`） | 短 | 客户端的空 reason 检查删除；工具改为拒绝空或只含空白的 reason（owner 决定，§9 F5） |
| `track-gc --track-id T [--keep N] [--dry-run] --force` | `calm.admin.track_gc`，`mcp/tools/admin.rs:74-132`（仅 Planner） | **默认 `keep=50`**（`:1046`，工具要求显式传 keep，`admin.rs:52`；第二份在 `calm-truth/src/track_vcs/gc.rs:16-17` 的 `DEFAULT_TRACK_HISTORY_PRUNE_KEEP`，注释写着与 `neige track-gc` 默认值对齐）；`keep>0`（`:1009-1017`，与工具 `admin.rs:97-103` 重复）；**确认门：不是 `--dry-run` 就必须 `--force`**（`:1037-1042`） | 短 | 确认门 → `commands.rs`，是 CLI 层唯一的非映射规则；keep 默认值直接引用 `calm_truth` 的 `DEFAULT_TRACK_HISTORY_PRUNE_KEEP`（`gc.rs:17` 原为 `pub(super)`，`gc` 模块私有，所以改 `pub` 并经 `track_vcs` 的 `pub use gc::{..}` 再导出；注释改为"`neige track-gc` 的默认值即此常量"）；`keep>0` 只保留工具那一处 |
| `vacuum --force` | `calm.admin.vacuum`，`admin.rs:134-147`（仅 Planner） | **确认门 `--force`**（`:1070-1075`） | 短 | `commands.rs` 确认门 |
| `--json`（全局或命令后） | — | 前置或出现在任意位置（`:634-637`、各分支）；ls/state/diff/log 输出紧凑 JSON；其余命令只把**错误**转成 JSON（`:617-623`，`AppError.structured` 在 `:1175-1180`） | — | `commands.rs` 解析；`mcp/cli/mod.rs::error_output` |
| usage / help / `--version` / 未知命令 | — | `help::request` 在任何网络连接**之前**处理（`main.rs:32-48`，`help.rs:170-185`）；usage 串（`:1191`）；未知命令提示（`help.rs:205-210`）；`--version` 打印客户端版本（`:32-35`） | 短 | `help.rs` 原样搬到 `mcp/cli/help.rs`；`--version` **留在转发器本地**（§3.4，打包依赖它） |
| 退出码 | — | 0 成功；1 usage；2 缺环境变量（`:1200-1207`）；3 连接失败（`:104-111`）；4 rpc/工具/协议错误（`:1209-1221`） | — | 内核只给出 0、1、4（§3.2）；转发器本地的固定失败是 2、3、4、5、141（§3.3），其中 4 与内核共用 |

结论：10 个命令都只调用**一个**工具，没有哪条命令组合多个工具。不属于纯工具调用的逻辑有：默认值（ls path 与工具重复→删；keep=50 与 `gc.rs` 重复→共用常量）、
`--result` 的 JSON-或-文本转换、两个 `--force` 确认门、diff 状态重命名、`cat` 的 JSON pretty 打印和三条渲染规则（`content_type` 缺失、`message`/`event_id` 为 null）、
三处真正的缺字段兜底（`kind`、`status`、`lifecycle`）、`structuredContent` 缺失时的文本回退、`isError=true` 分支（`:247-258`，内核这 10 个工具的 `ToolResult` 从不置位，`mcp/result.rs:35,45`，随胖客户端一起删除，内核 CLI 层不设该分支），以及一组与工具重复的校验。

## 2. 内核中的 CLI 层

**归属**：`calm-server` 新增 `mcp/cli/`，只依赖 `mcp_server` 已有的类型：

- `mcp/cli/mod.rs`（约 90 行）：`pub(crate) async fn serve(ctx, registry, conn, params) -> Result<Value, RpcError>`，返回 `{stdout, stderr, exit}`。
- `mcp/cli/commands.rs`（约 330 行）：命令表 `COMMANDS: &[Command { name, tool, parse: fn(&[String]) -> Result<Parsed, Usage> }]`。
  `Parsed { tool_args: Value, json: bool, render: Render }`。只处理 argv 形状、默认值和确认门。
- `mcp/cli/render.rs`（约 150 行）：`fn render(Render, &Value) -> Result<String, RenderError>`，逐字从 `cli/main.rs:344-615` 搬来，去掉兜底。
- `mcp/cli/help.rs`（约 200 行）：从 `cli/help.rs` 原样搬来，只改两处：root help 的标题行去掉版本号；`--version` 一行写作
  "`--version` (only argument): print the forwarder version"。这样只存在一个版本字符串（转发器的，§3.4）。track-gc 帮助里的 keep 默认值由 `DEFAULT_TRACK_HISTORY_PRUNE_KEEP` 渲染。

**只有一个执行来源**：把 `mcp/transport.rs:521-528`（`resolve_tools_call_identity` → `worker_grants::require` → `handler(ctx, identity, args)`）
提取成 `pub(crate) async fn call_registered_tool(ctx, registry, conn, thread_id, name, args) -> Result<ToolResult, RpcError>`（`thread_id` 是 `tools/call` 的 `_meta.threadId`，CLI 传 `None`），放进同级新模块 `mcp/transport/call.rs`
（`transport.rs` 已 1647 行，不再往里加）。`dispatch_tools_call` 和 `cli::serve` **都**调用它。`dispatch_request`（`transport.rs:337-444`）只加一行分支
`"neige/cli" => cli::serve(...)`，分支体在 `mcp/cli/mod.rs`。
CLI 层从 `ToolResult::into_structured()`（`mcp/result.rs:77`）拿数据，不再经过 `content[0].text`，于是 `main.rs:259-270` 的回退不复存在。

**授权与直接调用完全相同，权限不会变宽**：
- 连接身份仍由同一个 `handle_initialize`（`mcp/handshake.rs:25`）和同一个每卡 token 建立。`mcp/cli/` 不接触 token 以外的任何凭据。
- `threadId` 传 `None`：CardBound 连接走 `card_bound_tool_identity`（`transport.rs:1463`，含 session 活跃检查）；DaemonTrust 连接没有 thread，
  `cli::serve` 入口直接以 JSON-RPC error 拒绝（§3.2），不解析 argv、不调用任何工具（T12）。
- `worker_grants::require` 对每次 CLI 调用照样执行。**这一步是承重的**：隔离 worker 的原生白名单只有 4 个工具（`srv/dedicated_codex/policy.rs:42-47`），
  `calm.track.ls/cat/state/...` 不在里面。如果 CLI 路径绕过 grants，隔离 worker 就能用 `neige cat` 读到直接调用读不到的内容。
- 角色门保留在各处理函数内（`require_role_any`/`require_role`），CLI 层**不**另做角色判断，错误文本就是处理函数原本的 `RpcError`。
- CLI 层能到达的工具只有命令表里这 10 个名字，不存在 argv 直通任意工具名的入口。

## 3. 冻结的转发协议 v1

### 3.1 请求
逐字节冻结（每帧一行，`\n` 结尾，键顺序如下；`<T>` 是 JSON 转义后的 token，`<A>` 是 JSON 字符串数组）：
```
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"neige-forward","version":"1"},"_meta":{"dev.neige/auth":{"token":<T>}}}}
{"jsonrpc":"2.0","id":2,"method":"neige/cli","params":{"argv":<A>}}
```
`id` 必不可少：没有 `id` 的帧被 `parse_frame`（`mcp/framing.rs:40-52`）判为通知，`transport.rs:318-320` 直接丢弃，调用会永远挂起。
`clientInfo.version` 就是转发协议版本，只此一处。argv 是 `argv[1..]` 原样（非 UTF-8 见 §3.3）。

**不传 cwd、env 或 stdin，理由如下**：每个工具都从卡片身份解析 track（`track_file.rs:126-154`）；path 是 track 视图路径，不是文件系统路径；
`--artifact` 是不透明字符串（`calm-types/src/event.rs:47-49` 的 `ArtifactRef(String)`），不按 cwd 解析；没有任何命令读 stdin 或判断 tty。
环境变量里只有 socket 和 token 两项，属于传输层，本身就是连接凭据。

### 3.2 响应
- 成功：`result = {"stdout": String, "stderr": String, "exit": 0..=255}`。转发器把 stdout 和 stderr **逐字节**写出，然后 `exit(exit)`。
  `--json` 的输出由内核生成、转发器原样透传，转发器完全不知道 `--json` 的存在。
- 命令层面的错误（usage、工具的 `RpcError`、渲染错误）都是**成功的** JSON-RPC 响应，只是 `exit` 为 1 或 4，
  stderr 的格式与今天一致：`neige: <msg>` 或 `{"error":{message,detail}}`。
- 内核只给出退出码 {0,1,4}：`mcp/cli` 的退出码是只有这三个值的类型 `CliExit`，测试 `kernel_exit_codes_are_exactly_0_1_4` 断言该集合，并断言它与转发器保留的 {2,3,5,141} 不相交。
  2、3、5、141 只由转发器给出（§3.3）；4 两边都用：内核用于工具和渲染错误，转发器用于传输失败。
- JSON-RPC `error` 只用于传输层：围栏拒绝（§4）、协议版本不符、非 CardBound 身份，以及旧内核回的 `-32601`。

### 3.3 转发器自己的输出（冻结，只有这些）
| 情形 | stderr（纯文本） | 退出码 |
|---|---|---|
| `NEIGE_MCP_SOCKET` 或 `NEIGE_MCP_TOKEN` 未设或为空串 | `neige: missing <VAR> env var; run from a neige planner terminal` | 2 |
| 连接失败 | `neige: connect <sock>: <err>` | 3 |
| JSON-RPC error、断连、帧无法解析 | `neige: <method>: <message> (code N)`（无 code 时省略括号） | 4 |
| argv 含非 UTF-8（今天 `env::args()` 直接 panic，退出码 101） | `neige: argument <i> is not valid UTF-8` | 5 |
| 写 stdout/stderr 失败，含 EPIPE（`neige cat x \| head`） | 不再写任何东西，静默退出 | 141（128+SIGPIPE，与 shell 惯例一致） |

这些一律纯文本，因为转发器不解析 argv，也就不认识 `--json`（KNOWN GAP K2）。

### 3.4 help 与 `--version`
`--help`、`-h`、`help [cmd]` 和未知命令一样作为 argv 发到内核，由 `mcp/cli/help.rs` 返回，所以需要有效的 socket 和 token（K1）。帮助文字只在内核里有一份。

**唯一例外**：`argv == ["--version"]`（恰好一个参数）由转发器本地打印 `neige <CARGO_PKG_VERSION>` 并退出 0，不连 socket。
理由：发布打包对每个二进制跑 `--version` 并要求退出 0 且输出 semver（`crates/neige-app/src/package.rs:135,294-305`，`scripts/release/build-alpha.sh:73-83` 依赖它；
`crates/neige-app/src/identity.rs:31` 也这样取版本），打包环境没有 socket。这是转发器二进制的身份，不是命令语义，属于冻结的转发器表面（H11）。

### 3.5 为什么不需要再改
协议只承载 argv 进、字节和退出码出。命令、参数、默认值、渲染、帮助、错误文字全部在内核一侧，增删或修改它们都只动内核，转发器和协议不变。
只有需要 stdin、流式输出、tty 或 cwd 的命令才需要 v2，而目前没有这样的命令。真到那一步时，内核对未知的 `version` 以 §4 的同一条消息拒绝。

## 4. 旧客户端围栏

### 4.1 规则（`mcp/handshake.rs::handle_initialize`，放在取 token（`:31`）之前，不查数据库）
- `clientInfo.name == "neige"` → `RpcError::custom(-32426, msg)`，名为 `OLD_NEIGE_CLIENT_CODE`（沿用 `-32401`/`-32403` 映射 HTTP 状态码的约定，426 = Upgrade Required）。
- `clientInfo.name == "neige-forward"` 且 `version != "1"` → 同一个码，消息里写明不支持的版本号。
- 其他 `clientInfo`，或没有 `clientInfo`：行为与今天完全相同。
- 围栏在取 token **之前**判断：旧客户端即使带着无效 token 也得到 `-32426`，不是 `-32401`（T11）。

消息内容（路径取自 `srv/kernel_bin_path.rs:31` 的 `kernel_bin_dir()`，改为 `pub(crate)`，再 `.join("neige")`）：
`this neige binary parses commands itself and is no longer served; run /home/kenji/.local/share/neige-next/bin/neige. If that is the binary you just ran, the release is split: redeploy all binaries together.`
后半句覆盖拆分部署（§8 R2）：只换了内核时，被拒的正是消息里这个路径，消息本身要说明原因。
如果 `current_exe` 取不到，消息改为 `...; the kernel bin dir is unavailable: <err>`，但**照样拒绝**。

旧客户端看到的是 `main.rs:135-140` + `:1209-1221` 渲染的结果：
`neige: initialize: this neige binary ... run <abs>/neige (code -32426)`，退出码 4。带 `--json` 时输出 `{"error":{..."rpc_error":{code,message}}}`。

### 4.2 不会误伤的客户端（实测）
- 仓库里所有向内核 socket 发 initialize 的地方，只有 `cli/main.rs:122-125` 用了 `"neige"`（`rg clientInfo` 的结果：测试夹具
  `mcp-test`、`e2e-test-client`、`terminal-test`、`codex-e2e-test` 等；`plugin_host/http_mcp.rs:306` 的 `neige-kernel` 是内核**向外**连插件时用的）。
- `neige-mcp-stdio-shim` 不改 `clientInfo`，只注入 token（`crates/neige-mcp-stdio-shim/src/frames.rs:65-112`），所以对内核而言 clientInfo 就是上游客户端自己的。
- codex 0.153.4（`CALM_CODEX_BIN`）二进制里的名字是 `codex-mcp-client`；Claude 2.1.280 二进制里是 `claude-code`，标题为 "Claude Code"。

### 4.3 4140 盘上的副本（实测，假 socket 截获的 initialize）
截获方法：用一个假 UDS 监听，依次运行 `<bin> state`，记录发来的第一帧（草稿脚本 `cap.py`，没有接触运行中的内核）。
46 个二进制全部发出 `{"name":"neige","version":"0.1.0"}` 和 `protocolVersion 2024-11-05`，分布如下：
`neige-next/bin/neige`；`release-backups/*/bin/neige`（27 个，2026-09-08 至 09-24）；`.stage-*/old-bin/neige`（11 个）；
`neige-calm/target/debug/neige`；`wt1699-target`、`rv1781a-target`、`r1699a-target` 的 `debug/neige`；`neige-alpha*/.../bin/neige`（3 个）。
`clientInfo.name` 自 #344（`0d384ca69`）起就没变过，因此它们都会被围栏拦下。`version` 字段没有区分力，所以不拿它来判断。

## 5. 双重语义风险清单

| # | 可能留下两份真相的地方 | 处置 |
|---|---|---|
| H1 | 客户端命令表 vs 内核命令表 | `cli/main.rs` 的 `Cli::parse`、`Command` 和 `cli/help.rs` 删除，只保留 `mcp/cli/commands.rs`。 |
| H2 | gate guard 另有一份命令表和 `--help` 豁免（`srv/track_report_gate_guard.rs:55-87`） | 删除命令列表和 `--help` 豁免：以后**任何**直接调用 `neige` 的 gate 都会被拒，因为 help 也需要 socket（§9 F1）。`:119-129` 的正例和 `tests/cases/mcp_track_report_blocks.rs:1660-1662` 改为反例。 |
| H3 | `added→new` 重命名：客户端 `main.rs:604-611` 与 `calm-truth` 的 `observation_label`（`types.rs:65-71`，`read.rs:253` 在用） | 在 `calm-truth` 增加 `DiffStatus::from_wire_label`，并把 `observation_label` 改为 `pub`；`render::diff` 只调用它们。未知状态算渲染错误，不透传。 |
| H4 | 客户端与工具重复的校验：`log --limit 0`（客户端拒绝，工具 clamp 到 1）、空 `--reason`（客户端拒绝，工具接受）、空 `--to/--path`（客户端拒绝，工具当 None）、`keep>0`、空 key | 以工具为准，CLI 层只负责把字符串解析成整数，不检查范围或非空。owner 已决定（§9 F5）：`calm.task.fail` 工具改为拒绝空或只含空白的 reason；limit 0 和空 to/path 按工具现有行为。 |
| H5 | CLI 参数名 vs 工具参数名（`--idempotency-key`↔`idempotency_key`、`--include-empty`↔`include_empty`、`--track-id`↔`track_id`、`--dry-run`↔`dry_run` 等） | 命令表中每个选项都声明它对应的工具参数键。测试 `every_cli_option_maps_to_a_tool_schema_property`：每个键都必须出现在 `ToolDescriptor.input_schema.properties` 里。显式豁免表只有没有工具参数的 CLI 选项：`--json`、`--force`、`-h/--help`。 |
| H6 | 错误文字 | 工具的错误原样透传（`neige: <tool>: <msg> (code N)`，格式同 `main.rs:1209-1221`）。CLI 层自己的文字只有 usage 和确认门这两类，它们本来就只存在于 CLI。 |
| H7 | 工具描述（`srv/prompts/tools/calm.track.*.md` 等） | 实测这些描述都不提 `neige`，所以不需要改。`track_file.rs:36` 和 `track_history.rs:5-6` 注释里的"consumed by `neige`"改为"consumed by `mcp/cli`"。 |
| H8 | 提示词和模板里的 `neige` 用法：`prompts/planner.md:56,60,120,140-145`、`prompts/worker/head-cli.md:5-9`、`head-mcp.md:5`、`tail.md:3`、`templates/builtin/*.md:9`、`calm-types/src/report/default.md:5`；以及钉住它们的 golden：`tests/goldens/issue_development_planner_prompt.txt`、`worker_prompt_cli.txt` | 命令面**不变**，所以这些文件和 golden 都不改。新增测试 `prompt_neige_mentions_name_served_commands`：扫描 `prompts/**`、`templates/builtin/**` 和 `calm-types/src/report/*.md` 里的 `` `neige <word>`` ``，`<word>` 必须在 `COMMANDS` 中。以后改名时这条测试会先失败。扫描范围之外的无害命中：`docs/events-retention.md:122`（`neige vacuum --force`，命令面不变）、`docker/Dockerfile.server:9`（注释）、`calm-truth/src/track_vcs/gc.rs:16`（注释，随 keep 常量改写，见 §1）。 |
| H9 | 现有测试：`crates/neige-cli/tests/neige_cli.rs`（719 行，用假服务器钉客户端的解析和渲染）、`main.rs:1224-1570` 的单测 | 这两处测试本身就是第二份规格，全部删除。解析测试搬进 `mcp/cli/commands.rs`；渲染和 help 测试改为针对真内核运行（沿用 `tests/cases/neige_cli_task_report.rs` 的做法）。转发器只保留协议测试（§7 T6、T7）。 |
| H10 | 转发器内置的环境变量名和退出码 2/3/4/5/141 | 这属于冻结协议的一部分（§3.3），不算命令语义。 |
| H11 | `neige --version`：发布打包 `crates/neige-app/src/package.rs:135,294-305`、`scripts/release/build-alpha.sh:73-83`、`crates/neige-app/src/identity.rs:31` 都跑它 | 转发器本地回答，报的是转发器 crate 版本（二进制身份），内核不提供 `--version` 命令，因此只有一个来源（§3.4）。 |
| H12 | keep=50：`cli/main.rs:1046` 与 `calm-truth/src/track_vcs/gc.rs:16-17` | 删除客户端那份；内核 CLI 表引用 `DEFAULT_TRACK_HISTORY_PRUNE_KEEP`（改 `pub`，经 `track_vcs` 再导出，§1），注释反转为"CLI 默认值即此常量"。 |

## 6. #1784 的 PATH 前置：保留，不改

`srv/kernel_bin_path.rs`，调用处为 `shared_codex_appserver.rs:530,1831`、`claude_planner/session.rs:386`、`routes/terminal.rs:123`。

理由：围栏只保证旧客户端**不会以旧语义执行**，但被拦下时代理仍要多走一轮，看提示再改用正确路径。PATH 前置让裸 `neige` 在绝大多数情况下直接命中内核旁边的新转发器。
这部分已经写好并有测试，保留它没有成本；删掉反而要改 env 签名盐（`shared_codex_appserver.rs:1814`），导致共享 daemon 被接管重启。
它不再承担正确性责任，所以前置失效时（例如 #1791 的 Claude shell 快照重排）也只是多一次拒绝，不会产生分叉语义。
围栏消息与 PATH 前置用同一个 `kernel_bin_dir()`，两处给出的路径必然一致。

## 7. 切片：一个 PR

**改动**（按生产代码行数估算）：`mcp/cli/`（约 770 行，其中约 450 行从 `cli/` 搬入）；新模块 `mcp/transport/call.rs` 放 `call_registered_tool`（约 +40），`transport.rs` 净增约 1 行；`gc.rs` 常量改 `pub`（+1）；
`handshake.rs` 围栏（约 +45）；`kernel_bin_path.rs` 改为 `pub(crate)`（+2）；`calm-truth` 的 `from_wire_label`（+10）；`emit.rs` 的 reason 判空（+3）；gate guard（约 -25）；
`cli/main.rs` 从 1570 行改写为约 120 行，删除 `help.rs`，删除 `tests/neige_cli.rs`，换成约 150 行协议测试。
新增的内核集成测试约 400 行，放在 `srv/tests/cases/neige_cli_*.rs`。

不拆分的理由：围栏必须和新转发器一起部署，否则会拦下唯一可用的客户端；而如果先上 CLI 层、后上围栏，中间那段时间就会同时存在两种语义。

**硬前提**：4140 只通过 `~/.local/share/neige-next/deploy/apply.py` 发布。它已经是整目录原子切换：`BINS`（`apply.py:22`）列出全部 6 个二进制（含 `neige`），
先复制进 stage 目录，再 rename 整个 `bin/`（`apply.py:142-179`），不需要新机制。owner 已批准这条规则，由协调者在仓库外落实：#1801 之后禁止单个二进制的临时替换（例如预览换装）。
（实测：当前 `bin/` 的 6 个二进制与 `c5abb3c3d` 构建产物 sha256 全部相同；`bin/neige` 的 mtime 较早，是因为 cargo 没有重新链接它，而 `shutil.copy2` 保留 mtime。今天不存在拆分。）

**验收**：发布后先核对 `bin/` 下所有二进制的 sha256 与本次构建产物一致（不一致即拆分，验收失败）；然后，`neige-next/bin/neige state` 输出与发布前逐字节相同；运行任意 `release-backups/*/bin/neige state` 都得到 §4.1 的消息，退出码 4；
Planner（codex 和 claude 两种）、终端 PTY、claude worker 的 `neige task-completed` 都能端到端跑通（Tier 2 栈在专用主机上跑，本机不跑真 codex）。

**必须先红的测试**（标 M 的做单因子变异，预测失败集合只包含该测试）：

| # | 测试 | 钉住的内容 | 变异 |
|---|---|---|---|
| T1 M | `old_fat_neige_client_is_refused_with_kernel_bin_path` | 用真内核重放 §4.3 截获的原帧，断言 `-32426`，且消息里包含 `current_exe().parent()/neige` | 删掉 name 判断 |
| T2 | `forward_protocol_version_other_than_1_is_refused` | `neige-forward` 且 `version:"2"` → `-32426` | — |
| T3 | `non_neige_client_infos_still_initialize` | `codex-mcp-client`、`claude-code`、无 clientInfo，以及经 shim 的 round trip 都能成功 | — |
| T4 M | `cli_output_equals_direct_tool_call` | 对 ls、state、diff、log：`neige --json <cmd>` 的 stdout 等于同一个 token 直接 `tools/call` 得到的 `structuredContent`（紧凑 JSON）；对 cat、cat-at（`--json` 不影响输出）以及上述四条的文本模式：stdout 等于 `render(直接结果)`，其中 JSON 类型内容 pretty 打印 | 在 `commands.rs` 里把 diff 的 `to` 映射成 `from` |
| T5 M | `cli_authorization_equals_direct_call` | Worker 调 `vacuum --force`、Planner 调 `task-completed`、**隔离 worker 调 `cat`**：exit 4，stderr 与直接调用的 `RpcError` 文字相同 | 在 CLI 路径里跳过 `worker_grants::require` |
| T6 | `forwarder_writes_bytes_and_exit_verbatim` | 假服务器返回 `{stdout:"a\n",stderr:"b",exit:7}`，转发器逐字节写出并以 7 退出 | — |
| T7 | `forwarder_frames_are_frozen` | 逐字节钉住 initialize 和 `neige/cli` 两帧 | — |
| T8 M | `force_gate_refuses_before_the_tool` | `track-gc`（无 `--dry-run`）和 `vacuum` 不带 `--force`：exit 1，`track_vcs` commit 数不变、未执行 VACUUM | 分别删掉 `track-gc` 和 `vacuum` 的门（两次单因子变异，各自只红对应的子断言） |
| T9 | `every_cli_option_maps_to_a_tool_schema_property` / `prompt_neige_mentions_name_served_commands` | H5、H8 | — |
| T10 | gate guard 翻转后的正例和反例 | H2 | — |
| T11 M | `old_client_fence_precedes_token_check` | 旧客户端帧配无效 token → `-32426`，不是 `-32401` | 把围栏挪到取 token 之后 |
| T12 | `neige_cli_on_daemon_trust_connection_is_refused` | 用 daemon token 初始化后发 `neige/cli` → JSON-RPC error，零次工具调用 | — |
| T13 | `help_and_unknown_commands_are_served_by_the_kernel` | `--help`、`help cat`、`cat --help`、未知命令经真内核返回；stdout/stderr 与 `mcp/cli/help.rs` 一致，退出码 0/1 | — |
| T15 M | `task_fail_rejects_blank_reason`（工具级，直接 `tools/call`，外加一次 `neige task-failed --reason "  "`） | `""` 和 `"  "` 都得到 `-32602`，且不写任何事件 | 删掉工具里新增的 trim 判空 |
| T14 | `forwarder_local_failures_are_fixed` | 空环境变量、非 UTF-8 argv、stdout 关闭（EPIPE）、`--version` 不连 socket 并输出 semver：各自的消息和退出码见 §3.3/§3.4 | — |

**测试文件与定向过滤**（每行的过滤器都实际覆盖该行的测试）：

| 测试 | 文件 | 过滤器 |
|---|---|---|
| T1、T2、T3、T11、T12 | `crates/calm-server/tests/cases/neige_cli_fence.rs`（`mcp_core_suite`）；T3 的 shim 一腿就是同 suite 已有的 `mcp_shim_round_trip::shim_round_trip_initialize_and_tools_call_completes`，它经过同一个 `handle_initialize` | `-p calm-server --test mcp_core_suite -E 'test(neige_cli_fence) \| test(mcp_shim_round_trip)'` |
| T4、T5、T8、T13 | `crates/calm-server/tests/cases/neige_cli_commands.rs`（`mcp_core_suite`） | `-p calm-server --test mcp_core_suite neige_cli_commands` |
| T15 | `crates/calm-server/tests/cases/mcp_emit_tools.rs`（`mcp_core_suite`） | `-p calm-server --test mcp_core_suite task_fail_rejects_blank_reason` |
| T9、迁入的解析单测、`kernel_exit_codes_are_exactly_0_1_4`、渲染规则单测 | `crates/calm-server/src/mcp_server/cli/{commands,mod,render}.rs` | `-p calm-server --lib mcp_server::cli` |
| T10 | `crates/calm-server/src/track_report_gate_guard.rs`（单测）与 `crates/calm-server/tests/cases/mcp_track_report_blocks.rs`（`mcp_integration_suite`） | `-p calm-server --lib track_report_gate_guard`；`-p calm-server --test mcp_integration_suite task_gate` |
| T6、T7、T14 | `crates/neige-cli/tests/forwarder.rs` | `-p neige-cli` |
| H3 `from_wire_label` | `crates/calm-truth/src/track_vcs/types.rs`（单测 `from_wire_label_inverts_wire_label_and_rejects_unknown`） | `-p calm-truth from_wire_label` |

**变异的完整预测红集**（每次变异前写定；跑上表全部过滤器，实际红集必须与此逐一相同）：

| 变异 | 位置 | 预测红集 |
|---|---|---|
| M1 删掉 `name == "neige"` 判断 | `handshake.rs` | `old_fat_neige_client_is_refused_with_kernel_bin_path`、`old_client_fence_precedes_token_check` |
| M4 diff 第二个位置参数的键 `to` 改成 `from` | `cli/commands.rs` | `cli_output_equals_direct_tool_call`、`diff_maps_positionals_to_from_to_and_path`（即迁入的 `tests/neige_cli.rs:428-436` 参数映射与 `main.rs:1278` 单测）、`diff_rejects_the_same_key_twice` |
| M5 删掉 `call_registered_tool` 里的 `worker_grants::require` | `transport/call.rs` | `cli_authorization_equals_direct_call` |
| M8a 去掉 `track-gc` 的确认门 | `cli/commands.rs` | `force_gate_refuses_before_the_tool`（track-gc 子断言）、`track_gc_requires_force_unless_dry_run` |
| M8b 去掉 `vacuum` 的确认门 | `cli/commands.rs` | `force_gate_refuses_before_the_tool`（vacuum 子断言）、`vacuum_requires_force` |
| M11 把围栏挪到 token 校验成功之后 | `handshake.rs` | `old_client_fence_precedes_token_check` |
| M15 删掉 `calm.task.fail` 的 trim 判空 | `tools/emit.rs` | `task_fail_rejects_blank_reason` |

M5 只能改共用函数：CLI 与 `tools/call` 走同一个 `call_registered_tool`，不存在只属于 CLI 的 grants 调用可删，这正是 §2 要的结构。

**门禁**：上表的定向 nextest；`scripts/local-rust-gates.sh --quick`；`scripts/gate-prose-ratchet.sh`
（usage 长串 `main.rs:1191` 会移动位置，如果 `long_literal` 计数变了就用 `--update-baseline` 更新）；`scripts/gate-1316-terminology-ratchet.sh`。
工具注册 golden（`tests/goldens/mcp_tool_registry.json`）不因 `neige/cli` 变化（它不是工具）；唯一的变化是 `calm.task.fail` 的描述（`prompts/tools/calm.task.fail.md`）写明 reason 不能为空（F5），用 `REGEN_MCP_TOOL_REGISTRY_GOLDEN=1` 重新生成。

## 8. 风险

- R1 响应是一整行 JSON，`cat` 或 `diff` 输出很大时内存占用会升高。实际上限就是今天 `structuredContent` 已经承载的大小（内容本来就经同一条 socket 传一次），所以没有新增风险。
- R2 如果有人绕过 `apply.py` 只换了内核，旧的 `bin/neige`（包括 worker 的 `task-completed`）会全部被拦下，消息指向同一个 `bin/neige`。
  防线有三层：§7 的硬前提（只走 `apply.py`）、验收的 sha256 核对、以及围栏消息里的拆分提示（§4.1）。拆分提示成本很低，所以保留。
- R3 `current_exe` 在二进制原地被替换后会带 ` (deleted)` 后缀，但它的 parent 目录不变，给出的路径仍然正确。
  另有一个窗口：`deploy/apply.py:175-179` 先把 `bin/` 改名为 `.stage-*/old-bin`，再把新目录改名进来；这段时间里运行中的旧内核 `/proc/self/exe`
  解析到 `.stage-*/old-bin/calm-server`，围栏消息指向 `.stage-*/old-bin/neige`。对 v1 到 v1 的升级无害：那个 `neige` 也是 v1 转发器，照样能用；窗口随旧内核重启结束。

## 9. 与 issue 相左或补充的发现

- F1 issue 写的是"`neige` 只做转发"。这意味着 `--help` 也要连内核，所以 gate guard 的 `--help` 豁免（`track_report_gate_guard.rs:70-72`）会变成错误的豁免，必须删除（H2）。
- F2 issue 没提到 worker grants。CLI 路径如果只做角色判断，就会让隔离 worker 读到直接调用读不到的视图（§2、T5）。
- F3 状态重命名在内核里已经有一份（`calm-truth` 的 `observation_label`），客户端那份属于第三份来源，现在一并收敛（H3）。
- F4 只有 `clientInfo.name` 能区分新旧客户端，`version` 在所有副本中都是 `0.1.0`（§4.3），不能用来区分。
- F5 **owner 已决定**：`calm.task.fail` 工具（唯一来源）拒绝空或只含空白的 `reason`，改 `emit.rs:382-386`，错误文字为 ``task_fail: missing `reason` (non-empty)``，
  MCP 直接调用和 CLI 都受这一处约束（T15）。`log --limit 0` 和空的 `diff --to/--path` 按工具现有行为处理：分别 clamp 到 1、视为未传。检查一律不留在 CLI 层。
- F6 `--version` 不能进内核：发布打包在没有 socket 的环境里对 `neige` 跑 `--version`（H11）。

## 10. KNOWN GAPS

- K1 在卡片 shell 以外（没有 socket 或 token）运行 `neige --help`，只会得到缺环境变量的提示。
- K2 转发器本地失败（退出码 2、3、4、5、141；141 不输出任何内容）即使带了 `--json` 也输出纯文本。
- K3 新转发器连接 #1801 之前的内核时，只会得到 `neige/cli: Method not found`，不带路径提示；兼容只看 4140，这种情况靠原子发布排除。
- K4 未来需要 stdin、流式输出或 cwd 的命令要用 v2 协议，当前没有这样的命令。
- K5 直接用 `tools/call` 调隐藏的 `calm.admin.*` 仍然不经过 `--force` 确认门，与今天相同；本设计不改工具。

## 附录：第 1 轮评审处置

逐条先核实再修改，结论如下：
1. `--version` 会破坏打包：**接受**。核实 `neige-app/src/package.rs:135,294-305`、`build-alpha.sh:73-83`、`identity.rs:31`。改动见 §3.4、H11、F6、T14。
2. keep=50 有第二份、`ls "/"` 默认值与工具重复：**接受**。核实 `gc.rs:16-17` 和 `track_file.rs:112-118`。改动见 §1、H12。
3. T4 与清单矛盾：**接受**。核实 `main.rs:80,83` 中 cat/cat-at 的 `--json` 只影响错误。T4 已改。
4. 今天存在拆分部署：**前提驳回，防御保留**。sha256 显示 `bin/` 的 6 个二进制都与 `c5abb3c3d` 构建产物相同，mtime 不能说明问题（`copy2` 保留 mtime，cargo 没有重新链接 `neige`）。
   `apply.py:22,142-179` 本来就是整目录切换。硬前提、sha256 验收和拒绝消息里的拆分提示都保留（§4.1、§7、R2）。
5. 必须先红的测试：**接受**。T8 改为变异验证，新增 T11–T14。
6. 冻结帧缺 `id`：**接受**。核实 `framing.rs:40-52` 和 `transport.rs:318-320`，§3.1 现在写出了完整字节。
7. 转发器失败未定义完整：**接受**。§3.3 新增表格，非 UTF-8 为 5，EPIPE 为 141，空串按缺失处理。
8. 兜底分类：**接受**。核实 `types.rs:99,101` 均为 `Option`，§1 已把它们归为渲染规则。
9. 事实与规模：**接受**。核实 `transport.rs:1645-1647` 的 `kernel.sock` 和 `transport.rs` 的 1647 行。已修 §0 与 §2，新增同级模块，修正 H5 豁免表，
   写明 `isError` 分支删除，补上 H8 的无害命中。
10. R3 改名窗口：**接受**。核实 `apply.py:175-179`，已补进 R3。
owner 的决定（空 reason、limit 0、空 to/path）已作为决定写入 §9 F5、H4 和 T15。
