# 外部 MCP / CLI 接入 — v0 walking skeleton（#1164）

**状态：** r5。r4 经双通道窄评审（两路均 SHIP-WITH-FIXES，共同命中 install 路径断裂与物化定位两条 critical），本稿逐条修正。评审账目见 §8。
**范围：** 把场景跑通，不是把 connector 平台建完。副作用型 CLI 已拆出 #1167。
**判据来源：** `docs/architecture/955-kernel-app-boundary.md` §1.1。

---

## 0. 目标

让 agent 在 track 里调到两个**真实**的外部能力：

1. `https://mcp.wisburg.com/mcp?api_key=…` —— 远程 streamable-HTTP MCP server
2. `/usr/local/bin/longbridge` —— 本地只读查询型 CLI

**配置形态：** 在 `plugins_dir` **内部**建 connector 目录（`manifest.json` + 可选 `secrets.json`），然后走现成的安装流程：

```
POST /api/plugins/install  {"source":{"local_path":"<plugins_dir>/mcp.wisburg"}}
POST /api/plugins/{id}/enable
```

**目录必须在 `plugins_dir` 内**——这不是偏好，是绕开 #1168：`materialize_install_tree` 对外部源建的是**符号链接**（`routes/plugins.rs:1157`），而 `load_from_dir` 用 `entry.file_type()` 判定目录，**不跟随符号链接**（`registry.rs:124`），于是重启后 registry 里没有该条目，`enabled` 的 DB 行让 `spawn()` 在 `registry.get` 处 `NotFound`（`mod.rs:404`），全程只有一条通用 warn。源目录在 `plugins_dir` 内则命中 `src == dst` 短路（`routes/plugins.rs:1123`），落的是真目录，不受影响。

#1168 修好后本约束可放宽。**v0 仍是安装路径零新代码。**

---

## 1. 事实基线（全部已复核）

### 1.1 目标 server 的实测形态（2026-08-31 探测）

| | |
|---|---|
| 协议 | `2025-06-18`；`serverInfo = wisburg-mcp 0.8.4` |
| 会话 | **无状态**——不返回 `Mcp-Session-Id`；不 `initialize` 直接发 `tools/list` 也能工作 |
| 响应 | `content-type: text/event-stream`，但只有**单条** `event: message` + 一行 `data:` 即结束，非长连接流 |
| 工具 | 13 个，全为 `list-*` / `get-*` 只读查询 |
| 鉴权 | query param `api_key`；不带 key 可 `tools/list`，带 key `tools/call` 返回真实数据 |

⇒ 传输层 = 「POST 一个 JSON，剥掉 `data: ` 前缀再 parse」。不需要会话管理、服务端→客户端 GET 流、断线续传。

### 1.2 三条真正的拦截

**(a) 盘上的 manifest 不会自己启动。** `autospawn_enabled`（`mod.rs:377-393`）遍历的是 `repo.plugins_list_all()` 里 `enabled == true` 的 **DB 行**，全程不看 registry；`plugin_install` 的唯一生产写入方是 install REST 路由（`routes/plugins.rs:399`）。disk→registry 的水合存在，**registry→spawn 的不存在**。
⇒ 解法是走现成 install/enable（§0），不是新增水合。**附带约束见 §0 的 #1168。**

**(b) 三个工具目录读者都只读 `manifest.exposes_tools`：** `plugin_tool_descriptors`（`mcp_server/transport.rs:567`）、`plugin_tool_route`（`:739-744`）、boot 审计 `Event::PluginToolRegistered`（`state.rs:1109-1123`）。CLI 工具在 `cli_query.tools`、MCP 工具在运行时缓存，**都不是 `exposes_tools`** ⇒ 不处理则两个 connector 暴露**零个**工具。解法见 §2.7。

**(c) 可见性以 Running 为前提，而 `RunningPlugin` 强制持有进程与 MCP client：** `running_plugin_ids` 只返回 `Running` 的 id（`mod.rs:778-785`），`process` 与 `mcp` 均为必填（`mod.rs:121-124`）。远程 HTTP 与 `cli-query` 都没有进程。

### 1.3 其余相关事实

