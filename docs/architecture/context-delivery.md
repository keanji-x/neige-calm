# 上下文交付：减负而不丢契约

状态：待实施设计；本次仅修订本文。整体 **L2**：后续涉及授权、恢复与持久化语义，交付后由 Planner 安排双独立评审。本次不改生产代码、配置或其他会话，不安排评审、不手动提交。

## 1. 目标与边界

把模型输入组织为「常驻必要约束、当前任务摘要、本轮增量、按需说明、历史证据」，优先删除重复的展示性内容，保留唯一事实来源和恢复入口。不以更短为由删安全规则、隐藏工具 schema、改变工具授权或自动 compact 活跃会话。

借鉴 [#1893](1893-planner-prompt-diet.md) 的短主提示 + `guide/`：保留触发读取的索引，将低频操作细节交给现有读取路径。该文是历史设计，旧数据库统计、工具数量、提示字节目标及删除计划均不作为当前事实。以下核对基于源码 HEAD `99f42b2afc8dc0fcaea0c55ec072994d93305abc`。

仅有本会话和任务提供的当前 Area 报告背景，不是完整 provider 历史。任务背景称 `neige_workspace_ls` 曾因缺 grant 被拒绝；本文不绕过授权，不扫描其他 Area 数据库、会话或凭证。宿主通用指令与技能目录的重复只记录可证来源，未知留待授权后的独立观测。

## 2. 当前真实发送链路

以下路径均相对仓库根；链接指向实际实现。这里的“发送”指 Neige 到 provider 的边界，不代表已经获知 provider 最终模型请求、缓存或计费行为。

### 2.1 角色与 provider

| 入口/角色 | Codex | Claude | ACP（当前注册 OpenCode） |
|---|---|---|---|
| Planner | `profile_instructions` → `planner_instructions` → `SharedThreadStartParams.developer_instructions` → `thread/start` | 同一 profile 组装，另附长进程片段；session 保存文本，每轮 spawn 写说明文件，argv 用 `--append-system-prompt-file`；新建与 `--resume` 都传 | 同一 profile 组装；`session/prompt` 在尚无 `acp_submissions` 时前置 `Neige conversation instructions` 文本块 |
| Assistant | ordinary/launchpad head + mechanics + tail + discovery；thread/start 注入；不是缩短版 Planner | 同一 profile，discovery 改直接 MCP；不附 Planner 长进程片段；其余使用 managed session | 同一 profile，discovery 改 session tool list；首提交说明块 |
| PlainChat | **仅 Codex**；不传 developer instructions，`NoMcp` | 不适用 | 不适用 |
| task worker | `codex_adapter` 新 thread 注入 WorkerCodex；首 turn 单独传冻结的任务 prompt，已有 thread/turn 有恢复分支 | `claude_adapter` 命令行 `--append-system-prompt` 注入 WorkerClaude，任务 prompt 为位置参数；这是交互 worker 路径，不等同 managed Claude 每轮 spawn | **无 ACP task worker**；`TaskKind` 仅 Codex/Claude/Terminal。PlainChat 的 Worker 数据库角色不能当 task worker |

来源：[profile instructions](../../crates/calm-server/src/harness/instructions.rs)、[start adapter](../../crates/calm-server/src/operation/planner_harness_start_adapter.rs)、[role prompts](../../crates/calm-server/src/planner_card.rs)、[Claude wiring](../../crates/calm-server/src/claude_planner/wiring.rs)、[spawn](../../crates/calm-server/src/claude_planner/spawn.rs)、[ACP wiring](../../crates/calm-server/src/acp_planner/wiring.rs)、[ACP session](../../crates/calm-server/src/acp_planner/session.rs)、[Codex worker](../../crates/calm-server/src/operation/codex_adapter/mod.rs)、[Claude worker](../../crates/calm-server/src/operation/claude_adapter/mod.rs)、[TaskKind](../../crates/calm-truth/src/model.rs)。合法组合由 [profile binding](../../crates/calm-server/src/harness/profile.rs) 决定：Planner/Assistant 各支持 Codex、Claude、OpenCode，PlainChat 仅 Codex，任务 worker 仅 Codex/Claude（另有 Terminal）。provider 在建卡时绑定，runtime 不支持切换。provider 的 wire 组装在 [Codex](../../crates/provider/src/codex/mod.rs) 和 [ACP protocol](../../crates/provider/src/acp/protocol.rs)。

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
| 必要约束：kernel 角色/隔离/权限/工具调用规则，`planner_card` 与各角色 prompts | 建立上下文时常驻；保留授权边界、不可信数据边界、报告/任务身份；仅低频流程移 guides | 新上下文按绑定角色/provider 组装；不在通用层硬编码插件名 | 新上下文重送必要约束；未知是否保留时不凭 sent 标志省略 |
| 创建方法：card `TemplateContext` | 新 Planner 加载完整快照，包含全部表单注释；不做通用投影 | 创建快照不可被模板更新覆盖；来源精简只影响此后创建的快照 | 老会话不改 payload；无快照不回填猜测；读取原快照可核对 |
| 用户绑定输入：track owner binding 的 `template_input` | 启动及恢复；Bound Template Input 全量保留，含选择、含义和授权限制 | 绑定失效按现有 resolver 处理；用户改意覆盖原选择须进入摘要 | 与最新用户意图对照，歧义列待决，模板默认值不是审批 |
| 插件说明：builtin component / stored manifest，各自现有 owner | 现有组装入口全文保留；A 只删模板内已由该入口承载的重复教学 | 目前在启动/打开 session/首提交组装；运行时说明刷新**未实现，不在本切片实现** | 说明不是权限；内核始终逐调用校验 live enable/scope/role；旧说明可能留在上下文 |
| 仓库指导：仓库文件 + provider 文档化加载契约 | 任务涉及代码时按适用目录读取；不默认注入仓库全文副本 | cwd/head/指导文件改变重新判适用；不得删用户要求保留的规则 | 不同 provider 的新卡重新确认；摘要链接不代替实际约束 |
| 宿主通用说明/技能目录：外部宿主（具体来源未知） | 本设计不改；先让宿主提供有版本的来源清单，再判断同源重复 | 未证实前不去重；技能正文已有按需读取语义不等于目录零成本 | 不猜私有路径、不读密钥、不修改其他会话 |
| 工具发现：`SessionCatalog`/registry + provider loader | 常驻发现入口；按需取描述/schema，保留当前 listing/call 策略 | listing 依角色/profile/plugin 策略，workspace 四工具不按 grant 隐藏；调用实时校验 grant | 不同 provider 的新卡重新发现；旧工具集不是授权 |
| 当前任务摘要：Planner 对话/Report 的语义 owner；worker 为冻结任务 prompt + 正式消息 | 初次目标明确、目标改动、任务交接和恢复时写；不每轮重抄完整 report | 新用户 steering、覆盖关系、审批撤销、产出/验证变动即更新 | 按下文摘要契约重建；语义冲突由 Planner 判，不让 kernel 猜 |
| 本轮预算：`PlannerTurnBudget`/`FrozenBudget` | 每次 start 保留短时间值与冻结含义；提醒仅现有支持 steer 的分支 | 新 start 重采样；同 start retry 沿用冻结值 | 不用上一轮值恢复；不把剩余执行时间当上下文容量 |
| workspace 能力：`managed_track` live grant | 有 grant 每轮给短教学，保留必要分页/水位语义；无 grant 不注入；不省 live 查询 | 每轮校验；撤销后当轮无肯定声明 + 工具实时拒绝；不增加发送/撤权状态 | 每轮包括 resume 重新读；旧摘要和工具清单不能授予 grant |
| 状态增量：track-vcs head/diff，queue observations | 每轮只变动；已有 report-edit 去重继续保留；无 diff 不表示状态未变 | head、pending queue、rewind 改变；超时/丢 diff 标未知 | 当前 status/精确文件重读；不能以 last_sent_head 证明模型掌握所有状态 |
| 任务结果：completion/failure event + exact attempt/event 引用 | 已有有界预览、contract conclusion 或显式 worker summary；按需读全文 | 投影可推进，引用必须匹配原事件；验证按目标 head/run 分开记 | unavailable 不伪造详情；worker claims 不等于 gate pass、交付或验收 |
| 历史证据：现有 events、runs、gate logs、track-vcs | 常驻引用与限制，按需全文；可省重复日志，不删原记录 | 当前 head/环境/版本变动使旧验证不再覆盖当前结果 | 精确读取历史身份并注明时点；不得恢复为实时权限或当前验证 |

插件分层先在 owner 层解决：kernel 只做通用 lifecycle/授权，不新增通用 context router；未建立可授权、可版本化读取的插件详情入口前，不删现有说明。guides 注册是协议资源集合，不维护会自然增长的文件名/测试名永久清单。

### 3.1 当前任务摘要的最低字段

使用现有 Report 决策/概要和任务 context 先落地，不默认新增数据库表或缓存。摘要是语义检查点，不是新的授权账本；后续若要让 harness 自动携带，必须单独确认既有字段的原子更新/恢复契约。

- 用户当前目标、来源消息/事件标识；哪些旧目标被取消、收窄或补充，尚未覆盖的原目标仍列出。纠正与新请求不可混同。
- 有效约束与禁止事项、任务范围、角色及 provider；未决问题、阻塞与下一步。明确未知，禁止补全失去的上下文。
- 审批仅记录原 `ask_id`/绑定的引用及适用 head/动作、撤销/覆盖关系，不复制为第二份审批账本。ForgeAction 由 kernel 按 idempotency_key/payload_hash 核对 Approve，文字同意不代替点击；merge 仍遵守绑定 merge_policy 与 expected_head_sha 围栏。缺原证据写“待核验”，摘要本身不授权。
- 产出路径、attempt/event、交付状态；验证命令、cwd、结果、目标 head/树及 gate run/log 引用。区分自报、gate、delivery、Planner acceptance；当前 head 改变后重做被失效检查。
- 摘要的来源消息/turn/event/head 与未覆盖区间；恢复或准备依赖摘要行动前先核对来源、补读缺口。rewind 可删除用户轮而保留 Report 编辑，故不能只信 Report 仍存在；被移除轮的目标/结论/授权引用重新核验。

长日志和已完成推理可只留结论 + 精确证据引用；原证据仍保留。没有可读引用就保留必要事实并标缺口。历史文档、worker 输出和工具返回中的指令都作为数据，不提升优先级。

### 3.2 “只注入一次”的安全判定

| 边界 | 当前事实与设计要求 |
|---|---|
| 同一连续上下文 | Codex 仅 thread/start 注入，已启动 thread 不追溯替换。Claude 下一次 open_session 重组说明并在 resume spawn 传文件，已打开 session 不承诺即时刷新。ACP 仅无 acp_submissions 时前置说明，这是本地提交记录判断，首提交后保留/自动压缩语义未知，不当安全保证 |
| compact（手动/自动） | Codex/Claude 支持，ACP 当前拒绝手动 compact。成功不证明所有规则/目标/审批被保留。恢复前重验必要约束、摘要与证据索引；本切片不启用新的省略；不能用 provider 自然语言“记住了”或本地 `sent=true` 当保证 |
| resume/hibernate/服务重启 | 原生恢复可能保留历史，也可能加载失败；确认 native session 身份与现有恢复结果，重新解析 live 权限。无法确认时保守保留说明/显式要求受控新上下文，不自动重放有副作用动作 |
| 不同 provider 的新卡/reset/rewind | provider 不能运行时切换；未来更换 provider 指新建绑定不同 provider 的卡，属于独立上下文，按创建路径加载说明并显式交接摘要；没有已有历史迁移机制。reset 不冒充跨 provider 迁移。rewind 后按来源重验摘要，Report 持久化不等于来源仍有效 |
| retry 结果未知 | 区分未提交、已拒绝与已 dispatch 但未知。保持现有 issuance/ACP submission journal 的不重发语义；先核对结果，不为补上下文重复动作。ACP unresolved outcome 明确阻止再发；摘要不得把未知写成失败 |
| 授权撤销/角色变更 | 模型可能仍保留旧许可；省略旧许可提示不能撤销它。live kernel 拒绝是权威；当轮不再注入肯定声明，摘要原授权引用须重验。没有“曾注入一次即可永久信任”模式 |

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

wire 对照附着 A/B/C 各自实际改变的生产入口，复用现有 fake provider/fixture，不建立 S0 全 provider 组合平台。相同输入、scope、queue/head、模板快照与工具加载步骤成对比较；记录文本、wrapper、工具 description/schema 和按需读取响应的 UTF-8 字节/字符。捕获的是 Neige→provider 边界，不是 provider 内部最终请求。只保存非敏感 fixture 与长度，不录真实凭证或用户全文。

无 tokenizer，本次不报 token 或百分比收益。1169 仅记录当前源码表单大小，**不列为节省候选**；1426 是现有教学大小，不是承诺净省量；包装后须重测。首次少注入减少自有发送文本；每轮少注入减少累计新增文本，但 provider 可能重计历史或缓存，累计输入/计费与当前 retained context 分开取 provider 明示指标，不能用轮数乘字符冒充账单。UI 折叠、tool deferred、compact 后旧 reading 都不能用于估算收益。

## 5. 可独立实施的三个切片

以下均为待实施计划，不是本次测试结果。A/B 可直接实施，C 可先落实提示契约；不以新框架、通用解析器或 provider 保留实验为前置。整体 L2 不降低，实施后的双独立评审由 Planner 安排。

### A：在模板来源删重复教学，完整保留快照与表单

**owner / 最小文件面。** 模板 owner 修改 `crates/calm-server/templates/builtin/dev.md`，从报告教学删长正反例，保留四节结构、状态改写、证据引用与篇幅规则；从技术工作流删已由 gitforge instructions 完整承载的 issue 工具参数/返回字段/刷新重试、CI evidence 与 after_rerun 操作教学。插件 owner 的 `crates/plugin/src/builtin/gitforge/instructions.md` 作为这些技术细节的唯一来源，本切片无需扩写它。模板仍保留工作方法、仓库核对、可选 issue/无 issue 边界、验证职责、审查及 merge_policy/按 head 审批的全部必要语义；不把方法策略移给插件。

**注入前提与负例。** `planner_instructions` 对 compiled builtin catalog 先按 `plugin_documents_track`（track.plugin_scope 匹配，或 template_id 属 manifest.templates）附说明，此分支不依赖 enabled；[dev component](../../crates/calm-server/src/builtin_plugins/dev/mod.rs) 指向同一 gitforge 文本。enabled/scope/report-ref 过滤和 4096 cap 属后续 stored-manifest 分支，不能用它证明必达，也不能仅凭模板标题推断绑定。开发模板绑定路径即使插件禁用仍有技术文档，**工具仍不可用，不能据说明执行**。无绑定/其他模板/自定义快照不做运行时替换；若某来源不能证明对应说明必达，则保留该来源技术文字，不增加 fallback 或授权。

**旧/新语义。** 删除原 S1 整个自动 input-form 剥离机制及相关验收、mutation 承诺。任意旧/新快照的 body 均全文经 `TemplateContext::append_to` 序列化，完整保留表单、help/description、非表单注释、字面占位符；Bound Template Input 不变。更新模板只改变以后创建的快照，不回填旧卡；外层 TemplateContext 损坏/未来版本依现行拒绝，无快照保持 None。将来若需要独立模型内容投影，另设计 typed owner 契约，不是本切片前置。

**聚焦测试与正负验收。** 扩展现有 `builtin_plugins::tests::builtin_issue_instructions_remain_documentation_when_dev_is_disabled`、`builtin_legacy_issue_documentation_preserves_snapshot_and_authority`，调用真实 `planner_instructions`；复用 [template startup cases](../../crates/calm-server/tests/cases/template_startup_context.rs) 的创建路径和 `planner_instructions_for_test`，确认 `TemplateContext::append_to` 的实际输出。新建 dev 卡组合说明包含技术唯一来源、完整表单/绑定值/审批含义；旧快照只在表单说明里写限制的样本原样保留；禁用插件、无绑定、无快照、外层损坏/未来版本分别验证文档/拒绝边界。不要用 `shipped_dev_rendered_prompt_matches_full_golden`（不含快照）作快照保真证据；它只覆盖基础 renderer。

**wire 与回滚。** 在上述真实组装输出及对应 startup fake-provider 捕获成对比较：新 dev 快照净减且所有保留语义可观察，旧/自定义快照输出无删减；JSON 转义与插件段一并计入。回滚模板来源只影响未来创建，已有精简快照不会自动变长；不改 payload、迁移或重置会话。普通长度优化不 mutation；若新增关键保真断言，仅在独占 worktree 对生产 append 路径作单因素破坏，预测完整红集、跑所有可观察 target/filter，再安全恢复转绿。

### B（原 S2）：只缩短有 grant 的每轮 workspace 教学

**owner / 最小文件面。** `managed_track` owner 只精简 `crates/calm-server/prompts/workspace-reports.md`，扩展已有 run-loop tests；`managed_track.rs`、workspace 四工具的 description/schema/listing/admission 保持现行行为。不把教学搬入所有角色都列出的 description，不增工具、读取入口、grant 缓存、发送状态或撤权状态。

短教学保留：当前跨所有用户可见 Area 的只读能力、四工具发现/用途、ls→cat 选择 track_id、date 与 diff→log 的 through_event_id 水位、next_cursor 到 null 且分页保持水位、失败/未完成不等于无活动、来源引用、不能操作别的 Track、写自己 Report 前 read、Assistant 的现行操作/通用 CLI 限制。删重复措辞与长 JSON 示例；参数结构由既有 schema 支持，必要分页教学仍每轮可见。无 grant 当轮不发肯定声明；旧上下文可能留有声明，实时工具拒绝才是权限屏障。

**聚焦测试与正负验收。** 扩展 `-p calm-server` 下 `harness::run_loop::workspace_report_tests`：经真实 Fixture enqueue/issue，在 started_turns 捕获新建及 resumed Planner/Assistant 的实际发轮，核对短教学的必要语义与输入 segments 对齐；混合 result/budget/timeout 不偏移。现有 `ordinary_revoked_and_worker_turns_do_not_receive_workspace_grants` 还须增加同一会话 grant→撤销→下一轮的顺序样本，确认无肯定声明。实时拒绝复用 `mcp_server::tools::workspace_reports::tests::ordinary_assistants_and_persisted_workers_cannot_read_workspace_reports` 的 registry handler 调用，覆盖撤销后四工具、Worker/普通 Track/跨 Area identity；grant 查询失败不能变成肯定值。旧工具可见不等于可调用，Assistant 不必读无权访问的 track guide。

**总成本验收。** 在改变的发轮入口捕获短文，并在现有 catalog/provider listing fixture 核对无 grant 的所有合法路径：Planner/Assistant × Codex、Claude、OpenCode；PlainChat 仅 Codex、task worker 仅 Codex/Claude 检查不增 workspace 能力。沿用注册元数据选择合法路径，不建新组合平台。Claude direct MCP、ACP session list 与 Codex deferred search/describe 的文本、description、schema、wrapper 都成对计数；四 description/schema 本切片字节应相等，无 grant listing 的增长上限为 **0**，不得靠 provider 未加载的假定跳过。

对固定 n 轮与相同工具加载/读取序列，分别记录 `T = startup + Σ(本轮文本 + listing/description/schema + 按需读取响应)` 的 before/after；有权要求 T_after < T_before 且每个有权普通轮净减，无权要求 T_after ≤ T_before、listing/schema 不增长。工具描述读取也计成本；若需要额外 describe/read，必须包含其请求/响应，不能只比 1426 与短文长度。provider 可能重送历史/缓存；另记其可提供的 usage，不把此 T 当账单或实际 retained tokens。当前尚无短文 wire 实测，不报净省数。

**回滚与 mutation。** 恢复教学源文只影响以后发轮，不抹掉已发短文/旧能力声明，不改 grant。长度优化不新增 mutation；仅关键授权断言可 mutation 共享 `workspace_report_zone` 为始终有权，事前预测 prompt 与真实拒绝测试的完整红集，跑所有可观察 filter/target，核对后恢复转绿。不得只破坏提示分支冒充工具权限验证。

### C：用现有 Report、任务 context 与 prompts 明确交接摘要

**owner / 最小文件面。** Planner/worker 提示 owner 最小修改 `crates/calm-server/prompts/planner.md` 与 `crates/calm-server/prompts/worker/head.md` 的交接规则；使用现有 Report 概要/决策、任务 context 落实 §3.1，不新增 schema、自动摘要选择、持久化账本、router 或发送缓存。既有 `worker-summary-v1`、contract conclusion 与 exact receipt 继续使用，不把它们当自动恢复器。

摘要只保留当前目标及覆盖关系、约束/未决、下一步、产出/当前验证状态和精确证据引用；普通日志与长推理按需读。提示增加无条件规则：恢复或依赖摘要行动前检查来源与缺口、授权引用和当前 head，读不到标未知并停止依赖缺失事实的动作；不依赖模型收到 compact 通知。compact outcome 是 runtime fact，不必然是 transcript。compact 后内容保留仍未知，**不启用新的省略/只发一次**；现有 ACP 首提交策略也不证明保留，属于未解决限制。

**聚焦测试与正负验收。** 在现有 prompt 测试观察规则实际进入 `planner_instructions`/worker `task_prompt`，在 receipt/result owner 测试观察 exact attempt/event/unavailable，而非只测模板源码。用户先 A 后收窄 A 并追加 B 的交接样本保留 B 及覆盖关系；head H 审批引用不授权 H2；旧 gate 不验新树；rewind 后 Report 中被移除 turn 的摘要需重验来源；未知提交不重发。已有入口为 `-p calm-types` 的 `observation::receipt::tests`、`-p calm-server` 的 `harness::run_loop::result_receipt_tests`、`harness::run_loop::compact_tests`，以及 `-p provider dispatched_prompt_disconnect_is_unknown_and_never_resent`，各自只证实其实际层次，不把 compact 测试当语义保留保证。

未来不同 provider 的交接样本通过真实建卡/start 路径新建另一 provider 卡，完整加载该卡约束，显式传入有来源摘要；不迁移原 thread/history，不把 `assistant_provider_migration_preserves_history_current_links_queue_and_schema` 的 DB 迁移测试当上下文证据。fake wire 只验传输与字段，目标/审批的模型理解用受控样本人工核对，不声称字符串断言证明语义恢复。

**wire 与回滚。** 在实际改动的 prompt/start/worker 入口对相同交接任务比较常驻提示 + 摘要 + 按需读取总字节，规则新增的固定成本也列出；未减少总成本不称收益。回滚提示/约定仅影响未来组装，原 Report/事件继续可读。Codex 已启动 thread 不追溯替换；Claude 下次 open_session 加载新说明，仍包含既有快照；ACP 首提交后不保证刷新或保留。所有阶段回滚都不能抹去已有上下文。

### 执行与维护成本

实现时先用 `cargo metadata --locked --no-deps --format-version 1` 定位实际 target，再用 `cargo nextest list --locked -p <package> -E 'test(<filter>)'` 确认非空；新增场景在 owner 既有模块/target 注册。worker 仅跑聚焦测试并修格式，命令保持 `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked -p <package> <filter> --test-threads 8`。kernel gate 跑 quick、text/contract 与一次拒绝空选择的聚焦 replay，CI 跑广域；共享主机不跑真实 Codex E2E。

不新增永久测试名/文件大小 pin 或 provider 组合清单；普通增改测试只改所属模块及必要 golden，既有 CI 所属 job 每 workflow 一次，gate 只一次聚焦 replay。关键权限/保真 mutation 依 AGENTS 预测完整红集、跑完整可观察 target/filter、恢复检查 residue；不为普通长度优化加 mutation。本次 docs-only 不执行上述生产测试或 mutation。

## 6. 评审处置与本次交付

原渠道 A（event 375284）与 B（event 375740）的六项 blocking 逐条处置：

| 原发现 | 本版处置 |
|---|---|
| A1：识别条件不足导致畸形表单被删 | A 撤回整个自动剥离机制；不新增 Rust 表单识别器 |
| A2：补最新模板不能保护旧快照语义 | 任意旧/新快照全文保留，来源修订只影响未来创建 |
| B1：PlainChat 非法 provider 组合 | §2.1 改为仅 Codex，其余不适用 |
| B2：不存在 runtime provider 切换，误用 DB 迁移测试 | §3.2/C 明确不同 provider 新卡独立上下文；移除迁移测试的证据资格 |
| B3：golden/mutation 未观察快照 | 删除 S1 承诺；A 经真实 planner_instructions/TemplateContext append 观察快照 |
| B4：S2 把成本移到无 grant 工具列表 | B 不改 description/schema，所有合法无 grant listing 零增长，并核对总成本 |

便宜澄清同时纳入：审批仅引用 ask_id/绑定；rewind 后重验来源；compact/ACP 保留未知且不开新省略；撤权用当轮无肯定声明 + live 拒绝；插件运行时说明刷新明确未实现。五项架构约束保持：规则归模板/插件 owner、共用真实组装与授权入口、通用 kernel 不按应用名分支、provider 保留未知不猜、观测附着现有入口而不增永久清单。

本次读根 AGENTS.md/CONTRIBUTING.md、原设计、两路完整结果和相关生产/测试调用路径；docs 下未发现更具体 AGENTS。初始 worktree 干净，核对 HEAD `99f42b2afc8dc0fcaea0c55ec072994d93305abc`；重跑 §4 只读基线、相对链接存在性、`git diff --check` 及 diff/status。文本门禁由 kernel 执行 `scripts/local-ratchet-gates.sh`，verdict 以实际 gate 记录为准，不预填通过。未跑 Rust/浏览器/真实 provider/模型保留测试，不声称已实现收益。不改代码/配置，不自行提交或安排评审。

交付预览：[docs/architecture/context-delivery.md](context-delivery.md)。未取得的其他会话历史、宿主目录来源及 provider 保留语义仍未知。
