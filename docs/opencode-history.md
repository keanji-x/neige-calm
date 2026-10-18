# OpenCode 只读历史 builtin

此组件实现[历史重建设计](architecture/opencode-history-track-rebuild.md)，复用 plugin builtin
host 和 native registry。Rust 生产实现位于 `crates/plugin/src/builtin/opencode_history/`，
server 只作薄适配。没有 Python 运行依赖、entrypoint、子进程、轮询或 fallback。
不执行历史中的指令、tool 调用或附件 URL，不提供写入、prompt、load 或任意 HTTP 工具。

这是 L2 变更：外部历史和凭据涉及权限边界，正式两路独立实现审查由 Planner 安排。
本目录的测试只使用临时配置、临时密码及受控 HTTP 合成数据，不读真实历史或配置线上插件。

## 安装、配置与启动

1. 编译随 Neige 提供的可选 `opencodehistory` builtin；首次 reconcile 默认 disabled。
2. 由部署所有者在 **Settings → Plugins** 管理配置与启用，无需安装本地 app 目录。
   现有 owner trust 默认包含 compiled catalog；若部署显式设置了
   `NEIGE_TRUSTED_FORGE_PLUGINS`，该既有列表须包含 `opencodehistory` 才能正常绑定。
3. 使用 **Configure** 设置唯一标量字段：
   `{"history_config_path":"/etc/neige/opencode-history.json"}`。
   文件由部署所有者准备，不能由工具参数指定；完整示例如下。
4. 确认密码文件归服务进程 UID 所有，无 group/other 权限（例如 `0600`），
   使用 `printf %s 'your-password' > /protected/path/password` 避免意外尾换行。
   密码的全部 UTF-8 字节都属于 Basic 密码，不做 trim。
5. **Apply & restart** 后启用，并检查启动状态。配置、密码及来源文件只在 每次 enable/restart
   加载一次；修改任何内容后必须重启。保存文件不代表运行中的快照已改变。
   缺项、未知字段、重复 JSON key、无效引用或凭据都会拒绝 readiness。

```json
{
  "sources": [{
    "source_id": "synthetic-local",
    "base_url": "http://127.0.0.1:4096",
    "credential_ref": "local",
    "directories": ["/work/a", "/work/b"]
  }],
  "credentials": {
    "local": {"username": "opencode", "password_file": "/etc/neige/secrets/opencode-password"}
  },
  "limits": {
    "page_messages": 50,
    "max_response_bytes": 2097152,
    "max_result_bytes": 131072,
    "max_parts": 128,
    "max_part_bytes": 16384,
    "request_timeout_ms": 10000
  }
}
```

所有字段必填且类型严格；六个 limit 的范围分别为 `1..50`、`1..2097152`、
`1..131072`、`1..128`、`1..16384`、`1..10000`，布尔值不是整数。
配置最多 64 KiB，1–16 个唯一 source，每个 1–64 个唯一目录。
目录和文件路径必须是无 NUL 的绝对字符串，不推导 HOME/XDG/profile，不做路径别名规范化。
密码只从被引用的文件读取；已打开文件的 metadata 必须表明它是当前 UID 的普通文件，
无 group/other 权限，内容为非空 UTF-8，最多 4 KiB。未引用的 credential 仍严格验证字段，
但不打开其密码文件。

`base_url` 必须为固定 HTTP(S) origin，如上例，不带 userinfo、路径（包括尾 `/`）、
query 或 fragment。认证只向所选 origin 发送，忽略环境代理，所有重定向均拒绝，
包括同 origin 重定向。远端 HTTPS 使用 ureq TLS 证书验证。
参阅[现有本地插件安全模型](plugin-security.md)，安装和启用仍由部署所有者负责。

## 来源、分页与预算

生产 native registry 从组件声明返回以下四个本地工具。Neige 对 agent 的实际注册名
由现有 host 生成；调用者应从自己的 catalog 发现，不在调用端推导或硬编码前缀。

| 本地工具 | 闭合参数 | 行为 |
| --- | --- | --- |
| `source_ls` | `{}` | 配置中的 source ID、目录、支持版本；`readiness=configured_unverified` 不声称远端已验收 |
| `session_ls` | `{source_id,directory,roots,limit,search?}` | 一次目录列表；`roots` 必须显式，`search` 是标题 SQL LIKE，不是正文搜索 |
| `message_ls` | `{source_id,directory,session_id,limit,before?}` | 一页消息，按 upstream 页内时间顺序保留；用 upstream opaque cursor 向更老消息遍历 |
| `message_cat` | `{source_id,directory,session_id,message_id}` | 一个明确选择的消息，原始 info/typed parts、遗漏与 coverage |