- **registry 的 in-memory 条目可被覆写**：`PluginRegistry::insert` 在写锁内整体替换 `Manifest`（`registry.rs:196-203`）。读者安全（不会看到撕裂状态），但**整体替换**正是 §2.7 必须避开的（见 D11）。
- **`GET /api/plugins/{id}` 返回的是 DB 行里的 manifest**，不是 registry 的（`routes/plugins.rs:1204, 1226`，写入于 `:396` install 与 `:685` reload）。⇒ ①secrets 不进 manifest 的结论成立；②**物化的工具在所有 REST 面上都不可见**（详情、列表 `:253-267`、views `:720-726`），见 R10。
- **plugin 子进程不做 `env_clear()`**：`process.rs:88-104`。⇒ `cli-query` 必须自建环境。
- **`plugin_scope_for_track` 的 `Only(id)` 要求 `running ∧ trusted_forge_plugin(id)`**（`tool_visibility.rs:119-120`，未受信落 `None` 有测试钉住 `:237-262`）。
- **iframe 工具调用已 deny-by-default**：`can_call_tool` 对无 views 的 manifest 返回 `false`（`perms.rs:151-156`）。这条防线不用新写。
- **forge 凭据透传**：`FORGE_PASSTHROUGH_ENV_KEYS`（`forge_action_adapter/mod.rs:61-72`）。`cli-query` 刻意不走该路径（§2.3），这是与 #1167 的分界线。
- **树内只有一个 checked-in manifest**：`plugins/git-forge/manifest.json`，不含 `kind` 键。
- **生产运行形态**：`neige-app.service`（systemd，宿主机），`PATH=/home/kenji/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin`。`longbridge` 在 `/usr/local/bin` ⇒ 可达；`~/.nvm/*/bin`、`~/.pyenv/shims`、`~/.cargo/bin`、`~/gopath/bin` 均在服务 PATH 之外。PR 预览栈是 docker，宿主机 CLI 不可见。
- **`autospawn_enabled` 在 `AppState::new` 里 inline await**（`state.rs:1101`）。⇒ 任何落在 spawn 路径上的网络调用都会**阻塞启动**，见 §2.2 的超时要求。

### 1.4 r3 曾误列为「硬拦截」的两条

「协议版本严格相等」（`mcp.rs:414`）与「token 回显」（`mcp.rs:424-432`）都活在 `McpClient::initialize` 内，只能从 `connect_with_auth` 到达（`mcp.rs:292, 351`）。`mcp-http` 走独立的 `HttpMcpClient`，**不经过这段代码** ⇒ **`mcp.rs:414` 的放松工作删除**。

但握手并非无事可做：
- `spawn_admitted` 仍须**按 kind 分支**，分支点在 `ensure_plugin_token()`（`mod.rs:528`）**之前**，否则会给外部 connector 白铸 token 行。
- `HttpMcpClient` 需要自己的极简 `initialize`，不携带 `_meta["dev.neige/auth"]`、不声明 `experimental.dev.neige/kernel-callbacks`（后者在 `mcp.rs:351-368` 是无条件的）。
- **判据必须是显式 profile，不得用 `expected_echo.is_none()`**——那会顺带放宽测试用的无认证 stdio 客户端（`callbacks.rs:873-874, 999-1001`）。
- 「内核已知版本集合」在代码中不存在（只有 `KERNEL_PROTOCOL_VERSION`，`mcp.rs:45-47`）⇒ v0 的 `HttpMcpClient` **不做版本判定**，把服务端版本记进日志。

---

## 2. 形状

### 2.1 `Manifest.kind`

```rust
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectorKind {
    #[default] App,     // 今天的 plugin，语义一字不改
    McpHttp,            // 远程 streamable-HTTP MCP server
    CliQuery,           // 只读查询型 CLI
}
```

字段上必须有 `#[serde(default)]`（enum 的 `Default` derive 单独不够）。树内唯一 manifest（`plugins/git-forge`）不含 `kind` 键。

**不用 `flatten` + 内部标签 enum**（`kind` 键重复的 round-trip 隐患）。改为两个互斥可选顶层块 `mcp_http` / `cli_query`，`parse` 校验「`kind` 与对应块同时存在且互斥」。`entrypoint` 对非 `App` 变为可选（今天必填且无条件校验，`manifest.rs:65, 364`）。

> 已确认：`entrypoint` 变成 kind 条件之后，install 不再有任何会拒绝 connector manifest 的校验——它只做路径解析、`Manifest::parse`、semver/`min_kernel_version`、重名检查（`routes/plugins.rs:341, 352`），从不 stat entrypoint 二进制；空 `views` 本就合法（`manifest.rs:67`）。`secrets.json` 随整目录一起被 link/copy（`routes/plugins.rs:1157, 1190`）。

### 2.2 `mcp-http`

