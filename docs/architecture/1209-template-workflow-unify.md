# #1209 — Template 与 workflow 合并为一个概念

状态：设计中（**v5**，五轮独立评审后修订；逐条裁决见 §11）。目标是让 `POST /api/tracks` 的
模板字段只做一件事：**指名一个 template**；
插件绑定（binding + `input_schema`）降级为 template 的一个可选属性。成功判据是
`crates/calm-server/src/routes/tracks.rs:779` 那一行特例消失，且不是换个地方重新出现。

> **v4 的两条新约束来自人（2026-09-01，原话）**：
> 「我觉得这里你可以破坏兼容性，因为新的 FE 还没有上生产，所以我希望你尽可能保持一致。」
>
> 这句话拆成两个可执行后果，**都在本切片内**：
>
> 1. **B/M2 由人裁决**（§5.3 重写）：走诚实的 breaking 路线。**不**把这个能力降级到
>    Tier D、**不**撤回「公开插件契约破坏」的定级去凑一个 `Preserving` 判决。
>    让机器判决（`crates/neige-app/src/preflight.rs:204-227`）与本文的说法一致。
> 2. **D2 重开**（§3 重写）：v1–v3 保留 `workflow_id` 这个线上拼写、把改名甩给未排期的 S2，
>    其**未言明的前提**是「改名的代价是兼容性」。人已经取消了这个前提，并明确要一致。
>    **那道词汇缝正是 #1209 的标的物**，所以保留它的理由已经不存在。
>    D2 改为：线上字段改名为 `template_id`，原 S2 并入本切片。
>
> 这两条**都放大了切片**。§10.1 重新诚实陈述范围，并给出**两 PR 的切法**。

本文所有「现状」陈述都带 `path:line`。凡标 **OBSERVED** 的是读代码读到的；标 **INFERRED**
的是从读到的事实推出来的、没有单独跑过验证的。

**v2 新增的记账纪律**：「读代码可推出」与「有测试钉住」是两件事，本文此后分列，
绝不混写。一条行为只有在被点名的测试**真的断言了它**时才算 pinned；
只是「那条测试路过了这个分支」不算（v1 在 `forge_workflow_e2e.rs` 上犯过这个错，见 §11 F16）。

**v3 的基线（2026-09-01）**：worktree `1209-template`，分支 `feat/1209-template-unify`，
已快进到 `origin/main` = **`0b4b022f`**（v2 写的是 `6e0339b0`，之后进了
`67829da0` #1191 手机端导航框架与 `0b4b022f` #1147 S6）。
**本文引用的每一个文件都被逐一核过没被这两个提交碰到**（`git diff --stat 6e0339b0..0b4b022f --`
`routes/tracks.rs`、`routes/track_templates.rs`、`workflow_templates.rs`、`plugin_host/manifest.rs`、
`plugin_host/mod.rs`、`error.rs`、`lib.rs`、`calm-types/src/track_report.rs`、
`calm-truth/src/db/mod.rs`、`calm-truth/src/db/sqlite/area.rs`、`ci.yml`、
`plugins/git-forge/manifest.json`、`docs/deploy-and-upgrade.md`，以及四个被引测试文件
`tests/cases/track_workflow_templates.rs`（仍是 589 行）、`tests/cases/track_templates_read.rs`、
`tests/forge_workflow_e2e.rs`、`tests/cases/track_workspace_materialize.rs` —— **全部空输出**）。
注意：`crates/calm-server/tests/` 目录**整体**被动过（`replay_fixtures.rs`、
`terminal_card_endpoint.rs`、`track_workspace_repoint.rs`、`ws_terminal_e2e.rs`、
`claude_card_endpoint.rs`、`scheduler.rs`），只是不含本文引用的那四个。
**v3 新增的记账纪律**：评审通道给出的「引用漂移」修正**本身也要复核**——
v3 逐条打开后驳回了通道 A 六条 ±1 修正里的四条（见 §11 的 R10）。

**v4 的基线注记（两条，都是本轮实测）**：

1. **本 worktree 仍是 `0b4b022f`，不动。** 共享 `origin/main` 期间前进到 `355807d6`；
   `git diff --stat 0b4b022f..355807d6 --` 对本文引用的 Rust / 文档文件（`routes/tracks.rs`、
   `routes/track_templates.rs`、`workflow_templates.rs`、`plugin_host/manifest.rs`、
   `calm-types/src/model.rs`、`crates/neige-app/src/`、`routes/version.rs`、
   `docs/deploy-and-upgrade.md`、`docs/upgrade-stability.md`）**全部空输出**；
   **唯一被碰的是三个 OpenAPI 生成物**（`fe/core/api/generated/openapi.json`、
   `web/src/api/generated.ts`、`web/src/api/openapi.json`，`+99/−12`）。
   §3 的改名清单因此**只对生成物给「重新跑生成器」的指令，不给行号**——
   给了也已经过期（CLAUDE.md「Rebase Invalidates Gate Evidence」）。
2. **⚠️ #1230 的基线在四轮评审里动了四次，v5 因此不再记它的行号。**
   `1230-s1` worktree 的 HEAD 依次是 `b93fb767`（v2/v3）→ `7b85caa3`（v4 写的）→
   `d51571d7`（第 4 轮评审当时）→ **`3b9cc03c`（本轮实测，`git -C ../1230-s1 log --oneline -1`，
   工作区另有 `tests/cases/track_workflow_templates.rs` 未提交）**。
   **v4 §8.2 那组自称「对 `7b85caa3` 复测」的坐标复现不出来**（两个通道各自独立实测，
   见 §11 的 R4）——它们来自当时的 dirty working tree。
   **v5 的处置：删掉本文里全部 `1230-s1` 行号，只保留结构结论（形状），
   并把「对合流当时的 #1230 HEAD 重跑 grep」写成合并步骤。**
   这是 CLAUDE.md「Rebase Invalidates Gate Evidence」在一个跑动靶子上的必然结论：
   一份设计文档承载不了另一条活分支的坐标。

**v5 的记账纪律（本轮新增）**：
**一条自称 OBSERVED 的数据，如果给不出「跑哪条命令、在哪个 commit 上」，就不许写进本文。**
v4 在三处违反了它（#1230 复测坐标、`manifest.rs:302` 那条「pin」、
「#1209 对 wire 零改动」），三处全部被两个通道独立打掉。
本轮所有计数都附带产生它的命令（§3.2），所有 `1230-s1` 坐标都被删除。

---

## §0 强迫函数已经到了

#1209 正文写了「本 issue 只记账，不主张现在重构」，并列了四条触发条件。其中
**「某个模板不再由 Rust 硬编码」** 已经被 #1230 S1 触发：该切片让
`GET /api/track-templates` 改读已播种 template track 的 report，Rust 常量退化为
bootstrap（见 §8）。所以本文不再论证「要不要做」，只设计「怎么做」。

---

## §1 现状（OBSERVED）

### 1.1 create 路径上的两条路

`crates/calm-server/src/routes/tracks.rs:761-784` 是第一条路（插件绑定）：

```rust
let bound_plugin = match p.workflow_id.as_deref() {
    Some(workflow_id) => {
        let unknown_workflow = || CalmError::BadRequest(/* ... */);
        if workflow_id.trim().is_empty() { return Err(unknown_workflow()); }   // :770
        match resolve_trusted_workflow(&s, workflow_id).await {                 // :773
            Some(plugin) => Some(plugin),
            None if is_workflow_template_key(workflow_id) => None,              // :779  ← 特例
            None => return Err(unknown_workflow()),                             // :780
        }
    }
    None => None,
};
validate_workflow_input_binding(bound_plugin.as_ref(), p.workflow_input.as_ref())?;  // :790
p.plugin_scope = bound_plugin.as_ref().map(|m| m.id.clone());                        // :793
```

`crates/calm-server/src/routes/tracks.rs:799-814` 是第二条路（模板播种 + fork），
条件是**同一个字符串**再判一次 `is_workflow_template_key`：

```rust
if let Some(workflow_id) = p.workflow_id.as_deref()
    && is_workflow_template_key(workflow_id)                                    // :800
{
    ensure_workflow_templates(&s).await?;                                       // :803
    if fork_report_from.is_none() {
        fork_report_from = Some(lookup_workflow_template_track(&s, &template_key).await?
            .ok_or_else(|| CalmError::Internal(/* :807-811 */))?);
    }
}
```

同一个字段被两个互不相干的判据各读一次，中间靠 `:779` 这行「没绑到插件、但它是模板 key，
放行」把两条路缝起来。这就是 #1209 说的那道缝。

### 1.2 绑定解析器本身很小

`resolve_trusted_workflow`（`tracks.rs:937-950`）= 在 `running ∧ trusted` 的插件里找
`manifest.workflows[].id == workflow_id`，命中返回**整个 Manifest**。
`WorkflowDescriptor` 只有一个字段 `id`（`crates/calm-server/src/plugin_host/manifest.rs:472-475`，
doc 从 `:467` 起）。

也就是说：**一个插件「声明一个 workflow」在数据上只贡献了一个名字**；真正有用的
`input_schema` 挂在 Manifest 顶层（`plugins/git-forge/manifest.json:273-300`），
不在 workflow 条目上。`validate_workflow_input_binding`（`tracks.rs:958-995`）的注释也明说
「Workflow-level `input_schema` is never consulted」（`tracks.rs:957`）。

这条事实是整个统一模型的地基：**plugin 的 workflow 声明 ≈「我认领 template key X，
并为它提供输入 schema」**，本来就已经是「template 的一个属性」的形状了。

### 1.3 workflow id 的唯一性已经是不变量

`plugin_host/mod.rs:1099-1130` 在 spawn 的原子准入里跑 `find_workflow_conflict`（`:1114-1119`），
在「running ∧ trusted ∧ admitted」这同一个集合上强制 workflow id 唯一，注释
（`mod.rs:1093-1095`）点名三个消费者：`resolve_trusted_workflow`、`bound_workflow`、
MCP per-track tool scope。

推论（**INFERRED**）：「一个 template key 最多被一个插件绑定」今天就成立，
不需要本次设计新增任何锁或校验。

### 1.4 权威源

| 事实 | 权威 | 定义处 | 消费处 |
|---|---|---|---|
| template 名册（3 个 key）+ 出厂 title | Rust 常量 | `workflow_templates.rs:18`（`WORKFLOW_TEMPLATE_KEYS`）、`:25-38`（`WORKFLOW_TEMPLATES`）——**两份独立数组**，见 §2.3 | `tracks.rs:449`、`track_templates.rs:103-104`、`workflow_templates.rs:41` |
| template 出厂正文/任务 | Rust 常量函数 | `workflow_templates.rs:44-51`（`workflow_template_report`）、`:62-69`（`workflow_template_tasks`）、`:99-118`（`report_from_tasks`） | `tracks.rs:586`、`:523`；`track_templates.rs:117` |
| template 当前正文/title | 已播种 template track 的 report 行 | DB（system area 的 template track） | `tracks.rs:805-812`（fork 的是它，不是常量）；#1230 S1 让读口也认它 |
| binding + `input_schema` | 插件 manifest | `plugins/git-forge/manifest.json:273-300`（`input_schema`）、`:302-306`（`workflows`） | `tracks.rs:937-950`、`track_templates.rs:109-111` |
| 报告形状契约（contract prefix） | Rust 常量函数 | **`crates/calm-types/src/track_report.rs:137-144`** | `workflow_templates.rs:104`（拼进 body） |

（末行的定位是 v1 的错误：v1 把调用点 `workflow_templates.rs:99-105` 当成了定义处。）

注意第 3 行与第 2 行的关系：`tracks.rs:805-812` 里 fork 的是 **track 的 report 行**，
不是常量。所以「模板正文的真值今天已经在 DB 里」——这是 #1230 正文的核心发现，
读代码可确认（**OBSERVED**）。常量在两处进场：
`seed_workflow_template_track`（`tracks.rs:517-579`，其中 `:523` 取常量报告、
`:562-577` 落库）与 `restamp_template_report_if_placeholder`（`tracks.rs:581-613`）。
后者**先无条件取常量报告**（`:586-590`）**再**判是否早退（`:592-594`），
且早退判据是精确相等而非「用户改过」，详见 §2.3。

### 1.5 读口已经按统一后的形状写了

`crates/calm-server/src/routes/track_templates.rs:1-39` 的模块头是一份契约（`:41` 起是 `use`），明写：

* `:5-7` 「没有 `track_templates` 行；这个端点是 *join*，不复制不发明第三处」；
* `:20-22` 「`tasks` 从纯常量函数读，绝不从 template track 的 stored report 读：
  一次 *读* 不能触发写」；
* `:29-39` 词汇缝：读口说 template，写口说 `workflow_id`，并写死「合并落地时这里返回的
  形状一个字都不用改」。

第二条已经被 #1230 S1 改写（见 §8）；第三条是本设计必须兑现的承诺。

**v4 重读第三条**（人的新约束直接落在这里）：`:29-39` 那段的主题**就是**本次要删掉的那道缝，
所以它不是「改写措辞」而是**整段删除 + 换成一句「一个概念一个字段」**（§3 给了新文本）。
但要分清同一段里的两句话：

* **「读口说 template、写口说 `workflow_id`」——这句被删掉**（缝没了）。
* **「合并落地时这里返回的形状一个字都不用改」（`:39`）——这句仍然为真，而且更强了。**
  `TrackTemplate` 的字段集合是 `{key, title, tasks?, input_schema?}`
  （`routes/track_templates.rs`，`workflow_id` 在该文件里只出现在三处**注释**：
  `:32`、`:57`、`:62`——**实测 `grep -n workflow_id`**），
  **没有任何一个字段叫 `workflow_id`**。所以 §3 的改名**不改读口的响应形状**，
  §10.1 的 PR-1 验收 **A5** 与 §8.3 第一行照旧成立。改的只有那三处注释里的拼写。

### 1.6 前端

* `fe/core/domain/track.ts:166-182` — `workflow_id` / `workflow_input` 的注释已经按
  「template 的 key」措辞，并复述了词汇缝。
* `fe/core/domain/track.ts:198-215` — `trackTemplateSchema` + `trackTemplatesOperation`。
* `fe/web/src/features/area/new-track/public.tsx:247-256` — `needsInput(template)`
  判据是 `template.input_schema != null`，**不是 id 白名单**。
* `fe/e2e/track-create.spec.ts:84-155` — 用 `small-change`（无绑定模板）跑真内核。

**结论（OBSERVED）**：前端已经只看见一个概念。**统一本身**对 FE 是零改动。
**但 v4 的 D2 改名对 FE 不是零改动，而且面比 §3 v3 版写的大一倍**——因为仓里有**两个**前端：

| 目录 | 是什么 | 上生产了吗 |
|---|---|---|
| `web/` | **今天打包发布的那个 bundle** | **是**。`ci.yml:903-905` 与 `:1114-1116` 都是 `working-directory: web` + `npm run build`；`docs/deploy-and-upgrade.md:62` 的打包参数是 `--web-dist web/dist` |
| `fe/` | 新 FE（`fe/package.json` 的 `name` = `neige-calm-fe`） | **否**——这正是人说「新的 FE 还没有上生产」指的那个 |

**所以「可以破坏兼容性」不等于「`web/` 不用改」**：`web/` 是活的生产客户端，
它必须在**同一个 PR**里跟着改名，否则一个还在浏览器缓存里的旧 bundle 会继续发
`workflow_id`，撞上 `deny_unknown_fields`（`tracks.rs:196`）拿到一个 400——
那是 `docs/upgrade-stability.md:29` 明令禁止的「部分工作」。
处置见 §3 的 `WEB_COMPAT_VERSION` 裁决。

---

## §2 §决策 D1 — 统一后的数据模型

### 2.1 概念

```
Template {
    id:      &'static str,              // 名册键；写口的 `workflow_id` 就是它
    title:   String,                    // 已播种→report.summary；未播种→常量
    content: TrackReportPayload,         // 已播种→template track 的 report；未播种→常量渲染
    binding: Option<PluginBinding>,     // 可选属性，不是兄弟概念
}

PluginBinding {
    plugin_id:    String,               // 落到 tracks.plugin_scope
    input_schema: Option<Value>,        // 决定这个 template 收不收 workflow_input
}
```

`small-change` / `investigation` = `binding: None` 的退化情形，不再是特例。
`issue-development` = `binding: Some(git-forge)`。

### 2.2 落到代码上是一个函数

**PROPOSED**（新增，今天不存在）。v2 相对 v1 有三处改动：结构体改名、删掉 `title` 字段、
名册查找收敛成 `workflow_templates.rs` 里的唯一一个 helper。

```rust
// crates/calm-server/src/workflow_templates.rs —— 名册的唯一**可失败查找 helper**。
// ⚠️ v4 收窄（通道 A m4，重扫判定成立）：v3 写「唯一查找入口」是**假的**。
// 合并后 #1230 侧 `routes/track_templates.rs` 的 `current_definition` 回落分支里
// 开手写了 `WORKFLOW_TEMPLATES.iter().find(|t| t.key == key)`（v5：不记行号，见 §8 基线声明）
// 取 title——同一个数组，因此**漂移不可能发生**，但「唯一入口」这个全称句子不成立。
// 处置：§8.2 的合并规则把这个站点点名（它不吃 `is_workflow_template_key`，
// 所以合并树 grep 抓不到它），claim 收窄为上面这句。
// v2：删除 `WORKFLOW_TEMPLATE_KEYS`（`:18`）与 `is_workflow_template_key`（`:40-42`），
// 二者都从 `WORKFLOW_TEMPLATES` 派生（见 §2.3 与 §8.2 的 dead-code 论证）。
pub fn workflow_template(key: &str) -> Option<&'static WorkflowTemplate> {
    WORKFLOW_TEMPLATES.iter().find(|template| template.key == key)
}
```

```rust
// crates/calm-server/src/routes/tracks.rs
/// #1209 — 「这个 id 能不能建 track」的唯一答案，外加它可选的插件绑定。
/// 名字里的 "Admission" 是重点：它回答的是**准入**，不是「模板当前长什么样」——
/// 后者的权威是已播种的 report（§2.3 类别 2），不是这个结构体。
/// （v2 这里写的是「名字里不出现 Template」，下一行却叫 `TemplateAdmission`，
/// 自相矛盾；v3 改成陈述真正的意图。）
pub(crate) struct TemplateAdmission {
    pub key: &'static str,
    pub binding: Option<Manifest>,
}

pub(crate) async fn admit_template(s: &RouteState, id: &str) -> Option<TemplateAdmission> {
    let template = workflow_template(id)?;                 // 名册是唯一准入判据
    Some(TemplateAdmission {
        key: template.key,
        binding: resolve_trusted_workflow(s, id).await,    // 绑定是属性，不是准入
    })
}
```

注意三点：

1. **名册成员资格是准入判据，绑定不是。** 这一句是 `:779` 消失的全部原因。
2. `resolve_trusted_workflow` 一个字不改地活下来（`tracks.rs:937-950`），它仍然是
   「和 `bound_workflow` 同一个 filter」的绑定解析器（`tracks.rs:932-936` 的 doc 依旧成立），
   只是不再决定 create 是否放行。
3. **没有 `title` 字段。** v1 曾放一个 `title: &'static str`，两个通道都指出它是出厂标题的
   第二份拷贝：#1230 之后当前标题的权威是已播种 report（#1230 侧 `track_templates.rs` 的 `current_definition`），
   而 create 草图里根本没有消费者。删掉。读口需要标题时走 #1230 的 `current_definition`，
   不走这里。

### 2.3 统一后还剩几处权威（v2 重写）

v1 在这里写了「**2 处**可漂移权威，Rust 常量降级为 bootstrap、之后永远让位」。
两个通道都判它不成立，重扫后**判定成立、v1 错**。三条反证据，逐条读过：

* `restamp_template_report_if_placeholder` **无条件**先取常量报告（`tracks.rs:586-590`），
  才在 `:592-594` 早退；早退的判据是
  `report_startup_read_required()` = `summary != initial.summary || body != initial.body`
  （`crates/calm-types/src/track_report.rs:184-187`）——**精确相等**，不是「用户改过」。
  所以一份被改回 canonical placeholder 的已播种 report 会被常量**重新盖章**。
* #1230 S1 之后，每次 `PUT /api/track-templates/{id}` 都用 Rust intro + Rust renderer
  重写报告（#1230 侧 `track_templates.rs` 的 `PUT` handler：取 Rust intro/renderer，再落库）。
  intro 与 contract prefix 用户改不动。
* 报告形状契约（contract prefix，`crates/calm-types/src/track_report.rs:137-144`）
  **会**和已播种拷贝发散：改这个常量对任何非 placeholder 的已播种报告都不传播
  （同一条 `:592-594` 早退）。v1 说它和内容「同生共死」，错。

因此权威不是 2 类，而是**5 类**，其中 3 类会漂移：

| # | 类别 | 权威 | 会不会与别处发散 |
|---|---|---|---|
| 1 | **名册**（有哪几个 template） | Rust 常量 `WORKFLOW_TEMPLATES`（`workflow_templates.rs:25-38`） | 今天会：`WORKFLOW_TEMPLATE_KEYS`（`:18`）是第二份独立数组，`is_workflow_template_key`（`:40-42`）走它，而 create/读口走 `WORKFLOW_TEMPLATES`。**§2.2 把 KEYS 删掉、predicate 从 `WORKFLOW_TEMPLATES` 派生，这一类归零。** |
| 2 | **可编辑内容**（title + task 的 key/goal） | 已播种 template track 的 report 行 | 未播种时回落 Rust 常量（`workflow_templates.rs:44-51`、`:62-69`）。#1230 S1 让读口也认这一条，从而消掉「广告 vs 实际 fork」的漂移。 |
| 3 | **不可编辑内容**（intro + contract prefix） | Rust 常量（`workflow_templates.rs` 的 `*_INTRO`；`track_report.rs:137-144`） | **会**。常量改了不回灌已播种拷贝。已知、本次不修（§9 非目标 8）。 |
| 4 | **binding 声明**（哪个插件认领哪个 workflow id） | 插件 manifest（`plugins/git-forge/manifest.json:302-306`） | 不会：内核不复述。 |
| 5 | **binding 生效**（这次请求到底绑没绑上） | 运行态 × env 信任策略 | 不是静态权威，是运行时函数：`resolve_trusted_workflow` 要求 running（`tracks.rs:941-943`）∧ `trusted_forge_plugin`（`forge_trust.rs:1-8`，读 `NEIGE_TRUSTED_FORGE_PLUGINS` 环境变量）。同一份 manifest 在两台机器上可以给出不同答案。 |

`input_schema` 属于第 4 类：内核不可能知道第三方插件的输入 schema，schema 的校验方
和消费方是插件自己，抄进内核就是一个必然过期的副本（§6 展开）。

**本设计对这张表的贡献只有一条**：把类别 1 从「两份数组」压成一份。
类别 3 的漂移是既有的，本设计不改善也不恶化；类别 5 的运行时性不是缺陷，是
「信任是部署策略」的直接后果。**不再宣称任何「只剩 N 处权威」的口号。**

名册与内容分离是有意的：内容可编辑（#1230），名册不可（否则就是模板 CRUD，
那是另一个 epic，§9 非目标 1/2）。

---

## §3 §决策 D2（**v4 重开并推翻 v1–v3**）— 线上字段改名为 `template_id`

**结论：改名。`workflow_id` → `template_id`、`workflow_input` → `template_input`，
wire + 模型 + DB 列一起改，原 S2 并入本切片。旧拼写不留别名，直接 400。**

> **本文其余各节的拼写约定（读者须知）**：§1/§2/§4/§7 的现状描述、代码草图和错误矩阵
> **继续用 `workflow_id`**，因为它们描述的是**今天**以及 **PR-1 落地后**的状态——
> 那时字段还叫 `workflow_id`，只是它已经只有一个含义了（§10.1 的切片裁决）。
> **本节（§3）与 §10.2 的测试 #14/#15/#16 用新拼写。** 这不是遗漏，是分层：
> 概念统一与拼写更换是两个可以分别评审的事实，混着写会让下一个读者搞不清
> 哪一句在说今天、哪一句在说 PR-2 之后。

### 3.1 为什么推翻

v1 写「不改名」，理由是 #1209 正文那句「两个字段做同一件事比一道有记录的缝更糟」，
再加一句 C 方案「零行为、大 diff、与概念修复正交」。v2/v3 两轮都没有重新审这条。

**它的前提是「改名的代价是兼容性」，而这个前提今天被人取消了。**
把 v3 的三条路重估一遍：

| 方案 | v3 评价 | v4 重估 |
|---|---|---|
| A. 保持 `workflow_id` | 采纳，零迁移 | **驳回。** 「零迁移」是它唯一的优点，而人已经说迁移代价可以付。留下它就是留下 §1.5 `:29-39` 那道缝——**本 issue 的标的物就是那道缝**，把它写进注释不等于消灭它。 |
| B. 加 `template_id` 别名（两个字段都收） | 驳回 | **仍然驳回，理由不变且更强。** 统一恰恰消灭了需要两个名字的那个二义性；留一个 writeable 别名等于让写口重新有两条路。#1209 正文那句话原封不动成立。 |
| C. 改名（wire + 列 + 全体调用方） | 推迟到未排期的 S2 | **采纳。** 见 3.2–3.6。 |

**注意 B 与 C 的界线，因为下面 3.4 会用到一个看起来像 B 的东西**：
**「写口收两个字段」是 B，禁止；「历史事件日志按旧键名反序列化」不是 B**——
后者是单向的、只读的、作用在不可变历史记录上的兼容读，写口那侧一个字段都不多。

### 3.2 改名的调用方清单（**v5 重构：按「谁来抓」分三类，不按代码层分九层**）

> **v4 的这一节是本轮两个通道共同的 NEEDS-REVISION 头条。**
> v4 的九层表按**代码分层**组织，于是把「编译器能不能抓到」这件事散在九个格子的
> 「裁决」列里，读者拿不到「哪些站点不会有人替我发现」这张单子。
> 两个通道各自独立扫出 v4 漏掉的站点，**合起来 12 类**，其中一条是 BLOCKER（`today.rs`）。
> **v5 换成三分类，判据是「这个站点漏改了，谁会告诉我」。**

#### 可复现的规模数字（**OBSERVED**，2026-09-01，worktree `1209-template`，`0b4b022f`）

v4 写的「`grep -rln workflow_id crates/` = 170 个 Rust 文件」**复现不出来**
（通道 B 实测，v5 复跑确认）。可复现的是：

```sh
git grep -l 'workflow_id'    -- 'crates/'          | wc -l   # 173  (tracked files)
git grep -l 'workflow_id'    -- 'crates/**/*.rs'   | wc -l   # 168
git grep -l 'workflow_input' -- 'crates/'          | wc -l   # 165
git grep -l 'workflow_input' -- 'crates/**/*.rs'   | wc -l   # 162
git grep -l 'NewTrack {'      -- '*.rs'             | wc -l   # 147
```

**这些数字只用来说明「为什么要切成两个 PR」，不用来说明覆盖度。**
覆盖度由下面第 2 类的残留 grep 承担，不由计数承担。

#### 类别 1 — **语义站点**（要做判断，逐个读过）

漏改了 ⇒ 编译器报错，但**改法需要判断**，不能机械替换。

| 站点 | 判断内容 |
|---|---|
| `CreateTrackRequest.workflow_id`（`tracks.rs:210`）、`.workflow_input`（`:212-214`）、`deny_unknown_fields`（`:196`） | 旧拼写变未知字段 ⇒ 400（§3.5）；**绝不加别名** |
| `CreateTrackRequest::into_parts`（`tracks.rs:227-239`） | 跟改 |
| `NewTrack.workflow_id`（`crates/calm-truth/src/model.rs:108`）、`.workflow_input`（doc `:116-122`） | 改名 **+ 改 doc**（doc 里逐字写了旧名） |
| `calm_types::Track`（`crates/calm-types/src/model.rs:339` 定义，`workflow_id` 在 `:359`、`workflow_input` 在 `:370`，都带 `#[serde(default)]`） | **改名 + 加单向读别名**（§3.4）。它经 `TrackUpdatedPayload`（`crates/calm-types/src/event.rs:83`，`#[serde(flatten)]`）进历史事件——**全清单里唯一有 fail-open 危险的一格** |
| `crates/calm-server/src/plugin_host/workflow_input.rs` **整个模块**（模块名、模块 doc `:3`、`WORKFLOW_INPUT_MAX_BYTES`（`:27`）、`validate_workflow_input`（`:240`）；**26 处命中**，`grep -c workflow_input` 实测） | 见下面「用户可见错误词汇」 |
| `planner_harness_start_adapter.rs:162-180`（`bound_workflow`） | 跟改；fail-safe（`:181-190`）语义不变 |
| `mcp_server/tool_visibility.rs` | **不受影响**：真正的 gate 只读 `plugin_scope`（`:109`）。七处 `workflow_id` 命中 = 两条注释（`:18`、`:61`）+ 五处测试结构体字面量（`:200-209`、`:340-345`）。**两个通道本轮独立复核，判定 §5.1 的说法正确**——不要「顺手」改这里的 gate |

**用户可见错误词汇（本类里最容易漏的一格，v4 完全没记账）**：
`workflow_input` 出现在 `POST /api/tracks` 的 400 正文里——
`tracks.rs:965`（``track create: `workflow_input` requires `workflow_id` ``）、
`:974-975`（``does not declare an input_schema; `workflow_input` is not accepted``）、
`:987-988`（``requires `workflow_input` (required: [...])``）；
以及 `plugin_host/workflow_input.rs` 产出的 `workflow_input.<key>: …` 前缀
（`:247`、`:253`、`:264`、`:274`、`:278`），它经 `tracks.rs:992-993` 的
`track create: {reason}` 浮到线上——**正是 §4.4 矩阵行 10 的那条正文**。
所以 §4.4 里凡是写「统一后 = 同」的错误正文行，在 PR-2 之后**都不可能为真**；
§4.4 已按三列（今天 / PR-1 后 / PR-2 后）重排。

#### 类别 2 — **非类型检查站点**（编译器沉默，**必须靠残留 grep**）

**这一类是本设计真正的风险面。** 逐条给出「漏改了会发生什么」：

| 站点 | 漏改的后果 |
|---|---|
| **`crates/calm-server/src/routes/today.rs:149`（`UPDATE tracks SET … workflow_id=NULL … workflow_input=NULL …`）与 `:162`（`INSERT INTO tracks(… workflow_id, purpose, workflow_input …)`）** | **编译干净、clippy 绿，Today launchpad 路径运行期 `no such column`。见 §3.3，本轮 BLOCKER** |
| `TRACK_SELECT_COLUMNS`（`crates/calm-truth/src/db/rows.rs:87`）与 `TRACK_SELECT_COLUMNS_W`（`:94`） | 同上；**改这两个常量一次修好 10 个词法 SELECT**（清单见 §3.3） |
| `db/sqlite/track.rs:184` 的 INSERT 列表 | 同上 |
| 三组测试侧原始 SQL（§3.3 列全） | 测试运行期红（吵，不是生产风险，但会被误读成「改名把测试改坏了」） |
| **`web/src/api/wire.ts:96-106`** — `Omit<Schemas['CreateTrackRequest'], 'workflow_input'>`。**它是手写的，不是生成物**（不在 `ci.yml:1194` 的 `git diff --exit-code` 清单里，实测该清单是 `web/src/api/openapi.json`、`generated.ts`、`generated-terminal.ts`、`generated-events.ts`、`web/src/editor/types/`、`fe/core/api/generated/wire.ts`、`fe/core/api/generated/openapi.json` 七项） | `Omit` 的键是字符串字面量：改名后它**静默地不再 omit 任何东西**，那个 `workflow_input?: unknown` 覆盖退化成一个多余属性。**类型层的静默失败，不是编译错误** |
| **`web/src/track-fs-viewers/schemas.ts:152`（`workflow_id`）、`:160`（`workflow_input: z.unknown().default(null)`）** — **第三个 Zod 读取器**，读的是旧 `track.json` / FS snapshot | 机械改名 ⇒ 旧 snapshot 因 `.default(null)` **静默变成 `template_id=null`**——正是 §3.4 存在的理由。**必须走 §3.4 的 normalize 策略，不许机械改** |
| `docs/oracle/gates-types.yaml:1424`、**`docs/oracle/a11y-contract.yaml:596`**（把 `workflow_input.merge_policy` 写成 UI 契约）、**`docs/oracle/pages-shared.yaml:3542`**（「issue-dev 变体硬编码 workflow_id」）、**`:3586`/`:3590`**（`:3590` 说某测试用 `toEqual` 钉死**整个** create body，含 `workflow_input` 四个键） | oracle 判据与代码脱节；`:3590` 那条尤其——改名正好改的就是它钉的那个 body |
| `fe/e2e/track-create.spec.ts:57,59,60,141,142,143,154`（v4 只列了其中四个） | e2e 红（吵，但坐标不全会让实现者以为改完了） |
| **`crates/calm-server/tests/cases/track_projection_policy_patch.rs:155`** — 一份**字符串名册**（`"workflow_id"`、`"workflow_input"` 作为字面量列在数组里），不受字段类型保护 | 测试红或静默失去覆盖，取决于名册怎么用 |
| 只在**注释 / 文档 / CSS 类名**里的：`web/src/shared/components/issueUrl.ts:1,6,57` + `issueUrl.test.ts`、`fe/core/domain/issue-url.ts:2,48` + 其测试、`web/src/calm.css:4414`（`/* Raw workflow_input JSON escape hatch … */`）、`fe/web/src/features/area/README.md:71` | 没人会红。留下的是被行为打脸的注释——**§3.9 自己立的规矩要求同 PR 改掉** |
| **`NewTaskForm.tsx` 的用户可见文案**：v4 只列了 `:114`/`:458`；实测还有 `:124`（注释）、`:171`、`:369`、`:459`、`:569`（正文「the workflow_input is derived from it client-side」）、`:751`、**`:765`（可见文字 "Raw workflow_input JSON"）**、**`:769`（`aria-label="Raw workflow_input JSON"`）**、`:1062` | `:769` 的 aria-label 同时是 `a11y-contract.yaml` 与 `pages-shared.yaml` 的锚点 ⇒ oracle 与 UI 一起漂 |

**⇒ PR-2 的收尾门禁（这一条是本设计对「扫描完整性」的唯一真保证，见结尾的诚实标注）**：

```sh
# 在 PR-2 的最终树上跑。除 allowlist 外必须零输出。
git grep -n 'workflow_id\|workflow_input' -- . \
  ':!crates/calm-truth/migrations/00[0-7]*.sql' \
  ':!crates/calm-truth/src/db/sqlite/track_plugin_scope_migration_tests.rs' \
  ':!plugins/*/manifest.json' \
  ':!docs/architecture/1209-template-workflow-unify.md' \
  ':!docs/_1209-design-review-*.md'
```

**allowlist 的每一项都要有理由，否则它就是一张遮羞布**：

1. **旧迁移**（`0059`、`0061`、`0076` 等）——它们跑在**自己那个时间点的 schema** 上，
   改名迁移排在其后（§3.3）。改它们 = sqlx checksum 崩（CLAUDE.md「Never Edit Released Migration」）。
2. **`track_plugin_scope_migration_tests.rs`**——它**故意**停在 `0075` 的历史 schema 上建行
   （`:66` 的 `INSERT INTO tracks (… workflow_id …)`，注释 `:60-64` 自己写明了这一点）。
   那一行必须**保持旧名**：它构造的是改名迁移之前的世界。
3. **插件 manifest 的 `workflows[]`**——D4-A 之后不改名（§3.8 + §9 非目标 11）。
4. **内部 plugin-workflow 词汇**：`plugin_host/` 里描述「插件声明的 workflow」这个概念的
   标识符与文档（`WorkflowDescriptor`、`find_workflow_conflict`、`resolve_trusted_workflow`、
   `bound_workflow`）。它们指的是**插件那一侧**的东西，§3.8 划的界线保护它们。
   **注意**：`plugin_host/workflow_input.rs` **不**在这一项里——它产出的是**内核请求体字段**
   的错误词汇（类别 1 已点名）。
5. 本设计文档与评审档案自身。

**这条 grep 的正例/反例成对**：
把 `today.rs:149` 的 `workflow_id=NULL` 留成旧名 ⇒ **必须有输出**（今天它不在任何 allowlist 里）；
把 `0059_waves_workflow_id.sql` 留成旧名 ⇒ **零输出**（allowlist 第 1 项，正确行为）。

#### 类别 3 — **机械构造点**（编译器逐个报错，不需要判断）

`NewTrack { workflow_id: …, .. }` / `Track { … }` 这类结构体字面量填充位，
以及两个前端里由 TS 类型驱动的调用点。**147 个 `.rs` 文件含 `NewTrack {`**（上面的命令）。
高风险的共享 builder（漏改一个会让一批测试同时红，容易被误诊）：
`crates/calm-server/tests/support/codex_fixture.rs`、`support/mcp.rs`、`support/track_file.rs`、
`support/worker_flow.rs`、`crates/calm-truth-test-harness/src/lib.rs`。

