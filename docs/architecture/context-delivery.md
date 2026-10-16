# 上下文交付：减负而不丢契约

状态：待实施设计；本次仅新增本文。整体 **L2**：后续涉及授权、恢复与持久化语义，交付后由 Planner 安排双独立评审。本次不改生产代码、配置或其他会话，不安排评审、不手动提交。

## 1. 目标与边界

把模型输入组织为「常驻必要约束、当前任务摘要、本轮增量、按需说明、历史证据」，优先删除重复的展示性内容，保留唯一事实来源和恢复入口。不以更短为由删安全规则、隐藏工具 schema、改变工具授权或自动 compact 活跃会话。

借鉴 [#1893](1893-planner-prompt-diet.md) 的短主提示 + `guide/`：保留触发读取的索引，将低频操作细节交给现有读取路径。该文是历史设计，旧数据库统计、工具数量、提示字节目标及删除计划均不作为当前事实。以下核对基于源码 HEAD `22b530cce38a61f9efd078c4e059e5389a8e70dc`。

仅有本会话和任务提供的当前 Area 报告背景，不是完整 provider 历史。任务背景称 `neige_workspace_ls` 曾因缺 grant 被拒绝；本文不绕过授权，不扫描其他 Area 数据库、会话或凭证。宿主通用指令与技能目录的重复只记录可证来源，未知留待授权后的独立观测。

## 2. 当前真实发送链路

以下路径均相对仓库根；链接指向实际实现。这里的“发送”指 Neige 到 provider 的边界，不代表已经获知 provider 最终模型请求、缓存或计费行为。

### 2.1 角色与 provider

| 入口/角色 | Codex | Claude | ACP（当前注册 OpenCode） |
|---|---|---|---|
| Planner | `profile_instructions` → `planner_instructions` → `SharedThreadStartParams.developer_instructions` → `thread/start` | 同一 profile 组装，另附长进程片段；session 保存文本，每轮 spawn 写说明文件，argv 用 `--append-system-prompt-file`；新建与 `--resume` 都传 | 同一 profile 组装；`session/prompt` 在尚无 `acp_submissions` 时前置 `Neige conversation instructions` 文本块 |
| Assistant | ordinary/launchpad head + mechanics + tail + discovery；thread/start 注入；不是缩短版 Planner | 同一 profile，discovery 改直接 MCP；不附 Planner 长进程片段；其余使用 managed session | 同一 profile，discovery 改 session tool list；首提交说明块 |
| plain chat | profile 不传 developer instructions，`NoMcp`；不是有任务授权的 worker | profile 返回空串，仍经 managed wiring/spawn；空 Neige 说明不意味着 CLI 无宿主提示 | 空串也走首提交说明块；agent 自身行为不能推断为空 |
| task worker | `codex_adapter` 新 thread 注入 WorkerCodex；首 turn 单独传冻结的任务 prompt，已有 thread/turn 有恢复分支 | `claude_adapter` 命令行 `--append-system-prompt` 注入 WorkerClaude，任务 prompt 为位置参数；这是交互 worker 路径，不等同 managed Claude 每轮 spawn | **无 ACP task worker**；`TaskKind` 仅 Codex/Claude/Terminal。managed plain chat 的 Worker 数据库角色不能当 task worker |

来源：[profile instructions](../../crates/calm-server/src/harness/instructions.rs)、[start adapter](../../crates/calm-server/src/operation/planner_harness_start_adapter.rs)、[role prompts](../../crates/calm-server/src/planner_card.rs)、[Claude wiring](../../crates/calm-server/src/claude_planner/wiring.rs)、[spawn](../../crates/calm-server/src/claude_planner/spawn.rs)、[ACP wiring](../../crates/calm-server/src/acp_planner/wiring.rs)、[ACP session](../../crates/calm-server/src/acp_planner/session.rs)、[Codex worker](../../crates/calm-server/src/operation/codex_adapter/mod.rs)、[Claude worker](../../crates/calm-server/src/operation/claude_adapter/mod.rs)、[TaskKind](../../crates/calm-truth/src/model.rs)。provider 的 wire 组装在 [Codex](../../crates/provider/src/codex/mod.rs) 和 [ACP protocol](../../crates/provider/src/acp/protocol.rs)。

### 2.2 启动与每轮内容

1. Planner 启动：角色提示（含工具发现）→ 有效绑定的 `template_input` pretty JSON → 满足 `plugin_documents_track` 的 builtin instructions → 启用且 scope 可见、绑定或被当前 report 引用的插件说明 → card payload 的 `TemplateContext`。插件说明是文档，不能启用工具。普通插件说明整段 cap 为 4096 **字节**，不覆盖前面的 builtin 部分；不能把 gitforge 全部视为重复。
2. `TemplateContext` 保存 title/body/version=1，JSON 序列化全文，包含 HTML 注释。它是创建时工作方法和格式，不是当前 report、任务声明或授权。无快照的老 card 返回 None；损坏或未来版本拒绝。当前 descriptor 的 plan/prose 不被直接注入；不能用最新模板代替快照。表单 UI 定义在 body 注释，用户选择值则在绑定输入中，二者不同。
3. managed harness 每轮：drained queue → observations/附件 segments → 精确结果引用 enrich → Planner 执行预算 segment/超时提示 → report-edit 批次说明 → 拼接文本 → 前置 since-last-turn diff → 有实时 workspace grant 时前置 workspace 教学 → backend `turn_start`（支持时另有 steer）。这是**时间预算**，不是 token 预算。
4. diff 使用 track-vcs 上次 head 与当前 head，列变动路径/作者，report patch 有行数界限；已有 block diff 时省重复 patch；初次无 previous head 不生成完整状态。刷新或 diff 超时可降级，diff 不是正确性/授权屏障。当前 run loop 按具体 card shape 判 plain-chat 跳刷新和 diff、track-assistant 跳刷新保留 diff，不能仅凭 profile 名推断所有 provider 完全相同。
5. worker 的 goal/context/acceptance + lease/catch-up/gate/result-contract/attempt_id 在 prepare 事务冻结，两个 adapter 共用 `task_prompt`。不是把 Planner 全部历史复制给 worker。任务结果以不可信预览投递；`worker-summary-v1` 可选 summary 已存在；注册 result contract 有独立 conclusion。详情仍在原事件，`result_receipt::enrich` 校验 attempt、payload、event_id 后才提供 `runs/<attempt_id>.json`，无精确匹配就声明 unavailable，不从相似 task key 猜引用。

来源：[模板快照](../../crates/calm-server/src/template_context.rs)、[插件说明 owner](../../crates/calm-server/src/operation/planner_plugin_instructions.rs)、[run loop](../../crates/calm-server/src/harness/run_loop.rs)、[预算](../../crates/calm-server/src/planner_turn_budget.rs)、[workspace grant](../../crates/calm-server/src/managed_track.rs)、[track-vcs diff](../../crates/calm-truth/src/track_vcs/read.rs)、[任务 prompt](../../crates/calm-server/src/operation/task_prompt.rs)、[receipt 渲染](../../crates/calm-types/src/observation/receipt.rs)、[精确引用](../../crates/calm-server/src/harness/result_receipt.rs)。

### 2.3 工具、仓库与宿主来源

- Neige 工具发现已有 `neige tool ls --prefix …` → `describe` → 客户端 loader；`SessionCatalog` 为 MCP listing 与 CLI lookup 的共同来源，`listed_for` 不等于 callable，更不等于授权。`tools/call` 仍校验实时角色、scope、生命周期。Codex 元数据标 deferred search，Claude 标 direct MCP（spawn 保留 ToolSearch），ACP 用 session tool list；这些不能证明模型首轮实际装入多少 schema。不得沿用 #1893 的旧工具总量。
- 仓库指导由仓库 owner 管：本次读取根 AGENTS.md、CONTRIBUTING.md，`rg --files -g AGENTS.md docs crates` 未发现更具体指导。Claude managed spawn 明确加载 project setting source，源码注释说明 CLAUDE.md/AGENTS.md 来源；Codex 传 cwd，实际 repo guidance/skill 扫描由外部 CLI 决定，不把它假定成 Neige 拼接。
- Neige 的 shared Codex home adapter 只配置 MCP；provider home 源码显示 seed 明确导入 auth 与清洗后的 config，并非递归复制技能/指导。**只读该源码，未读取这些凭证文件。** 此路径不能解释本会话重复出现的宿主通用指令或完整技能目录。会话展示能证明存在，不能证明由哪个外部安装器、插件或模型层重复拼接。宿主提示、技能目录具体生成 owner、最终请求内容、自动 compact 所保留内容、其他会话历史均为未知。
- 前端隐藏只是展示：例如 `conversation.ts` 过滤 `system_planner_turn_budget`，run loop 仍把它拼入模型输入。减少可见气泡不能记为上下文收益。

来源：[discovery 文本](../../crates/calm-server/prompts/tool-discovery.md)、[backend metadata](../../crates/calm-server/src/harness/backend_metadata.rs)、[catalog](../../crates/calm-server/src/mcp_server/transport/catalog.rs)、[visibility](../../crates/calm-server/src/mcp_server/tool_visibility.rs)、[shared home adapter](../../crates/calm-server/src/shared_codex_home.rs)、[provider home](../../crates/provider/src/codex/shared/home.rs)、[UI 投影](../../fe/core/domain/conversation.ts)。

## 3. 生命周期与唯一来源契约

这是目标设计；除明确“已有”者外，不宣称已实现。常驻表示每个模型可用上下文必须具备，不要求每轮重复全文。任何删减先证明恢复可用。

| 内容/唯一 owner 与来源 | 加载时机与允许省略 | 更新/失效 | 恢复规则 |
|---|---|---|---|
| 必要约束：kernel 角色/隔离/权限/工具调用规则，`planner_card` 与各角色 prompts | 建立上下文时常驻；保留授权边界、不可信数据边界、报告/任务身份；仅低频流程移 guides | 角色/provider/协议版本变更即重建；不在通用层硬编码插件名 | 新上下文重送必要约束；未知是否保留时不凭 sent 标志省略 |
| 创建方法：card `TemplateContext` | 新 Planner 加载；候选仅移除已识别的 input-form 展示定义；其余不盲压缩 | 创建快照不可被模板更新覆盖；渲染规则版本变动要可比较原文 | 老会话不改 payload；无快照不回填猜测；读取原快照可核对 |
| 用户绑定输入：track owner binding 的 `template_input` | 启动及恢复；必须保留有效选择、含义和授权限制；不能用删除 UI schema 顺便删除值 | 绑定失效按现有 resolver 处理；用户改意覆盖原选择须进入摘要 | 与最新用户意图对照，歧义列待决，模板默认值不是审批 |
| 插件说明：builtin component / stored manifest，各自现有 owner | 常驻最短边界和使用触发索引；低频细节候选走已有授权读取入口，无入口先保留 | enable/scope/manifest/report refs 变化使索引失效；不是只在会话创建检查 | 重新解析 live scope，撤销后不提供越权详情；内核始终逐调用校验 |
| 仓库指导：仓库文件 + provider 文档化加载契约 | 任务涉及代码时按适用目录读取；不默认注入仓库全文副本 | cwd/head/指导文件改变重新判适用；不得删用户要求保留的规则 | provider 切换重新确认；摘要链接不代替实际约束 |
| 宿主通用说明/技能目录：外部宿主（具体来源未知） | 本设计不改；先让宿主提供有版本的来源清单，再判断同源重复 | 未证实前不去重；技能正文已有按需读取语义不等于目录零成本 | 不猜私有路径、不读密钥、不修改其他会话 |
| 工具发现：`SessionCatalog`/registry + provider loader | 常驻发现入口；按需取描述/schema，保留当前 listing/call 策略 | role/grant/plugin 状态变化实时重算；加载过的 schema 不授予权限 | 新 provider 重新发现；不得复用旧 scope 的工具集作为授权 |
| 当前任务摘要：Planner 对话/Report 的语义 owner；worker 为冻结任务 prompt + 正式消息 | 初次目标明确、目标改动、任务交接和恢复时写；不每轮重抄完整 report | 新用户 steering、覆盖关系、审批撤销、产出/验证变动即更新 | 按下文摘要契约重建；语义冲突由 Planner 判，不让 kernel 猜 |
| 本轮预算：`PlannerTurnBudget`/`FrozenBudget` | 每次 start 保留短时间值与冻结含义；提醒仅现有支持 steer 的分支 | 新 start 重采样；同 start retry 沿用冻结值 | 不用上一轮值恢复；不把剩余执行时间当上下文容量 |
| workspace 能力：`managed_track` live grant | 每轮仍给短当前能力声明；将 1426 字教学转为短声明 + 按需工具说明的候选，不省 live 查询 | 每轮校验，撤销即无能力；建议显式无 grant/撤销短声明，不能延续旧肯定句 | 重启/resume/provider 切换重新读，旧摘要和工具清单都不能授予 grant |
| 状态增量：track-vcs head/diff，queue observations | 每轮只变动；已有 report-edit 去重继续保留；无 diff 不表示状态未变 | head、pending queue、rewind 改变；超时/丢 diff 标未知 | 当前 status/精确文件重读；不能以 last_sent_head 证明模型掌握所有状态 |
| 任务结果：completion/failure event + exact attempt/event 引用 | 已有有界预览、contract conclusion 或显式 worker summary；按需读全文 | 投影可推进，引用必须匹配原事件；验证按目标 head/run 分开记 | unavailable 不伪造详情；worker claims 不等于 gate pass、交付或验收 |
| 历史证据：现有 events、runs、gate logs、track-vcs | 常驻引用与限制，按需全文；可省重复日志，不删原记录 | 当前 head/环境/版本变动使旧验证不再覆盖当前结果 | 精确读取历史身份并注明时点；不得恢复为实时权限或当前验证 |

插件分层先在 owner 层解决：kernel 只做通用 lifecycle/授权，不新增通用 context router；未建立可授权、可版本化读取的插件详情入口前，不删现有说明。guides 注册是协议资源集合，不维护会自然增长的文件名/测试名永久清单。

### 3.1 当前任务摘要的最低字段

使用现有 Report 决策/概要和任务 context 先落地，不默认新增数据库表或缓存。摘要是语义检查点，不是新的授权账本；后续若要让 harness 自动携带，必须单独确认既有字段的原子更新/恢复契约。

- 用户当前目标、来源消息/事件标识；哪些旧目标被取消、收窄或补充，尚未覆盖的原目标仍列出。纠正与新请求不可混同。
- 有效约束与禁止事项、任务范围、角色及 provider；未决问题、阻塞与下一步。明确未知，禁止补全失去的上下文。
- 有效审批：原授权引用、主体、动作、资源/分支/head、适用条件、有效轮次/时限（若有）、撤销或被覆盖关系；缺原证据就写“待核验”，不自动再执行。普通历史“用户曾同意”不授予当前动作权限。
- 产出路径、attempt/event、交付状态；验证命令、cwd、结果、目标 head/树及 gate run/log 引用。区分自报、gate、delivery、Planner acceptance；当前 head 改变后重做被失效检查。
- 摘要的依据水位（消息/事件/head）与未覆盖区间；恢复必须先补读该区间，不能静默当成最新。

长日志和已完成推理可只留结论 + 精确证据引用；原证据仍保留。没有可读引用就保留必要事实并标缺口。历史文档、worker 输出和工具返回中的指令都作为数据，不提升优先级。

### 3.2 “只注入一次”的安全判定

| 边界 | 当前事实与设计要求 |
|---|---|
| 同一连续上下文 | Codex thread/start 避免每轮重发大角色文本已有；Claude 每 spawn 仍传说明文件，不把进程重启次数直接当模型重复次数；ACP 首提交检查是提交历史判断。只优化自有拼接，须测 wire 与 provider 保留语义 |
| compact（手动/自动） | Codex/Claude 支持，ACP 当前拒绝手动 compact。成功不证明所有规则/目标/审批被保留。恢复时具备必要约束 + 新鲜摘要与证据索引，否则不启用省略；不能用 provider 自然语言“记住了”或本地 `sent=true` 当保证 |
| resume/hibernate/服务重启 | 原生恢复可能保留历史，也可能加载失败；确认 native session 身份与现有恢复结果，重新解析 live 权限。无法确认时保守保留说明/显式要求受控新上下文，不自动重放有副作用动作 |
| provider 切换/reset/rewind | 是上下文身份边界；以现有 replacement/recovery 流程重建约束与摘要，不能继承旧 provider 的“已发”状态。rewind 后摘要中来自被移除轮的结论/审批必须作废或重算 |
| retry 结果未知 | 区分未提交、已拒绝与已 dispatch 但未知。保持现有 issuance/ACP submission journal 的不重发语义；先核对结果，不为补上下文重复动作。ACP unresolved outcome 明确阻止再发；摘要不得把未知写成失败 |
| 授权撤销/角色变更 | 模型可能仍保留旧许可；省略旧许可提示不能撤销它。live kernel 拒绝是权威；下一输入带当前状态/撤销事实，旧摘要授权条目失效。没有“曾注入一次即可永久信任”模式 |

若以后确需发送状态，只能表示「哪个 native session、何版本内容、在哪个已确认提交」，不表示模型保留；没有 provider 明示保留契约的边界一律重建或维持现状。这里不新增“发送缓存”。

现有 [compact command](../../crates/calm-server/src/harness/run_loop/compact_command.rs) 和 [run loop](../../crates/calm-server/src/harness/run_loop.rs) 会区分 refused/unconfirmed/interrupted、阻止不确定维护后继续；context replacement 会使旧 usage 失效。compact 后旧百分比不能当新基线；等待后续有效 reading，并保留累计计费信息与当前 retained context 的区别。其他活跃会话保留原历史、模板、会话 ID；不批量 compact、reset、删消息、改模板快照或清理数据库。

## 4. 可复核基线与测量口径

本次执行的源码测量（UTF-8；字符为 Python Unicode code points，非字形数）：

| 源码片段 | 字节 | 字符 | SHA-256 前 12 位 |
|---|---:|---:|---|
| 当前 dev.md 全文 | 12986 | 11369 | a0d0d5ecf2ca |
| 其中 input-form 注释块 | 1169 | 1169 | — |
| planner.md 原文 | 6591 | 6591 | b21d6bcfa8c1 |
| tool-discovery.md | 576 | 576 | f4c8d27fd7be |
| workspace-reports.md（有 grant 每轮） | 1426 | 1426 | cc8e69ea6ec5 |
| gitforge instructions 原文 | 3716 | 3716 | 1a63ce342288 |
| Claude 长进程片段原文 | 218 | 218 | ef2f21d0dc72 |
| 默认 30 分钟 budget 渲染 | 227 | 227 | — |

任务背景提供本会话持久化模板正文 10727 字符（含表单 1169）；这是给定审计值，**本次未重读持久化 payload 测量**。它与当前 dev.md 的 11369 不同，不据此宣称线上会话自动获得当前模板。源码文件之和也不是完整渲染请求：占位符、JSON 转义/缩进、wrapper、binding、工具 schema、宿主内容、附件都会改变输入。

仓库根可重复运行（只读源码；预算提取遇形状变化直接失败，不静默给估算）：

```bash
git rev-parse HEAD
python3 - <<'PY'
from pathlib import Path
import hashlib, re
paths = [
    'crates/calm-server/templates/builtin/dev.md',
    'crates/calm-server/prompts/planner.md',
    'crates/calm-server/prompts/tool-discovery.md',
    'crates/calm-server/prompts/workspace-reports.md',
    'crates/plugin/src/builtin/gitforge/instructions.md',
    'crates/calm-server/prompts/claude-planner/long-lived-processes.md',
]
for p in paths:
    b = Path(p).read_bytes()
    s = b.decode('utf-8')
    print(p, len(b), len(s), hashlib.sha256(b).hexdigest()[:12])
    if p.endswith('/dev.md'):
        m = re.search(r'<!-- neige:input-form .*?-->', s, re.S)
        assert m
        print('input-form block', len(m[0].encode()), len(m[0]))
s = Path('crates/calm-server/src/planner_turn_budget.rs').read_text()
m = re.search(r'"(\[Trusted Planner turn budget\].*?)",\n', s, re.S)
assert m and m[1].count('{}') == 2
t = m[1].replace('\\n', '\n').replace('{}', '30', 1).replace('{}', '300', 1)
print('default budget render', len(t.encode()), len(t))
PY
```

后续实验须用 production renderer/现有 fake provider 捕获 fixture wire，以相同 track_id、输入、scope、队列、head 对照，分别记录：首次启动、普通轮、grant 变化、结果到达、compact 后恢复、provider 切换的文本/工具描述及 schema 字节/字符。不要复制 renderer 到测量脚本。观测只存片段类型/长度/版本与非敏感 fixture，不记录真实凭证或全量用户内容。

无 tokenizer，本次不报 token 或百分比收益。1169 是表单块候选移除原文大小，1426 是现有教学大小，不是承诺净省量；保留索引/语义输入、包装后须重测。首次少注入能减少保留上下文；每轮少注入还能减少累计新增内容，但 provider 可能重计历史或缓存，累计输入/计费与当前 retained context 分开取 provider 明示指标，不能用轮数乘字符冒充账单。UI 折叠、tool deferred、compact 后旧 reading 都不能用于估算收益。

## 5. 最小分阶段实现与精确验收

每阶段独立可回滚，前一阶段证据不足不推进。以下是将来实现入口与场景，**不是本次已运行的测试**；不新增永久测试名清单或按文件字节 pin。修改 Rust 时 worker 跑聚焦测试/修格式；kernel gate 跑 quick、text/contract 和一次拒绝空选择的聚焦 replay；CI 跑广域套件，不在共享主机跑真实 Codex E2E。

### S0：建立不含私密内容的 wire 对照

影响：现有 `planner_card`/`planner_harness_start_adapter` 测试、`tests/cases/claude_planner_wiring.rs`、`tests/acp_planner_provider.rs`、`operation/{codex_adapter,claude_adapter}/tests.rs`。复用 fake provider，按当前注册 profile/provider 的合法组合跑真实 renderer；不造 ACP worker。

验收：Planner 含快照及绑定输入；Assistant 无 Planner/task 权限；plain-chat 不注入 Planner 内容；两个 worker 都保留 goal/context/acceptance/gate/result contract/attempt_id；Claude resume argv 含说明文件；ACP 首次/后续说明差异与 journal 一致。首次/每轮/恢复捕获分开输出长度，不做整段敏感日志。回滚仅删除新增观测/fixture，不改会话状态。

聚焦入口：`planner_card::tests`、`planner_harness_start_adapter::tests`、`the_worker_prompts_match_their_reviewed_goldens`、`-p provider --lib codex::thread_start_wire_tests`（源码 `codex/mod.rs` 注册的 lib 模块）。

### S1：仅剥离模板表单展示定义

影响：`template_context.rs`、相关模板快照测试及既有 prompt goldens。现有 parser 实际在 [fe/core/domain/template-input.ts](../../fe/core/domain/template-input.ts)，Rust 侧尚无可直接复用的表单 parser；不复制前端编译/校验策略。先给模板 owner 增加明确的模型展示投影契约：只识别唯一、闭合的 `neige:input-form` 注释，JSON 可解析且 version=1；未知版本、多块或畸形保留全文。投影只移除 UI 定义，不验证/解释字段、不执行默认值编译；协议识别入口归 `template_context`，保持与前端的声明格式一致。保留原 payload 和所有方法/审批文字，Bound Template Input 全量保留。若任一模板的必要语义只存在表单 help/description 中，先在该模板 owner 正文补齐独立语义，再允许移除；不能泛化为所有 HTML 注释可删。

验收：合法 UI 块不出现在新渲染、用户 merge_policy/issue 值仍精确；非表单注释、fence、字面 `{track_id}` 保持；恶意/畸形块不得吞周围约束（无法安全识别则保留原文）；v1 损坏/未来版本仍拒绝；老卡无快照不回填；已启动会话不自动 reset。对比真实渲染差异，只删定义而不删审批含义。回滚 renderer 即恢复原文，无迁移。

聚焦入口：`template_context::tests`、`shipped_dev_rendered_prompt_matches_full_golden`、`planner_developer_instructions_append_template_input_when_present`；新增回归在这些 owner 的原测试模块。关键 mutation：生产渲染故意删掉绑定输入，预先列出必须红的完整集合，在整个受影响 target/filter 中核对后恢复；不得只测长度。

### S2：缩短每轮 workspace 教学，维持 live 能力声明

影响：`managed_track.rs`、`prompts/workspace-reports.md`、`prompts/tools/neige_workspace_{ls,cat,diff,log}.md`、run-loop workspace tests；不新增权限缓存。每轮查询和工具 admission 原样保留；分页、水位和参数教学归各工具现有 description，短声明保留当前能力、只读/不可跨 Track 操作、不完整读取不是无活动、调用前读取工具说明的触发。Assistant 无通用 track/card CLI 读取权限，不能要求它读 `guide/`；两角色均通过现有可见 workspace 工具描述取得细节，不为缩短提示新增读取权限。撤销/无权限不沿用旧肯定句。

验收：有 grant 的 Planner/Assistant 在新建、resume 都收到短声明；撤销后不再收到有权限句且真实 workspace read 拒绝；worker/普通 track 无能力提升；工具说明取得不授予跨 Area 权限且 Assistant 无需读 track guide；grant read 故障不伪造肯定值；混合 result/budget/timeout segments 对应不偏移。回滚短文/工具说明变更即可，不动 grants 和历史。

聚焦入口：`harness::run_loop::workspace_report_tests`（现有 `resumed_planner_and_assistant_receive_live_workspace_report_instructions`、`ordinary_revoked_and_worker_turns_do_not_receive_workspace_grants`）、`result_receipt_tests`。新增 live 工具拒绝回归使用现有 workspace 工具集成测试。mutation 将实时 grant 查询改为始终有权，必须同时击穿模型声明和工具拒绝断言；先预测完整红集再执行整个相关 target。

### S3：以现有摘要与证据链完成交接，再决定自动化

影响：先仅角色/worker prompts、模板 owner 的交接说明及已有 Report/context 使用，不改 schema；已有 `worker-summary-v1` 结果与精确引用继续复用。插件细节只有在可授权读取入口经过验证后才在其 owner 分层，不能塞入 kernel 通用插件特例。预算 227 字先不动：本轮动态冻结语义有价值。

验收：用户先目标 A、后收窄 A 并追加 B，恢复摘要保留 B 与 A 覆盖关系；授权仅针对 head H，换 H2 或撤销后不能合并；混合任务结果摘要保留 contract conclusion 和 exact event，unavailable 明示；旧 gate pass 不能验当前树；未知提交不能被摘要触发重发。compact 前后、新 provider、新会话均可从现有可读证据恢复最低字段；读不到则列未决并停止依赖缺失事实的动作，不自动向其他会话取内容。

聚焦入口：`calm-types` 的 `observation::receipt::tests`、`harness::run_loop::result_receipt_tests`、`compact_tests`、`-p provider dispatched_prompt_disconnect_is_unknown_and_never_resent`、`assistant_provider_migration_preserves_history_current_links_queue_and_schema`。fake transcript 只用于传输和字段恢复断言；模型是否正确保留目标/审批另做受控样本人工核对，不用字符串存在测试冒充语义保真。

回滚提示/交接约定，已有 Report/事件继续可读。若需要自动摘要选择、持久化新字段或 compact 定制提示，另写窄设计再评审：本阶段不引入 DB 表、通用路由器、发送缓存。任何后续“只发一次”优化必须先通过 §3.2 全部边界；否则维持现有注入。

### 测试执行协议与维护成本

实现时先确认目标/filter 有实际测试，再执行，例如：

```bash
cargo metadata --locked --no-deps --format-version 1
# 从 metadata 选择真实 target；用 nextest list 确认过滤器非空
cargo nextest list --locked -p calm-server -E 'test(template_context::tests)'
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked -p calm-server template_context::tests --test-threads 8
```

入口已按源码模块核对；实施时仍用 list 确认非空，避免后续重命名造成静默漏跑。实现 gate 必须声明真实目标，若 gate 指向不存在目标则报告给 Planner 重开任务，不修改测试布局凑命令。

新增测试只在 owning module/既有 target 注册一次，普通新增测试不改单独名称清单；改合法 profile/provider 时更新现有注册与测试参数化来源，避免第二份硬编码业务名单。prompt golden 随 owner 正常再生并审 diff，不另加全仓文件大小 pin。每 CI workflow 使用既有所属测试 job 跑一次；kernel gate 只一次聚焦 replay，非额外永久 CI job。mutation 仅独占可恢复 worktree 做关键权限/证据断言，预告完整红集、跑完整可观察 target/filter、恢复后转绿；本文无生产修改，不执行 mutation。

## 6. 本次检查与交付

已读根 AGENTS.md、CONTRIBUTING.md 与上列实现/测试入口；初始 worktree 干净。实际执行：源码搜索/定点读取、`git rev-parse HEAD`、上文 Python 基线测量、相对文件链接存在性检查、`git diff --check` 与最终 diff/status 检查。文本 gate 由本任务 kernel 执行 `scripts/local-ratchet-gates.sh`，实际 verdict 以任务 gate 记录为准，不在本文预填通过。未跑 Rust/浏览器/真实 provider 测试；无代码/UI 变更，不声称端到端收益。

交付预览路径：`docs/architecture/context-delivery.md`。未取得的历史及宿主目录来源维持未知，不影响先做 S0–S2 的最小演进。