> **【2026-09-02 · #1268】** 下面两个示例里的 `"manifest_version": 1` 仍然**有效**，
> 无需修改：#1268 把 `manifest_version` 提到 2，但**只对声明了非空 `templates[]`
> 的 manifest 强制**。connector（`mcp-http` / `cli-query`）从不声明模板绑定
> （§3「通道上不存在的能力」），在两代内核上读法完全相同，因此留在 1 是对的。
> 详见 `plugin_host::manifest::Manifest::manifest_version` 的字段文档。

```jsonc
{ "manifest_version": 1, "kind": "mcp-http",
  "id": "mcp-wisburg", "version": "0.1.0", "min_kernel_version": "0.0.1",
  "display_name": "Wisburg 研报",
  "mcp_http": {
    "url": "https://mcp.wisburg.com/mcp",
    "api_key_secret": "WISBURG_API_KEY",     // 值取自 secrets.json，§2.4
    "api_key_in": "query:api_key",           // 闭集：query:<name> | header:<name>
    "tools_allow": ["list-institutional-reports", "get-report-detail", "list-market-daily"],
    "request_timeout_ms": 10000
  } }
```

> **id 不含 `.`**（见 R11）：id 允许含 `.` 会让 `plugin.a.b_x` 在「id=`a.b`/工具=`x`」与「id=`a`/工具=`b_x`」之间产生歧义。`plugin_tool_route` 多候选时 fail-closed（工具变成不可调用，不是错路由，`transport.rs:748-760`），但 v0 直接用 `mcp-wisburg` / `cli-longbridge` 这种无点 id 避开。

**工具目录：手写允许列表 + enable 时取 schema**，随后物化（§2.7）。不手抄 schema（13 个 `inputSchema` 长且会漂移）；不做 DB 表与勾选 UI（允许列表已给完全控制权，且这正是长期形状——以后允许列表从 manifest 挪到 DB + 勾选 UI，运行时代码不动）。

`tools_allow` 里出现 server 没有的名字 → enable 时告警并忽略该条，不整体失败。

**超时是硬要求。** `tools/list` 落在 spawn 路径上，而 `autospawn_enabled` 在 `AppState::new` 里 inline await（`state.rs:1101`）⇒ 上游挂掉会**阻塞服务启动**。stdio 握手已有 10s 上限（`mcp.rs:373`），`reqwest` 默认**无**超时。⇒ `HttpMcpClient` 必须显式设 connect + request 超时（默认 10s，`request_timeout_ms` 可覆盖）；超时/失败 → connector 落 `Unavailable{reason}`，**不阻塞 boot**。非 app 无 supervisor，因此无自动重试——运维需手动 re-enable，这一条写进 §4 验收。

**客户端类型：**

```rust
#[derive(Clone)]
enum ConnectorClient {
    Stdio(Arc<McpClient>),         // app，今天的路径，行为不变
    Http(Arc<HttpMcpClient>),      // url + key + reqwest，无会话
    Cli(Arc<CliQueryRuntime>),     // 钉住的绝对路径 + 指纹 + 环境 + 工具表
}
```

**必须廉价 `Clone`（全部 Arc 包）**：访问器要在 `std::sync::MutexGuard` 下把它克隆出来（进程表锁不能跨 await 持有，`mod.rs:329-333`）。

`HttpMcpClient::request` = POST JSON → 读 body → 剥 `data: ` → parse。`McpClient` 今天**没有** `tools_list`（只有 `tools_call` `mcp.rs:507`、`resources_read` `:530`、通用 `call` `:542`、`notify` `:569`），在 `call()` 上包一层。

### 2.3 `cli-query`

```jsonc
{ "manifest_version": 1, "kind": "cli-query",
  "id": "cli-longbridge", "version": "0.1.0", "min_kernel_version": "0.0.1",
  "display_name": "Longbridge 行情",
  "cli_query": {
    "command": "longbridge",            // 裸名或绝对路径
    "search_path_extra": [],            // 仅本 connector 生效
    "env_allow": [],                    // 从服务环境放行的键（默认空）
    "secret_env": ["LONGBRIDGE_TOKEN"], // 值取自 secrets.json
    "timeout_ms": 20000,
    "max_output_bytes": 32768,
    "tools": [{
      "name": "quote",
      "description": "Get a quote for one symbol",
      "input_schema": { "type": "object",
        "properties": { "symbol": { "type": "string" } },
        "required": ["symbol"], "additionalProperties": false },
      "args": ["quote", "{{symbol}}"]
    }]
  } }
```