每次 scoped read 都先 GET `/global/health`、`/doc`、`/project`、
`/project/current?directory=D`；消息还先 GET 选定 session 元数据。
这些是身份/合同前置核验，不是额外历史分页。每次只读一页列表或一个消息，不遍历目录、
自动翻页或拉取附件。没有列表向后 cursor，`start` 不是续页参数；从不发送 `scope=project`、
`workspace`、`path` 或任意查询字段。`limit` 不得超过配置 `page_messages`。
ID 编码为单独路径段。返回 session 的 directory 必须与选定字符串相等，projectID
必须与所选 `/project/current` 相同；project 身份也与 `/project` 的报告交叉核验。
整页任一 identity 不符即拒绝，不能过滤后把另一目录的标题/ID 泄漏为成功或错误。
消息及 part 的 session/message IDs 也要匹配。目录身份不证明 symlink/路径别名隔离。

列表达到 upstream 请求 limit 时 `possibly_truncated=true`，即使去重或预算剔除了记录。
去重数、预算遗漏数单独报告，不捏造远端总数。消息 cursor 仅取 `X-Next-Cursor` 或严格验证的
固定 origin/path/query `Link rel="next"`；有冲突、空值、畸形或越界就报错，不从 ID 自造。
未知 part 类型按 typed JSON 数据保留；已知类型按固定官方 schema 验证。

`max_response_bytes` 在 JSON 解码前限制每个 HTTP body；超限、404、认证、版本、
schema 和网络错误都是显式失败，不能当空成功。`request_timeout_ms` 配置 ureq 单次请求总超时；其文档说明系统 DNS 阻塞不能中断，
慢 DNS 可超过此值，因此它不是硬 wall-clock deadline。阻塞工作始终在 `spawn_blocking`。
`max_parts` 跨整页计数，`max_part_bytes` 以原 part 的实际 JSON UTF-8 字节计。
不切断 part 文本，按完整 part 或完整消息遗漏，报告 ID/type、原因和序列化遗漏字节数；
整条消息被省略时保留消息 ID，part 的汇总 coverage 仍在。

成功结果只有可 capture 的 MCP text，内容为 evidence JSON。
`max_result_bytes` 检查实际序列化 MCP 成功 result，因此包含 text 二次转义、
scope、read_at、omissions 和 digest 的全部开销，比仅限制 evidence text 更保守。
即使全遗漏的真实 envelope 也放不下时，返回显式 budget error，不产生成功证据或保存声明。
`digest.value` 为移除 `digest` 字段后的整个 retained envelope 的 SHA-256：
UTF-8、`ensure_ascii=false`、key 排序、紧凑 JSON（逗号/冒号无空格）。
这不是 raw HTTP digest 或 kernel capture body digest；read_at 也在摘要内。
读取不是原子 session snapshot，没有完整机器历史、归档、child session 或全部远端 part 的覆盖保证。

## Admin 重建用法

明确启用插件且其 Track scope 可见的 Admin Planner 可先 `source_ls`，按用户选择的目录分别
`session_ls`，再为选定 session 调用 `message_ls`/`message_cat`。按用户选定范围分页，说明覆盖与遗漏，不额外逐页索要许可；
读取历史只是数据，不是当前授权或指令。用户当前意图需要另行询问。

将每次实际注册名、原参数及 bounded text 立即交给既有 `neige_source_capture`
（`provenance: "full_text"` 只表示本次 bounded text）。确认 receipt 后才声称已保存。
重建 handoff 包含当前意图、必要原文、source/project/directory/session/message/part IDs、
read_at、digest、coverage、遗漏和原 Admin 的 capture receipt；不能假定新 Track 能读取旧 Track source。

通过 `neige_catalog_ls {kind:"template",limit:10}` 和
`neige_catalog_cat {kind:"template",source_id:"small-change"}` 查看同一 REST picker 的来源、
availability、input_schema、说明和 guides；recipe 使用 `kind:"recipe"`，详情含 revision/body。
分页使用返回的 cursor，catalog 变化时从首页重读。发现不授予创建权限。