**生成物**（不手改，跑生成器）：`crates/calm-server/src/openapi.rs:47`/`:164-166` 注册
`Track` / `NewTrack` / `CreateTrackRequest`，符号名不动，但产物会变。
产物 = `ci.yml:1194` 那七项里**实际命中本字段的五个**（实测 `grep -c`）：
`fe/core/api/generated/openapi.json`(10)、`fe/core/api/generated/wire.ts`(4)、
`web/src/api/openapi.json`(10)、`web/src/api/generated.ts`(11)、
`web/src/api/generated-events.ts`(4)。
`generated-terminal.ts`(0) 与 `editor/types/` 随全量生成命令一起被检查，但本字段不命中。
**v4 只点名了三个生成物**（人当初给的那三个），漏了 `fe/core/api/generated/wire.ts`
与 `web/src/api/generated-events.ts`——后者正是 `Track.workflow_input` 到 UI 的那条路
（`web/src/api/wire.ts:96-106` 的注释自己写着这件事）。
**本文不给生成物行号**（`355807d6` 刚动过它们，给了也过期）。

#### 这份清单的诚实标注（**v5 的关键一句**）

类别 1 与类别 3 由编译器兜底；**类别 2 没有任何东西兜底，除了那条残留 grep**。
本文列出的类别 2 站点是**四轮评审 + 三个独立扫描者**的并集，
**不能证明它是全集**——一份文档做不到这件事。
**PR-2 的真正保证是那条 grep + allowlist，不是这张表。**
表的作用是让实现者知道要往哪些方向想，以及让评审者知道 allowlist 的每一项凭什么在那里。
（这一条也是 §11 结尾「本设计钉不住的」新增的一项。）

### 3.3 DB 列：改，用新迁移

* **禁止编辑 `0059_waves_workflow_id.sql`**：sqlx 对整个迁移文件（含注释）做 checksum，
  改一个已应用的迁移会让启动直接 `VersionMismatch`（CLAUDE.md「Never Edit Released Migration」）。
* **新增一条迁移** `0079_waves_rename_workflow_id_to_template_id.sql`：
  `ALTER TABLE tracks RENAME COLUMN workflow_id TO template_id;`
  （`workflow_input → template_input` 同理，原列由 `0061_waves_workflow_input.sql:8` 建）。
  **号段实测**：目录里最新是 `0078_cards_role_assistant.sql`，`0079` 空闲，不撞号
  （两个通道独立复核一致）。**rebase 之后要复查撞号**——这是唯一一处允许在实现期定的坐标。
* **它不是 destructive**：没有数据丢失，是 forward-only 的（旧二进制读不了新 schema，
  这正是 `docs/upgrade-stability.md:14` 要求的形状）。因此
  `NEIGE_DB_MIGRATION_POLICY` 保持默认 `forwardOnly`
  （`docs/deploy-and-upgrade.md:76-78`），**不要**标 `destructive`——
  标了会让 `preflight.rs:220-227` 以一个**不对的理由**判 breaking。
  本次的 breaking 由 3.6 的两个版本常量承担。
* **旧迁移里对 `tracks.workflow_id` 的引用不用管**：`0076_waves_plugin_scope.sql:41,45,47`
  是在**它自己那个时间点的 schema** 上跑的，改名迁移排在它之后，重放顺序不变。
  **这一条必须在 PR 描述里写出来**，否则下一个评审会以为它是漏网。
* **旧迁移测试 fixture 也保持旧名**：`track_plugin_scope_migration_tests.rs:66` 的
  `INSERT INTO tracks (… workflow_id …)` 跑在**停在 `0075` 的 schema** 上
  （该文件 `:60-64` 的注释自己写明了这个构造），它在改名迁移之前，机械改名会把它改坏。
  §3.2 的 allowlist 第 2 项就是它。

#### ⚠️ 运行期风险：手写列名的 SQL 有**五处生产站点**，不是三处（**v5 更正，本轮 BLOCKER**）

改列名会让所有**手写列名字符串**的 SQL 在**运行时**才炸，不是编译期
（CLAUDE.md「Card Column Add SELECT Audit」的同一个形状）。
**v4 只列了三处，并把 `today.rs` 归进 §3.2 的「机械、编译器抓」——正是那条教训点名的误分类。**
两个通道各自独立抓到这一条，判定成立。实测五处：

| # | 站点 | 形状 | 谁会告诉我漏了 |
|---|---|---|---|
| 1 | `crates/calm-truth/src/db/rows.rs:87` `TRACK_SELECT_COLUMNS` | 共享列表常量 | 没人（运行期） |
| 2 | `crates/calm-truth/src/db/rows.rs:94` `TRACK_SELECT_COLUMNS_W`（`w.` 限定版；**v4 写的 `:95` 差一行**） | 共享列表常量 | 没人。另有 `track_select_columns_lists_agree`（`rows.rs:162` 起的 test mod）钉住两份列表一致 |
| 3 | `crates/calm-truth/src/db/sqlite/track.rs:184` INSERT 列表 | 字面 SQL | 没人 |
| 4 | **`crates/calm-server/src/routes/today.rs:149`**：`UPDATE tracks SET purpose='launchpad', workflow_id=NULL, plugin_scope=NULL, workflow_input=NULL, updated_at=?2 WHERE id=?1` | 字面 SQL | **没人。Today launchpad 的「复用旧行」腿运行期炸** |
| 5 | **`crates/calm-server/src/routes/today.rs:162`**：`INSERT INTO tracks(id,area_id,title,sort,lifecycle,workflow_id,purpose,workflow_input,created_at,updated_at) VALUES(…)` | 字面 SQL | **没人。Today launchpad 的「新建行」腿运行期炸** |

**改 #1/#2 两个常量一次修好 10 个词法 SELECT**（实测消费点，`grep -rn TRACK_SELECT_COLUMNS crates/`）：
`calm-truth`：`db/sqlite/read.rs:123`、`:133`、`:154`、`:301`（`_W` 版）、
`db/sqlite/track.rs:268`、`track_vcs/snapshot.rs:40`、`:266`；
`calm-server`：`routes/today.rs:134`、`:143`、`track_lifecycle.rs:141`。
**但 #3/#4/#5 三处原始字符串必须单独列、单独改**——它们不走那两个常量。

**测试侧的原始 SQL**（会大声红，不是生产风险，但列出来免得被误诊成「改名把测试搞坏了」）：
`crates/calm-server/tests/forge_workflow_e2e.rs:160`、`:176`、
`crates/calm-server/tests/support/planner_turn.rs:121`、
`crates/calm-server/src/operation/child_track_adapter.rs:1350`
（**在 `#[cfg(test)] mod tests` 内**，该 mod 自 `:499` 起——所以它是测试不是生产，
通道 B 把它列进「显式测试 SELECT」是对的）。
`track_plugin_scope_migration_tests.rs:66` **不在**这一类，见上面的 allowlist 理由。

**验收（§10.1 的 PR-2 侧 **B1/B2/B3**）——v4 的往返验收不够，因为它碰不到 `today.rs`**：

1. `POST /api/tracks {template_id:"small-change"}` → 201 → `GET /api/tracks/{id}` 回显
   ——覆盖 #1/#2/#3。
2. **Today launchpad 两条腿都要真跑**（通道 B 明确要求）：
   (a) **新建腿**——空库上打 Today launchpad 端点，走 `:162` 的 INSERT；
   (b) **复用腿**——先造出一个 `purpose IS NULL AND title='Today'` 的旧行，再打同一个端点，
   走 `:149` 的 UPDATE。**只测其中一条会漏掉另一条**——两条走的是两条不同的字面 SQL。
3. **迁移 fixture 测试**（通道 B 要求，v5 采纳，§10.2 测试 #17）：
   停在 `0078` → 写入两列**非 NULL** 的旧列值 → 应用 `0079` →
   断言**新列值逐字保留** ∧ **旧列不再存在**。
   它钉住的是「`RENAME COLUMN` 真的保值」，而不是「改名之后能跑」——
   后者被 1/2 覆盖，前者不被任何东西覆盖。

### 3.4 事件日志：唯一一处需要兼容读的地方

`Track`（`crates/calm-types/src/model.rs:339`）不只是 REST 响应，它**被整体嵌进持久事件载荷**
——`TrackUpdatedPayload`（`crates/calm-types/src/event.rs:83`）用 `#[serde(flatten)]`
把整个 track 摊在事件 data 的顶层（该结构体 `:78-79` 的 doc 明写这是为了
"preserve the historical wire shape"）。
`workflow_id`（`model.rs:359`）与 `workflow_input`（`:370`）上挂的都是 `#[serde(default)]`。

**读历史事件的生产代码，点名**（**v5 补足，通道 A m3，判定成立**）：
v4 在这里引的是 goldens 文件，那是**测试数据**，不是读取者——一条不变量必须指着**读代码**
才不空洞（CLAUDE.md「Vacuous Invariant Audit」）。真正的读取者是
`Event::from_kind_and_payload`，调用点在
**`crates/calm-truth/src/db/sqlite/events.rs:577`**，即 `events_since` 的追赶路径。
**而它的失败模式比 v4 说的更糟**：`Err` 分支（`:578-585`）只
`tracing::error!(… "events_since: skipping row that no longer matches Event enum")`
然后**跳过这一行**——既不中止追赶，也不让调用方知道少了什么。

于是有两条独立的坏路，方向相反：

* **缺键 + `#[serde(default)]` ⇒ 静默 `None`**：历史事件行里写着 `"workflow_id": "小改动"`，
  新代码找 `template_id` 找不到 ⇒ `default` ⇒ `None`。replay 出来的 track 丢掉模板归属，
  **无任何报错**。
* **若为了防上一条而把 `default` 去掉 ⇒ 硬反序列化错误 ⇒ 整行被跳过**（`events.rs:578-585`）。
  比丢一个字段更糟。

两条都撞 `docs/upgrade-stability.md:29` 的「不兼容时明确拒绝，不能部分工作或**静默丢字段**」。
`docs/oracle/gates-types.yaml:1424` 那条 gate 讲的是「新增字段要有默认值」，
**改名是它没覆盖的一种情况**。

**裁决**：`Track` 的新字段带 `#[serde(alias = "workflow_id")]`（`workflow_input` 同理），
`#[serde(default)]` **保留**（去掉它会踩上面第二条）。

* 它是**反序列化单向**的：序列化只出 `template_id`，wire 上仍然只有一个名字。
* **载体只有 `calm_types::Track` 一个**（**v5 更正，两个通道独立提出，判定成立**）：
  v4 的 §3.4 与 §10.0 item 4 写「`Track` / `TrackRow` 都加 alias」，
  **`TrackRow` 那一半是错的且不可编译**——`crates/calm-truth/src/db/rows.rs:99` 是
  `#[derive(Debug, sqlx::FromRow)]`，**没有 serde**，给它加 serde helper attribute
  轻则无效重则编译失败。而且**本来就无事可做**：`FromRow` 按**列名**绑定，
  §3.3 的迁移就地改了列名，`TrackRow` 上不存在遗留键问题。
  **该指令已删除**（§10.0 同步）。
* 它**绝不加在 `CreateTrackRequest` 上**——写口的旧拼写必须 400（3.5）。
  这条不对称是本设计的一个**判断**，不是发现：请求是一个有活人在另一端的契约，
  拒绝是可观测、可修的；事件日志是不可变的历史记录，拒绝它等于弄坏 replay。

**zod 侧是三个读取器，不是两个（v5 更正，通道 B 独立发现，判定成立）**：

| # | 文件 | 读什么 | 处置 |
|---|---|---|---|
| 1 | `fe/core/api/schemas.ts:97` | REST 响应 + 历史 event | 单向 normalize：吃旧键，产出新键 |
| 2 | `web/src/api/schemas.ts:127` | 同上（生产 bundle） | 同上 |
| 3 | **`web/src/track-fs-viewers/schemas.ts:152`（`workflow_id`）、`:160`（`workflow_input`）** | **旧 `track.json` / FS snapshot**——一批**磁盘上已经写死**的历史文件 | 同上。**这一处最危险**：`:160` 今天是 `z.unknown().default(null)`、`:152` 是 `.nullable().default(null)`，机械改名后旧 snapshot **静默变成 `template_id=null`**，一个错误都不报 |

**三处的 normalize 必须同形**：读入时先把旧键搬到新键（旧键存在且新键缺失才搬），
再交给现有 schema；**不要**把旧键做成 schema 的一个可选字段——那是写口方案 B 的前端版。

**测试 #14 因此是「Rust + 三个 parser」，不是 v4 写的「两个前端各一条」**（§10.2）。
* **oracle 要加一行**（不是改那行）：`gates-types.yaml` 现有那条只覆盖「后加字段要有默认值」；
  新增一条覆盖「**被改名的 wire 字段必须在两侧都留反序列化别名**」，
  `authoritative_test` 指向 §10.2 的新测试 #14。

### 3.5 旧拼写的拒绝策略

`CreateTrackRequest` 有 `#[serde(deny_unknown_fields)]`（`tracks.rs:196`），
所以改名之后 `{"workflow_id": "small-change"}` 会在 **serde 层**被拒，得到一个
**400**，正文是 serde 生成的 `unknown field \`workflow_id\``——
**不经过 `admit_template`，因此不带本文任何一条错误文案**。

**裁决：接受这个形状，不做定制错误。** 理由三条：

1. 人已经明说可以破坏兼容性，而这正是「明确拒绝、不部分工作」（`upgrade-stability.md:29`）；
2. 做定制文案意味着在 `CreateTrackRequest` 上把 `workflow_id` 声明成一个字段再手动拒——
   那就重新引入了「写口认识两个名字」，是方案 B 的分期付款版；
3. 真正需要被**及时**告知的不是某个手搓 curl 的人，而是浏览器里的旧 bundle，
   而那条路已经被 3.6 的 `WEB_COMPAT_VERSION` 硬挡住了，不会走到这个 400。

**代价，明写**：任何在仓外用 `workflow_id` 调 `POST /api/tracks` 的脚本会拿到一个
「字段名错了」而不是「概念改了」的错误。这是一个**判断**，不是发现。

### 3.6 让旧前端硬失败，而不是「部分工作」

这是本次改名**最重要**的一条，也是 v1–v3 从来没有考虑过的
（因为它们假定不改名，所以没有旧客户端问题）。

`crates/calm-server/src/routes/version.rs:21` 的 `WEB_COMPAT_VERSION = 16` 同时充当
`web_compat_version` 与 `min_web_compat_version`（`:47-48`），前端拿它做硬闸：
`web/src/app/providers.tsx:117,135,298` 与 `fe/web/src/app/providers/public.tsx:9,63,70`
在 `minWebCompatVersion > 本 bundle 的 WEB_COMPAT_VERSION` 时画一个「请刷新」的硬遮罩。

**裁决：`WEB_COMPAT_VERSION` 16 → 17，三处一起改**
（`crates/calm-server/src/routes/version.rs:21-22`、`web/src/api/version.ts:100`、
`fe/web/src/app/providers/public.tsx:9`——**恰好三处，没有第四处**，两个通道独立复核一致）。

> **⚠️ v5 必须撤回 v4 的用词：这三处之间今天没有任何 lockstep 门禁。**
> 通道 B 实测，v5 复核成立：
> * Rust 侧 `crates/calm-server/tests/cases/version.rs:148`、`:153` 是**字面量断言**
>   （`assert_eq!(v["webCompatVersion"], 16)` / `minWebCompatVersion == 16`），
>   它钉的是「服务端这个常量等于 16」，**看不见任何前端**；
> * `web/` 与 `fe/` 各自的测试引用的是**自己那份本地常量**，两边各自漂移仍然全绿。
>
> **所以 v4 把这一步叫「三处 lockstep 改」、把计划测试 #15 叫「三方 lockstep pin」，
> 两处都是名不副实。** #15 只能证明**服务端的 floor 抬高了**（`> 16`），
> **证明不了两个 bundle 都是 17**。

**三种漂移后果，逐条写出来（都不会被今天的 CI 发现）**：

| 漂移 | 后果 |
|---|---|
| server 留 16，两个 bundle 抬到 17 | 旧生产 bundle **不被挡**，继续发旧字段名、一路拿 400 ⇒ 正是 `upgrade-stability.md:29` 禁止的「部分工作」。**本裁决完全失效** |
| server 抬到 17，**任一** bundle 留 16 | 那个 bundle 即便是**刚发布的新版**也会永久显示「请刷新」硬遮罩——刷新也好不了，因为刷下来的还是 16 |
| **某一个** bundle 抬到 17，server 留 16 | 新 bundle 能过，**旧 16 bundle 也被放行**，继续「部分工作」 |

**⇒ PR-2 必须二选一（这是硬要求，不是加分项）**：

* **(a) 加一条 CI 静态门禁**，直接比较三处**导出值**：从
  `crates/calm-server/src/routes/version.rs` 抽出 `WEB_COMPAT_VERSION`，
  从 `web/src/api/version.ts` 与 `fe/web/src/app/providers/public.tsx` 各抽一个，
  三者必须相等。**正例/反例成对**：三处都是 17 ⇒ 绿；把其中**任意一处**改回 16 ⇒ 红
  （今天这个反例是绿的，这就是它必须存在的理由）；**或者**
* **(b) 从单一源生成两个前端常量**（派生优于测相等，CLAUDE.md「Mirror Code Must Call The Original」）
  ——这是更彻底的写法，代价是要给两个前端各接一段生成。

**本文推荐 (a)**：它更便宜，且本切片只抬一次版本号；(b) 值得做但属于另一个 issue 的形状。
**测试 #15 保留**（它钉的是「服务端 floor 真的抬了」这件独立的事），
但 §10.2 已把它的名字与描述改成诚实的版本。

效果：

* 缓存里的旧 bundle 拿到「请刷新」遮罩，**不会**发出一个注定 400 的 create——
  这才兑现了 `upgrade-stability.md:29`；
* 它同时是**机器判决**的一半：`min_web_compat_version(17) > installed.web_compat_version(16)`
  ⇒ `compatibility_breaks` 为真（`crates/neige-app/src/preflight.rs:295`）⇒
  `Verdict::Breaking { WireIncompatibility }`。**这是一个在仓内、被 CI 钉住的常量**，
  不依赖任何人在打包时记得设环境变量。

**并且 `API_VERSION` 也要动**：`routes/version.rs:19` 的 `API_VERSION = "1"` → `"2"`。
它的 doc 说自己「Diagnostic only」，但 `preflight.rs:290` **确实**拿它比对
（它是那九个 compatibility 字段之一）。REST 请求体的字段名换了，
把它留在 `"1"` 就是又一条被行为打脸的契约常量。
pin：`crates/calm-server/tests/cases/version.rs:19,131` 已经在断言它等于常量，
改常量不改测试不会假绿（那条测试是自反的）；真正的 pin 见 §10.2 的 #15。

**`SYNC_EVENT_VERSION` 要不要动？裁决：不动，并说明理由。**
它是 `crates/calm-types/src/event.rs:243` 的 `13`，描述的是**事件帧**的版本。
`Track` 的字段改名确实改了 `track_updated` 的载荷形状，但 3.4 的 `alias`
让新读者能读旧帧，而**旧读者读新帧**这条路已经被 `WEB_COMPAT_VERSION`
和「breaking ⇒ 全进程重启」（3.7）关掉了——没有一个旧读者能活到读新帧。
**这是一个判断，不是发现**；下一位评审若认为帧形状变了就该动，改它是一行 + 一批 golden。

### 3.7 这次改名的机器判决与 ops 后果

`compute_verdict`（`crates/neige-app/src/preflight.rs:204-227`）会因为 3.6 的
`min_web_compat_version` 判 `Breaking { WireIncompatibility }`；
§5.3 另外要求 `productMajor` 也动，那条会更早短路成
`Breaking { ProductMajorChanged }`（`:206-211`）。两条都指向同一个 ops 后果
（`docs/deploy-and-upgrade.md:242-243`）：

* `allowBreaking=false` ⇒ `400 result=rejected`，不落盘；
* `allowBreaking=true` ⇒ 换全部 symlink → `202 committed` → **杀掉 calm-server 与
  proc-supervisor 并 exec 自己**，两个进程都换 PID。不是 `preserving` 那种
  「换 server symlink + `/restart` + 60s 健康检查 + 失败自动回滚」。
* **注意丢掉了什么——v5 逐行读代码重写这一格，因为 v4 与第 4 轮评审在这里都不精确。**

> **第 4 轮的两个通道都要求「补强证据」，其中通道 A 的具体主张是
> 「`apply_breaking` 从不调 `backup_db`，连 `apply_preserving` 在 `apply.rs:287` 做的
> 自动预备份都没有」。v5 逐行读过，判定：`Verdict::Breaking` 没有 `requires_db_backup`
> 字段这一半**成立**；「breaking 不备份」这一半**不成立，驳回**。反证据：**
>
> ```rust
> // crates/neige-app/src/apply.rs:364  async fn apply_breaking(...)
> let backup = if units_changed.contains(&UnitName::CalmServer) {      // :375
>     Some(backup_db(cfg, supervisor, &manifest.release_id).await?)    // :376
> } else { None };
> ```
>
> 本次升级**一定**改 calm-server，所以这个分支**一定**走。
> 而 `backup_db`（`apply.rs:604`）本身是正确的：它**先 `supervisor.stop_and_wait()`**
> （`:619`）再 `backup_sqlite_files_sync`（`:663` 起：`atomic_copy_file` 主库 +
> 逐个复制 `wal` / `shm` 两个 sidecar），最后 `resume` + `wait_for_spawn`。
> 也就是说**产品自己已经做了一次一致的三件套备份**，落在
> `<data_dir>/backups/<release_id>/calm.db{,-wal,-shm}`。

**所以真正丢掉的是三件事，逐条给证据**：

1. **`dryRun` 会对操作者说谎。** `Verdict::Breaking` 的构造里没有 `requires_db_backup`
   这个字段（`preflight.rs:250-263` 只在 `Preserving` 那一支算它），
   于是 `VerdictSummary::from` 对 breaking **硬编码 `requires_db_backup: false`**
   （`preflight.rs:104`）。操作者按 `docs/deploy-and-upgrade.md:346` 的 pre-flight 第 1 条
   去读 `requiresDbBackup`，会读到 `false`——**而实际上会备份**。
   两个方向都别扭：它既没提醒你备份，又让你以为没有备份可回滚。
2. **`POST /upgrade/rollback` 拒绝回滚一次 breaking apply。** 函数就叫
   `rollback_last_preserving`（`apply.rs:1252`），它取历史里最后一条
   `result == "committed"` 且非 rollback 的记录（`:1259-1262`），
   然后 **`if last.verdict_kind != "preserving"` 直接拒**（`:1266`）。
   `docs/deploy-and-upgrade.md:271` 的措辞（"most recent committed non-rollback
   **preserving** apply"）与代码一致。
   **⇒ 备份文件在磁盘上，但没有任何 API 能把它放回去。**
3. **没有健康检查自动回滚。** `preserving` 那条路失败会
   「revert symlinks + restore DB backup + `/restart`」（`deploy-and-upgrade.md:241`）；
   breaking 是「换全部 symlink → 202 → 杀两个进程 → exec 自己」，中间没有健康检查窗口。

**⇒ 结论（比 v4 更准，也更可执行）**：不是「没有备份」，而是
**「有备份、无回滚路径」**。所以 `docs/deploy-and-upgrade.md` 要写的不是
「记得手动备份」这句废话，而是一份**手动恢复**的可执行步骤（§10.1 的 PR-2 文档条目），
外加把 `:26` 那句 `backups/<release_id>/calm.db{,-wal,-shm}   (one per preserving apply)`
的括注改对——**breaking apply 也会写一份**。
加上 §3.3 的 forward-only 迁移（旧二进制读不了新 schema），
**恢复 = 停 unit + 把三件套拷回去 + 把 symlink 指回旧 release**，三步都要写死。

**人已经接受这个后果**（原话见文首），前提是新 FE 尚未上生产。
**但上面「`web/` 是生产 bundle」这条是本文实测出来的，人未必知道**——
§11 的「仍需人裁」里点名。

### 3.8 `workflow_input` 叫什么

**裁决：一起改成 `template_input`。**

理由：它是 1 号字段的配对物。留一个 `template_id` + `workflow_input` 的组合，
就是在一个请求体里**原地重造一遍本 issue 要消灭的那道缝**——而且是更糟的版本，
因为这一次两个拼写指的是**同一个东西的两个方面**，而不是两个曾经真的不同的概念。

**反方向的论证，写出来再驳**：`workflow_input` 校验用的 schema 归插件所有
（`input_schema` 在 Manifest 顶层，§6 的 D5 不动它），所以「workflow」这个词
在那一侧仍然准确。**驳**：准确的是**插件那一侧**的词汇，不是**内核请求体**这一侧的。
请求体里那个字段回答的问题是「这个 template 需要的输入是什么」——
`admit_template` 已经把 binding 降级成 template 的属性了（D1），
输入自然也是 template 的属性。

> **【2026-09-02 更新 —— 本节以下的裁定已被 #1268 推翻，理由被前提取消】**
>
> 下面这一整段（以及 §5 里同源的那条）论证的是「**不**给插件 manifest 的
> `workflows[]` 改名」。它的**唯一**支撑是「改名会让每一份第三方 manifest
> 在解析期就炸」——即 Tier A schema 破坏。**用户随后确认：当前不存在任何
> 第三方插件，仓内只有 `plugins/git-forge/manifest.json` 一份 manifest 声明
> 了这个数组。** 那个代价的承受者是空集，于是这条论证的前提不成立，结论随之
> 失效——不是当时判错了，是前提后来被取消了。
>
> #1268 因此把该键改成了 `templates[]`（`WorkflowDescriptor` → `TemplateDescriptor`，
> `HostError::WorkflowConflict` → `TemplateConflict`），并且**把这次改名做成了
> 响亮失败而不是静默失败**：`Manifest::parse` 现在显式拒绝仍带 `workflows` 顶层键的
> manifest，错误里点名新键。原因正是下面这段的镜像——manifest 顶层容忍未知字段，
> 所以「不炸」的默认行为是**静默地不声明任何绑定**，`issue-development` 会悄悄失去
> `input_schema`。
>
> **保留下文原文，因为「它当时为什么是对的」值得留档**：只要外部插件重新出现，
> 下面这条推理就会再次生效，届时任何同类改名都要重新走一遍它。

**顺带划一条界线：插件 manifest 的 `workflows[]` 不改名。**

> **v5 重写本段的理由（通道 A 的裁定，判定成立）。** v4 给的理由是
> 「内核与第三方插件文件用两套词是**有文档的适配边界**，不是缝」——
> **那正好是人在 H2 里刚刚推翻过的那一招**（v1–v3 就是用「把缝写进注释」来保留缝的）。
> 结论对，但用这个理由撑它，等于邀请下一轮把它一起推翻。
> **决定性的理由是另一个，而且本文在 §5.3 已经把它备好了，只是没连起来。**

按 §5.3 已经划好的 **schema vs 接受语义** 那条线：

* **改 `workflows[]` 是 Tier A *schema* 破坏。** `docs/upgrade-stability.md:9` 把
  plugin manifest 明列为 Tier A 契约。`workflows` 这个键是 `Manifest` 的**解析字段**
  （`plugin_host/manifest.rs:93-100` 的字段文档，`WorkflowDescriptor` 在 `:467-475`）；
  改它的名字会让**每一份第三方 manifest 在解析期就炸**——不是「行为变了」，是「读不进来了」。
* **D4-A 只改*接受语义*，schema 一个字节不动**（§5.3 已经论证过这一点）：
  `workflows[].id` 照旧解析，变的只是 `POST /api/tracks` 还认不认一个名册外的 id。

**所以两者的代价不在一个量级**：改名买到的是词汇整洁，付出的是本设计一直刻意避开的
那一类契约破坏——而本切片已经在 REST 那一侧付过一次 Tier B 了（§3.5），
没有理由再叠一次 Tier A schema 破坏来换命名一致。

**并且把残留的命名债诚实记下来，不假装它不存在**（通道 A 明确要求）：
D4-A 之后 `workflows[]` 的**每一个合法值都是一个 template key**，
于是这个容器的名字变得很怪——它叫 workflows，装的全是 template。
**这是真实的（虽然便宜的）命名债**，归宿是 §9 非目标 11 + 一个跟进 issue，
**不是**一段读起来像敷衍的话。改它的正确时机是将来 §5.2 方案 C
（插件贡献 template）落地时——那时 schema 本来就要动，一次付清。

### 3.9 词汇缝记账段落的新文本

`routes/track_templates.rs:29-39` 那段（读口说 template、写口说 `workflow_id`）**整段删除**，
换成：

> 一个概念（template），一个字段（`template_id`）。读口列出它，写口按它准入，
> 没有第二个拼写。插件 manifest 里那个 `workflows[]` 数组是**另一方**写的文件，
> 它声明的是「本插件认领哪几个 template key」——那不是缝，是一个有文档的适配边界
> （见 `plugin_host/manifest.rs` 的字段文档）。

（这条是 CLAUDE.md「Statement Widened Past Carrier / Mirror Code」那类教训的直接应用：
留在代码里的契约注释一旦被行为改动打脸，就必须同一个 PR 里改掉，不能留成假绿。
v4 相对 v3 的区别是：v3 是把缝的描述**改写成仍然为真**，v4 是**缝没了所以整段删**。）

> **⚠️ v5 的时序更正（两个通道独立提出，判定成立）：上面这段新文本属于 PR-2，不是 PR-1。**
> v4 的 §10.1 把 `track_templates.rs` 的模块头改写排进 PR-1，
> 但新文本里写的是 `template_id`——而 **PR-1 落地后字段仍然叫 `workflow_id`**
> （v4 自己的切线定义）。照 v4 执行，PR-1 会交付一段**提前撒谎的契约注释**,
> 正是这一节自己立的规矩要禁的东西。
>
> **裁决：PR-1 落一段对 `workflow_id` 诚实的临时文本，PR-2 换成上面的最终文本。**
> PR-1 的临时文本（缝确实在 PR-1 就没了，所以这段仍然是「删缝」，只是拼写还没换）：
>
> > 一个概念（template），一个字段（`workflow_id`）。读口列出它，写口按它准入，
> > **准入判据只有「在不在名册里」**，没有第二条路。
> > （#1209 PR-2 会把这个字段改名为 `template_id`；缝在 PR-1 就已经没了，
> > 剩下的只是拼写。）
>
> **两段都为真、都不提前**。多花的成本是 PR-2 里再改一次这段注释——五行，
> 换掉「PR-1 交付一份假文档」这个代价，值。
> 替代方案「把整段模块头改写全部推到 PR-2」也可以接受，
> 代价是 PR-1 交付一个**缝已经没了但注释还在描述它**的中间态——那也是假绿，只是反方向。
> **本文选临时文本。**

`fe/web/src/features/area/new-track/public.tsx:38-45` 那段 FE 侧的缝注释同理，整段删。

---

## §4 §决策 D3 — create 路径的新形状

### 4.1 代码草图（PROPOSED）

分两块。**准入 + 绑定**替换 `tracks.rs:761-793`，留在原位（在任何 DB 写之前）：

```rust
// #1209 — 一次查找。template 是概念；插件绑定是它的属性。
let admission = match p.workflow_id.as_deref() {
    Some(id) => Some(admit_template(&s, id).await.ok_or_else(|| {
        CalmError::BadRequest(format!(
            "track create: `workflow_id` must reference a known track template; got `{id}`"
        ))
    })?),
    None => None,
};

// 绑定从准入结果上取，不再自己去 registry 里捞一次。
let bound_plugin = admission.as_ref().and_then(|a| a.binding.as_ref());

// #891 / #1110 S2 — 语义不变：只有绑定插件声明了 input_schema 才收 workflow_input。
validate_workflow_input_binding(bound_plugin, p.workflow_input.as_ref())?;

// #1110 S4 — 绑定的插件 id 抄进 plugin_scope（无绑定 ⇒ None）。
p.plugin_scope = bound_plugin.map(|m| m.id.clone());
```

**v2 删掉了 `if id.trim().is_empty()` 这道守卫**（今天在 `tracks.rs:770-772`）。
理由：新模型下空白 id 本来就落不进名册，走同一个 `ok_or_else` 得到同一个 400 状态码，
这个分支不可能改变任何结果；而 §4.3 正在取缔的东西就是「零效果的遗留特例」。
唯一的行为差是错误正文里回显的 id 是空白串——这本来就是它。
**代价**：今天没有任何 Rust 测试 POST 过空白 `workflow_id`（只有
`fe/e2e/track-create.spec.ts:55-60` 从 FE 侧断言「Blank 根本不发这个字段」），
所以删守卫等于删一段无人钉住的代码。§10.2 新增测试 #12 补上这个 pin。

**播种 + fork**（今天的 `tracks.rs:799-814`）**移到 `tracks.rs:867` 的 area 404 与
`:823-847` 的 cwd 校验之后**，即紧挨着 `let workspace_root = ...`（`:899`）之前：

```rust
// #1110 S6 — 选了 template ⇒ 幂等播种，并在调用方没给 fork_report_from 时 fork 它。
// #1209 — 位置在 area/cwd 校验之后，见 §4.2。
if let Some(admission) = &admission {
    ensure_workflow_templates(&s).await?;
    if fork_report_from.is_none() {
        fork_report_from = Some(
            lookup_workflow_template_track(&s, admission.key).await?.ok_or_else(|| {
                CalmError::Internal(format!(
                    "track create: seeded template `{}` is missing after ensure",
                    admission.key
                ))
            })?,
        );
    }
}
```

### 4.2 §决策 D3b — 把播种移到 4xx 校验之后（v2 新增）

今天的顺序是错的，而 v1 逐字保留了它。**OBSERVED**：`ensure_workflow_templates`
在 `tracks.rs:803` 被调用，早于 cwd 形状校验（`:823-828`）、attached 校验（`:843-847`）
和 area 404（`:863-867`）。于是
`POST /api/tracks {workflow_id:"small-change", area_id:"nope"}` 会先铸出 system area、
3 个 template track、3 份 report（`tracks.rs:448`、`:449-455`、`:517-579`），**然后 404**。
这和 handler 自己的契约注释直接冲突：`tracks.rs:759-760`
「All branches that surface a 4xx short-circuit before any DB write.」

**裁决：本切片移动，不记账。** 理由：搬一个 `if` 块，零新概念，而且不搬的话
`:759-760` 那句注释必须改成一句更弱的话——留一句被行为打脸的契约注释正是
CLAUDE.md「Mirror Code / Statement Widened」那类教训点名的假绿。

搬完之后**仍然不能说「任何非 201 都没有写」**，要诚实。事务内还有**三**类判定落在播种之后
（v2 只写了两类，通道 B 的 M2 补了第三类，重扫**判定成立**）：

* folder-claim **409**（`tracks.rs:889-897`、`:923-928`）；
* 事务内 **500**（DB/IO）；
* **显式 `fork_report_from` 的两条 400**（v2 漏了）：源 track 不存在
  （`tracks.rs:1410-1418`，消息在 `:1413`）与跨 area 且源不在 system area
  （`tracks.rs:1424-1430`，消息在 `:1428`）。整个 fork 分支从 `tracks.rs:1408` 开始，
  **在事务内**。所以「合法 template + 无效显式 fork」= 先播种、再 400。

所以 `:759-760` 的注释应改写为：

> 本 handler 在**开事务之前**能给出的每一个 4xx（cwd 形状、attached 校验、area 404、
> 未知 template、`workflow_input` 绑定矩阵）都在任何 DB 写之前短路。
> 事务内才判定的 **400（显式 fork 源不存在 / 跨 area）、409（folder claim）、500**
> 不在此列——那时模板已经播种，且播种是独立提交，回滚不掉。

**关于「要不要把显式 fork 源前移到播种之前校验」的裁决：不做，接受并记账。**
这是一个**判断**，不是发现。前移意味着在事务外再读一遍 `track_get` + 源 area 的 kind，
而权威判定必须留在事务内（否则是 TOCTOU）——于是就得到两份同判据的代码，
正是 CLAUDE.md「Mirror Code Must Call The Original」点名的形状。残余副作用的边界很小：
播种是幂等的（测试 #5），最坏情况是「用户用错的 fork 源触发了一次本来迟早会发生的播种」。
代价明写在 §4.4 的新行 17 与 §4.2 的注释里，不假装它不存在。

**另一条搬位的副作用，明写（NIT 级）**：`ensure_workflow_templates` → `ensure_system_area`
（`tracks.rs:448`、`:459-485`）今天会在 `:863` 的 `area_get` **之前**铸出 system area。
于是在一个全新库上、以 system area 为目标的 create，搬位后会从 201 翻成 404。
**实践中不可达**：`area_create_system_tx` 用 `new_id()`
（`crates/calm-truth/src/db/sqlite/area.rs:73`，`let id = new_id();`），id 猜不到；
而 `GET /api/areas`（`routes/areas.rs:190-208`）反正也会铸它。不为它写测试。

pin：§10.2 测试 #13（参数化到三条**事务前**的 4xx）。事务内那三类不在 #13 的范围内。

### 4.3 `:779` 为什么是真的消失了，而不是换个马甲（v2 重写判据）

原来的控制流是：**先问插件，插件说不认识，再补一句「可它是模板 key 啊」**。
新的控制流只问一次「它在名册里吗」，绑定作为属性一起返回。

v1 在这里给的是一条**语法**判据：「不存在任何一个分支的条件是 `resolve_trusted_workflow`
返回 None；它的返回值只被 `and_then`/`map` 消费，从不进入 `match` 守卫」。两个通道都指出
这条判据不成立也不管用：在 v1 的草图里它平凡为真（那个调用是结构体字面量的最后一个
表达式，根本没被组合子消费），而一个重新伪装的特例
（`if binding.is_none() && !roster.contains(id)`、或用 `?` / `filter` / `then_some` 改写的
等价控制流）照样满足 grep。**判定成立，v1 错。**