**执行**：直接 `Command::new(<钉住的绝对路径>).args(...)`，**没有 shell**（与 forge 路径不同，后者生成 `/bin/sh` 脚本并 `sh -c`，`forge_action_adapter/mod.rs:200-211, 1303-1306`）。

**argv 模板**：`{{x}}` 必须**整体占据一个 argv 元素**，只做单参数替换，不拼字符串、不做 shell 解析。槽位名必须是 `input_schema` 的顶层 key；未在 schema 中出现 → parse 期报错。

**环境**：`env_clear()` + `{PATH, HOME, LANG}` 基础集 + `env_allow` + `secret_env`。子进程 `PATH` = 服务 PATH + `search_path_extra`，**只影响本 connector**。

**刻意不走 forge-action**：只读查询不需要幂等/parked/恢复，因此不碰 `trusted_forge_plugin`，也不会拿到 `FORGE_PASSTHROUGH_ENV_KEYS`（§1.3）。有副作用的 CLI 归 #1167。

**命令解析与钉住**：enable 时按「服务 PATH + `search_path_extra`」解析一次 → 存**绝对路径**；运行时直接 exec 绝对路径，不再做 PATH 查找。记录指纹（`<command> --version` stdout 首行；失败则记 size+mtime）。解析失败 → `Unavailable{reason}`，**reason 必须包含服务的 PATH 与搜索过的目录**。

**不做全局 PATH 管理**：隐式 env 开关（与项目共识相悖）、fail-open（为 A 加目录会静默改变 B 的解析）、不可审计、且解决不了 docker 那半边。

**输出**：stdout 截到 `max_output_bytes` 并加显式截断标记；stderr 单独截 4 KiB 一并返回；非零退出码 → `isError: true` + 输出，不 panic、不重试；超时 → SIGKILL + 明确错误。

### 2.4 secrets

**v0 不建表。** connector 目录下一个 `secrets.json`（`{"WISBURG_API_KEY": "sk-…"}`）：只由内核读；不进 manifest，因此不出现在 `GET /api/plugins/{id}` 的 manifest 字段；v0 没有任何读它的路由；要求权限 0600，否则拒绝 enable 并告警。以后挪进 DB 表时，manifest 里的引用名不变。

### 2.5 无进程 connector 的「在线」（§1.2(c) 的解法）

```rust
struct RunningPlugin {
    process: Option<Arc<PluginProcess>>,   // 由必填改为可选
    mcp: ConnectorClient,                  // 由 Arc<McpClient> 改为枚举（字段名保持 `mcp`）
    router: Option<JoinHandle<()>>,        // 由必填改为可选：非 app 无入站路由
    // subscriptions 保持，非 app 恒空
}
```

`running_plugin_ids`（`mod.rs:778-785`）只按 `status` 过滤，零改动。

| 站点 | 位置 | 处理 |
|---|---|---|
| `spawn_admitted`：铸 token → spawn → 握手 → router → supervisor，全无条件 | `mod.rs:521-621`，记录构造 `:640-652` | **按 kind 分支，分支点在 `ensure_plugin_token`（`:528`）之前** |
| `stop()` 克隆 process 并 abort 必填 router | `mod.rs:665-707` | kind 感知：非 app 只丢弃 client |
| `status()` / `list_running()` 解引用 `rp.process.pid()` | `mod.rs:743`、`:769` | `rp.process.as_ref().and_then(\|p\| p.pid())` |
| `stderr_tail` / 崩溃尾 | `mod.rs:790-793`、`:931-938` | 非 app 返回空；`/log` 路由已把 `None` 默认成 `[]`（`routes/plugins.rs:591-606`），路由不改 |
| `dispatch_neige_callback` 直接克隆 `rp.mcp` | `mod.rs:831-853` | 新增 arm：非 `Stdio` 一律返回 not-running |
| 入站 `neige.*` router 需要 `Arc<McpClient>` | `mod.rs:1073-1116` | 只对 app 构造 |
| `CallbackCtx.mcp: Arc<McpClient>` + 长生命周期事件桥 | `callbacks.rs:55-70, 691-725` | **类型不变**，只由 app 路径构造 |
| `rotate_plugin_token` 无条件删 token + restart | `mod.rs:363-371`，路由 `routes/plugins.rs:73, 1027` | 非 app 显式 4xx，**在删除与 restart 之前** |
| 崩溃退避 / crash-loop | `mod.rs:912-1013` | 非 app 不建 supervisor，自然不触发 |
| uninstall | `routes/plugins.rs:540-564` | 除其 `stop()` 调用外无额外假设（但见 R12） |