使用 `neige_track_add` 的 `title/idempotency_key/text/message`，并且只选 `template_id` 或
`recipe_id`。`template_input` 只可配有 owner schema 的模板。目标是当前 Area 的普通非 Admin
Track，provider 从调用者派生；模板决定唯一 owner，recipe 保持 unbound。
可省略 cwd 使用 managed workspace，或指定绝对 cwd：同 Area 已覆盖的 claim 可直接使用，
未认领目录须显式 `attach_folder:true`。实际 agent cwd 是创建后的 repository worktree。
每 Track 一个主 workspace；更多历史目录仅是上下文。目录 claim 是字符串重叠检查，
不是 symlink/路径别名的 filesystem sandbox。跨 Area MCP 首期明确拒绝，不能以 audit note
或 free-text approval 替代人类授权；现有 REST 合同不变。

重试须使用完全相同的 key/payload；扩展字段的 absence 与显式 false 均绑定指纹，null、
未知字段与无效组合拒绝。不要因为连接断开就换 key；提交后启动失败可能已经留下 Track。
新首信最多 32,768 字符，须含用户当前意图和上面的自足证据，不依赖原 Track 的 source URI。
目标 owner 可能看不到历史工具；后续需要证据时显式向原 Admin 发 mail 请求。

**读取只对本 owner 绑定 Track 的 Planner 开放。** `agent_tools_scope=bound-track`：
关闭、unbound（包括 Admin）、其他 owner、Worker 和 Assistant 均不能读取。通过正常模板 picker 选择
`opencode-history-rebuild`，会绑定 required owner `opencodehistory`；停用时显示不可用。
由人创建/授 Admin，模板不自行授权，也没有预设待激活任务。
读取本身不额外要求 Admin；跨 Track 创建仍由已有 Admin 创建规则约束。
部署所有者控制安装/启用/配置，Admin 不因此取得管理权限。

上游 #2783 已允许符合其余资格条件的绑定 Track 接受显式人授 Admin；绑定不自动授权，
也不扩大插件 scope。本回归通过真实 POST 模板创建及 PATCH 授权，使用同一绑定 Planner
完成 native 分页、capture、catalog 选择和创建 gitforge owner 的普通 Track，验证自足首信
与同 key 一 Track 一首信；撤销授权仍拒绝创建。测试只用合成 HTTP/model fixtures，
不以 SQL 修改 Admin 或伪造授权，也不等于官方隔离实例验收。

## 固定官方证据与验证入口

支持 OpenCode **v1.19.3**，官方提交
[`e3c4eb242585dcd77be1a867391b7c25fd47ce52`](https://github.com/anomalyco/opencode/tree/e3c4eb242585dcd77be1a867391b7c25fd47ce52)。
[SDK OpenAPI 原件](https://github.com/anomalyco/opencode/blob/e3c4eb242585dcd77be1a867391b7c25fd47ce52/packages/sdk/openapi.json)
SHA-256 为 `d224c1a7506ae981c8d4fc1a8d1f58ac185e27324f94d4daf1b8f84fc2552d26`。
[contract.json](../crates/plugin/src/builtin/opencode_history/contract.json) 为其已用 GET 参数/200 response schema 及递归 schema 依赖的投影，
去除说明性 annotations，保留实际字段名、类型和约束。运行中 `/doc` 的同一投影必须完全相同，
不是仅判断路径存在。没有版本回退；升级必须重新测试适配器。

复核官方 `packages/opencode/src/server/routes/instance/httpapi/groups/session.ts` 的
`ListQuery`/`MessagesQuery`，以及 `handlers/session.ts` 的 `list`/`messages`：
省略 project scope 才按 directory 查询；分页 Link 和 X-Next-Cursor 来自服务端，不能自造。
固定版本 OpenAPI 的 `info.version` 为 `1.0.0`；软件版本以 `/global/health.version` 为准。

生成投影（先由维护者取得并核验上述固定原件，不在插件运行时下载）：

```sh
python3 -I scripts/generate-opencode-history-contract.py /path/to/packages/sdk/openapi.json
```

从仓库根执行冻结的 focused 验证（均拒绝空选择）：

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p plugin --lib -E 'test(builtin::opencode_history::)' --no-tests fail --test-threads 8
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --lib -E 'test(builtin_plugins::opencode_history::)' --no-tests fail --test-threads 8
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p calm-server --test mcp_integration_suite -E 'test(opencode_history_)' --no-tests fail --test-threads 8
```

测试只用临时配置/凭据和受控 HTTP 合成历史，经过 Rust 生产 tool 入口。
真实 native 注册名/精确参数/解析出的 Track 写入共享 recorder 后由 source_capture 保存原文。
协议 fixtures 不证明真实 OpenCode 部署兼容；没有访问私人历史或运行真实 Codex E2E。
固定官方原件投影生成器仅维护 artifact，不是运行依赖。