v2 换成**语义判据 + 路由级集合测试**：

> **语义判据**：`POST /api/tracks` 是否接受一个 `workflow_id`，只能是
> 「它在名册里吗」的函数；插件是否绑定、是否在跑、是否受信，都不得改变这个答案。

它的可执行版本不是 grep，而是 §10.2 的测试 #8 与 #9 组成的一对：

* #9（路由 × 路由，不引用任何常量）：`GET /api/track-templates` 列出的每个 id，
  `POST /api/tracks` 都**不以准入理由拒绝**（全称量化）；抽样的名册外 id，`POST` 拒绝。
  **v3 诚实标注**：反方向是抽样而非集合相等，见 §10.2「#9 的形状」。
* #8（真路由 + 真插件）：一个 running ∧ trusted 的插件声明了名册外的 workflow id，
  create 仍然 400。这一条正是「绑定不得影响准入」的正例。

`grep -n 'is_workflow_template_key' crates/calm-server/src/routes/tracks.rs` 返回空
仍然保留在 §10.1 的验收里，但降级为「名字确实没了」的**必要不充分**条件，
不再单独承担「没换马甲」这个判断。

### 4.4 错误分类矩阵（v2 重建）

v1 的矩阵有四类错误（两个通道合并后逐条重扫，**全部判定成立**）：把「今天没有 404」写死了、
把 `issue-development` 的 happy path 写成无条件 201、把状态码和错误正文混在一列、
把若干行的前提省略掉。v2 重建。

**`Result<_>` → HTTP 的映射**（`crates/calm-server/src/error.rs:182-199`）：
`NotFound`→404、`BadRequest`/`PluginInstall`→400、`Forbidden`/`PluginPermission`→403、
`Internal`/`Db`/`Io`/`Serde`/`CodexAppServer`→500。
（附带 OBSERVED：`create_track` 的 utoipa 注解只声明了 201 与 500，`tracks.rs:724-727`，
没有 400/404/409。这是既有的 OpenAPI 记账缺口，本设计不修，记在这里免得下一个人以为它是权威。）

**错误优先级树（v3 新增，通道 B 的 M1）**。v2 只给了「共享前提」，于是把
「area 不存在 ⇒ 一律 404，与 `workflow_id` 无关」写成了一句错话：
**workflow 准入排在 area 查找之前**（今天 `tracks.rs:761` vs `:863-867`；
统一后 §4.1 的草图 `admit_template` 仍在 `:761` 的位置）。
所以「未知 id + 不存在的 area」是 **400，不是 404**。

> **⚠️ v4 更正：v3 那棵「9 级单树」把事务内的顺序写反了，而且把 DB/IO 写成了固定层级。
> 通道 B M1，重扫判定成立、v3 错。** 两条错各自独立：
>
> * v3 把「显式 fork 校验」放在第 7、folder claim 放在第 8。**实际相反**：
>   `enforce_folder_claim_tx` 在 `tracks.rs:1391`（其上的注释明写它 "Must stay first"），
>   `track_create_tx` 在 `:1401-1402`，显式 fork 校验到 `:1408` 才开始（源不存在的 400 在
>   `:1410-1418`）。所以**「folder 冲突 + fork 源不存在」是 409，不是 v3 那棵树预测的 400**。
> * v3 把 DB/IO 写成固定的第 9 级。**假**：`track_create_tx`（`:1401`）自己的 DB 错误
>   就排在 fork 400 之前。DB/IO 不是一个层级，它是**每一个 `await?` 都可能发生的横切错误**。

> **⚠️ v5 再更正一层：不是两棵树，是**四**个阶段（通道 B M5，重扫判定成立、v4 漏了一整段）。**
> v4 说「真实控制流只有事务前 + 事务内」。**假。事务在 `tracks.rs:1609` 就提交了**，
> 之后还有两段**同步跑、能返回非 2xx、而 track/cards/events 已经落盘**的代码。
> 而且最前面还有一段：旧拼写的 400 发生在 **serde extractor 里，函数体还没开始执行**。

真实顺序，**四个阶段**（v5）：

**阶段 0 — serde/JSON extractor**（在 handler 函数体**之前**）：
`CreateTrackRequest` 的 `#[serde(deny_unknown_fields)]`（`tracks.rs:196`）在这里拒掉旧拼写
（矩阵行 18/19/20）。**它不经过本 handler 的任何一行代码**，所以本文任何一条错误文案都不适用
（§3.5）。v4 的两棵树把这一层完全省掉了，于是行 18–20 在树上无处可挂。

**阶段 1 — 事务前**（自上而下短路；§4.2 搬位之后，这几级全部在任何 DB 写之前）：

```
1. 模板准入               tracks.rs:761-784（统一后：admit_template）    → 400  unknown template
2. 输入绑定矩阵           tracks.rs:790 → :958-995                      → 400  五种正文
3. cwd 形状               tracks.rs:823-828                             → 400
4. attached 工作区        tracks.rs:843-847                             → 400
5. area 存在性            tracks.rs:863-867                             → 404
────────────────────────── 以上全部无 DB 写 ──────────────────────────
6. 模板播种 + 隐式 fork    搬到 tracks.rs:899 之前                        → 副作用从这里开始
```

**阶段 2 — 事务内**（`write_with_actor_events_typed` 的闭包，从 `tracks.rs:1391` 起。
**这里的每一步都在第 6 步之后**，所以任何一步失败都留下已播种的模板 track）：

```
T1. folder claim          tracks.rs:1391-1399（其上注释明写 "Must stay first"）      → 409
T2. track_create_tx        tracks.rs:1401-1402                                       → 视错误而定
T3. 显式 fork 源校验       tracks.rs:1408 起；源不存在 :1410-1418、跨 area :1424-1430  → 400
T4. 其后的 card / report 写入                                                       → 视错误而定
── 事务在 tracks.rs:1609 提交 ──────────────────────────────────────────────────────
```

**阶段 3 — 事务后**（**v5 新增；这一段整个是 v4 漏掉的**）：

```
P1. materialize_workspace   tracks.rs:1620-1633   → 非 2xx，而 track/cards/events 已提交
P2. planner-harness start      tracks.rs:1660-1676   → 运行期失败降级为 warn；
                                                    但提交前的序列化/提交仍可返回错误
```

**P1 的孤儿结果今天就被一条测试明确钉住**：
`crates/calm-server/tests/cases/track_workspace_materialize.rs:270-313`
（`materialize_failure_fails_the_create`）断言「物化失败 ⇒ 非 2xx」**并且**
「track 行活下来」（`:307-313` 的 `orphans.len() == 1`），
其注释 `:293-306` 逐字写明这是**有意钉住的已知状态**，并写着
"Do not \"fix\" a failure here by loosening the assertion"。
**⇒ 「非 201 ⇒ 无副作用」这句话在本 handler 上永远不可能为真**，不只是「播种搬位之后仍不完全」。
`tracks.rs:759-760` 那句注释的改写文本（§4.2）因此还要再加一句：事务提交后的物化失败
同样返回非 2xx 且不回滚。

**横切、不属于任何一个阶段**：generic DB / IO 错误（`CalmError::Db` / `Io` ⇒ 500，
`error.rs:182-199`）**可能出现在任何一个可失败的 DB/FS 操作上**。
**v5 更正 v4 的措辞（通道 B，判定成立）**：v4 写「任何 `await?`」——**载体不对**，两个方向都错：
`materialize_workspace`（`tracks.rs:1620`）是一个**同步的 `?`**，不是 `await?`；
而 `resolve_trusted_workflow(&s, id).await`（`tracks.rs:937-950`）**根本没有 `?`**，它返回 `Option`。
正确的说法是「**任何可失败的 DB / FS 操作**」。

**矩阵行 17 仍然成立**：它的前提里已经有「无并发的 folder-claim 冲突」（下面共享前提 5），
在那个前提下 T1 不触发，T3 的 400 就是被观察到的结果。

**统一前后这四个阶段的形状都不变**，唯一的变化是阶段 1 里第 6 步的位置——
**v4 更正（B/M1 的尾巴 + 通道 A n1，两个通道独立提出）**：v3 说它今天在「第 1 步之后」，
**实测是「第 2 步之后」**——`validate_workflow_input_binding` 在 `tracks.rs:790`，
播种块在 `:799-814`。搬位是把它从「第 2 步之后」挪到「第 5 步之后」。

**全表共享的前提**（写一次，不再逐行重复）。它们是「为了让某一行只考察一个变量」而钉住的
其它输入，**不是**对优先级的断言——优先级看上面那棵树：

1. `area_id` 指向一个存在的 area（除非行内另说）。**这条前提的作用是让第 5 步不触发**；
   它**不**意味着 area 404 优先于 workflow 400。（v2 在这里写「一律 404，与
   `workflow_id` 无关」，**是错的**，通道 B M1 判定成立。）
2. `cwd` 要么省略，要么是通过 `:823-828` 形状校验与 `:843-847` attached 校验的绝对路径；
   否则 400（树的第 3/4 步）。
3. 请求体其余字段合法（`deny_unknown_fields`，`tracks.rs:196`）。
4. 未显式给 `fork_report_from`（除行 17 外）。显式给了就永远赢（`tracks.rs:804`，pin：
   `track_workflow_templates.rs:383`）——所有「+ fork 模板」的行都以此为前提。
5. 无并发的 folder-claim 冲突（树的第 8 步，与本设计正交）。
6. **插件态前提，逐行显式给**（v2 有几行省略了它，通道 B 逐行核时点名行 3/9/10）：
   任何一行只要没写明插件，就默认「**没有任何 running ∧ trusted 的插件认领本行的 id**」
   ——包括「一个插件都没注册」这个退化情形，它和「注册了但没 running / 没 trusted」
   在 `resolve_trusted_workflow`（`tracks.rs:937-950`）眼里是同一个 `None`。

> **⚠️ v5 把「统一后」这一列拆成两列（通道 B M5 的尾巴，判定成立）。**
> v4 的矩阵只有「今天 / 统一后」两组列，而 v4 同时又把切片切成了两个 PR，
> 于是「统一后」在同一张表里同时指 **PR-1 之后**（概念统一、字段仍叫 `workflow_id`）
> 和 **PR-2 之后**（拼写终态）。后果很具体：**行 6/8/9/10/11 的「统一后 = 同」在终态是假的**
> ——那几条正文里嵌着 `workflow_input` / `workflow_id` 这两个字面串（§3.2 类别 1 已点名它们的
> 产生处：`tracks.rs:965`/`:974-975`/`:987-988` 与 `plugin_host/workflow_input.rs`）。

**PR-2 那一列的通用规则**（写一次，表里只标例外）：
**凡是响应正文里出现 `workflow_id` / `workflow_input` 这两个字面串的，
在 PR-2 之后一律变成 `template_id` / `template_input`；状态码与判定顺序一个都不变。**
这条规则本身也是 §10.2 测试 #16 参数化要覆盖的面。

| # | 输入（在上述前提下） | 今天 状态码 | 今天 响应正文 | **PR-1 后** 状态码 | **PR-1 后** 响应正文 | **PR-2 后**（拼写终态） | 变化 |
|---|---|---|---|---|---|---|---|
| 1 | 无 `workflow_id`，无 `workflow_input` | 201（无 fork，`plugin_scope=null`） | — | 同 | — | 同（字段名换，无正文） | — |
| 2 | `workflow_id: "   "`（空白） | 400（`tracks.rs:770-772`） | `…must reference a registered trusted workflow; got \`   \`` | 400（名册未命中） | ``track create: `workflow_id` must reference a known track template; got `   ` `` | 同，但字段名变 `template_id`（§10.3 三条腿不受影响） | **正文变两次**（守卫也删了，§4.1） |
| 3 | `workflow_id: "missing-workflow"`（前提 6：无插件认领它） | 400（`tracks.rs:780`） | 同上文案 | 400 + `…known track template` | 同上 | 同，字段名变 | **正文变两次** |
| 4 | `small-change` / `investigation`（无插件绑定），无 input | 201 + fork | — | 同（名册命中，binding=None） | — | 同 | — |
| 5 | `issue-development` + git-forge running∧trusted，**带合法 `workflow_input`** | 201 + fork + `plugin_scope` | — | 同 | — | 同（请求体键名变 `template_input`） | — |
| 6 | `issue-development` + git-forge running∧trusted，**不带 input** | **400**（`tracks.rs:977-990`；git-forge 的 schema `required` 非空，`manifest.json:299`） | ``…plugin `dev.neige.git-forge` requires `workflow_input` (required: [...])`` | 同 | 同 | **正文变**：``requires `template_input` ``（产生处 `tracks.rs:987-988`） | **PR-2 正文变** |
| 7 | `issue-development` + git-forge **stopped 或 untrusted**，不带 input | 201 + fork，`plugin_scope=null` | — | 同（名册命中，binding=None） | — | 同 | — |
| 8 | `issue-development` + git-forge stopped/untrusted，**带 input** | 400（`tracks.rs:962-967`，走的是 `None`-plugin 臂） | ``track create: `workflow_input` requires `workflow_id` `` | 同 | 同（文案仍误导，见下） | **正文变**：``\`template_input\` requires \`template_id\` ``（产生处 `tracks.rs:965`）；**误导性不变**，见下 | **PR-2 正文变** |
| 9 | **名册内 template**，其绑定插件 running∧trusted 但**无 `input_schema`**，带了 input | 400（`tracks.rs:973-976`） | ``…does not declare an input_schema; `workflow_input` is not accepted`` | 同 | 同 | **正文变**：``…`template_input` is not accepted``（产生处 `tracks.rs:974-975`）。**注意 `input_schema` 这个词不变**——它是插件那一侧的字段（§3.8 的界线） | **PR-2 正文变** |
| 10 | **名册内 template**，其绑定插件 running∧trusted 且有 schema，`workflow_input` 违反该 schema | 400（`tracks.rs:992-993`） | `track create: <reason>`，其中 `<reason>` 形如 ``workflow_input.<key>: …`` | 同 | 同 | **正文变**：`<reason>` 变成 ``template_input.<key>: …``。**产生处不在 `tracks.rs`，在 `plugin_host/workflow_input.rs:247/:253/:264/:274/:278`** —— 这一格是 v4 整条漏掉的那个模块浮上线的地方 | **PR-2 正文变**（且要改另一个模块） |
| 11 | 有 `workflow_input` 但无 `workflow_id` | 400（`tracks.rs:962-967`） | ``…requires `workflow_id` `` | 同 | 同 | **正文变**：``\`template_input\` requires \`template_id\` `` | **PR-2 正文变** |
| 12 | `area_id` 不存在，`workflow_id` 是**名册内**的 key（若 id 不在名册里，按优先级树先得 400，见上） | 404（`tracks.rs:863-867`），**且模板已被播种**（`:803` 在 `:863` 之前） | ``area `<id>` `` | 404，**且未播种**（§4.2 搬位） | 同 | 同（正文不含本字段） | **副作用变**（v1 整行缺失） |
| 12a | 名册内 key + **`cwd` 不是绝对路径** | 400（`tracks.rs:823-828`），**且已播种** | ``…`cwd` must be absolute…`` | 400，**且未播种** | 同 | 同 | **副作用变**（v2 缺此行） |
| 12b | 名册内 key + **显式给了一个不是 git 仓库的 `cwd`**（v4 更正措辞，见下） | 400（`tracks.rs:843-847` → `validate_attached_workspace`），**且已播种** | 因 `validate_attached_workspace` 而异 | 400，**且未播种** | 同 | 同 | **副作用变**（v2 缺此行） |
| 13 | 名册命中但 `ensure` 后 lookup 不到那个 track | 500（`tracks.rs:807-811`） | ``…seeded template `X` is missing after ensure`` | 同 | 同 | 同（正文不含本字段） | — |
| 14 | `ensure_workflow_templates` 内部失败（建 area / 建 track / 落 report） | 该错误**原样上抛**（`tracks.rs:803` 的 `?`），可能是 500 也可能已留下部分播种副作用 | 因错而异 | 同 | 同 | 同 | — |
| 15 | **running ∧ trusted 插件声明了名册外的 workflow id**，且（无 required input 或给了合法 input） | **201**，`plugin_scope` 打上，**无 fork** | — | **400** + `…known track template` | — | 同，字段名变 | **有意变更 A**（PR-1），见 §5 |
| 16 | 同上，但插件 schema required 非空且没给 input | 400（`tracks.rs:977-990`） | ``…requires `workflow_input` `` | 400 | `…known track template` | 同，字段名变 | 状态码不变，**拒绝理由与正文变**（更早在准入处拒） |
| 17 | **破前提 4**：名册内 key + **显式** `fork_report_from` 指向不存在的 track（或跨 area 且源不在 system area） | 400（**阶段 2 事务内**：`tracks.rs:1410-1418` / `:1424-1430`），**且模板已被播种** | ``…fork source track `X` does not exist`` / ``…must be in the target area or the system area`` | 同 | 同 | 同（正文不含本字段） | **无变化**（搬位改不掉它：判定在事务内、播种在事务外，见 §4.2 的裁决） |
| **P1** | **（v5 新增）** 任何合法 create，但工作区物化失败 | **非 2xx**，**且 track / cards / events 已提交**（`tracks.rs:1620-1633`，在 `:1609` 提交之后） | 含 `materialize workspace` | 同 | 同 | 同 | **无变化**；本行的作用是钉住「非 201 ⇒ 无副作用」**永远为假**。已有 pin：`track_workspace_materialize.rs:270-313` |

**PR-2 新增的三行（D2 改名带来的，见 §3；它们发生在**阶段 0**，不经过 handler 函数体）**：

| # | 输入（在上述前提下） | 今天 / PR-1 后 | **PR-2 后** | 变化 |
|---|---|---|---|---|
| 18 | 请求体用**旧拼写** `workflow_id: "small-change"` | 201 + fork | **400**，serde 的 ``unknown field `workflow_id` ``（`deny_unknown_fields`，`tracks.rs:196`），**不经过 `admit_template`，不含本文任何一条文案** | **有意变更 C**，见 §3.5 |
| 19 | 请求体用旧拼写 `workflow_input` + 新拼写 `template_id` | 视绑定而定 | **400**，同上，``unknown field `workflow_input` `` | **有意变更 C** |
| 20 | 请求体同时给 `template_id` 与 `workflow_id` | — | **400**，同上（`workflow_id` 仍是未知字段）。**这一行的作用是钉住「写口只认识一个名字」**，即 §3.1 表里被驳回的方案 B 没有从后门溜进来 | **新** |

**v4 对行 12b 措辞的更正（通道 A n2，重扫判定成立）**：v3 把这个 400 记在
`attach_folder` 头上，**是错的**。`tracks.rs:843` 的守卫是 `if !cwd_omitted`，
**只看 `cwd` 给没给，完全不看 `attach_folder`**（实测 `sed -n '843,847p'`）。
所以任何显式的、通过了 `:823-828` 绝对路径形状校验、但目标不是 git 仓库的 `cwd`
都会 400，与 `attach_folder` 无关。§10.2 #13 的第 3 条腿同步更正。

**本次一共两处有意变更，逐条点名**（v4：变成三处，C 是新的）：

* **变更 A（行 15/16）**：非模板 workflow id 从「可创建（201）」变成「不可创建（400）」。
  行 16 提醒：这不是一个干净的 201→400，而是一个混合面——它的一部分今天已经 400，
  只是理由不同。§5 展开。
* **变更 B（行 2/3/15/16）**：未知/空白 id 的 400 **正文**从
  `must reference a registered trusted workflow` 改为 `must reference a known track template`。
  这不是润色：旧文案陈述的是「插件注册表」这个已经不再是准入判据的东西。
  两个既有测试钉住了旧子串：
  `crates/calm-server/tests/cases/track_workflow_templates.rs:586` 与
  **`crates/calm-server/tests/forge_workflow_e2e.rs:427`**（后者 v1 的文件清单里没有，§10.1 已补）。
  处置见 §10.3。
* **变更 C（行 18/19/20，v4 新增）**：请求体字段改名（§3）。旧拼写变成未知字段 ⇒ 400。
  它同时把变更 B 的落点也改了一次：新文案里那句话现在应该说
  ``\`template_id\` must reference a known track template``。§10.3 的三条腿据此更新。

**行 8/11 的文案，明写一条已知的烂**：``workflow_input` requires `workflow_id`` 在行 8
里是误导的——调用方**确实**发了 `workflow_id`，真实原因是「这个 template 此刻没有绑定」。
§6 把「binding: None ⇒ 永不接受输入」定成上限之后，这条文案会成为一个长期错误提示。
**裁决：本切片不改**（它不在 `:779` 的判据链上，且改它会再打红一批断言），
记为后续 issue 的候选。这是一个**判断**，不是发现；下一位评审若不同意，改它约 5 行。

---

## §5 §决策 D4 — 插件声明了但不是模板的 workflow（本 issue 最锋利的一叉）

> **【2026-09-02 更新 —— 本节的「不改插件侧拼写」那一半已由 #1268 取消】**
>
> 本节 §5.3 把「动插件 manifest 契约」定级为一次**公开插件契约破坏**，并以此
> 支撑 D4-A（只改接受语义、schema 一字节不动）。**D4-A 本身仍然成立**：名册外的
> id 依旧不可创建。被取消的只是**「所以那个数组也不能改名」这一条推论**——
> 用户确认当前没有任何外部插件，破坏的承受者是空集。#1268 已把
> `workflows[]` 改名为 `templates[]`，并加了一条显式拒绝旧键的解析期守卫。
> 下文原文保留，作为「在有外部插件的世界里，这条推理是对的」的留档。


### 5.1 这个路径今天有人走吗

v1 给了 4 个站点，两个通道各自重扫后都指出扫描不完整（结论不变、证据少报）。
**判定成立**。v2 给全表。

扫描命令：`rg -n 'workflows:|"workflows"' crates/ plugins/`（**OBSERVED**，
2026-09-01，worktree `1209-template`，基线 `6e0339b0`；`0b4b022f` 未碰这些文件，
表内坐标在新基线上仍成立）。命中 25 行，去掉
`manifest.rs` 自身的字段定义/校验/文档行后，**每一个构造带 `workflows` 的 Manifest 的站点**：

| 站点 | 声明的 workflow id | 是模板 key | 走 `POST /api/tracks`？ |
|---|---|---|---|
| `plugins/git-forge/manifest.json:302-306` | `issue-development` | ✓ | ✓（唯一的真实插件） |
| `crates/calm-server/src/routes/tracks.rs:3588` | `issue-development` | ✓ | ✗ 直接单测 `validate_workflow_input_binding` |
| `crates/calm-server/tests/cases/track_templates_read.rs:107` | `issue-development` | ✓ | ✗ 只读 `GET /api/track-templates` |
| `crates/calm-truth/src/db/sqlite/track_plugin_scope_migration_tests.rs:78-84` | `issue-development`（4 种畸形 JSON） | ✓ | ✗ 迁移回填单测 |
| `crates/calm-server/src/plugin_host/mod.rs:2373` | 参数化 | 视传入 | ✗ spawn 准入单测 |
| `crates/calm-server/src/plugin_host/manifest.rs:1388`、`:1409-1413`、`:1557` | `issue-development` / 参数化 | — | ✗ manifest 解析单测 |
| `crates/calm-server/src/plugin_host/manifest.rs:2246`、`:2312` | `wf.build` | ✗ | ✗ manifest 解析单测 |
| `crates/calm-server/src/mcp_server/tool_visibility.rs:375-377` | `WORKFLOW_ID`（非模板） | ✗ | ✗ 工具可见性单测 |
| `crates/calm-server/src/operation/planner_harness_start_adapter.rs:1829-1831` | `WORKFLOW_ID`（非模板） | ✗ | ✗ `bound_workflow` 解析单测 |
| `crates/calm-server/src/operation/child_track_adapter.rs:1980` | `[]`（空） | — | ✗ |
| `crates/calm-server/tests/cases/mcp_plugin_tools.rs:924-926` | `WORKFLOW_ID`（非模板） | ✗ | ✗ 见下 |
| `crates/calm-server/tests/plugin_workflow_uniqueness.rs:350-352` | `SHARED_WORKFLOW_ID`（非模板） | ✗ | ✗ spawn 准入 |
| `crates/calm-server/tests/codex_forge_e2e.rs:2834` | 断言 shipped manifest 的 workflows | ✓ | ✗ |

唯一一处把**非模板** `workflow_id` 落进 track 行的是
`crates/calm-server/tests/cases/mcp_plugin_tools.rs:671-682` —— `repo.track_create(NewTrack{..})`
结构体字面量，注释自己写着 `// Direct repo create (route validation is out of scope here).`
（`:670`）。绕过路由，本设计不影响它。

FE / e2e / oracle 侧无消费者：`fe/e2e/track-create.spec.ts:141` 用 `small-change`；
`docs/oracle/gates-types.yaml:1424` 只断言 `track.workflow_id → null` 的默认值。

**结论（OBSERVED）**：通过 HTTP 用非模板 workflow id 建 track 这条路，
**在本仓内**没有任何生产代码、任何插件、任何测试在走。
**这句话的边界，明写**：它只覆盖 checked-in 的东西，不覆盖运行时装进来的第三方插件——
见 §5.3 的重新定级。

另有两条下游确认它的消失不会波及别处：

* `bound_workflow`（`planner_harness_start_adapter.rs:162-180`）读的是 `tracks.workflow_id`
  这一列。收紧 create 之后这列只可能装模板 key；老数据里若有别的值，
  `bound_workflow` 的行为完全不变（它自己去 registry 解析，解析不到就 fail-safe 回
  vanilla prompt，`:181-190`）。**不需要数据迁移。**
* MCP per-track tool scope 读的是 `tracks.plugin_scope`（`mcp_server/tool_visibility.rs:109`），
  **不读 `workflow_id`**。不受影响。（v1 引的 `:114-128` 那段看不出字段来源。）

### 5.2 三个选项

| | 方案 | 结果 |
|---|---|---|
| **A（采纳）** | 拒绝：不在名册里就是 400 | 「binding 是 template 的属性」这句话真的成立。插件不能凭空造出一个可创建的东西。 |
| B | 保留：视作「无 report 可 fork 的退化 template」 | 这就是 `:779` 换马甲。它要求内核维持「可绑定但不是模板」这个类别——而这个类别正是本 issue 要消灭的二元性。而且它创建出的 track 没有 report 可 fork，等于一个既非模板又非空白的第三种出生方式。 |
| C | 升格：插件声明的 workflow 变成插件贡献的 template（进名册） | 方向上正确，但**今天做不了**：`WorkflowDescriptor` 只有 `id`（`manifest.rs:472-475`），没有 title，picker 无从展示；也没有 tasks/report，fork 无从谈起。真实成本见下。 |

**v1 在 C 这一格发明了一个不存在的障碍，删除。** v1 写「要做必须先扩 manifest schema
（unknown-field 严格解析 + `manifest.rs:763` 的字段白名单）」。事实恰好相反（**OBSERVED**）：

* Manifest 顶层**容忍**未知字段（`plugin_host/manifest.rs:15-20` 的 doc：
  「Unknown fields are tolerated (forwards compatibility)」）。
* `WorkflowDescriptor` 明写「Extra JSON keys are ignored」（`manifest.rs:467-475`），
  且有测试专门钉住这一点（`manifest.rs:1407-1416`
  `extra_workflow_descriptor_fields_are_ignored`，把 `plan_template`/`gates`/
  `planner_instructions`/`card_kinds`/`input_schema` 塞进去仍解析成功）。
* `manifest.rs:761-765` 不是字段白名单，是「connector-only 插件不得声明 `workflows`」
  这条校验（错误信息 `cannot own a track workflow`）。

也就是说**解析器这一侧对 C 是敞开的**，加字段是向后兼容的。C 的真实成本在别处：
① `WorkflowDescriptor` 要长出 title / tasks / intro 三类内容，各自需要一个权威归属决定
（插件常量？可编辑副本？——直接撞上 §2.3 的类别 2/3）；② picker 展示与 i18n；
③ 播种/重播种/插件卸载后那些已 fork 的 track 怎么办的生命周期；
④ 名册从编译期常量变成运行时集合之后，`GET /api/track-templates` 的稳定性语义。
这四条才是把 C 推成独立 epic 的原因。

**采纳 A**，并且 A 与 C 前向兼容：将来 C 落地时，名册从
「`WORKFLOW_TEMPLATES` 常量」变成「常量 ∪ 插件贡献」，而 A 的拒绝理由那句话
（「不在名册里」）**一个字都不用改**。这正是把准入判据从「绑定」搬到「名册」的红利。

### 5.3 代价，明写（v2 重新定级：这是一次**公开插件契约破坏**）

采纳 A 之后，一个第三方受信插件**无法**再通过声明一个新 workflow id 让用户建出绑定它的
track。v1 把这条风险定为「低」，理由是「今天无人使用 + 白名单极窄」。
两个通道里 B 指出这个推理不成立，**判定成立，v1 错**，理由是两条读出来的事实：

* **manifest 契约公开承诺了这个能力**：`plugin_host/manifest.rs:93-100` 的字段文档写着
  「Trusted forge plugins may declare workflow ids. Track create binds `workflow_id` to one
  of these so the kernel can copy the owning plugin into `plugin_scope` and validate
  `workflow_input` against this Manifest's `input_schema`」。这段话里**没有一个字**说
  「必须是三个内置模板 key 之一」。任何按文档写插件的人都会认为自己可以起新 id。
* **「受信」不是内置白名单**：`trusted_forge_plugin`（`crates/calm-server/src/forge_trust.rs:1-8`）
  读 `NEIGE_TRUSTED_FORGE_PLUGINS` 环境变量，逗号分隔任意 id，默认值只是
  `dev.neige.git-forge`。部署方可以把任何插件列进去。所以「白名单极窄」是**默认配置**的
  性质，不是**不变量**。

因此 §5.1 的仓内扫描只支撑「**本仓**无消费者」，不支撑「兼容风险低」。
重新定级为 **Tier：公开插件契约破坏（breaking）**，缓解措施是硬要求，不是加分项：

1. **改 manifest 的字段文档**（`manifest.rs:93-100`）——同一个 PR 里改。
   留一段被行为打脸的契约注释就是假绿（同 §3 末段的理由）。

**v3 把 2/3 从散文变成具名产物**（通道 A J5、通道 B M6 都指出 v2 的 2/3 没有落点：
仓内**没有** `CHANGELOG`、**没有** `docs/release*`，且 `grep -n plugin docs/deploy-and-upgrade.md`
**零输出**——重扫**判定成立**）：

2. **落点 = `docs/deploy-and-upgrade.md`，新增一节「插件兼容性」**，并把该文件写进
   §10.1 的 S1 文件清单。这是仓内唯一的升级载体；今天它一个字都没提插件，
   所以这一节是**新建**，不是追加到已有插件章节。内容两句话：
   > #1209 起，受信插件在 manifest 的 `workflows[].id` 里声明的 id **必须**是内核
   > template 名册里的 key（今天是 `issue-development` / `small-change` / `investigation`）。
   > 声明名册外的 id 不再让 `POST /api/tracks` 建出绑定它的 track——该请求返回 400
   > `track create: \`template_id\` must reference a known track template`。
   >
   > **⚠️ v5 的 PR 归属更正（两个通道独立提出，判定成立）**：上面这段引的是 **PR-2** 的正文。
   > v4 却把 `docs/deploy-and-upgrade.md` 排进 **PR-1** 的文件集，
   > 于是 PR-1 会发布一份描述着它并不产生的错误消息的升级文档
   > ——而且 **PR-1 单独落地的判决是 `preserving`**，这一节隐含的 breaking 姿态也不成立。
   > **裁决：整节移到 PR-2**（§10.1 的文件表已改）。
   > PR-1 在 `docs/` 下**不留任何东西**，它的插件契约变更只落在
   > `manifest.rs:93-100` 的字段文档里——那处在 PR-1 就已经为真（D4-A 在 PR-1 生效）。
   > 若评审更希望 PR-1 也有升级说明，可以在 PR-1 写一段**只讲 D4-A、不引任何字段拼写、
   > 不讲备份**的短文，PR-2 再补全——**本文推荐整节留在 PR-2**，理由是拆成两半会让
   > 同一个小节在两个 PR 里各改一次，评审成本大于收益。

3'. **升级前/回滚的备份姿态自成一节，不能挂在「插件兼容性」下面**
   （**v5 新增，通道 B M6，判定成立**）。v4 只在这一节里留了一句
   「同一节还要写升级前手动备份要求」——**一句占位符，且落点是错的**：
   备份与插件兼容性毫无关系，它是 §3.7 那条「有备份、无回滚路径」的后果。
   **落点应当是 `docs/deploy-and-upgrade.md:344` 的
   「## 8. Pre-flight checklist before applying to production」**，
   在**第 4 条（`allowBreaking: true`）之前**插入一个自己的小节。内容三段，都要可执行：

   1. **产品会自动备份，但你回滚不了**（§3.7 的三条证据）。所以 pre-flight 要先确认
      `<data_dir>/backups/` 可写、盘上有空间，并**记下当前 release id**（`GET /api/version`）。
   2. **手工的额外备份怎么做（不要 `cp` 三件套）**：SQLite 的 `calm.db` / `-wal` / `-shm`
      在**服务在跑的时候**不是一致的三个文件。两条正确路径，二选一：
      * **在线备份**（推荐，不停服）：`sqlite3 <data_dir>/calm.db ".backup '<dest>/calm.db'"`
        ——`.backup` 走 SQLite 的 online backup API，产出一个**单文件**的一致快照，
        不需要也不应该复制 `-wal` / `-shm`；
      * **停服后复制三件套**：与产品自己 `backup_db` 的做法一致
        （`crates/neige-app/src/apply.rs:604` 先 `stop_and_wait`，`:663` 起复制主库 + 两个 sidecar）。
      **禁止的做法**：服务在跑的时候直接 `cp calm.db calm.db-wal calm.db-shm`——
      三个文件不是同一时刻的，恢复出来可能是坏库。
   3. **怎么恢复**（因为 `POST /upgrade/rollback` 会拒绝一次 breaking apply，§3.7 第 2 条）：
      停 systemd unit → 把备份放回 `<data_dir>/calm.db`（若是 `.backup` 单文件，
      **同时删掉现存的 `-wal` / `-shm`**）→ 把 `current-*` symlink 指回旧 release → 起 unit。
      三步都要写出实际命令。

   顺带**改正一处既有的文档错误**：`docs/deploy-and-upgrade.md:26` 把
   `backups/<release_id>/calm.db{,-wal,-shm}` 括注成 "(one per **preserving** apply)"，
   而 `apply_breaking`（`apply.rs:375-376`）在 calm-server 变更时**同样**写一份。
3. **升级前扫描命令，内联写死**（升级说明里照抄即可，不依赖 `jq` 之外的东西）：
   ```sh
   # 在插件安装根目录下跑；有输出 = 该插件会被 #1209 打断
   for m in <plugins_dir>/*/manifest.json; do
     [ -f "$m" ] || continue      # v4：插件根目录可以不存在 / 为空，见下
     jq -r --argjson roster '["issue-development","small-change","investigation"]' \
       '(.workflows // [])[].id | select(. as $i | $roster | index($i) | not)
        | "\(input_filename): \(.)"' "$m"
   done
   ```
   **`[ -f "$m" ] || continue` 是必须的，不是防御性编程**（通道 B n4，重扫判定成立）：
   registry 明确容忍插件根目录不存在并按空注册表继续
   （`crates/calm-server/src/plugin_host/registry.rs:118-130`，doc 明写
   "If `dir` doesn't exist, returns an empty registry without erroring"），
   而 POSIX glob 在零匹配时会把**字面路径**交给 `jq`，于是这条本该输出「无影响」的命令
   反而报错退出。**正例/反例成对**：插件根目录有 3 个 manifest、其中 1 个声明名册外 id
   ⇒ 恰好 1 行输出；插件根目录不存在或为空 ⇒ **零输出、退出码 0**（v3 版在这一格是报错）。

**`productMajor` 要不要动？v3 裁决「不动」，v4 推翻——但推翻它的是人，不是本文。**

**先把 v3 错在哪写清楚**（通道 B M2，重扫判定成立）。v3 的论证是
「`deploy-and-upgrade.md:242` 的 breaking 三条判据本变更一条不占」。
问题在于它同时还说本变更是「公开插件契约破坏（breaking）」，于是文档自称 breaking、
机器判 `Preserving`——**同一个词在同一份设计里指两件相反的事**。而机器那一侧是有定义的：

* `docs/upgrade-stability.md:9` 把 **plugin manifest 明列为 Tier A 契约**；
* `crates/neige-app/src/manifest.rs:24` 把 `product_major` 定义为
  "Whole-product compatibility major"；
* `crates/neige-app/src/preflight.rs:118` 把 `Preserving` 定义为
  "Target can be applied without breaking live Tier A/B contracts"；
* 而 `compute_verdict`（`preflight.rs:204-227`）实际只看 `productMajor`、
  九个 compatibility 字段（`compatibility_breaks`，`:287-296`）、以及 destructive migration。