**防放宽回归测试**：`running_plugin_ids` 从来只看 `status`，崩溃路径在移除条目**之前**先置 `status = Crashed`（`mod.rs:961`），故把 `process` 变可选不削弱「进程死了工具立即不可见」。测试钉住即可。

### 2.6 `mcp_client()` 的接缝

生产消费者共三处，加一条绕过 accessor 的路径（两路评审独立确认**清单完整**）：

| 消费者 | 位置 | 处理 |
|---|---|---|
| 普通 agent 工具分发 | `transport.rs:698` | 改用新的 `connector_client()` |
| forge-action 分发 | `transport.rs:711`，helper 要求 `Arc<McpClient>` `:866-874` | **保持 stdio-only**，用 `mcp_client()` |
| 经 plugin 工具建卡 `create_via_tool_call` | `routes/cards.rs:378-418` | **保持 stdio-only** |
| `dispatch_neige_callback` 直接克隆 `rp.mcp` | `mod.rs:831-853` | 见 §2.5 |

**裁决：`mcp_client()` 语义收窄为「匹配 `(Running, ConnectorClient::Stdio)`」，另加 `connector_client()` 供普通工具分发。** app 恒为 `Stdio`（前提：App spawn 分支永远构造 `Stdio`），因此不会让任何 app 调用误报 not-running。

**一处措辞修正：** `create_via_tool_call` 对一个**正在运行**的 connector 会回 `404 "plugin X is not running"`（`routes/cards.rs:380-388`）——这句话是假的，运维会去查错方向。改为独立 4xx：「该 connector kind 不能建卡」。

iframe 路由（`routes/plugins.rs:934-1005`）不直接调 `mcp_client()`：硬门 `neige.` 前缀（`:941`）→ `status()`（`:953`）→ `dispatch_neige_callback`。加上 §1.3 的 `can_call_tool` deny-by-default，外部 connector 在这条路上已经是关的。

### 2.7 工具目录物化（§1.2(b) 的解法）—— r5 重写

三个要求，缺一不可：

**(1) 位置：`spawn_admitted` 内部，且在 live 表插入（`mod.rs:632-652`）之前。**
r4 只写「spawn 成功后」，两路评审共同指出这不够。若放在 `enable_plugin` 路由，会被 **boot autospawn**（`state.rs:1101`，且 boot 审计紧接着在 `:1104-1109` 读 `exposes_tools`）、**崩溃重启**（`mod.rs:912-1013`）、**`restart()`/`rotate_plugin_token`**（`mod.rs:366-371, 718-726`）、**`reload_plugin` 自己的 spawn**（`routes/plugins.rs:687`）四条路全部绕过。若放在 live 插入**之后**，则存在一个窗口：id 已被 `running_plugin_ids` 视为 Running（`mod.rs:778`），而 discovery/dispatch（`transport.rs:560, 678`）读到的工具是空的。⇒ **先发布目录，再发布 Running。两把锁都不得跨 await 持有。**

**(2) 手段：registry 上新增字段级单锁 mutator，而非 `get → 改 → insert`。**

```rust
/// 只替换 exposes_tools 字段；id 不在 registry 中时是 no-op（不插入）。
pub fn set_exposes_tools(&self, id: &str, tools: Vec<ExposedTool>) -> bool
```

`PluginRegistry::insert` 在写锁内**整体替换 Manifest**（`registry.rs:196-203`）。若用 `get`（克隆，`:170`）→ 改 → `insert` 这个非原子读改写，一次插队的 `/reload`（`routes/plugins.rs:684`）会让**整份 manifest 回退**——URL、`tools_allow`、permissions、views、templates 一起丢，且直到下次 reload 都无从察觉。字段级 mutator 把最坏后果限制为「工具列表陈旧」。

**(3) `id` 不存在时必须 no-op。** 这一条把 D11 在既有生命周期竞争中的**全部新增伤害**中和掉——尤其是「卸载 vs 正在 spawn」：uninstall 先 `stop()`（对 spawning 条目返回 `NotFound`，被当作良性）、删 DB、再移除 registry 条目（`routes/plugins.rs:545, 551, 563`），而在途的 spawn 仍会继续。若物化是 `insert`，它会把已被卸载的 manifest **复活**回 registry；no-op 语义下不会。

物化内容：`cli-query` 由 `cli_query.tools` 转换；`mcp-http` 由 `tools/list` 按 `tools_allow` 过滤后转换。**仅改内存，不回写盘上 manifest，也不写 DB 行。**

于是三个读者（`transport.rs:567`、`:739-744`、`state.rs:1109-1123`）**真的零改动**，boot 审计自动覆盖外部工具，不留审计洞。

**分发臂必须新增：** `transport.rs:696-720` 今天只有两臂（`kind: None` → `mcp_client().tools_call`；`ForgeAction` → forge）。`cli-query` 的 `kind` 也是 `None`，会走进第一臂并因无 MCP client 而 `-32002`。⇒ 第一臂改为按 `ConnectorClient` 变体分派：`Stdio`/`Http` → `tools_call`；`Cli` → 直接执行（§2.3）。

**合成 `ExposedTool` 的校验：** 今天**不存在** `ExposedTool::validate`（`Manifest::validate` 只覆盖 views / templates / permissions / entrypoint，`manifest.rs:327`）。物化时自行拒绝空名与会造成路由歧义的名字。

---

## 3. 不做的（v0 明确排除）

设置页 UI · 动态发现 + 勾选 + digest fail-closed · `connector_tools` / `connector_secrets` 表 · install 路由改动 · registry→spawn 自动水合 · 本地 stdio MCP（`npx` 那类）· `cli-action`（#1167）· 多用户 / 按用户存 key · `mcp.rs:414` 的版本放松（§1.4）· **per-plugin 生命周期串行化（R12，另立 issue）**。

**通道上不存在的能力**：外部 connector 调 `neige.*`（不构造 router + `dispatch_neige_callback` 拒绝非 Stdio）；渲染 `ui://` 或绑 `templates[]`（parse 期拒绝，且 `can_call_tool` 已 deny-by-default）；`cli-query` 执行任意命令（固定 argv 模板，无 shell）；`cli-query` 拿到 forge 凭据（不走 forge 路径）。

---

## 4. 验收（怎么算「场景跑通」）

1. 两个 connector 目录（位于 `plugins_dir` 内）各自 `install` + `enable` 后达 Running；**完整重启服务后仍 Running**。
2. **未绑定 workflow** 的 track 里，spec/worker 的 `tools/list` 能看到 `plugin.mcp-wisburg_list-institutional-reports` 与 `plugin.cli-longbridge_quote`，且看不到 `tools_allow` 之外的 wisburg 工具。
3. 两个工具各调用一次，返回真实数据。
4. 断言 `cli-query` 子进程环境**不含** `GH_TOKEN` / `GITHUB_TOKEN` / `SSH_AUTH_SOCK`。
5. 断言 `secrets.json` 的值不出现在 `GET /api/plugins/{id}` 的任何字段。
6. 断言停掉一个 `app` plugin 后其工具立即不可见（§2.5 防放宽回归）。
7. 断言 boot 审计的 `PluginToolRegistered` 覆盖外部 connector 的工具——这同时证明 §2.7(1) 的**顺序**正确（物化早于 live 插入，也早于 boot 审计循环读 `exposes_tools`）。
8. 断言 `POST /api/plugins/{id}/rotate-token` 对非 app 返回 4xx，且**未**删除 token 行、未触发 restart。
9. 断言含 `_` 的外部工具名路由唯一。**测试须使用真正含下划线的工具名**（不能用 `list-institutional-reports`，那是连字符）。唯一性靠 connector id 禁含 `_`（`manifest.rs:489-502`）**且 v0 的 id 不含 `.`**（R11）。
10. 断言上游 host 无响应时 **boot 不被阻塞**：connector 落 `Unavailable`，服务正常起来（§2.2 超时）。
11. 断言 `set_exposes_tools` 对不存在的 id 是 no-op：模拟「uninstall 后在途 spawn 完成」，registry 中**不应**出现被复活的条目（§2.7(3)）。

---

## 5. 风险