**顺带记一条本文原本欠着的分辨**（它是这个问题里唯一真正微妙的地方）：
Tier A 说的「plugin manifest」，是 manifest 的**schema**（内核会解析哪些字段），
还是内核对 manifest 所声明内容的**接受语义**？**#1209 只改后者**——
manifest schema 一个字节没动，`WorkflowDescriptor` 照旧解析 `workflows[].id`
（`plugin_host/manifest.rs:467-475`），变的只是 `POST /api/tracks` 还认不认一个
名册外的 id。这个分辨**支持**「Tier D / 撤回定级」那一支：
`upgrade-stability.md:41` 的 Tier D 明写包括「第三方 app 表达面」，
而「插件声明一个 id、内核据此放行 create」正是一种表达面。

**但本文不走那一支。裁决理由，五条**：

1. **人已经裁了**：「我觉得这里你可以破坏兼容性……我希望你尽可能保持一致」。
   「保持一致」在这里有一个精确的含义——**让机器判决与文档说法一致**，
   而不是反过来把文档说法改到迁就机器。
2. 走 Tier D 那一支需要「明确标记为实验能力」（`upgrade-stability.md:43`：
   「实验性能力必须明确标记，消费者必须容忍删除或破坏性变化」）。
   **今天没有任何这样的标记**——`manifest.rs:93-100` 的字段文档反过来把它写成一个承诺
   （§5.3 开头的两条证据）。**事后追认一段没有标记过的能力为实验性，就是用定义消灭发现。**
3. §3 的 D2 改名本身就把 REST 请求体改了，那是**无争议的 Tier B 破坏**。
   即便插件那一格能被辩成 Tier D，本次升级也照样是 breaking。所以这里没有省下什么。
4. 走 breaking 路线的代价是**一次**升级要 `allowBreaking=true`，不是「每次升级都被拒」——
   v3 说「把它撞上去会让每一次升级都被 `allowBreaking=false` 拒掉」，**那句话是错的**：
   `compute_verdict` 比的是 target 与 **installed** 的 `product_major`
   （`preflight.rs:206`），装上去之后 installed 也变成新值（`installed.rs:48`），
   下一次升级就回到 `preserving`。
5. 三条 breaking 判据里，本次真正占的是 **wire incompat**（§3.6），
   `productMajor` 是在它之上再加一道**意图声明**。两条都做，理由见下。

**⚠️ 实现指令，必须照做，因为「bump productMajor」按字面读是一条空指令。**
实测：仓内**没有任何 `productMajor` 常量可改**——
`crates/neige-app/src/package.rs:302-310` 的 `product_major()` 读环境变量
`NEIGE_PRODUCT_MAJOR`，**未设时硬编码返回 `Ok(0)`**（`:307`）。
也就是说，如果只在升级说明里写一句「打包时记得设 `NEIGE_PRODUCT_MAJOR=1`」，
那么一旦有人忘了，机器判决照样是 `Preserving`，本裁决**完全不产生任何后果**——
一条典型的空洞不变量（CLAUDE.md「Vacuous Invariant Audit」）。所以要**两件都做**：

* **(a) 代码事实**：把 `package.rs:307` 的 `Err(std::env::VarError::NotPresent) => Ok(0)`
  改成 `=> Ok(1)`。它的读代码就是 `package.rs:132`（`product_major: product_major()?`）。

> **⚠️ v5 更正：pin 只有一条，不是两条（两个通道独立提出，v5 逐行复核，判定成立、v4 错）。**
>
> * **`package.rs:546` 的 `assert_eq!(manifest.product_major, 0)` 是真 pin。**
>   它所在的 `package_directory_contains_v2_manifest_and_hashes`（`package.rs:506`）
>   用 **`with_env_removed("NEIGE_PRODUCT_MAJOR", …)`（`package.rs:508`，v4 写的 `:507`
>   差一行）** 包住整个函数体，所以它**真的**走了 `package.rs:307` 的默认值分支。
>   把默认值改成 `Ok(1)` ⇒ 这条**立刻红**。
> * **`manifest.rs:302` 的 `assert_eq!(v2.product_major, 0)` 不是 pin，是空的。**
>   它所在的 `v2_manifest_parses_with_per_crate_units`（`manifest.rs:271`）解析的是一段
>   **自带 `"productMajor": 0` 的硬编码字节串**（`manifest.rs:275`）——
>   它是一个纯 serde parser fixture，**从不调用 `product_major()`**，
>   默认值改成 1 它**照样绿**。
>
> **v4 说「两个方向都关上了」，那句话一半是假的**——正是本文在别处（§10.3、§9 的假门禁那几条）
> 反复取缔的形状：给一条论证配一个检测不到回退的断言。
> **v5 的表述：package smoke（`package.rs:546`）是本裁决的 pin，单数。**
> `manifest.rs:302` 的 fixture 值要不要顺手改成 1，是实现期的整洁问题，
> **不是默认值正确性的必要条件**，也不许被算作门禁。
* **(b) ops 指令**：升级说明里仍然写 `NEIGE_PRODUCT_MAJOR` 的存在与含义
  （`docs/deploy-and-upgrade.md:72` 已经写了 override 机制），
  但**不要**把「记得设它」当成本次的保障手段。

**ops 后果，明写，不埋**（`docs/deploy-and-upgrade.md:242-243`）：
本次升级的判决是 `breaking`，`allowBreaking=false` 时 `400 result=rejected` 不落盘；
`allowBreaking=true` 时换全部 symlink、`202 committed`、然后
**杀掉 calm-server 与 proc-supervisor 并 exec 自己**——两个进程都换 PID，
且**没有 `preserving` 路径那套健康检查自动回滚**。
配上 §3.3 的 forward-only 迁移，**回滚只剩「升级前备份」这一条**
（`docs/upgrade-stability.md:19`）。
**人在「新 FE 尚未上生产」的前提下接受了这个后果**；
本文实测出来的一条人未必知道的事实是 **`web/` 才是今天在跑的 bundle**（§1.6），
记在 §11 的「仍需人裁」里。
4. **（推荐，非阻塞）spawn 准入处 warn**：插件 spawn 时若声明了名册外的 workflow id，
   打一条 `tracing::warn!`。落点是 `plugin_host/mod.rs:1114-1119` 那个已经在做
   workflow 冲突检查的原子准入块——那里已经拿到 manifest 和 registry，加一条日志约 10 行，
   不改任何准入结果。**这条要不要做留给人裁**：它让 `plugin_host` 依赖 `workflow_templates`
   的名册，是一个新的模块方向依赖。

**过渡期「先警告后拒绝」**：考虑过，**不采纳**。它要求内核在一整个版本里维持
「可绑定但不是模板」这个类别活着——而这个类别正是本 issue 要消灭的东西（=方案 B 的
分期付款版）。既然仓内零消费者、且第三方消费者只能靠 (2)(3) 触达，不值得为它保留一个
概念上的第三态。**这是一个判断，不是发现。**

---

## §6 §决策 D5 — 输入参数的所有权

**结论：`input_schema` 继续由插件 manifest 拥有，读口 join，template 不自带参数声明。**

> **v4 复查：人的新约束（可以破坏兼容性 + 尽可能一致）不影响 D5，理由要写清楚。**
> D5 回答的是**所有权**问题，而所有权与「能不能破坏兼容」无关——把 schema 抄进内核
> 在任何兼容性预算下都是同一个错误（复述而不是调用原件）。
> **唯一受 §3.8 影响的是拼写**：请求体那一侧的字段改叫 `template_input`，
> 而插件 manifest 那一侧仍叫 `input_schema`、仍挂在 Manifest 顶层。
> **这不是新缝**：`input_schema` 从来就不是 `workflow_input` 的镜像拼写
> （一个是 schema、一个是 value；一个是插件的字段、一个是内核请求体的字段），
> §3.8 划的那条界线（内核 API 内部 vs 内核↔第三方文件）在这里同样适用。

理由：

1. schema 的**校验方和消费方**是插件（`validate_workflow_input_binding` 只是把
   manifest 里的 schema 拿来跑 `validate_workflow_input`，`tracks.rs:992-993`）。
   内核复述一份 schema 就是 CLAUDE.md「Mirror Code Must Call The Original」那条教训的原型。
2. 复述会新增第 3 处权威（§2.3 的下限被打破）。
3. 读口今天就是这么写的（`track_templates.rs:109-111`），而且模块头
   （`track_templates.rs:11-14`）已经论证过：读口和写口用**同一个** `resolve_trusted_workflow`。

**这条保证的精确范围（v3 收窄，通道 B m4，重扫判定成立）**：v2 说「广告了 schema 但
create 拒收在**结构上不可能**发生」，**太强**。`resolve_trusted_workflow` 每次调用都
**重新采样**运行态与信任态（`tracks.rs:941-943` 现取 `running_plugin_ids()`），
所以 GET 之后、POST 之前把插件停掉，就恰好产生这个错配。
既有测试自己就演示了停机后 schema 消失（`tests/cases/track_templates_read.rs:260-264`
的注释 + `stop`）。正确的表述是：

> **同一个运行态快照内**，读口与写口用同一个判据，因此不存在「判据不同」造成的漂移。
> 跨请求的运行态变化（插件在 GET 与 POST 之间 stop / 被移出信任名单）**会**产生
> 「picker 广告了 schema、create 拒收 input」——这是**已接受的竞态**，
> 不是判据漂移，其后果是一个 400，不是错误的 track。本设计不消除它
> （消除它需要把运行态快照钉进一次会话，属于另一个 epic）。

被否决的替代方案：**template 自己声明参数（Rust 侧）**。除上面三条外还有一个硬伤——
`issue-development` 的 `input_schema` 有 `enum` / `default` / `additionalProperties`
（`plugins/git-forge/manifest.json:283-300`），把它搬进 Rust 就是把插件的私有契约冻进内核二进制。

**这条决策的已知上限，明写**：`binding: None` 的 template **永远不能收输入**。
`small-change` 想要一个 "branch name" 参数？做不到——除非给它绑一个插件，或者将来引入
「template 自带参数 + 无消费方」的第三种形态。本设计不引入。
（§4.4 行 8 记了这条上限的副作用：那种情况下用户看到的错误文案会长期误导。）

**第二个插件出现时会怎样**（#1209 的触发条件之一）：结构上零变化。
`find_workflow_conflict`（§1.3）保证一个 key 最多一个插件；
`admit_template` 里的 `resolve_trusted_workflow` 天然按 key 找到那唯一一个。
真正需要动的只有 `trusted_forge_plugin`（`forge_trust.rs:1-8`）这个信任策略——与本设计正交。

---

## §7 §决策 D6 — 播种的不对称性保留

**保留。** 写触发播种，读不触发。

v1 在这里写的不变量量化到了「每一个端点」，却打算用「两个 GET 前后数 overlay」来钉它。
两个通道都判它钉不住，**判定成立，v1 错**，证据两条：

* #1230 S1 的那条断言只从**未播种**状态调用两个 GET，然后检查 overlay 仍为空
  （#1230 侧 `tests/cases/track_workflow_templates.rs` 的那条 read-only case）；
* 它用的 helper `seeded_templates` 只枚举 `kind == "template"` 的 overlay
  （`tests/cases/track_workflow_templates.rs:168-184`，本 worktree 与 1230 侧同一段），
  **不看 track 行、不看 card、不看 report 内容**。

于是这些生产改动全都能溜过去：给**别的**读路由（`GET /api/areas`、track detail）加
`ensure_workflow_templates`；GET 建一个 system area 但不建 overlay；GET 改写已播种的
report/title/doc_rev；GET 建不带 overlay 的 track；只在「已经播种」那条分支里写。

**v2 把不变量收窄到测试真能守住的范围，并把测试加强到能守住它。**

> **INV-1209-SEED（v3）——「这两条读路由不得物化 template 播种状态」**：
> 对 `GET /api/track-templates` 与 `GET /api/track-templates/{id}` 中的任意一条，
> 一次请求前后，以下快照必须逐字节相等：
> areas 全表、tracks 全表、cards 全表、**overlay 全表（不再只筛 `kind=="template"`）**、
> **`area_folders` 全表**、**`events` 全表的 `(count, max(id))`**，
> 以及三个模板 track 的 report payload（或其 `doc_rev`）。
> 起始状态：**A 未播种**、**B 已播种且 report 可读**，各断言一次。

**名字为什么从「读不触发播种」改成「不得物化 template 播种状态」**（通道 B m1，
重扫**判定成立**）：v2 引用 `track_templates.rs:20-22` 的「一次*读*不能触发写」并当成
本不变量的表述，但快照钉不住那句话——它既不看 `events`，也不看 `area_folders`，
也不看非 template overlay。而 `log_pure_event`（`crates/calm-truth/src/db/mod.rs:683`，
doc 从 `:669` 起，明写「the event itself is the only write」）说明**一行 event 本身就是一次写**：
在 GET 里追加一条 pure event，v2 版 #10 全绿，却已经违反了「a read stays a read」。
v3 的处置是**两头都收**：把 `events`/`area_folders`/全部 overlay 纳入快照（成本几乎为零，
都是同一个 `sqlx` helper 多两条 `SELECT`），**同时**把不变量改名到它真正守得住的范围。
**仍然不宣称**「这两条 GET 完全不写库」——那是一句更宽的话，需要 DB 层的写拦截，本切片不建。

三处收窄，逐条说明为什么：

1. **只说这两条路由，不说「所有端点」。** 「任何端点都不许播种」是一个全称否定，
   要么给 fail-closed 的路由扫描，要么就不要写（CLAUDE.md「Vacuous Invariant Audit」）。
   本切片不建路由扫描门禁——那是给一行代码建一套基础设施。
   **代价明写**：往别的读路由里加 `ensure_workflow_templates` 这件事，本设计钉不住，
   只能靠评审。
2. **快照而不是计数。** 计数只否掉「多出来几行」，否不掉「改写已有内容」。
3. **两种起始状态。** 只测未播种的话，「只在已播种分支里写」这类改动是绿的。

**第三种起始状态 C（已播种但 report 读不出来）——不加，改为对 §8.1b 下硬裁决**
（通道 A m2，重扫**判定成立**：这是文档自己打开的活岔路，不是假想）。
若 #1230 用「读时修复」（`resolve_report_for_track` 失败就用常量重新盖章）来关掉 F13 的洞，
那么一次 GET 就写了库，而 #10 在 A、B 两态下**依然全绿**。
两条出路里 v3 选后者，理由是前者要在测试里制造一个「overlay 在、report 载荷损坏」的
人造状态，而那个状态的**唯一**合法产生方式在 #1230 那边、还没定形：
* **裁决**：§8.1b 必须朝 **500（上抛）** 方向落地——这是 #1209/#1230 的**合流硬前提**，
  见 §8.1b 的 v3 版本。只要它上抛，状态 C 在两条 GET 上就是一个 500，
  不可能是一次静默的写，#10 缺这一态就无害。
* ~~**代价明写**：若 #1230 的作者坚持「读时修复」，则 **#10 必须补状态 C**，
  且本设计的「读不物化播种状态」这条不变量要重新论证。~~
  **✅ v5：这条分支作废。** 上游已经把 `current_definition` 改成 `?` 上抛
  （§8.1b 的 CLOSED 方框，实测于 `1230-s1@3b9cc03c`），状态 C 在两条 GET 上就是一个 500，
  不可能是一次静默的写。**#10 不需要状态 C，本不变量不需要重新论证。**

依据：`track_templates.rs:20-22` 已经把「一次读不能引发写」写成契约；
#1230 S1 的模块头（`track_templates.rs` 的 "Editable templates (#1230)" 章节）重申并把它
落在 `current_definition` 上（`lookup` miss 就是「未播种」，绝不 ensure）。
本设计不动这条契约，只把它的测试加强到配得上它。落点见 §10.2 测试 #10。

为什么不对称是对的（而不是需要修的丑）：播种要写 3 个 track + 3 份 report + 可能还要建
system area（循环在 `tracks.rs:449-455`；建 area 在 `:459-485`；每个模板的建 track + 落 report
在 `:517-579`）。让任何人打开 New track 对话框就产生 6 行以上写入，
既不可撤销，也让「从未用过模板的库」和「用过的库」在 DB 上不再可区分。

---

## §8 与 #1230 S1 并行落地

#1230 S1 = worktree `.claude/worktrees/1230-s1` 的 `b93fb767`（+1414/−53，尚无 PR）。

> **⚠️ v5 基线声明（放在这里而不是藏在 80 行外的文首注记里，通道 A n2）**：
> 本节的**结构性结论**（哪些文件相撞、什么形状、按什么规则合并）在
> `b93fb767` / `7b85caa3` / `d51571d7` / **`3b9cc03c`（本轮实测的 HEAD）** 四个基线上都复核过，
> **仍然成立**。
> **但本节此后不含任何 `1230-s1` 行号**——那条分支在四轮评审里动了四次，
> 记行号只会把过期坐标洗进设计（文首「v5 的记账纪律」）。
> 另：`3b9cc03c` 已经是 S1 + S2 合并后的形态（"选择器读 seeded report + PUT 写口 +
> Templates 二级导航"），所以下文说「#1230 S1」时指的是**那条分支当时的全部内容**，
> 不是某一个提交。**#1230 S2 对 `fe/` 的影响本文仍未评估**（§8.3 末行）。

**两条 v1 的事实错误，先更正（两条都由通道 B 提出，重扫判定成立）：**

* **不是「基于同一个 `origin/main`」。** `b93fb767` 的 parent 是 `d27014d8`
  （`git log --oneline -1 b93fb767^`），而当前 `origin/main` / 本 worktree 是
  **`0b4b022f`**（v2 写的 `6e0339b0` 已被 `67829da0` #1191 与 `0b4b022f` #1147 S6 推进）。
  两者现在相差**三**个提交。所以 #1230 落地前必然要 rebase，
  rebase 之后本节的每条冲突判断都要重跑（CLAUDE.md「Rebase Invalidates Gate Evidence」）。
  **好消息（已核）**：那三个提交没碰本节涉及的任何文件（见文首基线段的逐文件核对），
  所以本节 8.1/8.2 的观察在 `0b4b022f` 上**仍然成立**，不需要重扫；
  但 #1230 自己 rebase 之后仍要重跑，因为它的 diff 会移位。
* **本节关于 #1209 侧的冲突判断，大部分是「预测」（v3 已把两条转为实测）。** 到今天为止
  `git status` 里除了本设计文档没有任何 #1209 代码 diff，所以「干净并集」「追加位置不同」
  这类话没有可比较的实物。**v3 收紧了这条对冲**：两个通道都指出，其中**两个**文件的判断
  今天就能定死，不必对冲（A n1 说 `tracks.rs`、A J6 + B M5 说 `workflow_templates.rs`），
  重扫**判定成立**——那两条已改标 **MEASURED** 并附 hunk 头 / 实测站点表。
  过度对冲与对冲不足一样费读者。
  **剩下的**（`routes/track_templates.rs` 与测试文件的具体落点）仍带 `PREDICTED，待实现 diff 复核`
  标记**；实现 PR 的第一件事就是拿真 diff 重跑一遍本节。

两边都改 `routes/tracks.rs`、`routes/track_templates.rs`、`workflow_templates.rs`、
`tests/cases/track_workflow_templates.rs`。以下按文件给出合并规则。**不假设谁先落地。**

### 8.1 #1230 S1 做了什么（OBSERVED，读 `git show b93fb767`）

| 变更 | 位置 |
|---|---|
| `ensure_workflow_templates` / `lookup_workflow_template_track` 提为 `pub(crate)` | `tracks.rs:446`、`:487`（`git show --numstat b93fb767` 对 tracks.rs 是 **`2 2`**，不是 v2 写的 `+4/−4`；两个 hunk 头是 `@@ -443,7 +443,7 @@` 与 `@@ -484,7 +484,7 @@`） |
| 新增 `workflow_template_intro` / `workflow_template_report_from_tasks` / `workflow_template_tasks_from_body` | `workflow_templates.rs`（+169） |
| `GET /api/track-templates` 的 title/tasks 改读已播种 report，回落常量 | 新 `current_definition`（该版 `track_templates.rs:256-279`） |
| 新增 `GET`/`PUT /api/track-templates/{id}` | 该版 `track_templates.rs:303-420+`；`known_template` 在 `:297-301` |
| 模块头新增 "Editable templates (#1230)" 章节 | 该版 `track_templates.rs:45-110` |
| OpenAPI + 生成物 + FE wire | `openapi.rs`(+4)、`fe/core/api/generated/openapi.json`(+167)、`web/src/api/generated.ts`(+160)、`web/src/api/openapi.json`(+167) |
| 端点计数棘轮 +1 | `fe/tools/architecture/openapi-contract.test.ts`（±1，v1 的表漏了这一行） |
| 测试 | `tests/cases/track_workflow_templates.rs` +402/−0（**追加在当前 EOF**，见 §8.2） |

（`git show --numstat b93fb767` 的完整 9 个文件，逐行核过。）

### 8.1b #1230 自己的一个洞 —— **v5：已由上游关闭，本条结案**

> **✅ CLOSED（2026-09-01，v5 实测）。** v3/v4 把它写成「#1209/#1230 的合流硬前提」。
> **上游已经按本文的裁决方向修好了，而且注释逐字复述了本文的发现。**
> 实测（`git -C ../1230-s1 show HEAD:crates/calm-server/src/routes/track_templates.rs`，
> HEAD = `3b9cc03c`）：`current_definition` 现在是
>
> ```rust
> // A seeded template's report is the authority. A *read failure* on it is an
> // error, never a reason to answer with the constants: the first cut used
> // `if let Ok(...)` here and so reported stale constant content with
> // `seeded: false` whenever the report card was unreadable — i.e. it turned
> // an outage into exactly the drift this endpoint exists to remove.
> if let Some(track_id) = lookup_workflow_template_track(s, key).await? {
>     let (_, _, report) = resolve_report_for_track(s.repo.as_ref(), &track_id).await?;   // ← `?`，上抛
> ```
>
> **两个 `?` 都在，降级分支没了。** 因此：
>
> * **本条从「合流硬前提」降为「已满足的前置事实」**，不再需要 #1230 作者做任何事；
> * **§7 的起始状态 C 分支（「若 #1230 坚持读时修复，则 #10 必须补状态 C」）随之作废**，
>   §7 已标注；
> * **§11「仍需人裁」里的相关条目移除**。
>
> **保留下面的原文**，因为它记录了这个洞是怎么被发现、以及为什么裁决方向是「上抛」——
> 那段推理仍然是 §7 不变量能成立的理由。**不要把它读成一个待办。**

（以下为 v3/v4 原文，仅作历史记录。）

**这不是 #1209 的 bug，但它就在本设计要依赖的那条「已播种 ⇒ report 是权威」上，
所以列为 #1230 的 pre-merge 裁决项。** `current_definition` 写成

```rust
if let Some(track_id) = lookup_workflow_template_track(s, key).await?
    && let Ok((_, _, report)) = resolve_report_for_track(s.repo.as_ref(), &track_id).await
```

（#1230 的**早期**版本；**这个形状今天已经不存在了**，见上面的 CLOSED 方框）。
overlay **已存在**但 report 读取/解析失败时，第二个 `let Ok(..)` 悄悄失配，
函数落到常量分支并**报 `seeded: false`**。
于是 picker 展示 Rust 出厂内容，而 create 仍然 fork 那份读不出来的 DB report——
正是 #1230 存在的理由（picker vs fork 漂移）被重新造出来一次。

**v3 裁决（从「建议」升级为合流硬前提）**。两个通道都指出 v2 只是「记了一段话」，
没有进入任何切片的门禁（A m2 从 INV-SEED 一侧、B M7 从产品洞一侧），**判定成立**。
v2 的「#1209 不依赖它被修好」这句话**字面为真但不足**：#1209 的确不经过
`current_definition`（create fork 的是 `tracks.rs:805-812`），但 §7 的 INV-1209-SEED
**依赖它朝哪个方向修**——「读时修复」会让一次 GET 变成一次写，而 #10 照绿（§7 状态 C）。

> **裁决**：`current_definition` 在「overlay 存在 ∧ report 读取/解析失败」时
> **必须 `?` 上抛（500）**，不得降级为常量 + `seeded: false`。
> **归属**：#1230 的作者（这行代码在 `1230-s1` `routes/track_templates.rs:256-258`，
> 回落分支在 `:270-278`）。
> **门禁**：#1230 侧新增一条真路由测试——播种后把该 template track 的 report 载荷弄坏，
> `GET /api/track-templates/{id}` 必须 500，且**库不被改写**（复用 §10.2 #10 的 snapshot helper）。
> **合流硬前提**：两条切片合并落地前，这条必须已落在 #1230 的 PR 里；
> 若 #1230 的作者选择「读时修复」而非上抛，则 #1209 的 §7 与 §10.2 #10 必须补状态 C
> 并重新论证不变量（§7 已写明这条代价）。

替代方案（保留降级，但把 `seeded: false` 三态化并同步 fork 侧）**不采纳**：
它要求 fork 侧也跟着降级，等于把 picker-vs-fork 漂移重新造一遍——那正是 #1230 存在的理由。

### 8.2 逐文件的冲突解决规则

**`crates/calm-server/src/routes/tracks.rs`**（**MEASURED，不是 PREDICTED**）— 冲突面为零。
通道 A 的 n1 指出这条今天就能定死，**判定成立**：
`git diff b93fb767^ b93fb767 -- crates/calm-server/src/routes/tracks.rs | grep '^@@'` 给出
**恰好两个 hunk**：`@@ -443,7 +443,7 @@` 与 `@@ -484,7 +484,7 @@`（即 `:446`、`:487`
两行可见性改动，numstat `2 2`）。#1209 动的是 `:761-793`、`:799-814`（后者搬到 `:899` 之前，
§4.2）与新增 `admit_template`。**两侧 hunk 的行区间不相交，是今天可验证的事实。**
**规则：两侧全取（union），无需人工裁决。**
新增的 `admit_template` 保持 `pub(crate)`（`tracks.rs` 内部使用；读口**不**调它，见下）。

**`crates/calm-server/src/workflow_templates.rs`** — **v1 说「#1209 一行不改」，这是错的，
而且照做会打红 CI。** 通道 A 提出，重扫**判定成立**：

* `is_workflow_template_key`（`:40-42`）今天的**生产**调用点恰好只有 `tracks.rs:779` 与
  `tracks.rs:800`，两处都被本切片删除。其余引用只有 `workflow_templates.rs:511`、`:520`，
  都在 `#[cfg(test)] mod tests`（该 mod 从 `:372-373` 开始）内。
* `WORKFLOW_TEMPLATE_KEYS`（`:18`）的生产引用只有 `:41`（即 `is_workflow_template_key`
  自己），其余 `:510`、`:560` 同在 test mod 内。
* `workflow_templates` 是 `pub(crate) mod`（`crates/calm-server/src/lib.rs:635`），
  模块不对外可达 ⇒ 里面的 `pub` 项照样吃 `dead_code`。
* CI 全局 `RUSTFLAGS: "-D warnings"`（`.github/workflows/ci.yml:15`）。
* **v3 更正（通道 A J1，我独立复跑，判定成立、v2 错）**：v2 写
  「`cargo clippy --workspace --all-targets`（`:305`）因为带 `--all-targets` 会把 test mod
  一起编，**看不见**这个死代码」——**这是错的**。`--all-targets` 展开为
  `--lib --bins --tests --benches --examples`，其中**朴素的 `--lib` 目标不带 `cfg(test)` 编译**，
  `dead_code` 照常发出。我在 scratchpad 建了一个无依赖玩具 crate（同样形状：
  `pub(crate) mod` + `pub const` + 只被 `#[cfg(test)] mod tests` 使用的 `pub fn`）实跑：
  ```
  $ cargo clippy --all-targets
  warning: constant `KEYS` is never used
  warning: function `is_key` is never used
  warning: `dctest` (lib) generated 2 warnings
  ```
  **所以两个作业都会红**：lint 作业 `cargo clippy --workspace --all-targets --features
  calm-server/codex-e2e -- -D warnings`（`ci.yml:304-305`）**先红**，
  release 构建 `cargo build --release -p calm-server … --locked`（`ci.yml:901`，另 `:1012`）**也红**。
  **结论不变（必须删符号），只是机制说错了。** 元教训一并记下：这条链在 v1 提出、
  v2 标注「独立复核全部成立」，但直到 v3 才**有人真的跑过**
  （CLAUDE.md「Review Cannot Replace Execution」）。

「等 #1230 保住调用方」不是合法解——用户已经决定两条并行跑，本设计必须**顺序无关**。

**规则（v2）**：#1209 在这个文件里做一次小手术，正好把这个问题和 §2.3 类别 1 的
「两份名册数组」一起解决：

1. 删除 `WORKFLOW_TEMPLATE_KEYS`（`:18`）与 `is_workflow_template_key`（`:40-42`）。
2. 新增 `pub fn workflow_template(key: &str) -> Option<&'static WorkflowTemplate>`
   （§2.2），由 `WORKFLOW_TEMPLATES` 派生。它有生产调用方（`admit_template`），
   **无论谁先落地都不是死代码**。
3. 把 test mod 里 `:510` 与 `:560` 两处 `for key in WORKFLOW_TEMPLATE_KEYS` 改成
   遍历 `WORKFLOW_TEMPLATES`，并把 `:511`、`:520` 的 predicate 断言改用
   `workflow_template(..).is_some()`。
   **⚠️ 这两处只是「#1209 单独落地」时的清单。合并树上是 7 处，见下。**
4. **合并规则升级：本文件从「全取 #1230 侧」变成第三个需要人裁的文件。**
   #1230 在这里是 `+143/−26`（新增 `workflow_template_intro` /
   `workflow_template_report_from_tasks` / `workflow_template_tasks_from_body`）。

**v3：把这里的 `PREDICTED` 换成实测事实（通道 A J6 + 通道 B M5，两个通道独立提出，
重扫判定成立、v2 把影响面少报了 3 倍）。** 仅凭 `b93fb767` 就能定死，不需要 #1209 的 diff：

* **文本层是干净并集，今天可验证。**
  `git diff b93fb767^ b93fb767 -- crates/calm-server/src/workflow_templates.rs | grep '^@@'`
  给出五个 hunk，旧行锚点 **7、68、209、253、375**。**没有一个碰 `:18` 或 `:40-42`。**
  所以删除这两个符号 git 不会报冲突——**而这正是危险所在**。
* **语义层是 7 处，也今天可验证。** 在 `1230-s1` 里（`grep -n` 实测）：

  | 符号 | 站点 | 性质 |
  |---|---|---|
  | `WORKFLOW_TEMPLATE_KEYS` | `workflow_templates.rs:451` | test mod（#1230 新增） |
  | `WORKFLOW_TEMPLATE_KEYS` | `workflow_templates.rs:470` | test mod（#1230 新增） |
  | `WORKFLOW_TEMPLATE_KEYS` | `workflow_templates.rs:627` | test mod（= 本 worktree 的 `:510`） |
  | `WORKFLOW_TEMPLATE_KEYS` | `workflow_templates.rs:677` | test mod（= 本 worktree 的 `:560`） |
  | `is_workflow_template_key` | `workflow_templates.rs:628` | test mod |
  | `is_workflow_template_key` | `workflow_templates.rs:637` | test mod（负例 `"missing-workflow"`） |
  | `is_workflow_template_key` | **`routes/track_templates.rs:298`**（`fn known_template` 在 `:297`，import 在 `:122`） | **生产** |

  即 **6 个 test 使用点 + 1 个生产使用点 + 1 处 import**（v4 措辞更正，通道 A m3 + 通道 B n3
  都指出 v3 那句「6 处 + 1 import」与它自己上面这张 7 行表自相矛盾——**判定成立**：
  表里 6 行是 test、第 7 行是生产，import 是第 8 处编辑）。v2 只列了 2 处。

* > **⚠️ v5：上表的坐标一律作废，本节此后不再记 `1230-s1` 的任何行号。**
  >
  > **v4 那组自称「对 `7b85caa3` 复测」的坐标复现不出来。** 两个通道各自独立实测，
  > v5 复核，三点结论：
  > 1. `7b85caa3` 与 `b93fb767` 在这条 grep 上**逐字节相同**（根本没漂）；
  > 2. 第 4 轮评审当时的 tip `d51571d7` 又是另一组坐标；
  > 3. **本轮实测的 HEAD 是 `3b9cc03c`**，坐标是第四组
  >    （`workflow_templates.rs` 六个 test 点 + `routes/track_templates.rs` 一个生产点 +
  >    一处 import；此外还有 `routes/tracks.rs` 三处——那三处正是 §4.1 要删掉的
  >    `:779` / `:800` 与它们的 import，属于 #1209 侧，不算 #1230 的接触面）。
  >
  > v4 那组数字既不是 (1) 也不是 (2)——它们来自当时的 dirty working tree。
  >
  > **写进设计的只有形状：`WORKFLOW_TEMPLATE_KEYS` / `is_workflow_template_key` 在 #1230 侧
  > 恒为 6 个 test 使用点 + 1 个生产使用点（`routes/track_templates.rs` 的 `known_template`）
  > + 1 处 import，四个基线上都成立。**
  > **合流时的动作是：对当时的 #1230 HEAD 重跑
  > `git grep -n 'WORKFLOW_TEMPLATE_KEYS\|is_workflow_template_key' -- crates/`，
  > 按输出逐条改，不要照抄本文任何行号。**

* **⚠️ 第 8 个站点，它不吃上面那条 grep（通道 A m4，判定成立）**：
  #1230 的 `current_definition` 回落分支里**开手写**了一次名册查找
  （`WORKFLOW_TEMPLATES.iter().find(|template| template.key == key).map(..)` 取 title）。
  它用的是 `WORKFLOW_TEMPLATES` 而**不是** `is_workflow_template_key`，
  所以 §10.1 的 PR-1 验收 **A7** 的 grep **抓不到它**。
  （**v5 实测该分支在 `3b9cc03c` 上仍然存在**，形状不变；同样不记行号。）
  **漂移不可能发生**（同一个数组），因此这不是正确性风险；
  它证伪的是 §2.2 v3 那句「名册的唯一查找入口」——**该 claim 已在 §2.2 收窄**。
  **合并规则**：这一处**两侧都不动**（同 `list_track_templates` 那条的理由：
  它已经在遍历/命中名册，再套一层可失败查找只会把「查不到」变成一个静默的空标题）。
  写在这里是为了让合并的人**知道它存在**，不要以为验收 A7 覆盖了全部名册查找。

* **三种落地顺序的结论（v3 明写）**：
  1. **只有 #1209 落地**：绿。两个符号删净；`workflow_template()` 有生产调用方
     （`admit_template`）；`WORKFLOW_TEMPLATES` / `WorkflowTemplate.title` / 三个 key 常量
     都还有生产消费者（`tracks.rs:449`、`routes/track_templates.rs:103-104`、
     `workflow_templates.rs:44-51`/`:62-69`）。`ci.yml:305` 与 `:901` 都不红。
  2. **#1230 先、#1209 后**：只有当合并**同时**改掉上表 7 处才绿。git 在其中绝大多数上
     **不会**冲突（不同文件 / 不同 hunk），所以**破坏在编译前是静默的**。
  3. **#1209 先、#1230 变基**：#1230 的分支在 `routes/track_templates.rs:298` 编译不过，
     同样**没有任何 git 冲突来预警**。
  **所以「顺序无关」这句话只对本 PR 自己的 CI 成立，对这一对不成立。**
  唯一的守法是把它变成一条机器可跑的验收（§10.1 的 PR-1 验收 **A7**）：
  ```sh
  grep -rn 'WORKFLOW_TEMPLATE_KEYS\|is_workflow_template_key' crates/   # 必须零输出
  ```
  在**合并后的树**上跑，而不是只在自己分支上跑。

5. #1230 侧的 `known_template`（`routes/track_templates.rs` 里那个调 `is_workflow_template_key`
   的一行 predicate；**行号见合流时的 grep，本文不记**）要改写为
   `workflow_template(id).is_some()`——这也顺带兑现了「只有一个名册」。
   （通道 A m4 说 `fn` 在 `:296`；**实测在 `:297`**，v2 的坐标是对的，该修正驳回。）

**`crates/calm-server/src/routes/track_templates.rs`** — 第二个需要人裁的文件。

* 模块头 `:1-39`：#1230 改写了「tasks 从常量读」那段（该版 `:22-26`、`:45-110`），
  #1209 要改写「词汇缝」那段（`:29-39`，见 §3）。**两段不重叠，两侧全取。**
  合并后必须再读一遍全文确认没有互相打脸的句子——尤其 `:39`
  「When the merge lands, the shape returned here does not change.」这句，#1209 必须兑现它
  （本设计不改 `TrackTemplate` 的任何字段）。**这一条是可机检的**：合并后
  `TrackTemplate` 的字段集合与 `fe/core/domain/track.ts:198-211` 的 zod schema 都不动。
* `list_track_templates` 里 `resolve_trusted_workflow(&s, template.key)` 那一行
  （`:109-111` / #1230 版 `:199-201`）：**v3 撤回 v2 的「改走 `admit_template`」，
  规则改为「两侧都不动这一行」**（通道 A m5，重扫**判定成立**）。
  理由：该循环本来就在遍历 `WORKFLOW_TEMPLATES`（`routes/track_templates.rs:103-104`），
  准入在进入循环体时**已经成立**；再调一次 `admit_template` 就是一次**冗余的可失败查找**，
  而它的失败模式是 `Option` ⇒ 读口会**静默地宣告「这个 template 没有 schema」**而不是报错。
  那正是 `track_templates.rs:11-14` 的模块头存在要防的 picker-vs-create 漂移。
  `resolve_trusted_workflow` 本来就是**共享的绑定解析器**（`tracks.rs:932-950` 的 doc），
  统一之后它一个字不改（§2.2 第 2 点），读口继续直接调它就是最短、最诚实的写法。
  **附带收益**：本文件因此少一个与 #1230 的接触点。
* `GET/PUT /api/track-templates/{id}`：**#1209 不碰其逻辑**，只按上面第 4 条把
  `known_template` 的 predicate 换成 `workflow_template(id).is_some()`（因为
  `is_workflow_template_key` 已被删）。这是**合并前提**，不再是加分项。

**测试** — `crates/calm-server/tests/cases/track_workflow_templates.rs` — **第一个需要人裁的
测试文件**。v1 说「追加位置不同」，这是错的（通道 B 提出，重扫**判定成立**）：
本 worktree 该文件共 589 行，最后一个 case 是 `unknown_workflow_id_still_400s`（`:568-589`）；
#1230 的新 case **正是从当前 EOF 追加**（本文不记它那一侧的行号）。#1209 的新 case 若按默认习惯也追加到 EOF，
就是**同一个插入锚点**，git 必然报冲突。
**规则**：(a) 两侧全取，人工确认没有任何一侧的 case 被吞；
(b) 合并后按 CLAUDE.md「Merge Union Check」做「恰好等于两父并集」的机器校验，
**双向查**（缺失 + 多出）；
(c) #1209 侧还要改 `:586` 那条文案断言（§10.3），那一行落在 #1230 追加段之前，
属于内容冲突而不是位置冲突。

**OpenAPI / FE 生成物** — **⚠️ v5 重写整条规则；v4 这里留着一句在 v4 自己那一轮就已经变假的话。**

> v4 写的是「**#1209 对 wire 零改动（§3）**，#1230 新增两个端点。规则：全取 #1230 侧，
> 然后重新跑一次生成器」。**「#1209 对 wire 零改动」在 v3 为真，在 v4 为假——
> §3 就是 wire 改动。** 两个通道独立指出这一条（v5 判定成立）。
> **后果很具体**：一个按字面执行合并的人会**整份取走 #1230 的生成物**，
> 然后因为「规则说取那一侧」而**永远不会把改名和解进去**——
> 而生成物是产出物，取哪一侧本来就是个伪问题。

**v5 的规则**：

1. **两侧的生成物都不取。** 合并冲突时对这 7 个产物一律 `git checkout --ours` / `--theirs`
   都不对——它们不是源。
2. **先把两侧的 **Rust** 源合并干净**（`openapi.rs` 的注册、`CreateTrackRequest` / `Track` /
   `NewTrack` 的字段、#1230 的两个新端点）。
3. **再从合并后的 Rust 重新生成**：`cd web && npm run gen:api`（`ci.yml:1186-1187`）
   与 `cargo run --bin emit-openapi > fe/core/api/generated/openapi.json`（`ci.yml:1190`）。
4. **裁决权交给 `ci.yml:1194` 的 `git diff --exit-code`**（覆盖那 7 个产物）。
   它绿了就是对了；它红了就是 Rust 那一侧还没合干净，**不要去手改产物让它绿**。

（rebase 会让生成物的 hash 型断言失效，参见 CLAUDE.md「Rebase Invalidates Gate Evidence」——
这也是「重新生成而不是取一侧」的另一个理由。）

### 8.3 哪些 #1230 的面在统一模型下不变形

| #1230 的面 | 统一后 |
|---|---|
| `GET /api/track-templates` 响应形状 | **不变**（#1209 兑现 `track_templates.rs:39` 的承诺） |
| `GET/PUT /api/track-templates/{id}` | 行为**不变**；`known_template` 的**实现**换一行（`is_workflow_template_key` → `workflow_template(id).is_some()`，§8.2） |
| 「读不触发播种」 | **不变**（§7），但断言强度按 §7 的 v2 版本提高 |
| 「已播种 ⇒ report 是权威」 | 语句不变，但 §2.3 不再把它包装成「只剩 2 处权威」；且 §8.1b 记了 #1230 自己在这条上的一个洞 |
| `input_schema` 仍走 `resolve_trusted_workflow` | **形状与实现都不变**（v3 撤回 v2 的「改走 `admit_template`」，见 §8.2 的 m5 裁决） |

**v4 复查：§3 的改名把 #1230 的合流面改成什么样？答案是「几乎没变」，理由要写出来。**

| 面 | 改名的影响 |
|---|---|
| `GET /api/track-templates` 的响应 | **零影响。** 该端点的 `TrackTemplate` 里根本没有 `workflow_id` 字段（§1.5 的 v4 复读，实测该文件里 `workflow_id` 只出现在 `:32`/`:57`/`:62` 三处注释）。#1230 S1 的 `:39` 承诺照旧兑现 |
| `GET/PUT /api/track-templates/{id}` | **零影响。** `TrackTemplateDefinition` 是 `{id, title, tasks, seeded}` |
| #1230 碰过的三处注释 | **有影响**：`:32`/`:57`/`:62` 里的 `workflow_id` 拼写要跟改。这三行落在 #1230 S1 也改过的模块头/doc 区，是**内容冲突**，人裁 |
| #1230 S2（设置页 astryx 重写 + Templates 二级导航与模板编辑器，已并进 `1230-s1@3b9cc03c`）| **仍然未评估。** 它属于 `fe/`，而 `fe/` 正是 §3.2 类别 1/2/3 都要改的目录之一 ⇒ **可能有新的接触面**。本文**不假装评估过它**。**合流前必须在合并树上跑 §3.2 那条残留 grep**（验收 B10）——那条 grep 覆盖全仓，所以它同时也是对 S2 的扫描 |

**没有任何一个 #1230 S1 的面在统一后需要改响应形状**，且需要改实现的只剩**一处一行**
（`known_template` 的 predicate；v2 说两处，v3 撤回了 `list_track_templates` 那处）。
#1230 改的是「内容的权威」，#1209 改的是「准入的判据」，两者正交——正交不等于零接触面。

---

## §9 风险与非目标

### 明确不做

1. **用户自建 template。** `as_template` 已经是 `CreateTrackRequest` 的公开字段
   （`tracks.rs:224`），overlay 那条路「已经存在一半」（#1209 正文）。本设计**不**把它接上。
   谁若假设本次之后可以建自定义模板，会撞上：名册仍是 Rust 常量
   （`workflow_templates.rs:25-38`），`admit_template` 只认名册，
   自建的 `as_template` track 不会出现在 `GET /api/track-templates` 里，也不能作为
   `workflow_id` 传入。

   > **#1318 S2 补记（本条的前提已消失）。** 上面两句现在时的陈述在今天的树上都是假的：
   > `as_template` 已从 `CreateTrackRequest` 删除（发过去是未知字段 ⇒ 422），
   > `kernel`/`view`/`template` overlay 那条路连同它的写口与六个读者全部退休，
   > 所以「已经存在一半」的那一半也没有了。**结论没变**（仍然不做用户自建 template），
   > 变的是理由：不是「接上一半」，而是「那一半被拆掉了」。
2. **模板 CRUD（新建/删除）。** #1230 只做「编辑已有三个」；本设计不扩。
   而且 system area 的 track 通过 API 不可删（`tracks.rs:3060-3092`，判定在 `:3085-3092`，
   2026-09-01 裁定）。
3. **第二个插件。** 见 §6 末段：结构上已就绪，但 `trusted_forge_plugin`
   （`forge_trust.rs:1-8`）的信任策略不在本次范围。
4. **插件贡献 template（§5.2 的 C）。** 需要先扩 `WorkflowDescriptor`
   （`manifest.rs:472-475`）。真实成本见 §5.2 末段（**不是** v1 说的解析器障碍）。
5. ~~**`workflow_id` 改名。**~~ **v4 删除本条：改名已经进入本切片**（§3 的 D2 重开）。
   原 S2 并入 S1（或按 §10.1 的两 PR 切法成为 PR-2）。
6. **template 自带参数声明。** §6，明确上限：无绑定 ⇒ 无输入。
7. **数据回填 / 清洗。** §5.1 第二条：老数据里若存在非模板 id，
   `bound_workflow` 的 fail-safe 已经覆盖，不需要回填也不需要清洗。
   **v4 收窄措辞**：v3 这条写的是「数据迁移」，改名之后**是有一条迁移的**
   （§3.3 的 `ALTER TABLE tracks RENAME COLUMN`）。那条迁移是纯改名，
   不回填、不清洗、不丢数据——本条否定的是**回填/清洗**，不是**迁移文件**。
   （这是 CLAUDE.md「Statement Widened Past Carrier」的同一个形状：
   v3 的句子写得比它的载体宽。）
8. **让 intro / contract prefix 的常量改动回灌已播种拷贝**（§2.3 类别 3 的漂移）。
   既有问题，本次不修。
9. **`workflow_input` requires `workflow_id`** 这条误导文案（§4.4 行 8）。
10. **「任何端点都不许触发播种」的 fail-closed 路由扫描门禁**（§7 收窄理由 1）。
11. **插件 manifest 的 `workflows[]` 不改名**（**v5 新增为显式条目，通道 A m9，判定成立**）。
    v4 删掉了旧的第 5 条「`workflow_id` 改名」非目标（因为改名进了本切片），
    于是「`workflows[]` **不**改名」这个决定只活在 §3.8 的散文里——
    而人的一致性指令恰恰是「尽可能保持一致」，一个反向的例外必须被显式记账，
    否则下一个读者会以为它是漏网。
    **理由见 §3.8（改它是 Tier A *schema* 破坏；D4-A 只改接受语义）。**
    **顺带记下残余的命名债**：D4-A 之后 `workflows[]` 的每一个合法值都是 template key，
    这个容器的名字因此变得别扭。**这是真实的技术债，挂一个跟进 issue**，
    正确的偿还时机是 §5.2 方案 C（插件贡献 template）落地时——那时 schema 本来就要动。
12. **`WEB_COMPAT_VERSION` 三处常量的「单一源生成」**（§3.6 的选项 b）。
    本切片只做选项 (a)（一条比较三者的 CI 静态门禁）。
    **v5 明写这一条，因为 (a) 是「测相等」而 (b) 才是「派生」**——
    按 CLAUDE.md「Mirror Code Must Call The Original」，(b) 才是最终形态。
    挂跟进 issue，不在本切片。
13. **`POST /upgrade/rollback` 支持回滚一次 breaking apply**（§3.7 第 2 条）。
    这是产品既有的缺口，本切片只在 `docs/deploy-and-upgrade.md` 里写出手工恢复步骤，
    **不改 `apply.rs:1252` 的 `rollback_last_preserving`**。挂跟进 issue。

### 风险

| 风险 | 严重度 | 缓解 |
|---|---|---|
| **变更 A 是公开插件契约破坏**，伤到运行时装入的第三方插件 | **中**（v1 记为「低」，判定错，见 §5.3） | 同一 PR 改 `manifest.rs:93-100` 的字段文档 + **`docs/deploy-and-upgrade.md` 新增的「插件兼容性」一节**（§5.3 缓解 2，v3 已把 v2 的「release note」换成这个具名落点；**v4 更正本格漏改的旧措辞**，通道 A n4）+ 该节内联的升级前 `jq` 扫描；可选的 spawn-time warn |
| 变更 B/C 打红切片外的**文案断言** | 低（吵，不是正确性风险） | **v5 更正为三条**（v4 说两条）：`track_workflow_templates.rs:586`、`forge_workflow_e2e.rs:427`（这两条被**变更 B** 打红）、**`forge_workflow_e2e.rs:203`**（`contains("workflow_id")`，被**变更 C** 打红，v4 的任何清单里都没有它）。§10.3 给了前两条的**三条腿**替代断言（v2 的单腿版检测不到回退），第三条改成 `contains("template_id")` 且**不加**三条腿 |
| **测试 #8 按 v2 配方写成假绿**（stub 带 required schema ⇒ 撞 required-input 400 而非准入 400） | **高（若不处置：整个 `:779` 保证失去唯一的定向反例）** | §10.2「关于 #8 的测试设计」的 v3 警告框：stub 去掉 `input_schema` 或带合法 input + 断言拒绝**理由** + 零播种副作用 |
| **合并树上 `WORKFLOW_TEMPLATE_KEYS` 漏改**（8 处编辑只改了 2 处，git 无冲突提示） | **中**（v4 下调，通道 A n5，判定成立：CI 触发于 `pull_request`、构建的是 PR 的 merge commit（`.github/workflows/ci.yml:3`），所以**后落地的那个 PR 必然自动变红**。v3 的「中高」偏高。验收 A7 仍然保留——它更早、更本地、且能在合并前就指出漏了哪一处） | §8.2 的**形状**结论（6 test + 1 生产 + 1 import，四个基线复测；**v5 已删除全部行号**，合流时重跑 grep）+ §10.1 的 PR-1 验收 **A7** 的合并树 grep |
| 与 #1230 S1 的合并冲突 | **中高**（v1 记为「中」，且低估了面：三个文件人裁，不是一个） | §8.2 的逐文件规则；`track_workflow_templates.rs` 与 `workflow_templates.rs` 都是同锚点冲突；两侧「并集校验」双向查；**生成物两侧都不取、从合并树重新生成**（v5 更正 v4 那条「全取 #1230 侧」的规则——它建立在一句已经变假的「#1209 对 wire 零改动」上） |
| **#1209 先落地 ⇒ release 构建 dead_code 红** | **高（必然发生，若不处置）** | §8.2 的 `workflow_templates.rs` 手术：删两个符号、加一个有生产调用方的 `workflow_template()` |
| 合并后 `track_templates.rs` 模块头出现互相打脸的句子 | 中 | §8.2 要求合并后整篇重读；`:39` 那句承诺可机检（`TrackTemplate` 字段集合 + zod schema 不动） |
| `:779` 换马甲而评审看不出来 | 中 | §4.3 的**语义**判据 + 测试 #8/#9（路由级，不是 grep） |
| 搬动播种位置引入回归（§4.2） | 中 | 测试 #13；且搬动后的顺序仍不保证「任何非 201 都无写」，§4.2 已把注释改写到与行为一致 |
| S1 与 #1230 都动 `track_workflow_templates.rs`，rebase 后门禁证据作废 | 中 | 合并后完整重跑 `cargo nextest --features calm-server/codex-e2e`（CLAUDE.md「Rebase Invalidates Gate Evidence」） |
| **（v4，v5 升级）改名后手写列名的 SQL 在运行时才炸** | **高（若不处置）。v5 上调影响面：v4 只列了三处并把 `today.rs` 归为「编译器抓」——那正是本仓 Card-Column-SELECT 教训点名的误分类，两个通道独立抓到，是本轮的 BLOCKER** | §3.3 现在点名**五处生产站点**（`db/rows.rs:87`、`:94`、`db/sqlite/track.rs:184`、**`routes/today.rs:149`、`:162`**）+ 10 个词法 SELECT 消费点 + 4 处测试侧原始 SQL；验收从「一次 create+read 往返」扩成 **往返 + Today launchpad 两条腿 + 迁移保值 fixture**（§10.1 PR-2 的 A5/A6/A7） |
| **（v5）PR-2 的改名扫描漏掉一个非类型检查站点** | **中高**（后果按站点而异：`wire.ts` 的 `Omit` 是类型层静默失效、`track-fs-viewers/schemas.ts` 是旧 snapshot 静默 null、oracle 是判据与代码脱节；**共同点是没有任何东西会红**） | §3.2 类别 2 的站点表 + **PR-2 收尾的残留 `git grep` + 显式 allowlist**（那条 grep 才是真保证，站点表只是方向）；§3.2 末段已把这条不确定性写诚实 |
| **（v5）`WEB_COMPAT_VERSION` 三处漂移** | **中高**（三种漂移后果见 §3.6，**今天全部是绿的**：Rust 侧断言字面量、两个前端各读自己的常量） | §3.6 要求 PR-2 加一条比较三处导出值的 CI 静态门禁（选项 a），并给出成对的正例/反例；测试 #15 保留但**不再自称三方 lockstep pin** |
| **（v5）历史事件读取器把整行跳过** | **中**（比丢字段更糟，但只在「有人为了防 fail-open 而删掉 `#[serde(default)]`」时才发生） | §3.4 明写两条方向相反的坏路 + 点名真正的读取者 `events.rs:577`（`Err` 分支 `:578-585` 只记日志并跳过行）；裁决是 **alias + 保留 `default`**，两条都躲开 |
| **（v4）改名后历史事件静默丢字段** | **高（若不处置：replay 出来的 track 丢掉模板归属且无报错）** | §3.4 的 `#[serde(alias)]`（Rust）+ zod 两侧；§10.2 新增测试 #14（拿一条真的旧 golden 喂进去，断言字段还在）；oracle 新增一行 |
| **（v4）旧浏览器 bundle 半工作** | **高（若不处置：缓存里的 `web/` bundle 一直发旧字段，拿一串 400）** | §3.6 把 `WEB_COMPAT_VERSION` 16→17 **三处一起改**（v5 撤回 v4「lockstep」这个用词——今天没有任何门禁比较它们）+ **PR-2 新建一条比较三处导出值的 CI 静态门禁**（验收 B6）；§10.2 的测试 #15 只钉服务端 floor |
| **（v4）`productMajor` 裁决落空** | **中高（若只写进升级说明）** | §5.3 的实现指令 (a)：改 `package.rs:307` 的默认值，让 `package.rs:546` / `manifest.rs:302` 两条既有断言变成 pin |
| **（v4）切片被改名的机械 diff 淹没，`:779` 的判据看不见了** | **中高**（这正是 v1 当初把改名推迟的理由，那条理由今天仍然成立，只是不再压过一致性） | §10.1 的**两 PR 切法**：PR-1 只做统一（判据清晰、可评审），PR-2 只做改名（大 diff、零概念） |

---

## §10 切片与验证计划

### §10.0 实现者须知

#### 前言 — 实现者绝不能丢的东西（**v5 新增，按「丢了会怎样」的严重度排，不按代码顺序**）

这份文档很长。如果你只读一页，读这一页。
**每一条都满足同一个形状：照直觉写会写错，而且写错了 CI 是绿的。**

| 序 | 丢了会怎样 | 是什么 | 详见 |
|---|---|---|---|
| **1** | **生产 Today 页面在运行时炸**（`no such column`），`cargo build` / `clippy` / 单元测试全绿 | `routes/today.rs:149` 的 UPDATE 与 `:162` 的 INSERT 把列名写成字面 SQL。DB 列改名后它们**编译干净**。手写列名的生产站点一共**五处**（另三处：`db/rows.rs:87`、`:94`、`db/sqlite/track.rs:184`） | §3.3 |
| **2** | **历史事件里的模板归属静默消失**，replay 出来的 track 少一个字段且无报错 | `calm_types::Track` 的新字段必须带 `#[serde(alias = "workflow_id")]` 且**保留 `#[serde(default)]`**。读取者是 `events.rs:577`，它的 `Err` 分支会**跳过整行**（`:578-585`）——所以不能靠删 `default` 来防 | §3.4 |
| **3** | **旧 FS snapshot 静默变成 `template_id=null`** | Zod 读取器有**三个**，不是两个。第三个是 `web/src/track-fs-viewers/schemas.ts:152`/`:160`，读旧 `track.json`。它有 `.default(null)`，机械改名 = fail-open | §3.4 |
| **4** | **缓存里的生产 bundle 一直发旧字段名、一路拿 400**（= `upgrade-stability.md:29` 禁止的「部分工作」） | `WEB_COMPAT_VERSION` 16→17 **三处**（`routes/version.rs:21-22`、`web/src/api/version.ts:100`、`fe/web/src/app/providers/public.tsx:9`）。**今天没有任何 CI 门禁比较这三处**，三种漂移全绿——PR-2 必须补一条静态门禁 | §3.6 |
| **5** | **一整类改名站点没人告诉你漏了** | 非类型检查站点（字面 SQL、`Omit<..,'workflow_input'>` 的字符串键、oracle YAML、字符串名册、注释/CSS/aria-label）。**唯一的真保证是 PR-2 收尾的残留 `git grep` + allowlist**，不是本文的站点表 | §3.2 类别 2 |
| **6** | **`productMajor` 裁决完全落空**，机器照判 `Preserving` | 改 `package.rs:307` 的默认值 `Ok(0)`→`Ok(1)`。**pin 只有 `package.rs:546` 一条**（`manifest.rs:302` 是 parser fixture，改默认值它不会红） | §5.3 |
| **7** | **测试 #8 是一个永远绿的假测试**（检测不到它唯一的存在理由） | stub 插件**不要带 `input_schema`**（别抄 `track_templates_read.rs:106`），否则会先撞 required-input 400 而不是准入 400 | §10.2 警告框 |
| **8** | **测试 #9 丢掉鉴别力** | 正例腿断言 `status == 201`，**不是**「正文不含 known track template」。v3 弱化过一次，那个弱版对「换措辞的特例」是绿的 | §10.2「#9 的形状」 |
| **9** | **PR-1 交付一段提前撒谎的契约注释** | `track_templates.rs` 模块头：PR-1 落**临时文本**（说 `workflow_id`），PR-2 才落最终文本（说 `template_id`） | §3.9 方框 |
| **10** | **合并树静默编译不过，而 git 不给任何冲突提示** | `WORKFLOW_TEMPLATE_KEYS` / `is_workflow_template_key` 在 #1230 侧有 6 test + 1 生产 + 1 import；**合流时对当时的 #1230 HEAD 重跑 grep**，不要照抄任何行号 | §8.2 |
| **11** | **合并把改名整个丢掉** | OpenAPI/FE 生成物：**两侧都不取**，先合 Rust，再重新生成，由 `ci.yml:1190/1194` 裁决 | §8.2 末段 |
| **12** | **升级后回不去** | breaking 路径**会**自动备份（`apply.rs:375-376`），但 `POST /upgrade/rollback` **拒绝**回滚一次 breaking apply（`apply.rs:1266`）。文档要写手工恢复步骤，且**不要**教人 `cp` 三件套 | §3.7 |

#### 逐条清单（v4 版，v5 已按上表更正）

这份文档经过五轮双通道评审，下面这些条目**每一条都是某一轮里被抓到的一个具体错误的处置**。
**实现时按顺序核对，不要凭记忆重写。**

1. **测试 #9 的正例腿断言 `status == 201`，不是「正文不含 known track template」**
   （§10.2「#9 的形状」）。v3 把它弱化过，那次弱化只丢了鉴别力：一个换了措辞的特例
   （`if id == "investigation" { return Err(BadRequest("investigation is disabled")) }`）
   会让弱版 #9 保持绿，而 §10.1 的 PR-1 验收 A3 明说这种改动必须红。
2. **测试 #8 的 stub 插件不要带 `input_schema`**（§10.2 的警告框）。照抄
   `track_templates_read.rs:106` 会造出一个永远绿的假测试——它检测不到它唯一的存在理由。
3. **`WEB_COMPAT_VERSION` 16→17，三处一起改，并且 PR-2 要补一条比较三处的 CI 静态门禁**
   （§3.6）。**v5 更正**：v4 把这一步叫「三处 lockstep」，
   但**今天没有任何东西比较这三处**——三种漂移全部是绿的。
4. **`calm_types::Track` 的新字段要带 `#[serde(alias = "workflow_id")]`（并保留 `#[serde(default)]`），
   而 `CreateTrackRequest` **绝不能**带**（§3.4 / §3.5）。这条不对称是有意的。
   **v5 删除 v4 的 `TrackRow` 指令**：`crates/calm-truth/src/db/rows.rs:99` 只 derive
   `Debug, sqlx::FromRow`，加 serde attribute 轻则无效重则编译失败，
   而且本来就无事可做——`FromRow` 按列名绑定，迁移已经就地改了列名。
5. **`package.rs:307` 的默认值 0→1**（§5.3 的实现指令 a）。只在升级说明里写
   「打包时设 `NEIGE_PRODUCT_MAJOR`」等于什么都没做。
   **pin 是 `package.rs:546`，单数**（§5.3 的 v5 方框）。
6. **播种块搬到阶段 1 的第 5 步之后；阶段 2 的顺序是 folder claim → track_create_tx → 显式 fork；
   并且还有阶段 0（serde extractor）与阶段 3（事务后 materialize / harness start）**
   （§4.4 的四个阶段）。不要按 v3 那棵 9 级单树、也不要按 v4 那两棵树写测试期望。
7. **DB 列改名后，**五处**手写列名字符串要一起改**（§3.3）：
   `db/rows.rs:87`、`:94`、`db/sqlite/track.rs:184`、**`routes/today.rs:149`、`:162`**。
   它们**编译期不报错，运行时才炸**。**v5 更正**：v4 写「三处」并把 `today.rs` 归为机械改名。
8. **两个前端都要改**（§1.6）：`web/` 是今天在跑的那个，`fe/` 是还没上生产的那个。
   **`web/src/api/wire.ts:96-106` 是手写的，不是生成物**，它的 `Omit` 键是字符串字面量。
9. **合并树 grep（PR-1 验收 A7）抓不到全部名册查找**：`current_definition` 回落分支里那处
   开手写的 `WORKFLOW_TEMPLATES.iter().find(..)` 不吃它（§8.2 的第 8 个站点）。
10. **不要照抄本文里任何 `1230-s1` 行号（本文已全部删除）与生成物行号**：
    前者在四轮评审里动了四次（`b93fb767` → `7b85caa3` → `d51571d7` → `3b9cc03c`），
    后者被 `355807d6` 动过（文首基线注记）。
11. **`docs/deploy-and-upgrade.md` 属于 PR-2，不属于 PR-1**（§5.3 的 v5 归属更正），
    且备份那一段要自成小节、落在「## 8. Pre-flight checklist」里，
    不能挂在「插件兼容性」下面。
12. **§8.1b 已经由上游关闭**，不是待办（§8.1b 的 CLOSED 方框）。

### §10.1 §决策 D7 — 切片

用户明确不喜欢碎片化，且本次改动规模不大（核心是 `tracks.rs` 一段 54 行换成 ~30 行）。

#### S1 — 统一 create 路径（唯一必做切片）

* **范围**：新增 `workflow_template()` 与 `admit_template()`；删除
  `WORKFLOW_TEMPLATE_KEYS` / `is_workflow_template_key`；重写 `tracks.rs:761-793`
  并把 `:799-814` 的播种块搬到 `:899` 之前（§4.2）；删除 `:770-772` 的空白守卫；
  删除 `:779`；改写 `track_templates.rs` 的词汇缝段落与 `manifest.rs:93-100` 的字段文档；
  **`list_track_templates` 不动**（v3 撤回，§8.2）；改两处旧文案断言；
  新增测试 #8/#9/#12/#13 并加强 #10；写 `docs/deploy-and-upgrade.md` 的插件兼容性一节。
* **文件**（v1 漏了后三个；v3 又补两个）：
  * `crates/calm-server/src/routes/tracks.rs`
  * `crates/calm-server/src/routes/track_templates.rs`（**只改模块头的词汇缝段落**）
  * **`crates/calm-server/src/workflow_templates.rs`**（§8.2 的 dead-code 手术 + test mod）
  * **`crates/calm-server/src/plugin_host/manifest.rs`**（`:93-100` 的字段文档，§5.3）
  * ~~**`docs/deploy-and-upgrade.md`**~~ —— **v5 移到 PR-2**（§5.3 的归属更正）：
    该节引用的 400 正文是 PR-2 拼写，且备份姿态是 PR-2 的迁移 + breaking 判决的后果；
    **PR-1 单独落地是 `preserving`**，写这一节就是发一份描述自己并不产生的东西的文档
  * `crates/calm-server/tests/cases/track_workflow_templates.rs`（+4 case，改 `:586`）
  * **`crates/calm-server/tests/forge_workflow_e2e.rs`**（改 `:421-429`——注释 `:421-422`
    + 断言块 `:423-429`，其中文案在 `:427`；v2 写的 `:425-429` 与 §10.3 自相矛盾，v3 更正）
  * `crates/calm-server/tests/cases/track_templates_read.rs`（可能 +1 case）
* **可能被动到、但预期不需要改的第五个读播种路径的文件**（v3 新增，通道 A n3）：
  `crates/calm-server/tests/cases/track_workspace_materialize.rs:224-259`
  （`seeded_workflow_template_tracks_are_materialized`，POST `workflow_id: "small-change"`
  并断言每个 track 行都被物化）。它走的是 **201** 路径，搬位后仍绿——
  列在这里是为了让实现者**事先**知道它存在，而不是从 CI 里才发现。
* **不碰**：无。**v4 删掉了 v3 这一格的「不碰：FE、OpenAPI、wire、迁移」**——
  §3 的改名把这四样**全部**拉了进来。
* **v4 新增的范围（D2 改名，§3）**：请求体 / 领域模型 / DB 列三层改名 + 一条新迁移 +
  `#[serde(alias)]` 兼容读 + `WEB_COMPAT_VERSION`/`API_VERSION` 两个常量 +
  `productMajor` 默认值 + **两个前端** + OpenAPI 生成物重跑 + oracle 加一行。
* **v4 新增的文件**（在上面那张清单之外；层号对应 §3.2 的表）：
  * `crates/calm-truth/src/model.rs`（层 3）
  * `crates/calm-truth/src/db/rows.rs`、`crates/calm-truth/src/db/sqlite/track.rs`（层 5，**手写列名**）
  * `crates/calm-truth/migrations/00NN_tracks_rename_workflow_id_to_template_id.sql`（**新建**，层 5）
  * `crates/calm-types/src/model.rs`（层 4，**加 `serde(alias)`**）
  * `crates/calm-server/src/operation/planner_harness_start_adapter.rs`、
    `crates/calm-server/src/routes/today.rs`（层 6）
  * `crates/calm-server/src/routes/version.rs`（`WEB_COMPAT_VERSION`、`API_VERSION`，§3.6）
  * `crates/neige-app/src/package.rs`（默认 `productMajor`，§5.3；连带它的两条断言与
    `crates/neige-app/src/manifest.rs` 的解析断言）
  * OpenAPI 生成物 6 个（**跑生成器，不手改**，§3.2 层 7）
  * `web/` 与 `fe/` 的 wire schema / 调用点 / 测试（§3.2 层 8 的完整清单）
  * `fe/e2e/track-create.spec.ts`（**七处命中**：`:57,59,60,141,142,143,154`——v4 只列了四处）
  * **oracle 三份**（v4 只列了一份）：`docs/oracle/gates-types.yaml:1424`、
    `docs/oracle/a11y-contract.yaml:596`、`docs/oracle/pages-shared.yaml:3542,3586,3590`
  * `crates/calm-server/tests/goldens/events/track_updated.{full,min}.json`（`Track` 的产物）
  * **（v5 新增）** `crates/calm-server/src/plugin_host/workflow_input.rs`——模块名、
    `WORKFLOW_INPUT_MAX_BYTES`、`validate_workflow_input`、以及 `:247/:253/:264/:274/:278`
    产出的 `workflow_input.<key>:` 错误词汇（矩阵行 10 的正文来源）
  * **（v5 新增）** `crates/calm-server/src/routes/today.rs`——**两处字面 SQL**（`:149`、`:162`）
  * **（v5 新增）** `crates/calm-truth/src/db/rows.rs:94`（`TRACK_SELECT_COLUMNS_W`，v4 写成 `:95`）
  * **（v5 新增）** `web/src/api/wire.ts:96-106`（**手写，不是生成物**）、
    `web/src/track-fs-viewers/schemas.ts:152,160`（**第三个 Zod reader**）
  * **（v5 新增）** `crates/calm-server/tests/forge_workflow_e2e.rs:203`（**第三条文案断言**，§10.3）
  * **（v5 新增）** 测试侧原始 SQL 四处：`forge_workflow_e2e.rs:160,176`、
    `tests/support/planner_turn.rs:121`、`operation/child_track_adapter.rs:1350`（在 test mod 内）
  * **（v5 新增）** 字符串名册：`crates/calm-server/tests/cases/track_projection_policy_patch.rs:155`
  * **（v5 新增）** 注释 / 文档 / CSS：`web/src/shared/components/issueUrl.ts:1,6,57` + 其测试、
    `fe/core/domain/issue-url.ts:2,48` + 其测试、`web/src/calm.css:4414`、
    `fe/web/src/features/area/README.md:71`、
    `NewTaskForm.tsx` 的 `:124,171,369,459,569,751,765,769,1062`（**`:765`/`:769` 是用户可见
    文字与 aria-label，同时是两份 oracle 的锚点**）
  * **（v5 新增）** 生成物是**五个命中本字段的**，不是 v4 写的三个：
    `fe/core/api/generated/openapi.json`、**`fe/core/api/generated/wire.ts`**、
    `web/src/api/openapi.json`、`web/src/api/generated.ts`、**`web/src/api/generated-events.ts`**
  * **（v5 新增）** `docs/deploy-and-upgrade.md`（从 PR-1 移来：插件兼容性一节 + **备份小节** +
    `:26` 括注更正）
  * **长尾**：改完上面这些之后由**编译器**列出（类别 3）；
    **类别 2 的收尾靠验收 B10 的残留 grep，不靠编译器**（§3.2 末段）
* **是否保行为**：**不再是「几乎是」。** 三处有意变更 = §4.4 的变更 A（非模板 id 201→400）、
  变更 B（错误正文改写）、**变更 C（请求体字段改名 ⇒ 旧拼写 400）**，
  外加 §4.2 的副作用顺序修正，外加 §3.6/§5.3 让升级判决从 `preserving` 变成 `breaking`。
* **规模**：**本文继续不给行数**（v2 在这一格被两个通道判错过，理由是「没有测量依据」）。
  能测的两件事写在这里：(1) **v5 更正为可复现的数字**（v4 的「170」复现不出来）——
  `git grep -l 'workflow_id' -- 'crates/**/*.rs' | wc -l` = **168**，
  `git grep -l 'NewTrack {' -- '*.rs' | wc -l` = **147**，
  其中绝大多数是编译器会逐个报错的结构体字面量填充位（§3.2 类别 3）；
  **但类别 2 的站点编译器一个都抓不到**（§3.2 的诚实标注）；
  (2) 核心生产改动仍然只是 `tracks.rs` 一段 54 行换成 ~25 行。
  **这两个数字放在一起正好说明为什么要切成两个 PR。**

**⚠️ v4 的切片裁决：改成两个 PR。**

v1 把改名单独切出去的理由是「零行为、大 diff，混进来会淹没判据」。
人取消的是「不许改名」这个约束，**没有**取消那条理由——那条理由今天仍然成立
（§9 风险表 v4 新增的最后一行）。所以：

| PR | 内容 | 判据 |
|---|---|---|
| **PR-1（概念）** | §2/§4/§5/§7 的全部内容：`admit_template`、删 `:779`、删两个符号、播种搬位、`manifest.rs:93-100` 字段文档、`track_templates.rs` 模块头的**临时文本**（§3.9 方框）、测试 #8/#9/#12/#13 并加强 #10、§10.3 的三条腿（用 `workflow_id` 拼写）。**不碰 `docs/`、不碰 wire、不碰迁移** | **`:779` 消失且没换马甲。** diff 小、判据清晰、可逐行评审 |
| **PR-2（拼写）** | §3 的全部内容：类别 1/2/3 三类站点的改名 + `0079` 迁移 + `Track` 的 `serde(alias)` + **三个 Zod reader 的 normalize** + `WEB_COMPAT_VERSION`/`API_VERSION` + **三处版本常量的 CI 静态门禁**（§3.6 选项 a）+ `productMajor` 默认值 + 两个前端 + 生成物重跑 + oracle 三份 + `docs/deploy-and-upgrade.md`（插件兼容性一节 **+ 备份小节**）+ `track_templates.rs` 模块头的最终文本 + 测试 **#14/#15/#16/#17/#18** + **收尾残留 grep** | **零概念变化、机械可核。** 评审方式是「编译器 + 生成物 diff + 五条新测试 + 残留 grep」，不是逐行读 |

**⚠️ v5 更正两处（通道 A m5 + 通道 B M6，都判定成立）**：

* v4 的 PR-2 内容栏只写「测试 #14/#15」，判据栏却写「**三**条新测试」——自相矛盾。
  漏掉的第三条是 **#16 `old_field_spelling_is_an_unknown_field`**，
  而它是 **§3.5 整个旧拼写拒绝策略的唯一 pin**（矩阵行 18/19/20 全靠它）。
  v5 另新增 **#17（迁移保值 fixture，§3.3）** 与 **#18（Today launchpad 两条腿，§3.3）**，
  所以 PR-2 是**五**条新测试。
* `docs/deploy-and-upgrade.md` 从 PR-1 移到 PR-2（上面的文件表）。

**切线画在哪：PR-1 落地后 `POST /api/tracks` 的字段仍叫 `workflow_id`，
但它已经只有一个含义了。** PR-2 只换拼写。

**PR-1 自己的错误文案，明写（v5 新增，通道 A m7，判定成立）。**
v4 的 §4.4 变更 B 给的新文案用的是 PR-2 的字段名，于是 PR-1 的文案从未被写出来。
**PR-1 落的是：**

```
track create: `workflow_id` must reference a known track template; got `{id}`
```