| # | 风险 | 处置 |
|---|---|---|
| R1 | 绑定 workflow 的 track 里外部工具不可见 | **接受**。#955 §3.3(a) 既有上限，归 #761。不放宽唯一豁口 |
| R2 | 远程 MCP 是新的数据外流面 | v0 只有一个手写声明的 URL，落盘可审计；每次 `tools/call` 记 target host。不引入静默出网 |
| R3 | 上游改 schema，物化的目录陈旧 | v0 在每次 enable / 重启时重取。**不做** digest fail-closed。如实记录：v0 期间上游改 schema 到重启前不会被发现 |
| R4 | CLI 输出灌爆 agent 上下文 | `max_output_bytes` 默认 32 KiB + 显式截断标记 |
| R5 | docker 预览栈里 `cli-query` 一律不可用 | 落 `Unavailable{reason}`，reason 含 PATH 与搜索目录 |
| R6 | connector 把 secret 打进自己的输出 | 残余风险。不做模式脱敏（假安全感）。缓解靠最小授予 |
| R7 | 存量 app manifest 携带自定义 `"kind"` 键 → enum 解析失败 | 树内唯一 manifest 已核对不含。加测试：未知 `kind` 值明确报错而非静默当 app |
| R8 | `/reload` 把解析后的 manifest 重新序列化写回 DB 行（`routes/plugins.rs:685`） | 存量 blob 将开始带 `"kind":"app"`。非 shape break，不 bump `manifest_version`；Tier-A 记一笔 |
| R9 | `/reload` 覆写内存条目会抹掉物化的工具 | reload 先 `stop()` 再 `spawn()`（`routes/plugins.rs:638, 687`），而物化在 `spawn_admitted` 内（§2.7(1)）⇒ **正常路径自愈**。异常路径归 R12 |
| **R10** | 物化的工具在**所有 REST 面上不可见**（详情/列表/views 都读 DB 行的 manifest，`routes/plugins.rs:1226`） | 运维只能从 agent 的 `tools/list` 和 boot 审计事件确认。**v0 接受并如实记录**；有 UI 时顺带解决 |
| **R11** | connector id 含 `.` 会造成 `plugin.a.b_x` 的路由歧义 | `plugin_tool_route` 多候选时 fail-closed（不可调用，非错路由，`transport.rs:748-760`）。v0 直接用无点 id（`mcp-wisburg` / `cli-longbridge`）避开 |
| **R12** | **既有的生命周期竞争**：uninstall vs 在途 spawn（`routes/plugins.rs:545-563` + `mod.rs:632`）· 并发 install（`plugin_install` 是 UPSERT 而非冲突插入，`out_of_domain.rs:379`，重名检查 `:370` 非原子）· 并发 disable→enable（`routes/plugins.rs:433, 463` + `mod.rs:450, 709`）· reload vs 在途 spawn（`stop()` 只看 `live`，`mod.rs:665`） | **全部是 v0 之前就存在的**，根因是缺 per-plugin 生命周期锁（现有 admission 锁只覆盖 spawn-vs-spawn）。**v0 不解**：需要独立一片，且 v0 的运维是单人手工 curl。**D11 的新增伤害已被 §2.7(2)(3) 中和**——字段级 mutator 使最坏后果退化为「工具列表陈旧」，no-op 语义杜绝「卸载后被复活」。**另立 issue，不是口头承诺** |

---

## 6. 裁决记录

| # | 问题 | 裁决 |
|---|---|---|
| D1 | 做成插件还是内核泛化 | **内核泛化**。元插件会变成第二个 plugin host，且外部工具挤进一个 id 会让按来源隔离失效 |
| D2 | agent 侧铸名 | 统一 `plugin.<id>_<tool>`，不开第二前缀；**v0 的 id 不含 `.`**（R11） |
| D3 | v0 的工具目录来源 | MCP：手写 `tools_allow` + enable 时取 schema；CLI：全手写。均物化进 registry（D11） |
| D4 | manifest 形状 | 不用 flatten+tagged union，改互斥可选块 + parse 期校验 |
| D5 | secrets | v0 用同目录 `secrets.json`（0600），不建表；引用名与将来 DB 版一致 |
| D6 | `cli-query` 是否走 forge-action | **不走**。只读查询不需要幂等/parked/恢复；走了反而拿到 forge 凭据 |
| D7 | 是否管理全局 PATH | **不管理**。per-connector 解析 + 钉住绝对路径 + 指纹 |
| D8 | 无进程 connector 的在线判定 | `process` / `router` 改 `Option`，`mcp` 改 `ConnectorClient` 三变体（全 Arc 包，廉价 Clone）；`running_plugin_ids` 零改动 |
| D9 | 配置与启动方式（**r5 收窄**） | 走现成 install + enable，**且 connector 目录必须在 `plugins_dir` 内**以绕开 #1168 的符号链接盲区。安装路径仍零新代码 |
| D10 | 外部 server 握手 | `HttpMcpClient` 自带极简 initialize，不携带 auth `_meta`、不声明 kernel-callbacks、**不做版本判定**；`mcp.rs:414` 放松工作删除。分支点在 `ensure_plugin_token` 之前，判据是显式 profile 而**非** `expected_echo.is_none()` |
| D11 | 工具目录如何抵达三个读者（**r5 重写**） | 物化进 registry in-memory，但必须：①在 `spawn_admitted` 内、live 插入**之前**；②用**字段级** `set_exposes_tools` 而非 `get→改→insert`；③**id 不存在时 no-op**。三条合起来既保证三个读者零改动，又中和 R12 的既有竞争 |
| D12 | `mcp_client()` 的处置 | 语义收窄为匹配 `(Running, Stdio)`；另加 `connector_client()`。forge / cards / callbacks 三处零改动；cards 的误导性 404 改为独立 4xx |
| **D13** | 既有生命周期竞争是否在 v0 解决 | **不解，另立 issue**。理由：根因是缺 per-plugin 生命周期锁（独立一片）；v0 运维是单人手工操作；且 D11 的新增伤害已被 §2.7(2)(3) 完全中和。**不接受「以后会做」的口头承诺——issue 是交付物的一部分** |

---

## 7. 分片

| PR | 内容 | 估算 |
|---|---|---|
| **P1** | `ConnectorKind` + 互斥块 parse/校验 · `RunningPlugin` 三字段可选化 · `ConnectorClient` 三变体（Arc） · `spawn_admitted` 按 kind 分支 · §2.5 全部站点 · `connector_client()` 接缝 + cards 4xx · `set_exposes_tools` + §2.7 物化与顺序 + 分发臂 · `tools_list` | ~600 |
| **P2** | `HttpMcpClient`（POST + SSE 剥壳 + 极简 initialize + 显式超时）· `tools_allow` 过滤 · `secrets.json` 读取 · `Unavailable` 状态 · mcp-http 端到端 | ~350 |
| **P3** | `cli-query`：解析钉住 + 指纹 + 环境构造 + spawn/捕获/超时/封顶 + argv 模板 + `CliQueryRuntime` | ~300 |

P2/P3 在 P1 后可并行；三片各带 §4 的对应断言（#7、#11 属 P1；#10 属 P2；#4 属 P3）。

---

## 8. 评审账目

**r4 双通道窄评审（只审 D9/D11/D12）：两路均 SHIP-WITH-FIXES。**

**共同命中 2 条 critical：** D9 的 install 路径因符号链接盲区在重启后失效（→ D9 收窄 + 另立 #1168）· D11 的物化位置未写死，会被四条 spawn 路径绕过且可能晚于 Running 发布（→ D11 重写第①条）。

**仅 codex 命中：** 三条**既有**生命周期竞争——uninstall vs 在途 spawn 会复活 connector · 并发 install 因 `plugin_install` 是 UPSERT 而非冲突插入而双双成功 · 并发 disable→enable 可留下 `enabled=true` 但无运行时（→ R12 + D13）· reload 无法停止 spawning 条目（`stop()` 只看 `live`）。

**仅 subagent 命中：** `get→改→insert` 是非原子读改写，会整份回退 manifest 而非只丢工具（→ D11 第②条，这条直接决定了 R12 可以被接受）· `ConnectorClient` 必须廉价 `Clone`（进程表锁不能跨 await）· `tools/list` 在 inline-await 的 boot 路径上且 `reqwest` 无默认超时（→ §2.2 + R10 之外新增验收 #10）· `GET /api/plugins/{id}` 返回 DB 行 manifest，物化工具在 REST 面全不可见（→ R10）· id 含 `.` 的路由歧义（→ R11）· 不存在 `ExposedTool::validate`。

**两路共同确认（非发现，但值得记）：** install 对 connector manifest 无额外拒绝（entrypoint 变条件后）· `secrets.json` 随整目录被携带 · `exposes_tools` 只有那三个生产读者 · `GET /api/plugins/{id}` 不会泄漏合成工具 · D12 消费者清单完整且收窄不伤 app 路径 · `PluginRegistry::insert` 锁层面安全（读者看不到撕裂状态）。

**更早轮次：** r1/r2 因范围收缩移交 #1167 的 7 条已全文写入该 issue；因不做安装/建表而暂不适用的 5 条已在 §3 列明为「恢复动态发现时必须重新引入」；r3 被推翻的 4 条（丢进 plugins_dir 重启 / 不碰三个读者数据源 / 两条「硬拦截」定性 / 树内两个 manifest）已分别落到 D9、D11、§1.4、§1.3。

## Related

- #1167 CLI connector（副作用型，argv → forge-action operation）
- #1168 install 符号链接被 `load_from_dir` 静默跳过（D9 的约束来源）
- #955 kernel ↔ app 能力边界
- #489 plugin origin / trust / capability
- #761 workflow 组合（解 R1）