**PR-2 再把它改成 `` `template_id` ``。** 也就是说这句文案在两个 PR 里各改一次。
**§10.3 的三条腿在两个阶段都成立**——它们钉的是
「`known track template` 在场 / `registered trusted workflow` 不在场 / 被拒的 id 被点名」，
**没有一条钉字段名**。**明说这一点，免得有人「好心」在 PR-1 加一条断言字段名的第四条腿，
然后在 PR-2 把它打红。**

**两个 PR 可以分别 merge，但发布必须绑在一起（v5 收紧，通道 B M6）**：
PR-1 单独发布是 `preserving`、PR-2 再来一次 `breaking`，用户吃两次重启。
**所以约束的落点不是「必须同一个 PR」也不是「必须同时 merge」，而是
「必须进入同一个 release artifact」**——这一条写进 PR-2 的描述与 release 检查表。
§5.3 的 `productMajor` 与 §3.6 的两个版本常量**都放在 PR-2**，
正是为了让「PR-1 已 merge、PR-2 未 merge」这个中间态**保持 `preserving`**、不惊动用户。

**若人更希望一个 PR**：合成一个也可以，代价是评审 PR-1 那部分判据的人要在一份
以改名为主的大 diff 里找它们。**这是留给人的选择，本文的推荐是两个。**
#### 验收判据（**v5 按 PR 分区；v4 把十条堆在 S1 名下、在切分宣布之前，等于把切分本身作废**）

> **通道 A M9 + 通道 B M6 都提出这一条，判定成立。** v4 的十条验收里
> 第 8 条用的是 PR-2 拼写（`{template_id:"small-change"}`）、第 9/10 条是 PR-2 专属、
> 1–7 条属 PR-1，而两 PR 切法在它们**之后**才宣布，且没有重新划分。
> **既然拆分的理由是「PR-1 的判据必须保持可评审」，判据不拆就没有拆分。**
> 下面按 PR 重排，并补上 v4 的 PR-2 完全缺失的五项
> （alias、三处版本常量、迁移保值、`productMajor`、旧拼写拒绝）。

##### PR-1 的验收（A1–A7）—— 判据是「`:779` 消失且没换马甲」

  A1. `grep -n 'is_workflow_template_key' crates/calm-server/src/` 返回空——**必要不充分**
     （符号没了 ≠ 特例没了，§4.3）；
  A2. 测试 #8 绿：running∧trusted 插件声明名册外 id，create 400。
     **反例**：若谁给 `admit_template` 加一条 `.or_else(|| resolve_trusted_workflow(..))`
     兜底，这条必须红；
  A3. 测试 #9 绿：`GET /api/track-templates` 列出的**每一个** id，`POST /api/tracks`
     **返回 201**（**v4 恢复 `== 201`，撤回 v3 的弱化；理由见 §10.2「#9 的形状」**）。
     **反例**：任何让写口特别拒绝一个已列出 id 的改动必须红——**包括换了措辞的特例**
     （`if id == "investigation" { return Err(BadRequest("investigation is disabled")) }`），
     而 v3 的弱版对这个反例是**绿**的。
     **诚实标注**：反方向（写口不收未列出的 id）由 #8 + 两个手写 sentinel 承担，
     那是**抽样**，不是集合相等——见 §10.2「#9 的形状」；
  A4. **（v4 更正，通道 A m1，判定成立：v3 那句「逐行」是假的；v5 按 PR 分区再更正一次）**
     §4.4 矩阵的**行 1–12b / 15 / 16** 逐行落到 §10.2 的编号上（这些是 PR-1 的面）；
     **行 18–20 属于 PR-2**（由 #16 承担，见下面的 B5）；
     **行 13 / 14 / 17 / P1 是有意不钉的**——13/14 是 `ensure` 内部失败的两种形状、
     17 是阶段 2 事务内的显式 fork 400、**P1 是阶段 3 的物化失败**
     （它其实**已经**被 `track_workspace_materialize.rs:270-313` 钉住了，
     只是钉的是「孤儿状态是已知的」，不是「无副作用」）。
     §10.2 #13 的「不在范围内」一栏（以及 §4.2 的裁决）明确拒绝为 13/14/17 写「无副作用」断言。
     v3 一边写「逐行有断言」、一边在 §10.2 #13 里亲手把行 17 排除掉，是本文内部的一处自相矛盾；
  A5. `TrackTemplate` 字段集合与 `fe/core/domain/track.ts:198-211` 未变——兑现
     `track_templates.rs:39` 的承诺，可机检。**v4 复查：§3 的改名不影响这条**
     （该端点的响应里没有 `workflow_id` 字段，§1.5 的 v4 复读）；
  A6. `cargo build --release -p calm-server --locked`（复刻 `ci.yml:901`）与
     `cargo clippy --workspace --all-targets --features calm-server/codex-e2e -- -D warnings`
     （复刻 `ci.yml:304-305`）**都**本地绿。
     **v3 更正理由**：v2 写「`cargo clippy --all-targets` **不能**替代 release 构建」，
     **是错的**——就 dead-code 这个风险而言 clippy 其实**先**红（§8.2 有实跑证据）。
     两条都保留是因为它们复刻的是 CI 的两个不同作业，不是因为其中一条看不见死代码；
  A7. **（v3 新增，针对 §8.2 的合并树风险）** 在**合并后的树**上
     `git grep -n 'WORKFLOW_TEMPLATE_KEYS\|is_workflow_template_key' -- crates/` **零输出**。
     **正例/反例成对给出**：只在 #1209 分支上跑必绿（那侧只有 2 处），
     所以这条**必须**在合并树上跑；#1230-first 且漏改 `known_template` 那个生产使用点
     或那 6 处 test 站点时，这条必须红——而 git 不会给任何冲突提示（§8.2 顺序 2/3）。
     **诚实标注**：这条 grep **不覆盖** `current_definition` 回落分支里那处开手写的
     `WORKFLOW_TEMPLATES.iter().find(..)`（§8.2 的第 8 个站点）——它不用这两个符号。
     那一处两侧都不动，所以不需要门禁；写在这里是为了不让人以为 A7 是全称的。

**⇒ PR-1 到此为止。它不碰 wire、不碰 DB 列、不碰 `docs/`、不碰前端。**
**删掉 `:779` 的就是这一片。**

##### PR-2 的验收（B1–B10）—— 判据是「零概念变化、机械可核」

  B1. **（往返，针对 §3.3 手写列名站点 #1/#2/#3）** 一次**真跑的往返**：
     `POST /api/tracks {template_id:"small-change"}` → 201 → `GET /api/tracks/{id}`
     → 响应里 `template_id == "small-change"`。
     **正例/反例成对**：改完 `db/rows.rs:87`/`:94` 与 `db/sqlite/track.rs:184` 之后必绿；
     **漏改其中任何一处**，这条必须红（`sqlx` 的列名错误发生在**运行时**，
     `cargo build` 与 `clippy` 都是绿的，CLAUDE.md「Card Column Add SELECT Audit」）。
     **v5 的诚实标注**：**这一条碰不到 `today.rs`**——它是 B2 存在的全部理由。
  B2. **（v5 新增，本轮 BLOCKER 的验收；测试 #18）** **Today launchpad 两条腿都真跑**：
     (a) 空库上打 Today launchpad，走 `today.rs:162` 的 INSERT；
     (b) 先造一个 `purpose IS NULL AND title='Today'` 的旧行再打，走 `:149` 的 UPDATE。
     **正例/反例成对**：两处都改完 ⇒ 两条腿都绿；
     **只改其中一处** ⇒ **恰好一条腿红**（这也是「必须两条腿都测」的证明——
     测一条时另一条的漏改是绿的）。
  B3. **（v5 新增；测试 #17）** 迁移保值 fixture：停在 `0078` → 写入两列非 NULL 的旧列值 →
     应用 `0079` → 断言新列值**逐字保留** ∧ 旧列**不存在**。
     **反例**：把 `RENAME COLUMN` 写成 `ADD COLUMN` + `DROP COLUMN`（丢值）⇒ 必须红。
  B4. **（v5 新增；测试 #14）** 历史兼容读：Rust 侧 + **三个** Zod parser 各一条。
     **反例**：拿掉 `calm_types::Track` 上的 `#[serde(alias = "workflow_id")]` ⇒ Rust 那条红；
     把 `web/src/track-fs-viewers/schemas.ts` 的 normalize 去掉 ⇒ 第三条红
     （**这一条是 v4 完全没有的**，v4 只安排了「两个前端各一条」）。
  B5. **（v5 新增；测试 #16）** 旧拼写拒绝：矩阵行 18/19/20 三种输入参数化，全部 400。
     **反例**：给 `CreateTrackRequest` 加回一个 `workflow_id` 字段（哪怕只是 `#[serde(alias)]`）
     ⇒ 必须红。**这是 §3.5 整个拒绝策略的唯一 pin**，v4 把它从 PR-2 的内容栏里漏掉了。
  B6. **（v5 新增；§3.6 选项 a）** 三处 `WEB_COMPAT_VERSION` 的 CI 静态门禁。
     **正例/反例成对**：三处都是 17 ⇒ 绿；**任意一处**改回 16 ⇒ 红。
     **今天这个反例是绿的**——这就是这条门禁必须新建的理由。
     另：测试 #15 绿（服务端 floor 严格大于 16）。
  B7. **（§5.3）** `productMajor` 默认值 0→1，且 `package.rs:546` 的断言被同步改成 1。
     **反例**：只改升级说明、不改 `package.rs:307` ⇒ 机器判决仍是 `Preserving`，本裁决落空；
     只改默认值不改断言 ⇒ `package.rs:546` 红。
     **不要把 `manifest.rs:302` 算进来**——它是 parser fixture，改默认值它不会红（§5.3 方框）。
  B8. **（生成物）** `cd web && npm run gen:api`（`ci.yml:1186-1187`）与
     `cargo run --bin emit-openapi > fe/core/api/generated/openapi.json`（`ci.yml:1190`）
     之后 `git diff --exit-code`（`ci.yml:1194`，覆盖 7 个产物）**为绿**。
     **反例**：手改了产物而没重跑生成器 ⇒ 红；改了 `CreateTrackRequest` 而没重跑 ⇒ 红。
  B9. **（两个前端）** `fe/` 与 `web/` 各自的 vitest 都绿。
     **反例**：只改了 `fe/` 而没改 `web/` ⇒ `web/src/api/schemas.test.ts` 与
     `web/src/shared/components/NewTaskForm.issueDev.test.tsx` 必须红（§1.6 的理由）。
  B10. **（v5 新增，PR-2 的收尾门禁，也是本设计对扫描完整性的唯一真保证）**
     §3.2 末尾那条**残留 `git grep` + allowlist** 零输出。
     **正例/反例成对**：把 `today.rs:149` 的列名留成旧拼写 ⇒ **必须有输出**；
     把 `0059_waves_workflow_id.sql` 留成旧拼写 ⇒ **零输出**（allowlist 第 1 项，正确行为）。
     **allowlist 的每一项都要在 PR 描述里逐条说明凭什么在那里**——
     一张没有理由的 allowlist 就是一张遮羞布。

**⇒ 发布约束（不属于任何一个 PR，属于 release）**：PR-1 与 PR-2 必须进入**同一个 release
artifact**。检查方式：release 前确认两个 PR 的 commit 都在待发布 ref 的祖先里。

#### S2 — `workflow_id` → `template_id` 机械改名（**v4：并入本切片，见上面的 PR-2**）

**v1–v3 把这一片列为「不排期」，v4 撤回。** 它的调用方清单已经从「一段散文」升级成
§3.2 那张分层表（v4 自己扫的），迁移策略见 §3.3，兼容读见 §3.4，
旧拼写的拒绝策略见 §3.5，旧前端的硬失败见 §3.6，`workflow_input` 的配对改名见 §3.8。

**保留 v1 那条理由中仍然为真的部分**：它零概念、大 diff，和 PR-1 的判据混在一起会淹没后者。
所以它不是「不做」，而是**做，但单独一个 PR，与 PR-1 同一次发布**（上面的切片裁决表）。

#### S3 — 插件贡献 template（**非目标，见 §9**）

---

### §10.2 §决策 D8 — 测试 / oracle 计划

对每条断言给出「什么样的生产改动会让它变红」（mutation 判据）。
**「测试真的断言了这条」与「测试路过了这条分支」分列**——v1 在 #3/#4 上混了，v2 更正。

| # | 不变量 | 钉它的测试 | 位置 | 该测试**断言了**什么（逐字核过） | 生产侧变异 ⇒ 变红 |
|---|---|---|---|---|---|
| 1 | 未知 id ⇒ 400，**且拒绝理由是准入而不是 registry** | `unknown_workflow_id_still_400s` | `tests/cases/track_workflow_templates.rs:568-589` | 状态码 400（`:582`）+ 错误子串（`:586`，**本切片要改成 §10.3 的三条腿**） | (a) 把 `ok_or_else(..)?` 换成 `unwrap_or(None)`；(b) **NEW：把错误文案「恢复 registry 措辞」**（改回 `registered trusted workflow`）——v2 版断言对 (b) 是绿的，v3 版必须红 |
| 2 | 无绑定 template ⇒ 201 + fork | `investigation_and_small_change_auto_fork_without_plugin` | 同上 `:450` | 201 + fork 到模板 report | 让 `admit_template` 在 `binding.is_none()` 时返回 `None` |
| 3 | 有绑定 template ⇒ `plugin_scope` 落库 | `git_forge_workflow_registers_and_track_create_binds` | `tests/forge_workflow_e2e.rs:120` | 201（`:155`）、`workflow_id` 回显（`:156`）、`plugin_scope`（`:157`）、`workflow_input` 回显（`:158`）、DB 里的 `workflow_id`（`:165`）与 `plugin_scope`（`:172`）。**不断言模板 report 被 fork**——v1 把 `:120-171` 当成 fork 的 pin，错 | 删掉 `p.plugin_scope = bound_plugin.map(..)` 那行 |
| 4 | 绑定插件 **untrusted** ⇒ 仍 201，`plugin_scope=null` | 同文件（无独立测试名，在 `git_forge_workflow_registers_and_track_create_binds` 尾部） | `tests/forge_workflow_e2e.rs:434-454` | 201（`:449`）、`workflow_id` 回显（`:450`）、`plugin_scope` 为 null（`:451-454`）。两处 v1 说错了：(a) **只测 untrusted**，`stop(PLUGIN_ID)` 在 `:456-459` 且其后**没有再 create**，所以「stopped ⇒ 201」这一半**今天无人钉住**；(b) 注释 `:446-448` 说「the template report is still forked」，但这段**没有任何 fork 断言**——fork 由 `track_workflow_templates.rs:450` 的测试 #2 承担 | 让 `admit_template` 要求 `binding.is_some()` |
| 5 | 每个 key 只播种一个 track（幂等） | `matching_workflow_id_seeds_one_track_per_template_key` | `tests/cases/track_workflow_templates.rs:209` | 每个 key 恰好一个 system-area 模板 track | 去掉 `lookup` 早退（`tracks.rs:450-453`） |
| 6 | 用户 area 里伪造同 key overlay 不能劫持 fork | `stolen_user_area_template_key_does_not_hijack_auto_fork` | 同上 `:481` | fork 源仍是 system area 的那一个 | 去掉 `lookup_workflow_template_track` 的 `track.area_id == system.id` 过滤（`tracks.rs:505-507`） |
| 7 | 显式 `fork_report_from` 优先 | `explicit_fork_report_from_is_not_overwritten` | 同上 `:383` | fork 源是调用方指定的那个 | 把 `if fork_report_from.is_none()` 改成无条件赋值 |
| 8 | **NEW**：受信运行插件声明的非模板 workflow id ⇒ 400 | **PROPOSED** `plugin_declared_non_template_workflow_id_is_rejected` | 新增到 `tests/cases/track_workflow_templates.rs` | — | 在 `admit_template` 里加回 `.or_else(\|\| resolve_trusted_workflow(..))` 兜底 |
| 9 | **NEW**：读口列表与写口准入是同一个集合（路由 × 路由） | `listed_template_keys_create_their_exact_recipes`（合并后） | 新增，驱动真路由 | — | 任何让写口特别接受一个未列出 id、或特别拒绝一个已列出 id 的改动 |
| 10 | 读不触发播种（INV-1209-SEED v2，§7） | #1230 S1 带了弱版本，**本切片加强** | #1230 侧 `tests/cases/track_workflow_templates.rs` 的那条 read-only case | 今天只断言：未播种态下两个 GET 之后 `kind=="template"` 的 overlay 仍为空 | 见下文「#10 的形状」的 5 条变异清单 |
| 11 | 读口 `input_schema` 与 create 的接受面一致 | `bound_template_carries_the_plugin_input_schema` + `unbound_templates_carry_no_input_schema` | `tests/cases/track_templates_read.rs:243`、`:333` | 绑定态有 schema / 无绑定态无 schema | 让读口不走 `resolve_trusted_workflow`（例如硬编码 id 白名单） |
| 12 | **NEW**：空白 `workflow_id` ⇒ 以**准入**理由 400，且零播种副作用（今天 Rust 侧零覆盖，§4.1 删了那道守卫） | **PROPOSED** `blank_workflow_id_is_rejected` | 新增到 `tests/cases/track_workflow_templates.rs` | — | **v3 更正**：有人把守卫「恢复」成一个 **skip**（`if id.trim().is_empty() { /* 当作没选模板 */ }` ⇒ 201、`plugin_scope=null`、不 fork） |
| 13 | **NEW**：**事务前**的 4xx 不留播种副作用（§4.2） | **PROPOSED** `pre_transaction_4xx_with_template_does_not_seed`（参数化） | 新增到 `tests/cases/track_workflow_templates.rs` | — | 把播种块搬回 `tracks.rs:761` 之后、cwd/area 校验之前（即今天的顺序） |
| 14 | **NEW（v4，§3.4；v5 从「2 个 parser」扩到「3 个 parser」）**：历史事件 / 旧 snapshot 里的旧字段名仍然读得出来 | **PROPOSED** `legacy_track_payload_keeps_its_template_id` | **四条**：Rust 侧一条 + **三个 Zod parser 各一条**（`fe/core/api/schemas.ts`、`web/src/api/schemas.ts`、**`web/src/track-fs-viewers/schemas.ts`**） | — | Rust：**拿掉 `calm_types::Track` 上的 `#[serde(alias = "workflow_id")]`** ⇒ 旧 golden 解析出 `template_id: None` ⇒ 必须红。**每个 Zod parser 各自**：去掉它的 normalize ⇒ 那一条红。**四条要能各自独立变红**——共用一个 helper 会让「只漏改第三个 reader」这个真实的回归方向变绿 |
| 15 | **NEW（v4，§3.6；v5 更名并降低 claim）**：**服务端**的兼容 floor 真的抬了 | **PROPOSED** `web_compat_floor_is_above_the_previous_bundle` | 新增到 `tests/cases/version.rs` 附近 | — | 把 `WEB_COMPAT_VERSION` 改回 16 ⇒ 必须红。断言 `GET /api/version` 的 `minWebCompatVersion` **严格大于** 16（把 16 写成测试里的字面量常量，配一句注释说明它是历史值、不要跟着改）。**⚠️ v5：它不是「三方 lockstep pin」**——它看不见任何前端，两个 bundle 的漂移由 §3.6 选项 (a) 的 CI 静态门禁承担（验收 B6） |
| 16 | **NEW（v4，§3.5）**：写口只认识一个拼写 | **PROPOSED** `old_field_spelling_is_an_unknown_field` | 新增到 `tests/cases/track_workflow_templates.rs` | — | 给 `CreateTrackRequest` 加回一个 `workflow_id` 字段（哪怕只是 `#[serde(alias)]`）⇒ 必须红。参数化到矩阵行 18/19/20 三种输入。**这是 §3.5 整个拒绝策略的唯一 pin**（v4 把它从 PR-2 的内容栏里漏了） |
| 17 | **NEW（v5，§3.3）**：`RENAME COLUMN` 真的保值 | **PROPOSED** `rename_migration_preserves_column_values` | 新增到 `crates/calm-truth` 的迁移测试族（形状抄 `track_plugin_scope_migration_tests.rs` 的「停在某个版本」配方，但**停在 `0078`**） | — | 停在 `0078` → 写入两列**非 NULL** 的旧列值 → 应用 `0079` → 断言新列值逐字保留 ∧ 旧列不存在。**变异**：把迁移写成 `ADD COLUMN` + `DROP COLUMN`（丢值）⇒ 必须红；只 rename 一列 ⇒ 必须红。**没有这条，「迁移不丢数据」这句话在本设计里没有载体** |
| 18 | **NEW（v5，§3.3，本轮 BLOCKER 的 pin）**：Today launchpad 的两条字面 SQL 都还能跑 | **PROPOSED** `today_launchpad_survives_the_column_rename`（两条腿） | 新增到 `routes/today.rs` 的路由测试 | — | (a) 空库 ⇒ 走 `today.rs:162` 的 INSERT；(b) 预置一个 `purpose IS NULL AND title='Today'` 的旧行 ⇒ 走 `:149` 的 UPDATE。**变异**：只把两处中的**一处**留成旧列名 ⇒ **恰好一条腿红**。这条成对性本身就是「必须测两条腿」的证明 |

#### 关于 #8 的测试设计（最重要的一条）

它必须**驱动真路由**：注册并 spawn 一个受信 stub 插件，其 manifest 声明
`workflows: [{"id": "not-a-template"}]`，然后 `POST /api/tracks` 带
`workflow_id: "not-a-template"`，断言 400。
stub 的搭法可以抄 `tests/cases/track_templates_read.rs:77-167`（`boot(running: bool)`，
那里已经有一个「受信 + running」的 stub 插件 boot 流程；`Manifest::parse` 在 `:98`，
`json!` 字面量 `:99` 起，`"input_schema": stub_input_schema()` 在 **`:106`**，
`"workflows": [ { "id": ISSUE_DEVELOPMENT } ]` 在 **`:107`**）。

> **⚠️ v3：照 v2 的配方直接抄会造出一个假绿测试。这是第 2 轮最锋利的一条
> （通道 B M3），重扫判定成立——而 #8 正是整个「删掉 `:779`」保证所依赖的那条测试。**
>
> 那个 stub 的 manifest **带 `input_schema`**（`track_templates_read.rs:106` →
> `stub_input_schema()` 在 `:57-64`，`"required": ["issue_url"]` 在 `:61`）。
> 于是一个**不带 input** 的 `POST` 会先撞上 required-input 400
> （`tracks.rs:977-990`，正是 §4.4 行 6/16 的那条），**而不是**准入 400。
> 后果：即便有人把「非模板插件 fallback」原封不动加回去（= `:779` 换马甲），
> 这个测试**照样绿**——它检测不到它唯一存在的理由。
>
> **必须三条一起做**：
> 1. **stub 不带 `input_schema`**（或者 `POST` 带上通过该 schema 的合法 input）——
>    抄 boot 流程可以，**不要**抄 `stub_input_schema()`；
> 2. 断言的不只是状态码，还有**拒绝理由**：正文含 `known track template`
>    **且不含** `requires \`workflow_input\``、**且不含** `registered trusted workflow`；
> 3. 同时断言**零播种副作用**（复用 #13 的 helper）——准入 400 发生在播种之前。
>    **⚠️ v4 降级这一条（通道 A m5，重扫判定成立）**：v3 把它写成三条「硬要求」之一，
>    暗示三条**各自**都在鉴别。**第 3 条其实什么都没钉住**：一个名册外的 id
>    在**正确代码**下不播种，在**被点名的那个变异**（加回插件 fallback）下也不播种——
>    因为播种的条件是名册命中。**真正承载 #8 的是第 1、2 条腿。**
>    第 3 条作为**便宜的保险**保留（它能抓住一个「fallback 被塞进模板播种分支」的变体，
>    那时会播种后 lookup 失败成 500），但**不要再把它算作三条鉴别腿之一**。
>
> **变异判据（正例/反例成对）**：在 `admit_template` 里加回
> `.or_else(|| resolve_trusted_workflow(..))` 兜底 ⇒ #8 **必须红**（且是红在状态码上，
> 不是红在文案上）；只把错误文案改回旧措辞 ⇒ #8 也必须红（第 2 条断言）。

**不要**用「直接调 `admit_template` 返回 None」这种单测代替：那是在测函数，
不是在测准入。参考 CLAUDE.md「Test Must Drive Production Wiring」。

#### #9 的形状（v2 重写：删掉假门禁那一半）

v1 的 #9 有一半是假门禁（通道 A 提出，重扫**判定成立**）：v1 断言
`GET /api/track-templates` 的 id 集合 == `WORKFLOW_TEMPLATE_KEYS`，但
`list_track_templates` 直接遍历 `WORKFLOW_TEMPLATES`（`track_templates.rs:103-104`），
统一后 `admit_template` 也遍历同一个数组。**不存在能编译通过又让两者不等的单点变异。**
v1 声称的变异（「名册里加一个 key 而不加常量内容 ⇒ 红」）实际是经**另一条**机制变红的：
`workflow_template_report` 返回 `None`（`workflow_templates.rs:49`）→
`seed_workflow_template_track` 抛 `CalmError::Internal`（`tracks.rs:523-527`）→ POST 腿 500。
和集合相等那一半无关。

v2 的 #9 **不引用任何 Rust 常量**，只对比两条路由。但 v2 写的那个形状有两处毛病，
两个通道各抓到一处，**都判定成立**：

**毛病 1（通道 A J3）：「每个列出的 id ⇒ 201」这条腿在生产里是假的。**
当 git-forge running ∧ trusted 时，`issue-development` **会被列出**
（`routes/track_templates.rs:100-111`），而一个**不带 `workflow_input`** 的 POST 是
**400**——这正是本文自己矩阵的行 6（`tracks.rs:986-989` + `plugins/git-forge/manifest.json:299`
的 `"required": ["issue_url", "repo", "issue_number"]`）。
v2 的测试之所以会绿，只是因为 `tests/cases/track_workflow_templates.rs:46` 的 `boot()`
**不启动任何插件**——一条**未言明的前提**。而测试 #8 马上就要在**同一个文件**里
引入一个受信 running 的 stub 插件；谁把 fixture 提到共享 helper，#9 就会因为
**与准入无关**的理由变红。

**毛病 2（通道 B M4）：负方向只有两个手写 sentinel，不是集合相等。**
若写口额外接受一个 `legacy-workflow`，而那两个 sentinel 仍被拒，测试继续绿，
但「写口不收任何读口没列的 id」已经被违反。**v2 把它称作「集合相等门禁」是名不副实的。**

**v4 的 #9（恢复 `== 201`，撤回 v3 的弱化）**：

> **v3 错在哪（通道 A M1，重扫判定成立）。** v2 断言「每个列出的 id ⇒ 201」，
> 通道 A 在第 2 轮说这条腿「在生产里是假的」（git-forge running 时
> `issue-development` 无 input 会 400），v3 于是把它弱化成
> 「正文不含 `known track template`」。**但 v3 在同一段里还钉了另一个前提：
> `boot()`（`tests/cases/track_workflow_templates.rs:46`）不启动任何插件。
> 那个前提把原来的反对意见整个消掉了**——没有 running 插件时
> `resolve_trusted_workflow` 返回 `None`（`tracks.rs:937-950`），
> `validate_workflow_input_binding(None, None)` 走 `:962-970` 的早退 `Ok(())`
> （**实测 `tracks.rs:972` 就是 `(None, None) => Ok(())`**），
> `issue-development` **根本走不到** `:977-990` 的 required-input 臂。
> **所以在写死的前提下，每个列出的 id 都必须 201。**
>
> 弱化的代价是纯损失：一个换了措辞的特例
> ——`if id == "investigation" { return Err(BadRequest("investigation is disabled")) }`
> ——正文里没有 `known track template`，**弱版 #9 保持绿**，
> 而 §10.1 的 PR-1 验收 **A3** 白纸黑字说这种改动必须红。

```
let listed: Set<String> = GET /api/track-templates 返回的 id 集合;
assert!(!listed.is_empty());                       // 空集会让下面的全称量词平凡为真

// 正方向：全集，断言状态码本身
for id in &listed {
    let (status, body) = POST /api/tracks { template_id: id, /* 无 template_input */ };
    assert_eq!(status, 201, "listed template `{id}` was not creatable: {body}");
}

// 负方向：抽样，不是集合相等
assert POST { template_id: "definitely-not-a-template" } == 400 + `known track template`
assert POST { template_id: "issue-development-x" }       == 400 + `known track template`   // 近似串
```

* **前提，显式写进测试的文档注释里，并且这次它是承重的**：本 case 依赖
  `boot()`（`tests/cases/track_workflow_templates.rs:46`）不启动任何插件。
  **`== 201` 的正确性依赖它**（见上面的方框）。
  若将来该 harness 长出插件 fixture，本 case 必须显式使用「无插件」的那一支；
  **若前提被放宽**，断言改成显式允许清单，而**不是**退回 v3 的弱版：
  ```
  assert!(status == 201
      || (status == 400 && body.error.contains("requires `template_input`")),
      "listed template `{id}`: {status} {body}");
  ```
  这一条仍然排除掉「特别拒绝一个已列出 id」的所有措辞变体。
  （这也是对「#8 会污染同文件 fixture」这个风险的书面处置。）
* **正方向是全集**：任何让写口特别拒绝一个已列出 id 的改动都会红，**包括换措辞的**。
* **负方向诚实降级为抽样**：结构上的保证由「写口只有 `workflow_template()` 这一条
  可失败名册查找链」承担——**派生优于测相等**（CLAUDE.md「Mirror Code Must Call The Original」），
  外加 #8 作为「插件 fallback」这个具体复发形状的定向反例。
  **不再宣称 #9 是集合相等门禁。**

它仍然是 §4.3 那条语义判据的可执行版本，只是把它的强度写诚实了：
「写口的接受面 ⊇ 读口的列表」是被全称量化钉住的；
「⊆」只被抽样 + 结构论证覆盖。

**加一条来自 §8.2 的**：删掉 `WORKFLOW_TEMPLATE_KEYS` 之后，
「两份数组漂移」这个失败模式在类型上就不存在了，因此不需要为它写集合相等测试——
这正是「派生优于测相等」（CLAUDE.md「Mirror Code Must Call The Original」）。

#### #10 的形状（INV-1209-SEED v2 的可执行版本）

对 `GET /api/track-templates` 与 `GET /api/track-templates/{id}` 各跑一遍，
**每遍在两种起始状态下各跑一次**：

```
// 起始状态 A：未播种（今天 #1230 只测了这个）
// 起始状态 B：已播种（先 POST /api/tracks {workflow_id:"small-change"} 触发 ensure）
let before = snapshot(&repo).await;   // areas / tracks / cards / **全部** overlays
                                      // + area_folders + (events.count, events.max_id)
                                      // + 三个模板 track 的 report payload（或 doc_rev）
let (status, _) = get(app, uri).await;
assert_eq!(status, OK);
assert_eq!(snapshot(&repo).await, before);
```

`snapshot` 是新 helper，不复用 `seeded_templates`（`tests/cases/track_workflow_templates.rs:168-184`）
——后者只枚举 overlay，正是 §7 列出的那批漏网改动能溜过去的原因。
**v3 相对 v2 多了三张表**（`area_folders`、`events` 的 `(count, max_id)`、
以及**不再筛** `kind=="template"` 的全部 overlay），理由见 §7 的改名段：
`log_pure_event`（`crates/calm-truth/src/db/mod.rs:683`）让「加一行 event」成为一次
v2 版快照看不见的写。

> **⚠️ v4 补足：v3 说这个 helper 是「同一个 sqlx helper 再加两条 SELECT」，这句话是错的
> （通道 A m2，重扫判定成立）。** 测试拿到的是 `Arc<dyn Repo>`，**不能写裸 SQL**，
> 只能用 trait 上有的方法。逐条对照 `crates/calm-truth/src/db/mod.rs`：
>
> | 快照的一栏 | trait 上有没有现成的 | 怎么取 |
> |---|---|---|
> | areas | **有** | `areas_list()`（`:293`） |
> | area_folders | **有** | `area_folders_list_all()`（`:316`） |
> | tracks | **有，但按 area 分片** | `tracks_by_area(area_id)`（`:322`）对 `areas_list()` 的每个 area 跑一遍 |
> | cards | **有，但按 track / 按 area 分片** | `cards_by_track(track_id)`（`:380`）对上一行枚举出的每个 track 跑一遍 |
> | **全部** overlays | **没有** | 只有 `overlays_by_kind(entity_kind)`（`:436`，参数是**entity kind** 不是 overlay kind；既有 helper 传的是 `"view"`，见 `track_workflow_templates.rs:169`）。**helper 必须自己枚举全部 entity kind 并逐个调**，并在文档注释里写明这份枚举是**手工维护**的——新增一种 entity kind 而不更新它，#10 会静默失去覆盖 |
> | events 的 `(count, max_id)` | **没有 count** | `events_latest_id()`（`:830`）只给 max_id；count 用 `events_raw_window_since(0, probe_limit)`（`:780-784`，返回 `(count, max_id)`，**注意它是 bounded probe，`probe_limit` 要给得足够大**） |
> | 三个模板 track 的 report | **有** | 走既有的 report 读路径（同 §8.1b 用的那条） |
>
> **可实现，但不是「两条 SELECT」**，实现者要按上表逐项接线。
> **两处最容易写错**：全 overlay 那栏（会不自觉地退回 `overlays_by_kind("view")`
> 从而复现 v3 要修的那个漏洞）和 events count 那栏（`events_latest_id` 只有 max_id，
> 单靠它检测不到「删一行再加一行」）。

**变异清单（每条都必须让 #10 红）**：在 `list_track_templates` 开头加
`ensure_workflow_templates`；在 GET 里 `ensure_system_area`；在 GET 里改写某个模板
report 的 summary；在 GET 里建一个不带 overlay 的 track；只在「已播种」分支里写一次；
**（v3 新增）**在 GET 里 `log_pure_event` 记一条纯事件。最后三条今天是绿的。

**#10 不覆盖的（诚实记账）**：起始状态 C = 「已播种但 report 读不出来」。
处置见 §7 末段与 §8.1b——靠把 #1230 的 `current_definition` 钉成「上抛 500」
来消灭这一态，而不是靠给 #10 加一个人造损坏状态。
**✅ v5：这个前提已经由上游满足**（`1230-s1@3b9cc03c` 的 `current_definition` 两个 `?` 都在，
§8.1b 的 CLOSED 方框）。所以这不再是一个「悬着的依赖」，而是一个**已成立的事实**：
状态 C 在两条 GET 上就是一个 500，不可能是一次静默的写，#10 缺这一态无害。

#### #12 / #13 的形状（v3 补足）

**#12 `blank_workflow_id_is_rejected`**（通道 A m1 + 通道 B m3，都判定成立）：

* **输入统一用三个空格 `"   "`**——与 §4.4 行 2 一致。v2 的矩阵写三个空格、
  变异描述却说空串，v3 统一到 `"   "`（`trim()` 语义下两者同类，但文档不该自相矛盾）。
* **必须排除「因为别的校验才 400」**：请求用**合法**的 `area_id`、省略 `cwd`、不带
  `workflow_input`，并断言正文含 `known track template` **且**回显了那个空白 id。
* **必须断言零播种副作用**（复用 #13 的 helper）。
* **变异（v3 更正，v2 的「让名册查找对空串返回 `Some(..)`」不是现实的生产编辑）**：
  有人把被删的守卫「恢复」成一个 **skip** ——
  `if id.trim().is_empty() { /* 当作没选模板 */ }` ⇒ 201、`plugin_scope=null`、不 fork。
  这是删掉 `tracks.rs:770-772` 之后**真正会招来的**回归方向，且**只有 #12 会红**
  （#1 `unknown_workflow_id_still_400s` 发的是 `"missing-workflow"`，不受影响）。
  按 v2 的写法，#12 与 #1 之间不存在任何能让前者红、后者绿的单点变异——那才是真问题。

**#13 `pre_transaction_4xx_with_template_does_not_seed`**（通道 A J4，判定成立）：

* **参数化到三条事务前的 4xx**（v2 只有第一条）：
  1. `area_id` 不存在 ⇒ 404（`tracks.rs:863-867`）——矩阵行 12；
  2. `cwd` 非绝对路径 ⇒ 400（`tracks.rs:823-828`）——矩阵行 12a；
  3. **显式给一个不是 git 仓库的绝对路径 `cwd`** ⇒ 400（`tracks.rs:843-847`）——矩阵行 12b。
     **v4 更正（通道 A n2，判定成立）**：v3 这条腿写的是「`attach_folder` 目标」，
     **归因错了**。`tracks.rs:843` 的守卫是 `if !cwd_omitted`，**只看 `cwd` 给没给**，
     与 `attach_folder` 无关（实测）。测试要**显式不设** `attach_folder`
     才能证明这条腿测的是 cwd 校验；把它写成「设 `attach_folder: true`」
     会让人以为守卫在那个字段上，下一轮又要重扫一次。
* 每条都带一个**名册内**的 `workflow_id`，断言状态码 + `snapshot` 与请求前逐字节相等。
* **变异**：把播种块搬回今天的位置（`tracks.rs:761` 之后）⇒ 三条**全部**变红。
* **不在 #13 范围内**：矩阵行 17（显式 fork 的事务内 400）与 409/500——
  它们在事务内判定、在播种之后，§4.2 已裁决接受并记账，**不给它们写「无副作用」的断言**
  （写了就是钉一句代码不打算说的话）。

#### #14 的形状（**v5 新增：点名它的载体，否则实现者会用错形状**）

**通道 A m4，判定成立。** v4 只说「拿一条真的旧 golden 喂进去」，没说用哪套机制。
仓里已经有正好合适的一套：**`crates/calm-server/tests/cases/event_serde_goldens.rs:11-27`**
的模块 doc 定义了 golden 文件格式，把每份 golden 分成两半：

* **`wire`** —— 喂给 `Deserialize` 的输入；
* **`canonical`** —— 期望的 `Serialize` 输出（省略时默认等于 `wire`）。

每条测试跑三步（`:22-27`）：① `wire` → `Event`，与 in-code 期望值比对；
② 序列化期望值，与 `canonical` 做 canonical-JSON 相等；
③ **断言 `canonical` 是 serde 不动点**（反序列化再序列化返回它自己）。

**⇒ #14 的 Rust 那条腿就应该是一份这样的 golden**：
**`wire` 里放旧拼写 `"workflow_id": "small-change"`，`canonical` 里放新拼写 `"template_id"`。**
这正好把 §3.4 的「单向 alias」表达成一条机器判据——
第 ① 步证明旧键读得进来，第 ③ 步证明新键是唯一的输出形状。

> **⚠️ 不说清这一点会怎样**：实现者把旧拼写直接写进 `canonical`，
> 第 ③ 步（不动点）会失败，然后他很可能靠**删掉这个用例**来「修好」它——
> 于是本次唯一的 fail-open 防线被那个「修复」删掉了，而且 CI 是绿的。
> 这是 CLAUDE.md「Mutation-Verify Critical Assertions」的反面教材形状，
> 所以写进 §10.0 的清单。

另外三条腿（三个 Zod parser）**必须各自独立**，理由见测试表 #14 那一格。

#### 钉不住的（诚实记账）

* **「没有任何第三方插件依赖非模板绑定路径」**——只能用一个扫 `plugins/*/manifest.json`
  的测试钉住*仓内*插件；运行时装进来的第三方插件钉不住。这是 §5.3 那个契约破坏的
  残余风险，只能靠 §5.1 的调查 + release note + 升级前扫描。
* **「除这两个 GET 外没有别的端点触发播种」**——§7 收窄理由 1，本切片不建路由扫描门禁。
* **「`:779` 没有换马甲」的语法层面**——§4.3 已经把判据换成语义 + 测试 #8/#9。
  仍然存在一种测试打不到的情形：有人写一个语义等价但集合仍相等的特例
  （例如按名册接受、却在别处按 binding 改变别的行为）。这是评审项，写进 PR 描述。
* **（v5 新增，最重要的一条）「§3.2 的改名站点清单是全集」——钉不住，也没打算钉。**
  类别 1 与类别 3 由编译器兜底；**类别 2（非类型检查站点）没有任何东西兜底**。
  本文列的是四轮评审 + 三个独立扫描者的**并集**，一份文档做不到证明它是全集。
  **PR-2 的真保证是验收 B10 那条残留 `git grep` + 显式 allowlist**，不是这张表。
  **对评审者的具体要求**：不要去核对那张表是否穷尽（做不到），
  去核对 **allowlist 的每一项凭什么在那里**——那是一份有限的、可逐条论证的清单。
* **「三处 `WEB_COMPAT_VERSION` 从此不会再漂」**——验收 B6 的静态门禁只保证
  **本次**三者相等，不保证将来某个人加第四处。真正的结构解是 §9 非目标 12 的
  「单一源生成」（派生优于测相等），不在本切片。

### §10.3 旧文案断言改钉什么（§4.4 变更 B 的落地）—— **v5：是三条，不是两条**

`crates/calm-server/tests/cases/track_workflow_templates.rs:586` 与
`crates/calm-server/tests/forge_workflow_e2e.rs:427` 都断言错误正文含
`must reference a registered trusted workflow`。变更 B 之后两条都会红。

> **⚠️ v5 新增第三条（通道 A m7，判定成立）。** v4 的这一节开篇说「两条旧文案断言」，
> §10.1 的文件清单也只列了两条。**实测还有第三条**：
>
> ```rust
> // crates/calm-server/tests/forge_workflow_e2e.rs:203
> body["error"].as_str().unwrap_or("").contains("workflow_id")
> ```
>
> 它**断言的是字段名本身**，所以：
> * **变更 B（PR-1，只改文案措辞）不影响它**——PR-1 的新文案仍然含 `workflow_id`；
> * **变更 C（PR-2，改名）会让它红**——正文里那个字段名变成 `template_id`。
>
> 它**不在 v4 的任何文件清单里**，所以会以一个「莫名其妙红了的 e2e」的形式出现在 PR-2。
> **处置**：把它改成 `.contains("template_id")`，并且**不要**给它加上下面那三条腿——
> 它钉的是「400 正文点名了出问题的字段」，与准入判据的措辞是两件事。
> **文件清单已在 §10.1 的 PR-2 侧补入这一行。**

**`forge_workflow_e2e.rs:423-429` 这条的立意本身死掉了**，必须正视：它的注释写着
`// #891 slice ② review fix — pin the trust-check wording so this case // keeps
discriminating the trust gate from input validation.`（`:421-422`）。
统一之后 create 路径上**没有任何东西再区分 trust gate**：untrusted + 模板 key = 201；
untrusted + 非模板 = 与未知 id 完全相同的 400。这个「区分」不再是一个真事实，
继续 pin 它就是钉一句代码不再说的话。

> **⚠️ v3 更正：v2 在这里给的替代断言不是一个 pin。** 两个通道独立提出
> （A J2、B m2），我重扫**判定成立、v2 错**。v2 说「两处都改成断言
> `.contains("missing-workflow")`」，但：
> * 两个调用点发的都是 `workflow_id: "missing-workflow"`
>   （`tests/cases/track_workflow_templates.rs:577`、`tests/forge_workflow_e2e.rs:415`——
>   `grep -rn "missing-workflow" crates/calm-server/tests/` 全仓只有这两行）；
> * **今天的**消息本来就回显 id（`crates/calm-server/src/routes/tracks.rs:765`：
>   `` track create: `workflow_id` must reference a registered trusted workflow; got `{workflow_id}` ``）。
>
> 所以 `.contains("missing-workflow")` 在变更 B **之前和之后都绿**。
> 也就是说：本文用整个 §4.4「变更 B」论证了旧文案陈述的是一个**不再是准入判据**的东西，
> 然后给它配了一条**检测不到回退**的断言。这正是 CLAUDE.md「Fake Gate Shapes」里的形状。

**裁决（v3，v4 只改字段拼写）**：两处都改成**三条腿**的断言。
**v4 注**：变更 C（§3）之后新文案里那个字段名是 `template_id`，
所以这两条 case 发出去的请求体也要跟改；三条腿的**结构**一个字不变
（`known track template` 在场 / `registered trusted workflow` 不在场 / 被拒的 id 被点名），
因为它们钉的是**准入判据的措辞**，与字段拼写正交。

```rust
let error = body["error"].as_str().unwrap_or("");
assert!(error.contains("known track template"), "body={body}");        // 新判据在场
assert!(!error.contains("registered trusted workflow"), "body={body}"); // 旧判据不在场
assert!(error.contains("missing-workflow"), "body={body}");           // 被拒的 id 被点名
```

**变异判据，点名写进 §10.2 与 PR 描述**：**「把 registry 措辞恢复回去」**
（即把 `admit_template` 的 `ok_or_else` 文案改回
`must reference a registered trusted workflow`）⇒ 这两条 case **必须红**。
按 v2 的写法它们是绿的；按 v3 的写法，第 1、2 条腿各自独立地让它红。

并把 `forge_workflow_e2e.rs:421-422` 的注释改写为：

> #1209 — create 的准入判据是「在不在 template 名册里」，与插件信任无关。
> 这里只保证 400 的正文点名了被拒的 id；trust gate 的可观测后果由同一测试
> `:434-454` 的「untrusted ⇒ 201 且 `plugin_scope` 为 null」承担。

这样每条断言都还在钉一件**当前为真**的事：一条钉「未知 id 被点名拒绝」，
一条钉「信任只影响绑定、不影响准入」。**替代方案「直接删掉文案断言」被否决**：
那会让这条 case 退化成只看状态码，而 400 在 create 路径上有六种来源（§4.4）。

---

## 一句话总结

把 create 的准入判据从「有没有插件绑定」搬到「在不在 template 名册里」，
绑定降级为解析结果上的一个 `Option` 字段。判据搬家之后，`:779` 那行不是被移动，
是没有位置可放了。

守住这句话的不是 grep，是两条路由级测试（#8/#9）：读口列出来的**每个** id 写口都 **201**
（v4 恢复 `== 201`，§10.2），写口对名册外 id 的拒绝由 #8 的定向反例 + #9 的抽样承担
（**不是**集合相等门禁，§10.2 已把这一点写诚实），而插件绑定与否不改变准入这个答案。

**v4 追加一句**：判据搬完家之后，那个字段就该改叫它现在唯一指的东西——
`template_id`（§3 的 D2 重开）。v1–v3 保留旧拼写的唯一理由是兼容性成本，
而人已经把那个成本批了。**留着旧拼写，就是把本 issue 的标的物写进注释再交付一次。**

**v5 追加一句，关于那次改名怎么才算做完**：
判据搬家（PR-1）由两条路由级测试守住；**改名（PR-2）守不住的部分不是「大」，是「静默」**。
类别 1 与类别 3 的站点漏改了，编译器会逐个报错；
**类别 2 的站点漏改了，没有任何东西会红**——
`today.rs` 那两行字面 SQL 会在生产的 Today 页面上炸，
`track-fs-viewers/schemas.ts` 会让旧 snapshot 静默丢掉模板归属，
`wire.ts` 的 `Omit` 会在类型层悄悄失效，三份 oracle 会与代码脱节。
**所以 PR-2 的完成判据不是「本文那张站点表都改完了」——那张表证明不了自己是全集，
而是「残留 grep 在一份每项都有理由的 allowlist 之外零输出」（验收 B10）。**
一份设计文档能给的是方向和 allowlist 的理由；能给出保证的是那条 grep。

---

## §11 Disposition history

### v4 → v5（2026-09-01/02，基线 `0b4b022f`）

第 4 轮两个通道**罕见地高度一致**：都给 **NEEDS-REVISION**，
对「改名计划是否完整」都答 **NO**，而且**独立扫出了同一个 BLOCKER**
（`routes/today.rs` 的两条字面 SQL）。
本轮的性质与前三轮不同：**推理没有被推翻，扫描没有做完，而且 v4 发出了三条经不起重跑的证据主张。**

**本轮的元教训，写在最前面**：v4 在 §3.2 开了一次全仓扫描，
把「长尾交给编译器」当成收尾。**那句话对类别 1/3 成立，对类别 2 不成立**——
而类别 2 恰好是本次唯一有生产运行时风险的一类。
**处置不是把表补长（补不完），是换一条收尾门禁**（验收 B10 的残留 grep + allowlist）。

#### 第 1 张表 — 逐条裁决

| # | 来源 | 发现 | 裁决 | v5 做了什么 |
|---|---|---|---|---|
| **T1** | **A/B1 + B/M2（两个通道独立发现，本轮 BLOCKER）** | `routes/today.rs:149`（UPDATE）与 `:162`（INSERT）把列名写成字面 SQL；改名后**编译干净、运行期炸**。v4 把 `today.rs` 归进「机械、编译器抓」，§3.3 说「三处」，§10.0 item 7 根本没提它 | **ACCEPTED（本轮最重要的一条）** | 实测两行内容逐字确认。§3.3 改成**五处生产站点**的表，每行给「谁会告诉我漏了」；`today.rs` 从机械类移进运行期风险类；顺带更正 `TRACK_SELECT_COLUMNS_W` 是 `:94` 不是 v4 写的 `:95`，并列出它俩的 **10 个词法 SELECT 消费点**与 4 处测试侧原始 SQL。验收从「一次往返」扩成 **B1 往返 + B2 Today 两条腿 + B3 迁移保值**；新增测试 #17/#18；§10.0 前言把它排在第 1 位 |
| **T2** | **B/M1 + A/M4,M5,M7,n1** | §3.2 的「完整调用方清单」不完整：oracle 三份不是一份、`web/src/api/wire.ts` 是手写不是生成物、`plugin_host/workflow_input.rs` 整个模块一次都没出现、`fe/e2e` 坐标不全、字符串名册、注释/CSS/aria-label、生成物漏两个 | **ACCEPTED，且改的是结构不是内容** | 逐条实测全部成立。§3.2 **按通道 B 的处方重构成三类**（语义 / 非类型检查 / 机械构造），每一类的组织原则是「谁来抓」。**关键改动是加了一条 PR-2 收尾的残留 `git grep` + 五项显式 allowlist（每项附理由）**，并在节末明写「这张表不能证明是全集，真保证是那条 grep」 |
| **T3** | **B/M3 + A/M1（两个通道独立发现）** | §3.4 / §10.0 item 4 要求 `Track` **和 `TrackRow`** 都加 `#[serde(alias)]`；`TrackRow` 只 derive `Debug, sqlx::FromRow`，加 serde attribute 不可编译，且本来无事可做 | **ACCEPTED** | 实测 `crates/calm-truth/src/db/rows.rs:99` 确为 `#[derive(Debug, sqlx::FromRow)]`。删掉 `TrackRow` 指令；载体收敛为 `calm_types::Track`（`crates/calm-types/src/model.rs:339`，经 `TrackUpdatedPayload`（`event.rs:83`）`flatten` 进历史事件）。§10.0 item 4 同步 |
| **T4** | **B/M3（第三个 Zod reader）** | zod 侧不是两个 reader 是三个；`web/src/track-fs-viewers/schemas.ts:152,160` 读旧 `track.json`/FS snapshot，机械改名 ⇒ 旧 snapshot 静默 `template_id=null` | **ACCEPTED（这条只有通道 B 看到）** | 实测 `:160` 是 `z.unknown().default(null)`、`:152` 是 `.nullable().default(null)`——正是 fail-open 的形状。§3.4 改成三 reader 表，要求同形的单向 normalize，并禁止「把旧键做成 schema 的可选字段」（那是写口方案 B 的前端版）。测试 #14 从「两个前端各一条」改成 **Rust + 三个 parser 四条，且必须各自独立变红** |
| **T5** | **A/m3 + A/m4** | §3.4 的 fail-open 证据引的是 goldens（测试数据）而不是读取者；测试 #14 没点名载体 | **ACCEPTED，且发现失败模式更糟** | 实测 `Event::from_kind_and_payload` 的调用点在 `crates/calm-truth/src/db/sqlite/events.rs:577`（`events_since` 追赶路径），其 `Err` 分支（`:578-585`）只 `tracing::error!` 然后**跳过整行**。§3.4 因此写出**两条方向相反的坏路**（缺键静默 `None` / 删 `default` 后整行被跳），裁决改为 **alias + 保留 `default`**。§10.2 新增「#14 的形状」，点名 `event_serde_goldens.rs:11-27` 的 `wire`/`canonical` 三步契约，并写明「不说清会怎样」 |
| **T6** | **B/M4** | 三处 `WEB_COMPAT_VERSION` 数目确为三，但 **CI 从不比较它们**：Rust 侧断言字面量（`tests/cases/version.rs:148,153`）、两个前端各读自己的本地常量；计划测试 #15 只能证明服务端 floor | **ACCEPTED（v4 的「三处 lockstep」是名不副实）** | 实测三处都是 `16`，且 `version.rs:148`/`:153` 确实是字面量断言。§3.6 加了**三种漂移后果表**（今天全绿），并把「PR-2 必须二选一：(a) 比较三处导出值的 CI 静态门禁 / (b) 单一源生成」写成硬要求，推荐 (a)。#15 更名为 `web_compat_floor_is_above_the_previous_bundle` 并降低 claim；(b) 进 §9 非目标 12 |
| **T7** | **B/M5** | §4.4 不是两棵树：事务在 `tracks.rs:1609` 提交后还有 `materialize_workspace`（`:1620-1633`，可返回非 2xx 而副作用已提交）与 planner-harness start（`:1660-1676`）；旧拼写 400 发生在 serde extractor、函数体之前 | **ACCEPTED（v4 漏了一整段）** | 实测四个坐标全部成立，且 P1 的孤儿结果**今天就被 `track_workspace_materialize.rs:270-313` 明确钉住**（注释 `:293-306` 逐字写着「不要靠放松断言来修」）。§4.4 改成**四个阶段**（0 serde extractor / 1 事务前 / 2 事务内 / 3 事务后），矩阵新增行 P1；`tracks.rs:759-760` 的注释改写文本再加一句 |
| **T8** | **B/M5 的尾巴** | 横切错误的载体写错：v4 写「任何 `await?`」，但 `materialize` 是同步 `?`、`resolve_trusted_workflow(...).await` **根本没有 `?`** | **ACCEPTED** | 实测两处都成立。改为「**任何可失败的 DB / FS 操作**」，并把两个反例写进正文——这条修的不是措辞，是一个会让人按错模型写测试期望的判据 |
| **T9** | **B/M5 + A（矩阵）** | 「统一后」在同一张表里同时指 PR-1 后与 PR-2 后；行 6/8/9/10/11 在终态仍承诺旧的 `workflow_input`/`workflow_id` 错误串 | **ACCEPTED** | 矩阵拆成 **今天 / PR-1 后 / PR-2 后** 三组列，先给通用映射规则（含旧串的正文一律换，状态码与顺序不变），再逐行标例外。行 10 尤其点名：它的正文产生处**不在 `tracks.rs`**，在 `plugin_host/workflow_input.rs` —— 正是 T2 里 v4 整个漏掉的那个模块 |
| **T10** | **A/M3 + B/MINOR2（两个通道独立发现）** | §8.2 自称「对 `7b85caa3` 复测」的 `1230-s1` 坐标**在任何提交上都复现不出来**（`7b85caa3` 与 `b93fb767` 逐字节相同；`d51571d7` 又是另一组），来自 dirty working tree | **ACCEPTED（一条自称 OBSERVED 的数据不可复现，本仓有专门的记忆条目）** | 实测本轮 HEAD 已经是**第四个**基线 `3b9cc03c`。**处置不是更新坐标，是删掉全部 `1230-s1` 行号**——四轮四变，文档承载不了另一条活分支的坐标。只留「6 test + 1 生产 + 1 import」这个在四个基线上都成立的形状，外加「合流时对当时的 HEAD 重跑 grep」这条动作。文首新增「v5 的记账纪律」，§8 开头新增基线声明（通道 A n2 也提了这个位置问题） |
| **T11** | **A/M2 + B/MINOR1（两个通道独立发现）** | §5.3 说 `productMajor` 有两条 pin；`manifest.rs:302` 是空的——它只解析一段硬编码 `"productMajor": 0` 的字节串（`:275`），从不调 `product_major()` | **ACCEPTED（v4 自己造了一个假门禁）** | 逐行实测：`package.rs:508` 的 `with_env_removed("NEIGE_PRODUCT_MAJOR", …)` 包住函数体 ⇒ `:546` 是真 pin（v4 写的 `:507` 差一行，一并更正）；`manifest.rs:271` 那条是纯 parser fixture ⇒ 永远绿。§5.3 改成**单数**：package smoke 是 pin，manifest 测试是 fixture，**且不许被算作门禁**。验收 B7 同步 |
| **T12** | **A/M6 + B/MINOR2 尾巴** | §8.2 仍写「#1209 对 wire 零改动（§3）」——v3 为真、v4 为假；照字面执行的人会整份取走 #1230 的生成物，永不与改名和解 | **ACCEPTED** | 规则整条重写：**两侧生成物都不取** → 先合 Rust → 从合并树重新生成（`ci.yml:1186-1187` + `:1190`）→ 由 `ci.yml:1194` 的 `git diff --exit-code` 裁决 → **不许手改产物让它绿** |
| **T13** | **B/M6 + A/M8** | PR-1 被要求落 §3.9 的新模块头，而新文本写 `template_id`，PR-1 却仍发 `workflow_id`；`docs/deploy-and-upgrade.md` 在 PR-1 的文件集里但内容是 PR-2 的 | **ACCEPTED（两半都是时序问题，不是决定问题）** | §3.9 加时序方框：**PR-1 落一段对 `workflow_id` 诚实的临时文本**（给出全文），PR-2 换最终文本；明写替代方案（整段推到 PR-2）及其代价，并说明为什么选临时文本。`docs/deploy-and-upgrade.md` **整节移到 PR-2**（PR-1 单独落地是 `preserving`，写那一节就是发一份描述自己不产生的东西的文档） |
| **T14** | **A/M9 + B/M6** | 十条验收堆在 S1 名下、在两 PR 切法宣布**之前**，且没重新划分；PR-2 另缺 alias / 三处版本 / 迁移保值 / `productMajor` / 旧拼写拒绝的验收 | **ACCEPTED（判据不拆就等于没拆）** | 验收重排成 **PR-1 的 A1–A7 + PR-2 的 B1–B10**，五项缺失全部补上（B4/B6/B3/B7/B5），并新增 **B2（Today 两条腿）** 与 **B10（残留 grep）**。A4 顺带按四阶段模型更正「有意不钉」的行号集合 |
| **T15** | **A/m5 + B/M6** | PR-2 内容栏只列 #14/#15，判据栏却写「三条新测试」；#16 是 §3.5 整个拒绝策略的唯一 pin | **ACCEPTED** | 切片表的 PR-2 内容栏补 #16，并因 T1 新增 #17/#18 ⇒ **五条新测试**，判据栏同步 |
| **T16** | **B/M6 第三点** | 「升级前备份」只有一句占位符，落在名为「插件兼容性」的小节下，没有命令、没有目标路径、没有 `calm.db/-wal/-shm` 一致性方法 | **ACCEPTED（发现成立），但 v5 逐行读代码后改写了它的前提** | 见下面第 2 张表的 **X1**——通道 A 的证据主张有一半是错的，我驳回并给了更准的版本。落点改到 `docs/deploy-and-upgrade.md:344` 的「## 8. Pre-flight checklist」，在 `allowBreaking: true` 那条之前，自成小节，三段可执行内容（自动备份的事实 / 在线备份命令 / 手工恢复三步），外加更正该文件 `:26` 的错误括注 |
| **T17** | **A/m1** | §8.1b 宣布的「合流硬前提」**已经被上游满足了而文档不知道**：`current_definition` 已 `?` 上抛，注释逐字复述了本文的发现 | **ACCEPTED（结案）** | 实测 `1230-s1@3b9cc03c` 的 `current_definition`：两个 `?` 都在，降级分支没了，注释写着 "A *read failure* on it is an error, never a reason to answer with the constants"。§8.1b 加 **CLOSED 方框**；§7 的状态 C 备案作废；§11「仍需人裁」移除相关条目 |
| **T18** | **A/m7** | PR-1 自己的错误文案从未被写出来（§4.4 变更 B 给的是 PR-2 字段名） | **ACCEPTED** | §10.1 明写 PR-1 落 ``track create: `workflow_id` must reference a known track template; got `{id}` ``，PR-2 再改一次；并**明说 §10.3 的三条腿在两个阶段都成立**（没有一条钉字段名），免得有人加第四条腿然后在 PR-2 把它打红 |
| **T19** | **A/m7 尾巴** | §10.3 说「两条」旧文案断言；还有第三条 `forge_workflow_e2e.rs:203` 的 `contains("workflow_id")`，不在任何文件清单里 | **ACCEPTED** | 实测该行确为 `body["error"].as_str().unwrap_or("").contains("workflow_id")`。§10.3 加方框说明它**只被变更 C 打红、不被变更 B 打红**，处置是改成 `template_id` 且**不要**给它加三条腿；文件清单补入 |
| **T20** | **A/m9 + A 的 §3.8 裁定** | 「manifest `workflows[]` 不改名」不在 §9 非目标里；且 §3.8 给的理由（「有文档的适配边界」）恰是人刚推翻过的那一招 | **ACCEPTED（结论对，理由换）** | §3.8 按 §5.3 已有的 **schema vs 接受语义** 重写：改 `workflows[]` 是 Tier A **schema** 破坏（`upgrade-stability.md:9` + `manifest.rs:93-100`/`:467-475`，第三方 manifest 解析期就炸），而 D4-A 只改接受语义、schema 一字节不动。**并诚实记下残余命名债**（D4-A 后该数组每个合法值都是 template key，容器名变怪），进 §9 非目标 11 + 跟进 issue |
| **T21** | **A/m6 + B/M2 尾巴** | 带字面列名的测试 fixture 未记账；`track_plugin_scope_migration_tests.rs` 那一族跑在历史 schema 上，改与不改取决于它停在哪 | **ACCEPTED** | §3.3 单列四处测试侧原始 SQL（含 `child_track_adapter.rs:1350`——**实测在 `#[cfg(test)] mod tests` 内，该 mod 自 `:499` 起**，通道 B 归类为测试是对的）。`track_plugin_scope_migration_tests.rs:66` 进 §3.2 的 allowlist 第 2 项，理由是它**故意**停在 `0075`（该文件 `:60-64` 的注释自己写明了） |
| **T22** | **B/M1 尾巴** | 「170 个 Rust 文件」不可复现 | **ACCEPTED** | v5 复跑，通道 B 的数字逐个复现：`workflow_id` 173/168、`workflow_input` 165/162、`NewTrack {` 147。§3.2 **附上产生它们的命令**，并明写「这些数字只用来说明为什么切两个 PR，不用来说明覆盖度」 |
| **T23** | **A/m8 + B「DB 与版本号核证摘要」** | 迁移号核证 | **ACCEPTED（正面确认）** | 两个通道独立确认：最新是 `0078_cards_role_assistant.sql`，`0079` 空闲；`0059:6` / `0061:8` 是两列的创建者；`0076` 重放顺序安全的说明正确；**仓内无 `.sqlx` / `SQLX_OFFLINE` / `query!`，不需要 `cargo sqlx prepare`**。§3.3 把 `00NN` 定为 `0079` 并注明「rebase 后要复查撞号」——这是本文唯一允许留到实现期的坐标 |
| **T24** | **A（`tool_visibility` 复核）+ B（MCP 面）** | 两个通道独立复核 `mcp_server/tool_visibility.rs` 干净、无 MCP 工具面暴露该字段 | **正面确认，不动** | §3.2 类别 1 把这条写成**显式的「不要动」**（七处命中 = 两条注释 + 五处测试结构体字面量；真 gate 只读 `plugin_scope`，`:109`），免得实现者「顺手」改坏它 |
| **T25** | 两个通道的其它正面确认 | v4 的核心推理（D2 重开、alias 的不对称、`productMajor` 空指令的发现、两 PR 建议、`compute_verdict` 比的是 target vs installed、§1.5 新模块头的最终语义与 D4-A 一致）以及新材料里绝大多数行号引用 | **不动** | v5 **没有**碰这些结论。唯一被改的是它们的**归属与时序**（模块头进 PR-2、`productMajor` 的 pin 收敛为单数） |

#### 第 2 张表 — v5 驳回的评审主张（带反证据）

**本轮只有一条，但它很重要：一条被两个通道之一当成 MINOR 提出的「证据补强」，
其证据本身是错的，而且如果照抄进文档，会让升级说明教操作者做一件不必要的事、
同时漏掉真正的缺口。**

| # | 通道说 | v5 实测 | 裁决 |
|---|---|---|---|
| **X1** | **A/m2**：「`Verdict::Breaking` 根本没有 `requires_db_backup` 字段（`preflight.rs:250-263` 只为 `Preserving` 计算），`apply_breaking`（`apply.rs:259-271`）从不调 `backup_db` —— 即**连 `apply_preserving` 在 `apply.rs:287` 做的自动预备份都没有**」 | **前半成立，后半为假。** ① `Verdict::Breaking` 确实没有 `requires_db_backup` 字段（`preflight.rs:250-263` 只在 `Preserving` 那一支算它）——**成立**。② 但 `apply.rs:259-271` 是**调用点**（`match` 的一个分支），不是函数体；**函数在 `apply.rs:364`**，而它 `:375-376` 写着 `let backup = if units_changed.contains(&UnitName::CalmServer) { Some(backup_db(...).await?) }`。本次升级必改 calm-server ⇒ **这个分支一定走**。③ `backup_db`（`apply.rs:604`）本身是正确的：先 `stop_and_wait`，再 `backup_sqlite_files_sync`（`:663` 起复制主库 + `wal` + `shm` 两个 sidecar）。 | **REJECTED（后半），并用更强的版本替换。** 真正丢掉的是**三件别的事**，逐条有证据：(1) `VerdictSummary::from` 对 breaking **硬编码 `requires_db_backup: false`**（`preflight.rs:104`）⇒ **`dryRun` 会对操作者说谎**；(2) 函数就叫 `rollback_last_preserving`（`apply.rs:1252`），`:1266` 直接拒非 preserving ⇒ **备份在磁盘上，但没有任何 API 能放回去**；(3) 没有健康检查自动回滚。**结论从「没有备份」改成「有备份、无回滚路径」**——于是 `deploy-and-upgrade.md` 要写的不是「记得备份」这句废话，而是**手工恢复的三步**，外加更正该文件 `:26` 那句 "(one per **preserving** apply)" 的错误括注 |

**这条的元教训与 v3 的 R10 是同一个**（CLAUDE.md「Verify By Reading Actual Artifact」）：
通道给的坐标看起来像函数定义，其实是调用点；**不打开读一遍就照抄，等于把错误洗进设计**。
v5 的做法仍然是**两个方向都验**——本轮 24 条采纳、1 条驳回。

#### 本轮的「仍需人裁」变动

* **移除**：§8.1b 的合流硬前提（T17，已由上游满足）。
* **新增**：无。§3.6 的 (a)/(b)、§9 非目标 12/13 都是本文自己做的判断，不需要人签字。
* **保留**（从 v4 继承）：`web/` 是生产 bundle 这条事实需要人确认；breaking 的三条 ops 后果
  需要人签字（**措辞按 T16/X1 更正**：不是「没有备份」而是「有备份、无回滚 API」）；
  一个 PR 还是两个；spawn-time warn；§4.4 行 8 的误导文案。

### v3 → v4（2026-09-01，基线 `0b4b022f`）

第 3 轮两个通道**分裂**：通道 A 给 **APPROVE-WITH-NITS**（认为所有条目都可以在实现 PR 里顺手处理），
通道 B（codex）给 **NEEDS-REVISION**，并给出两条通道 A 完全没看到的 MAJOR。
**分裂本身是本轮最有信息量的事实**：一个「批准」的通道漏掉了两条 MAJOR，
其中一条（B/M2）暴露了整份文档内部的一处自相矛盾。
本轮**没有任何一方是全对的**——通道 B 的 M1 是对的但它自己的一条 NIT 计数说法不精确（n3），
通道 A 的 M1 抓到的是**v3 自己在上一轮引入的**一处弱化。

本轮另有**两条不来自评审通道的输入**：**人的新约束**（文首方框）。
它们不是「发现」，是**决定**，因此单列在下面第二张表里。

#### 第 1 张表 — 评审通道的逐条裁决

| # | 来源 | 发现 | 裁决 | v4 做了什么 |
|---|---|---|---|---|
| S1 | **B/M1** | §4.4 的错误优先级树与真实事务控制流不符：显式 fork 校验被排在 folder claim 之前；「DB/IO 永远是第 9 级」为假 | **ACCEPTED（本轮最实在的一条）** | 独立复核：`enforce_folder_claim_tx` 在 `tracks.rs:1391`（其上注释明写 "Must stay first"）、`track_create_tx` `:1401-1402`、显式 fork 校验 `:1408` 起。§4.4 拆成**两棵树**（事务前 1–6 / 事务内 T1–T4），并把 generic DB/IO 标成**每个 `await?` 都可能发生的横切错误**而不是固定末级。行 17 在「无 folder 冲突」前提下仍成立，明写 |
| S2 | **B/M1 的尾巴 + A/n1** | §4.4 说今天的播种在「第 1 步之后」；实为第 2 步之后 | **ACCEPTED** | 实测：`validate_workflow_input_binding` 在 `tracks.rs:790`，播种块 `:799-814`。改为「第 2 步之后 → 搬到第 5 步之后」 |
| S3 | **B/M2** | §5.3 的「不动 `productMajor` ⇒ preserving」与仓库自己的兼容性定义冲突：文档自称 breaking、机器判 `Preserving` | **ACCEPTED（发现成立），但处置由人裁决** | 见第 2 张表的 H1。§5.3 整段重写：先写清 v3 错在哪（同一个词指两件相反的事）、写出 Tier A 的「manifest schema vs 接受语义」分辨（它其实**支持** Tier D 那一支）、再说明为什么不走那一支。**并纠正 v3 的一处事实错误**：`compute_verdict` 比的是 target 与 **installed** 的 `product_major`（`preflight.rs:206`），装上后 installed 也更新（`installed.rs:48`），所以不是「每次升级都被拒」而是**一次** |
| S4 | **A/M1** | 测试 #9 的正例腿被 v3 弱化成「正文不含 `known track template`」，漏掉了它要防的回归类 | **ACCEPTED，撤回 v3 的弱化** | 复核：v3 自己钉的前提（`boot()` 不起插件）把第 2 轮那条反对意见整个消掉了——无 running 插件 ⇒ `resolve_trusted_workflow` 返回 `None` ⇒ `validate_workflow_input_binding(None,None)` 走 `tracks.rs:972` 的 `(None,None) => Ok(())` 早退 ⇒ `issue-development` 走不到 `:977-990`。§10.2「#9 的形状」恢复 `assert_eq!(status, 201)`，并给出「前提被放宽时」的显式允许清单版本；§10.1 验收 #3 同步，反例点名那个换措辞的特例 |
| S5 | **A/m2** | §10.2 #10 的 snapshot 不是「同一个 sqlx helper 加两条 SELECT」：trait 上**没有** all-overlays 访问器，也**没有** events count | **ACCEPTED** | 复核 `crates/calm-truth/src/db/mod.rs`：`overlays_by_kind(entity_kind)`（`:436`，既有 helper 传的是 `"view"`，`track_workflow_templates.rs:169`）、`area_folders_list_all`（`:316`）、`events_latest_id`（`:830`，**只有 max_id**）、`events_raw_window_since(since,limit) -> (count, max_id)`（`:780-784`）。§10.2 加了一张**逐栏点名访问器**的表 + 两处最容易写错的地方 |
| S6 | **A/m4** | §2.2 的「名册唯一查找入口」在合并后为假：`current_definition` 回落分支开手写了一次 `WORKFLOW_TEMPLATES.iter().find(..)` | **ACCEPTED** | 复核 `1230-s1@b93fb767 routes/track_templates.rs:270-274`（`Ok(Definition {` 在 `:270`、`.find` 在 `:273`）。§2.2 的 claim 收窄为「唯一的**可失败查找 helper**」；§8.2 把它列为**第 8 个站点**并明写**验收 #7 的 grep 抓不到它**（它不用那两个符号）；合并规则 = 两侧都不动 |
| S7 | **B/n3 + A/m3** | §8.2 的「6 处 + 1 import」与它自己那张 7 行表矛盾 | **ACCEPTED（措辞）** | 改为「**6 个 test 使用点 + 1 个生产使用点 + 1 处 import**」。**并 v4 复测**：在 `1230-s1` 的**当前** HEAD `7b85caa3` 上同一条 grep 给出同样的 **6+1+1** 形状、**全部新坐标**（坐标 **v5 已判定不可复现并全部删除**，见 T10）。结论：形状可写进设计，行号不可 |
| S8 | **B/n4** | §5.3 的 `jq` 扫描循环在插件根目录为空/不存在时会报错 | **ACCEPTED** | 复核 `plugin_host/registry.rs:118-130` 明写容忍缺失目录。循环里加 `[ -f "$m" ] || continue`，并给出**成对**的正例/反例（3 个 manifest 含 1 个越界 ⇒ 1 行输出；目录不存在 ⇒ 零输出 + 退出码 0） |
| S9 | **A/m1** | §10.1 验收 #4「矩阵逐行有断言」为假：行 13/14 无测试，行 17 被 §10.2 #13 明确拒绝——而那条拒绝是 v3 自己加的 | **ACCEPTED（v3 的内部自相矛盾）** | 验收 #4 改写为「行 1–12b/15/16/18–20 落到 §10.2 的编号；13/14/17 **有意不钉**（§4.2 的裁决）」 |
| S10 | **A/m5** | §10.2 #8 的第 3 条「零播种副作用」不鉴别任何东西 | **ACCEPTED（降级，不删除）** | 复核成立：名册外 id 在正确代码与被点名的变异下**都不播种**。警告框里把它从「三条硬要求之一」降为**便宜的保险**，并写明真正承重的是第 1、2 条腿；同时说明它仍能抓住「fallback 被塞进播种分支」那个变体 |
| S11 | **A/n2** | 矩阵行 12b 与 #13 第 3 条腿把 400 归给 `attach_folder`；实际 `tracks.rs:843` 只看 `!cwd_omitted` | **ACCEPTED** | 实测 `sed -n '843,847p'` 确认守卫条件。行 12b 与 #13 第 3 条腿都改成「显式给一个不是 git 仓库的绝对路径 `cwd`」，并要求测试**显式不设** `attach_folder` |
| S12 | **A/n3** | §5.3 引「九个 compatibility 字段（`:74`）」，实际在 `docs/deploy-and-upgrade.md:73` | **ACCEPTED** | 实测第 73 行是 `` `compatibility { ... }` (9 fields sourced from ``。§5.3 重写时该引用已随整段改掉；相关论证现在直接引 `preflight.rs:287-296` 的 `compatibility_breaks` 源码，比引文档更稳 |
| S13 | **A/n4** | §9 风险表仍写缓解「release note」，§5.3 早已换成具名的 `docs/deploy-and-upgrade.md` 一节 | **ACCEPTED** | 该格改为具名落点 + 内联 `jq` 扫描 |
| S14 | **A/n5** | §8.2 说 #1230-first 的破坏「静默到编译期」、只有验收 #7 守着；但 CI 触发于 `pull_request` 并构建 merge commit，后落地的 PR 必然自动红，「中高」偏高 | **ACCEPTED（下调，不删门禁）** | 复核 `.github/workflows/ci.yml:3` 的 `pull_request` 触发。风险等级 中高 → **中**，理由写进格子；验收 #7 保留，理由是它**更早、更本地，且能指出漏了哪一处**（CI 只告诉你编译不过） |
| S15 | 两个通道的正面确认 | 测试 #8 真的会红、§10.3 三条腿可证伪、§8.1b 是真门禁、`jq` 过滤器对真 manifest 可用、§8.2 的 7 站点表在 `b93fb767` 上逐字复现、`list_track_templates` 的撤回是自洽的、v3 的四条引用驳回全部正确 | **不动** | v4 **没有**碰这些结论。唯一的例外是 §8.2 的站点表：**措辞**按 S7 改、**坐标**按 S7 复测——表本身没有被判错 |

#### 第 2 张表 — 人的决定（不是评审发现）

| # | 决定 | 本文原来的立场 | v4 做了什么 |
|---|---|---|---|
| **H1** | 「可以破坏兼容性」 ⇒ B/M2 走诚实的 breaking 路线：**不**降级到 Tier D、**不**撤回定级，让机器判决与文档一致 | v3 裁「不动 `productMajor`，走 preserving」 | §5.3 重写。**并给出实现指令**，因为「bump `productMajor`」按字面读是**空指令**：仓内没有任何 `productMajor` 常量，`package.rs:302-310` 的默认值是硬编码的 `Ok(0)`（`:307`），只能被 `NEIGE_PRODUCT_MAJOR` 覆盖。裁决 = **改 `package.rs:307` 的默认值为 1**，让 `package.rs:546` 与 `manifest.rs:302` 两条既有断言变成本裁决的 pin（漏改默认值 ⇒ 红；漏改断言 ⇒ 红）。ops 后果（`allowBreaking=true`、杀两个进程、**没有** preserving 那套自动回滚、回滚只剩升级前备份）明写在 §3.7 与 §5.3，不埋 |
| **H2** | 「尽可能保持一致」 ⇒ 重开 D2：线上字段改名 `template_id`，原 S2 并入 | v1–v3 全部裁「不改名」，理由是 #1209 正文那句「两个字段做一件事更糟」+「改名代价是兼容性」 | §3 整节重写。**v4 自己扫出**分层调用方清单（§3.2 的 9 层表，`grep -rln workflow_id crates/` = 170 个文件，其中长尾是编译器会报错的填充位——**这条是 INFERRED，明确标注**）。逐项裁决：DB 列**改**（新迁移，禁碰 `0059`，`forwardOnly` 不是 destructive，§3.3）；事件日志**加单向读别名**（§3.4，唯一的 fail-open 防线）；写口旧拼写**变未知字段 ⇒ 400**、**不做定制文案**（§3.5）；`workflow_input` → **`template_input`**（§3.8，理由：不改就是在一个请求体里原地重造这道缝）；插件 manifest 的 `workflows[]` **不改名**（§3.8 的界线：内核 API 内部 vs 内核↔第三方文件） |
| **H3** | （H2 的连带发现，**不是人说的**） | — | **实测发现仓里有两个前端，而人说「没上生产」的那个不是在跑的那个**：`web/` 才是打包发布的 bundle（`ci.yml:903-905`/`:1114-1116` 的 `working-directory: web`；`docs/deploy-and-upgrade.md:62` 的 `--web-dist web/dist`），`fe/` 是新 FE。§1.6 加了这张表；§3.6 因此新增 **`WEB_COMPAT_VERSION` 16→17 三处 lockstep** 的硬裁决——没有它，缓存里的旧 bundle 会一直发旧字段拿 400，正是 `upgrade-stability.md:29` 禁止的「部分工作」。**这条也是本次机器判 breaking 的另一半**（`preflight.rs:295`），而且它**在仓内、被 CI 钉住**，不依赖任何人打包时记得设环境变量 |
| **H4** | （切片后果） | v3：单 PR | 两条新约束都放大了切片。§10.1 给出**两 PR 的切法**（PR-1 概念 / PR-2 拼写，同一次发布），并保留了 v1 那条**仍然为真**的理由：大改名 diff 会淹没 `:779` 的判据。**若人更想要一个 PR，也可以，代价写在那里** |

#### v4 对 §1–§9 其余决定的复查（人的新约束是否波及）

逐条查过，**只有 D2 被推翻**：

* **D1（binding 是属性）**：不受影响。
* **D2**：**推翻**，见 H2。
* **D3/D3b（单一准入 + 播种搬位）**：不受影响；§4.4 的树按 S1/S2 更正。
* **D4-A（拒绝名册外 id）**：不受影响；它的**定级**按 H1 改，**决定**没变。
* **D5（`input_schema` 留在插件）**：**特意复查过**，不受影响——所有权与兼容性预算正交。
  唯一的连带是拼写（`template_input` vs 插件侧的 `input_schema`），§6 新增一段说明它不是新缝。
* **D6（播种不对称）**：不受影响。
* **§1.5 的模块头**：**受影响最深的一处**——那段的主题就是被删掉的那道缝。
  §1.5 的 v4 复读把它拆成两句：「读口说 template、写口说 `workflow_id`」**删掉**，
  「返回的形状一个字都不用改」（`:39`）**仍然为真而且更强**（该端点响应里根本没有这个字段）。
* **§8 的合流分析**：**基线过期**（文首基线注记 2）。结构性结论经 S7 复测后仍成立；
  §8.3 新增一张表说明改名对 #1230 各个面的影响，并**明写没有评估过 #1230 S2**。

### v2 → v3（2026-09-01，基线 `0b4b022f`）

两个通道再次均给出 **NEEDS-REVISION，无 BLOCKER**，且**两边都批准了 §1–§9 的设计决定**
（D1 binding 为属性、D2 保留 `workflow_id`、D3/D3b 单一准入 + 播种挪位、D4-A 拒绝名册外 id、
D5 `input_schema` 留在插件、D6 播种不对称、§8 的合流策略）。
本轮所有发现都落在**验收 / 测试 / 合流**三节。下表逐条裁决，
**每条都在本 worktree 里独立复核过 `path:line` 之后才落笔**。

| # | 来源 | 发现 | 裁决 | v3 做了什么 |
|---|---|---|---|---|
| R1 | A J2 + B m2 | §10.3 的 `.contains("missing-workflow")` 变更 B 前后都绿，不是 pin | **ACCEPTED** | 复核：两个调用点都发 `missing-workflow`（`track_workflow_templates.rs:577`、`forge_workflow_e2e.rs:415`，全仓仅此两处），今天的消息本就回显 id（`tracks.rs:765`）。§10.3 改成**三条腿**（`known track template` 在场 + `registered trusted workflow` 不在场 + id 被点名）；§10.2 #1 的变异列点名「恢复 registry 措辞」 |
| R2 | A J3 + B M4 | 测试 #9 的「每个列出 id ⇒ 201」腿在生产里为假；负方向只有两个 sentinel | **ACCEPTED** | 复核：`tracks.rs:986-989` + `manifest.json:299` 使 `issue-development` 无 input ⇒ 400；`boot()`（`:46`）不起插件是未言明前提，而 #8 正要在同文件引入受信 stub。§10.2「#9 的形状」重写为「**没有列出的 id 会以 unknown-template 400 被拒**」+ 显式无插件前提 + **把负方向诚实标注为抽样**；§10.1 验收 #3 同步；「一句话总结」的口号也改了 |
| R3 | A J4 + **B M2** | 搬位改变的 4xx 副作用面不止 area-404；**B 另找到事务内的显式 fork 400** | **ACCEPTED（两半都成立）** | 复核：`tracks.rs:823-828`（cwd 形状）、`:843-847`（attached）今天同样先播种再 400；`tracks.rs:1408` 的 fork 分支在**事务内**，`:1410-1418`（源不存在）与 `:1424-1430`（跨 area）都是 `BadRequest`。§4.4 补行 **12a / 12b / 17**；前提 4 允许行 17 破例；§4.2 把「事务内还有什么」从两类改成三类；#13 参数化到三条**事务前** 4xx。**行 17 裁决：接受残余、不前移**（前移=事务外再抄一遍判据，撞 Mirror Code；播种幂等，代价有界）——**这是判断，不是发现** |
| R4 | A J5 + B M6 | F11 的缓解 2/3 无产物、无归属、不在文件清单；`productMajor` 未裁 | **ACCEPTED** | 复核：无 `CHANGELOG`、无 `docs/release*`、`grep -n plugin docs/deploy-and-upgrade.md` 零输出——全部属实。§5.3 把 2/3 落到**具名文件** `docs/deploy-and-upgrade.md`（新建「插件兼容性」一节）+ **内联 jq 扫描命令**；§10.1 文件清单补入该文件。**`productMajor` 裁决：不动**——`deploy-and-upgrade.md:242` 的 `breaking` 三条判据（productMajor / wire / 破坏性迁移）本变更一条不占，撞上去会让每次升级被 `allowBreaking=false` 拒掉。**这是判断，不是发现** |
| R5 | A J6 + B M5 | `workflow_templates.rs` 的合并手术是 7 处不是 2 处；这一对**不是**顺序无关 | **ACCEPTED（本轮第二锋利）** | 复核实测：`1230-s1` 里 `WORKFLOW_TEMPLATE_KEYS` 在 `:451`/`:470`/`:627`/`:677`，`is_workflow_template_key` 在 `:628`/`:637`，生产调用方 `routes/track_templates.rs:298`（`fn` `:297`、import `:122`）= **6 + 1 import**。§8.2 换上实测清单 + 三种落地顺序的逐一结论 + **§10.1 新增验收 #7：在合并后的树上 grep 两个符号必须零输出**（并成对给出「只在本分支跑必绿」这个反例） |
| R6 | A J1（**实跑**） | §8.2 的 clippy 论断为假：`--all-targets` 的 `--lib` 目标不带 `cfg(test)`，dead_code 照常发 | **ACCEPTED** | **我独立复跑了玩具 crate**（`pub(crate) mod` + `pub const` + 只被 `#[cfg(test)]` 用的 `pub fn`），`cargo clippy --all-targets` 输出两条 `never used`。§8.2 改写为「lint 作业（`ci.yml:304-305`）**先**红，release 构建（`:901`）**也**红」；§10.1 验收 #6 的理由更正（两条都留是因为复刻两个 CI 作业，不是因为 clippy 瞎）。**结论不变：仍然删符号。** 元教训入文首：这条链 v1 提出、v2 标「独立复核成立」，到 v3 才有人真的跑（CLAUDE.md「Review Cannot Replace Execution」） |
| R7 | A m1 + B m3 | #12 的变异不现实（与 #1 近乎重复）；空白 id 空串 vs 三空格自相矛盾 | **ACCEPTED** | §10.2 新增「#12 / #13 的形状」：变异改述为**「守卫被恢复成 skip ⇒ 201/`plugin_scope=null`/不 fork」**（这才是删守卫真正招来的回归，且只有 #12 会红）；输入统一为 `"   "`；补「合法 area/省略 cwd/无 input」前提 + 断言具体准入错误 + 零播种副作用 |
| A m2 | A only | INV-SEED 缺起始状态 C（已播种但读不出） | **ACCEPTED，但换了处置方式** | 复核：若 #1230 用「读时修复」关 F13，一次 GET 就写库而 #10 两态全绿——活岔路属实。**不给 #10 加人造损坏态**，改为对 §8.1b 下**硬裁决：必须朝 500 上抛**（见 R9）。代价写进 §7：若 #1230 坚持降级，#10 必须补状态 C |
| A m5 | A only | 读口调 `admit_template` 是冗余可失败查找，失败模式是静默「无 schema」 | **ACCEPTED，且 v3 撤回了 v2 的改动** | 复核 `routes/track_templates.rs:100-111`：循环本就遍历 `WORKFLOW_TEMPLATES`，准入已成立。**规则改为「`list_track_templates` 这一行两侧都不动」**，继续直接调共享解析器 `resolve_trusted_workflow`。附带收益：与 #1230 少一个接触点（§8.3 的「两处实现改一行」降为一处） |
| B M3 | B only | **测试 #8 按 v2 配方会假绿** | **ACCEPTED（本轮最锋利）** | 复核：`track_templates_read.rs` 的 stub manifest 带 `"input_schema": stub_input_schema()`（`:106`，`required: ["issue_url"]` 在 `:61`）；无 input 的 POST 撞 `tracks.rs:977-990` 的 required-input 400 ⇒ 即便把插件 fallback 加回去测试仍绿。§10.2 加了警告框 + 三条硬要求（stub 去掉 schema 或带合法 input；断言**拒绝理由**；断言零播种副作用）+ 成对变异判据；§9 风险表新增一行 |
| B M1 | B only | 矩阵把 area-404 的优先级写反了；行 3/9/10 缺插件前提 | **ACCEPTED** | 复核：准入在 `tracks.rs:761`，area 查找在 `:863-867` ⇒「未知 id + 不存在 area」是 **400**。§4.4 新增**错误优先级树**（9 级，含事务内三类）；前提 1 改写为「它的作用是让第 5 步不触发，**不是**优先级断言」；新增前提 6（默认无 running∧trusted 插件）；行 3/9/10 各自补前提 |
| B M7 | B only | F13 只是一段话，不在任何切片门禁里 | **ACCEPTED（升级为合流硬前提）** | §8.1b 从「建议裁决」改写为**裁决 + 归属 + 门禁**：`current_definition` 必须 `?` 上抛 500（`1230-s1 routes/track_templates.rs:256-258`，回落分支 `:270-278`）；归属 #1230 作者；门禁 = #1230 侧一条真路由测试（坏掉 report 载荷 ⇒ GET 500 且库不被改写）；合流前必须已落。与 A m2 用同一条裁决关闭 |
| B m1 | B only | INV-SEED 的快照撑不起「a read stays a read」的措辞 | **ACCEPTED（两头都收）** | 复核 `log_pure_event`（`crates/calm-truth/src/db/mod.rs:683`，doc `:669` 起明写「the event itself is the only write」）。§7 把不变量**改名**为「不得物化 template 播种状态」并删掉「一次读不能触发写」这个更宽的引用；**同时**把快照扩到 `area_folders` + `events(count,max_id)` + **全部** overlay（不再筛 `kind=="template"`）；#10 的变异清单加「GET 里记一条 pure event」 |
| B m4 | B only | 「同一 resolver ⇒ 不可能广告后拒收」过强 | **ACCEPTED** | 复核 `tracks.rs:941-943` 每次重采样 running；`track_templates_read.rs:260-264` 自己演示了 stop 后 schema 消失。§6 第 3 点收窄为「**同一运行态快照内**判据相同」，并把跨请求竞态记为**已接受**（后果是一个 400，不是错误的 track） |
| B n1 | B only | `TemplateAdmission` 的说明自相矛盾 | **ACCEPTED** | §2.2 的注释改成陈述 "Admission" 的意图，并注明 v2 的自相矛盾 |
| B n2 | B only | numstat 写成 `+4/−4`（实为 `2/2`）；基线已前移 | **ACCEPTED** | §8.1 改为 `2 2` 并附两个 hunk 头；文首新增基线段（`0b4b022f`）+ 逐文件核对「新提交没碰本文引用的任何文件」 |
| A n1 | A only | §8.2 对 `tracks.rs` 的 `PREDICTED` 今天就能定 | **ACCEPTED** | 该条规则改标 **MEASURED**：两个 hunk `@@ -443,7 +443,7 @@`、`@@ -484,7 +484,7 @@`，与 `:761-814` 不相交，是今天可验证的事实 |
| A n2 | A only | 应提一句 system-area 目标的 201→404 翻转 | **ACCEPTED** | §4.2 补一段，并说明不可达（`area_create_system_tx` 用 `new_id()`，`crates/calm-truth/src/db/sqlite/area.rs:73`）+ 不为它写测试 |
| A n3 | A only | `track_workspace_materialize.rs` 是第五个读播种路径的文件，不在清单 | **ACCEPTED** | §10.1 新增「预期不需要改、但事先知会」一栏，坐标 `:224-259`（`Entry point 2 of 5` 注释在 `:224`，`async fn` 在 `:227`） |
| A m3 | A only | `manifest.json` 的 `required` 是 `:299` 不是 `:298`（且这条错在 v2 的勘误表**内部**） | **ACCEPTED** | 实测 `:299` 是 `"required": ["issue_url", "repo", "issue_number"],`。§4.4 行 6 与 §11 勘误表都已改 |
| **R10** | A m4 | 「勘误表没扫干净的其它 ±1 漂移」6 条 | **部分 REJECTED —— 6 条里 4 条是通道自己漂了** | 见下方「对通道 A m4 的逐条反驳」 |

#### 对通道 A m4 的逐条反驳（v3 逐条打开，带反证据）

| A 说 | 实测 | 裁决 |
|---|---|---|
| `known_template` 的 `fn` 在 `1230-s1 track_templates.rs:296`（文档写 `:297-301` 是错的） | `grep -n 'fn known_template'` ⇒ **`297:fn known_template(id: &str) -> Result<()> {`**，`298` 是 `is_workflow_template_key(id)` | **REJECTED**，v2 坐标正确 |
| `current_definition` 的回落分支 `Ok(Definition {` 在 `:269`（文档写 `:270-278`） | 逐行数 `sed -n '250,280p'`：`:266-269` 是四行 `//` 注释，**`:270` 才是 `Ok(Definition {`** | **REJECTED**，v2 坐标正确 |
| `find_workflow_conflict` 调用在 `mod.rs:1115-1120`（文档写 `:1114-1119`） | `sed -n '1110,1124p'`：**`1114: match find_workflow_conflict(`**，实参 `1115-1118`，`1119: ) {` | **REJECTED**，v2 坐标正确 |
| 「does not declare an input_schema」臂在 `tracks.rs:972-975`（文档写 `:973-976`） | **`972` 是 `(None, None) => Ok(()),`**；`973` 才是 `(None, Some(_)) => Err(...`，格式串在 `974`（`grep -n` 佐证），臂止于 `976` | **REJECTED**，v2 坐标正确 |
| trusted-stub manifest 在 `track_templates_read.rs:99-100`，`:107` 是它的 `input_schema` 行 | `grep -n`：`98: Manifest::parse(`、`99: &json!({`、**`106: "input_schema": stub_input_schema(),`**、**`107: "workflows": [ { "id": ISSUE_DEVELOPMENT } ],`** | **REJECTED 其结论**（`:107` 确实是 workflows 行，v2 在 §5.1 表里用它指「声明 workflow 的站点」是对的）；**ACCEPTED 其动机**：§10.2 的「manifest 在 `:107`」措辞含混，v3 改为逐行给 `:98`/`:99`/`:106`/`:107` |
| §10.1 写 `forge_workflow_e2e.rs`（改 `:425-429`）与 §10.3 的 `:421-422`+`:427` 自相矛盾 | 实测：注释 `:421-422`、`assert!` 块 `:423-429`、文案 `:427` | **ACCEPTED**：§10.1 改为 `:421-429` 并注明构成 |

**这条本身是本轮最值得记住的一课**：通道给的「引用漂移修正」如果不复核就照抄，
等于把错误坐标洗进设计文档——而 v2 恰恰因为照抄了一条通道坐标，在自己的勘误表里
留下了 `manifest.json:298`（A m3 抓到的那条）。**v3 的做法是两个方向都验。**

#### 本轮没有被采纳的其它东西 / 明写的判断

1. **行 17 的显式 fork 前移**：不做，接受残余（理由见 §4.2）。**判断**。
2. ~~**`productMajor`**：不动。~~ **v4 推翻**：动，见 §11 的 H1。这不再是本文的判断，
   而是人的决定 + 一条实现指令（改 `package.rs:307` 的默认值，否则裁决是空的）。
3. **#10 的起始状态 C**：不加，改为对 §8.1b 下 500 的硬裁决（§7）。**判断**，
   且它把两个通道的两条发现（A m2 / B M7）用一条裁决同时关掉。
4. **#9 不再自称集合相等门禁**：这是把 v2 的一句过强表述改诚实，不是降低要求——
   真正的结构保证由「写口只有一条名册查找链」承担，`grep` 与 #8 是它的两条辅助腿。

### v1 → v2（2026-09-01）

两个独立评审通道（A = subagent、B = codex 只读）均给出 **NEEDS-REVISION**，无 BLOCKER。
下表逐条裁决。**每条都在本 worktree 里独立复核过 `path:line` 后才落笔**；
被驳回的条目给出反证据。

| # | 来源 | 发现 | 裁决 | v2 做了什么 |
|---|---|---|---|---|
| F1 | A+B | 错误分类矩阵有多行错（无 404、happy path 无条件 201、缺前提、状态码与正文混列、「ensure 后找不到」以偏概全） | **ACCEPTED** | §4.4 整表重建：拆状态码/正文两列、前提统一前置（5 条）、补 area-404 行与 required-input 行、把 `ensure` 内部失败与「lookup miss」拆成两行、点名**两处**有意变更（A: 201→400；B: 错误正文） |
| F2 | A+B | 新文案打红 `track_workflow_templates.rs:586` 与 `forge_workflow_e2e.rs:427`；后者不在文件清单里，且其立意在统一后死掉 | **ACCEPTED** | §10.3 新增：两处改钉「正文点名被拒的 id」；`forge_workflow_e2e.rs:421-422` 的注释改写；§10.1 文件清单补入 `forge_workflow_e2e.rs`。**否决了「直接删掉文案断言」**（400 有六种来源，只看状态码不够） |
| F3 | A+B | 「只剩 2 处权威、Rust 常量降为 bootstrap」不成立 | **ACCEPTED** | §2.3 重写为 5 类（名册 / 可编辑内容 / 不可编辑 intro+contract / binding 声明 / binding 生效），点名 3 类会漂移，**删掉所有「只剩 N 处权威」的口号**。A 与 B 在此不冲突，B 更完整，采 B 的分类 + A 的 `restamp` 精确条件 |
| F4 | A+B | INV-1209-SEED 被计数断言钉不住（全称量化 / 只枚举 overlay / 只测未播种态） | **ACCEPTED** | §7 收窄为「这两条路由 × 两种起始状态 × 快照相等」；§10.2 #10 给出 snapshot 测试形状 + 5 条必须变红的变异；**明写「其它端点不许播种」本切片钉不住**（拒绝写真空不变量） |
| F5 | A only | #1209 先落地则 `is_workflow_template_key` / `WORKFLOW_TEMPLATE_KEYS` 变死代码，release 构建必红 | **ACCEPTED**（**最重要的一条**） | 独立复核全部成立：生产调用点只有 `tracks.rs:779`/`:800`；其余在 `#[cfg(test)] mod tests`（`workflow_templates.rs:372` 起）；`pub(crate) mod`（`lib.rs:635`）；`RUSTFLAGS: "-D warnings"`（`ci.yml:15`）；`cargo build --release -p calm-server`（`ci.yml:901`、`:1012`）；~~`clippy --all-targets`（`:305`）抓不到~~ ← **v3 实跑推翻，见 R6：clippy 其实先红**。§8.2 给出**顺序无关**的解：删两个符号、新增 `workflow_template()`（有生产调用方）；§10.1 验收 #6 加了 release 构建 |
| F6 | A only | `ensure` 早于 cwd 校验与 area 404，4xx 也会先播种 6 行，打脸 `tracks.rs:759-760` 的注释 | **ACCEPTED**（采纳「本切片搬动」而非「记账」） | 新增 §4.2：播种块移到 area 404 与 cwd 校验之后；同时把 `:759-760` 的注释改写成**搬动后仍然为真**的版本（事务内 409/500 之后播种仍在，诚实写出）；pin = 测试 #13 |
| F7 | A+B | `ResolvedTemplate.title` 是无消费者的出厂标题副本 | **ACCEPTED** | §2.2 删字段；并采纳 B 的补充建议把结构体改名 `ResolvedTemplate` → `TemplateAdmission`、函数 `resolve_template` → `admit_template`（它回答准入，不回答「模板现在长什么样」） |
| F8 | A only | 测试 #9 的集合相等腿是假门禁，变异列归错机制 | **ACCEPTED** | §10.2「#9 的形状」重写：删掉「读口 == Rust 常量」腿，改成**路由 × 路由**；变异列更正为「让写口特别接受未列出 id / 特别拒绝已列出 id」；并说明真正的漂移已被 F12 的派生消除，不需要相等测试 |
| F9 | A only | `trim().is_empty()` 守卫在新草图里结果中性且无测试钉 | **ACCEPTED** | §4.1 删除该守卫，并明写代价（今天 Rust 侧零覆盖）；§10.2 新增测试 #12 补 pin |
| F10 | A + B(NIT) | §4.2 的机械判据可被重新伪装的特例满足 | **ACCEPTED** | 原 §4.2 → §4.3，机械判据换成**语义判据 + 路由级集合测试（#8/#9）**；grep 降级为「必要不充分」；B 的 NIT（草图里那句「只被 and_then/map 消费」本就不符合草图）一并更正 |
| F11 | B only | 201→400 是**公开插件契约破坏**，不是低风险 | **ACCEPTED** | 独立复核 `manifest.rs:93-100` 的字段文档确实承诺了这个能力，`forge_trust.rs:1-8` 确实是 env 可配。§5.3 重新定级为 breaking，缓解升级为硬要求（改字段文档 + release note + 升级前扫描），可选 spawn-time warn（留人裁）；**明确否决**「过渡期先警告后拒绝」并给出理由 |
| F12 | B only | 「单一名册」实为两个可独立漂移的数组 | **ACCEPTED**，且与 F5 合并成同一个编辑 | §2.3 类别 1 + §8.2：删 `WORKFLOW_TEMPLATE_KEYS`，`workflow_template()` 从 `WORKFLOW_TEMPLATES` 派生，#1230 的 `known_template` 改走同一入口 |
| F13 | B only | #1230 的 `current_definition` 在 report 读失败时静默回落常量并报 `seeded:false` | **ACCEPTED（记为 #1230 的 pre-merge 裁决项，不在 #1209 范围）** | 新增 §8.1b：给出建议裁决（上抛 500 或三态化），并说明 **#1209 不依赖它被修好**（本设计只依赖 `tracks.rs:805-812` 的 fork 事实，不经过 `current_definition`） |
| F14 | B only | #1230 合流面被低估：测试文件同锚点追加；`b93fb767` 的 parent 不是当前 main；无 #1209 实现 diff | **ACCEPTED** | §8 开头更正 parent（`d27014d8` vs `6e0339b0`）并给所有 #1209 侧判断打上 `PREDICTED，待实现 diff 复核`；§8.2 把 `track_workflow_templates.rs` 列为人裁文件（本 worktree 589 行，#1230 从 EOF 追加 402 行，同锚点）。人裁文件从 1 个变成 **3 个**（+`workflow_templates.rs`，见 F5） |
| F15 | B only | §5.2 的 C 方案里「manifest 严格 unknown-field 解析 + 字段白名单」是发明的 | **ACCEPTED** | 独立复核：`manifest.rs:15-20` 明写容忍未知字段；`:467-475` 明写 `WorkflowDescriptor` 忽略额外键，测试在 `:1407-1416`；`:761-765` 是「connector 不得声明 workflows」而非白名单。§5.2 删掉该论据，换成真实成本（内容权威归属 / picker 展示 / 生命周期 / 名册运行时化） |
| F16 | A(m6) + B(MINOR 1,2) | 引用漂移 | **ACCEPTED（逐条核，部分更正**通道给的坐标**）** | 见下方「引用更正清单」 |

### 引用更正清单（F16 的落地）

两个通道各给了一批坐标；我逐条打开核对，**大部分成立，少数通道自己也偏了一两行**。
v2 采用的是我自己读到的坐标：

| 位置 | v1 写的 | v2 更正为 |
|---|---|---|
| contract prefix 定义 | `workflow_templates.rs:99-105` | `crates/calm-types/src/track_report.rs:137-144`（`:104` 是调用点） |
| `report_startup_read_required` | 未引 | `crates/calm-types/src/track_report.rs:184-187` |
| `restamp` 取常量 / 早退 | `:581-604`、`:592-594` | 取常量 `tracks.rs:586-590`、早退 `:592-594`、整函数 `:581-613` |
| git-forge `workflows` | `manifest.json:305-309` | `:302-306`（`input_schema` 在 `:273-300`，`required` 在 **`:299`** —— v2 这里写 `:298`，是**勘误表内部自己漂了一行**，v3 更正，见 R10 上方的 A m3） |
| `CreateTrackRequest.workflow_id` | `tracks.rs:209` | `:210`（struct `:197`、`deny_unknown_fields` `:196`、`as_template` `:224` 原本就对） |
| `track_templates.rs` 模块头 | `:1-43` | `:1-39`（`:41` 起是 `use`） |
| schema 违反的 400 | `tracks.rs:988-989` | `:992-993` |
| required 缺失的 400 | `tracks.rs:981-985` | `:986-989`（整臂 `:977-990`） |
| §6 的 schema 校验点 | `tracks.rs:986-989` | `:992-993` |
| MCP 读 `plugin_scope` | `tool_visibility.rs:114-128` | `:109` |
| system-area fork 过滤 | `tracks.rs:507-509` | `:505-507` |
| 播种写入 | `tracks.rs:446-455`、`:459-484` | 循环 `:449-455`、建 area `:459-485`、建 track+落 report `:517-579` |
| 迁移测试里的 `workflow_id` 列 | `track_plugin_scope_migration_tests.rs:78-83` | `:65-72`（`:76-85` 是 manifest fixture） |
| `bound_workflow` | `planner_harness_start_adapter.rs:161-177`、fail-safe `:181-188` | `:162-180`、fail-safe `:181-190` |
| `WorkflowDescriptor` | `manifest.rs:473-475` | `:472-475`（doc 从 `:467` 起） |
| `#1230` 版 `current_definition` | `:256-279` | 命中分支 `:256-264`，回落分支 `:270-278` |
| `#1230` 的 read-only 测试 | 「#1230 追加段」 | `1230-s1` `tests/cases/track_workflow_templates.rs:634-665`，helper `:168-184` |
| trusted-stub boot 配方 | `track_templates_read.rs:39-167` | `:77-167`（`boot(running: bool)`，manifest 在 `:107`） |
| `forge_workflow_e2e` 各断言 | `:157`、`:167-171`、`:441-453` | `:155`/`:156`/`:157`/`:158`/`:165`/`:172`；untrusted 段 `:434-454`；stop `:456-459` |
| `find_workflow_conflict` 调用点 | `mod.rs:1085-1122` | `:1114-1119`（注释里的三个消费者在 `:1093-1095`，原本就对） |
| §8.1 文件表 | 缺 `openapi-contract.test.ts` | 已补（`git show --numstat b93fb767` 的 9 个文件全列） |
| §5.1 扫描 | 4 个站点 | 13 行的完整表（`rg 'workflows:\|"workflows"' crates/ plugins/`） |
| `fe/.../new-track/public.tsx` | `:248-256` | `:247-256` |
| `tracks.rs:3060-3092` | — | 判定在 `:3085-3092`（原范围不错，补精确行） |

### 通道分歧与本文的判断（明写）

1. **F11 的缓解手段**：B 给了三选一（release note / 升级前扫描 / 过渡期告警后拒绝）。
   本文**同时要求前两条**、**否决第三条**、把 spawn-time warn 列为可选并交人裁。
   否决理由写在 §5.3：过渡期方案要求内核维持「可绑定但不是模板」这个类别活着，
   而这正是本 issue 要消灭的二元性。
2. **F6 的处置**：A 给了二选一（搬动 / 记账并修注释）。本文选**搬动**，
   并且**同时**改注释——因为搬动之后那句注释仍然不是全真（事务内 409/500 之后播种仍在）。
   A 的「搬完就让假注释变真」这个说法本身略乐观，这是本文对 A 的一处修正。
3. **F2 的处置**：A 给了「决定改钉什么，或删掉文案断言」。本文选**改钉**并明确否决删除。
4. **F3 的分类**：A 只要求「写出精确条件」，B 要求「拆成五类」。本文采 B 的结构，
   因为只写精确条件仍会留下「2 处权威」这个数字，而那个数字本身就是错的。
5. **§4.4 行 8 的误导文案**：两个通道都提到，但都没要求本切片修。
   本文**明确不修**并记为后续候选——这是一个判断，不是发现，欢迎下一轮推翻。

### 仍需人裁的（本文没有替用户决定）

**v4 把这张单子重新扩到 5 条**——两条来自 v3（下面第 1/2 条），三条是本轮新产生的。

**⚠️ 需要人先看一眼的一条（v4 新增，排最前）**：
**`web/` 才是今天在跑的前端 bundle，`fe/` 是还没上生产的那个**（§1.6 的实测表）。
人说「新的 FE 还没有上生产，所以可以破坏兼容性」——这句话对 `fe/` 成立，
但 `web/` 是活的生产客户端，本设计因此要求 `web/` 在同一次发布里跟着改名，
并把 `WEB_COMPAT_VERSION` 抬到 17 让缓存里的旧 bundle 拿到「请刷新」硬遮罩（§3.6）。
**如果人的本意是「连 `web/` 也不用管」，那 §3.6 可以简化；
如果人不知道 `web/` 是生产的那个，这条需要确认。** 本文按「要管」写。

**第二条要人签字的（v4 新增，v5 按实测更正措辞）**：升级判决变成 `breaking`
⇒ `allowBreaking=true`、杀掉并重启 calm-server 与 proc-supervisor、
**没有 `preserving` 路径那套健康检查自动回滚**。

> **v5 更正（§3.7 的 X1 驳回）**：v4 写的是「回滚只剩升级前手动备份」，
> 而实测 **`apply_breaking`（`apply.rs:364`）在 calm-server 变更时 `:375-376` 就会自动备份**，
> `backup_db`（`:604`）还会先停服再复制三件套。
> **所以要人签字的不是「记得手动备份」，而是这三条**：
> 1. **`dryRun` 会说 `requiresDbBackup: false`**（`preflight.rs:104` 对 breaking 硬编码），
>    而实际上会备份——操作者按 pre-flight 检查表读到的是假信息；
> 2. **`POST /upgrade/rollback` 拒绝回滚一次 breaking apply**
>    （`rollback_last_preserving`，`apply.rs:1252`，判定在 `:1266`）——
>    **备份文件在磁盘上，但没有任何 API 能把它放回去**；
> 3. 加上 §3.3 的 forward-only 迁移，**恢复只能手工三步**（停 unit / 放回备份 / symlink 指回旧 release），
>    这三步要写进 `docs/deploy-and-upgrade.md` 的 pre-flight 小节。
>
> 人已经在原则上接受了 breaking，但**这三条具体后果本文是第一次写准**。

**第三条（v4 新增）**：切片切成一个 PR 还是两个（§10.1 的切片裁决表）。
本文推荐两个（PR-1 概念 / PR-2 拼写，同一次发布）；一个也可行，代价是评审
`:779` 那条判据的人要在一份以改名为主的大 diff 里找它。

**从 v3 继承下来的两条**：

* §5.3 的可选项 4：spawn 准入处对「名册外 workflow id」打 warn。做，就给
  `plugin_host` 引入一个对 `workflow_templates` 名册的新方向依赖；不做，
  就只能靠 release note + 升级前扫描触达第三方插件作者。
* §4.4 行 8 那条误导文案要不要顺手改（约 5 行 + 若干断言）。
  **三轮评审都提到、三轮都没人要求本切片修**，本文继续不修。

~~**移出本单子的**：§8.1b（现为硬裁决，若 #1230 作者不接受，回到 §7 的状态 C 分支）。~~

**v5 更新**：**§8.1b 已经彻底结案**——上游（`1230-s1@3b9cc03c`）已经把 `current_definition`
改成 `?` 上抛，注释还逐字复述了本文的发现。它既不在本单子里，也不再是任何人的待办
（§8.1b 的 CLOSED 方框，§11 的 T17）。**§7 的状态 C 分支同时作废。**

**v5 新增的两条「本文自己决定、写出来供推翻」的判断**（不需要人签字，但评审可以推翻）：

1. **`WEB_COMPAT_VERSION` 走选项 (a)（比较三处的 CI 静态门禁）而不是 (b)（单一源生成）**，
   §3.6。(b) 才是符合「派生优于测相等」的最终形态，但它要给两个前端各接一段生成，
   本切片只抬一次版本号，不值得。(b) 进 §9 非目标 12。
2. **`docs/deploy-and-upgrade.md` 整节留在 PR-2 而不是拆成两半**，§5.3。
   拆开的话同一个小节要在两个 PR 里各改一次，评审成本大于收益。
