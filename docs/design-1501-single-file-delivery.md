# F4：一个被验证版本的文件交接

兼容补充（CR-1501-F4-COMPAT）：总任务 orchestrator 批准同步提高后端和两套
前端的 Web compatibility floor 至 25，并更新旧前端事件失效策略。旧 bundle
不能只读取服务端 event version 后继续接收它不认识的新判别值。同步刷新既有
失效列表 mutation 的补丁上下文，保留其单因素变异和原有预期红集；不放宽门禁。
该申请仅涉及版本常量、相应事件接线与既有验证，无样式或新 UI 功能。

状态：首版已实现，随 [#1581](https://github.com/keanji-x/neige-calm/pull/1581)
完成最终检查合入生效。基线 `965cad698`，关联 #1501、#1492。
承接 Planner 功能实施计划；只推进一个来源、一个普通 JSON 文件、一个消费者。

## 用户闭环

A 的公开执行契约声明 JSON 输出；B 声明消费 A 的该输出。A 完成并确认停止后，
内核封存文件，在封存版本上检查 JSON 格式。B 的 claim 原子绑定这个版本，
Operation 准备并核验输入后才启动 B。Planner 无须声明搬运任务、复制临时路径
或计算文件 hash。原工作区随后变化或删除，都不能改变 B 的输入。

首版验证策略明确为 JSON 文档格式。它只证明文件符合这项公开要求，不证明任意
业务内容正确，不替代另有要求的语义验收或机器 gate。验证证据必须指明策略和
实际内容版本；不把 Worker 自报、task done 或普通工具成功当成文件验证。

## 已核实的边界与实施选择

- `calm-task-artifacts` 已有不可变存储、幂等捕获和物化，但没有生产调用者；
  当前捕获会扫描整个 Git 工作区。输出槽位并不能限制捕获范围。
- 隔离 Codex 已有可信的命名空间停止凭据和带 owner 标记的工作区；它不是 Git
  工作区，包含私有 `.codex`，不能为复用库而伪造 Git 身份或扫描所有文件。
- 因此扩展原库的一个显式普通文件捕获入口：从内核已验证的文件描述符捕获，
  只保留声明的文件，复用原有摘要、持久化、重放与物化算法。原 Git 捕获契约不变。
  这是比旧总纲“首版 Git 工作区”更窄的实际文件范围，保留其不可变性与停止边界。
- 已有下载接口从可变保留目录读取，且仅供 User 使用；不把它改造成交接协议。
- 复用 scheduler 的唯一 claim、Operation 的外部动作恢复和 task context 冻结。
  不引入新的调度器、日志系统、模型轮询或全 Provider 接口。

## 必须保持的契约

1. 输出路径、槽位和 JSON 验证策略属于公开的冻结契约。只支持一项普通文件；
   路径遍历、保留私有路径、链接、特殊文件、超限和不支持的路线明确拒绝。
2. 源 attempt、Operation、停止证明与根目录身份都由内核解析。验证后的描述符
   必须到达最终文件读取，不能转换回路径后重新猜测来源。
3. 捕获和验证通过 recoverable Operation 执行。稳定操作身份先于外部效果；
   已冻结请求重放只能使用原快照，不能重读“现在的源文件”。
4. 消费资格必须指向该快照及 JSON 策略的实际成功证据。格式失败或准备失败
   保留具体原因，B 不得启动；不把旧 task done 改写为“交接也已完成”。
5. B claim 在同一事务冻结源 attempt、快照、用途、目的位置及保留引用。之后
   的重启、源变更或重试不能静默切换版本。原有恢复不能绕过冻结输入契约。
6. 物化至新建的内核控制输入目录；冲突的既有目录不能覆盖。物化回执必须与
   冻结绑定相同，启动时仍经过原有授权和 TaskLaunch 围栏。
7. 无文件消费的纯顺序关系继续保持原语义。首版不直接解禁隔离路线的任意
   dependencies/gates；只承认本闭环明确实现的消费能力。

## 验收与范围控制

通过真实 authoring、claim、Operation、文件存储和启动入口验证 A→检查→B，
替换 Provider 仅限明确的外部进程边界。再做一次独立本地 Planner 实验。

必须检查：捕获后修改/删除源文件，B 仍收到原字节；错误版本、错误来源、损坏
存储、准备重放冲突、撤回授权均不能启动 B；重启不重新选择输入。关键不变量
执行有预测红集的生产 mutation，两路独立完整审查及相应 gates/CI 收敛后合入。

多文件/目录、Git 整树、任意 gate、多个来源、合并冲突、全局 GC 和完整失败
候选继承不加入本 PR。F5 后续复用本次精确绑定与准备机制，单独增加修复用途。

## 内核接线

执行选择显式区分空工作区的单文件 producer 和带文件输入的 consumer；旧选择
保持不变。不复用 `depends_on` 暗示文件复制。首版 producer 不能同时声明输入，
consumer 不能同时声明受本协议管理的输出；由此限定为两节点闭环。

输出公开声明槽位、源相对路径和 `json-document-v1` 验证策略；输入公开声明
本 Track 的来源 key、槽位及 JSON 输入用途。consumer 必须引用实际 producer，
未知/自引用/不支持的来源作为声明诊断，不猜测其他 Track 或旧成功执行。

复用 scheduler 的异步单飞模式：声明的 producer 输出本身就是 publication 意图，不依赖消费者已存在。当 producer 报告完成时，在 Track 调度锁之外
等待其现有 Operation 真正停止并成功，然后提交稳定身份的文件 publication
Operation。该 Operation 在停止凭据及冻结输出契约下捕获单文件、重新打开不可变
快照并验证 JSON，将成功证据持久化。消费者等待既有 OperationCompletionBus，
完成后重新 poke scheduler；丢通知及重启由已有操作恢复/调度 sweep 处理。
不新增业务完成事件，不用一次新的 TaskCompleted 伪装文件已验收。

实施审计修正：OperationCompletionBus + poke 不能唤醒已收到 A 完成报告的 Planner。
因此 publication Operation 实际终结后，追加一次 kernel-only
`task.file_publication_settled`（producer attempt、publication Operation ID）。
它不修改 task 的业务状态，不授予重试权限。沿用已有 Event/Dispatcher/Harness
SystemContext 及持久化 catch-up 水位；producer sweep 修复 Operation 终结与事件
追加之间的崩溃窗口，事务内按 Operation 去重。不新建 outbox 或观察引擎。
新事件版本为 19，0102 迁移和生成协议同步更新。

publication 是“已结束执行的产物动作”，不是新的 Worker 启动。注册时必须显式
列入这种任务绑定类别，校验当前 producer、接受的完成报告、停止凭据与冻结的
claim 契约/ready/release；不能把它伪装为非任务操作，也不能修改既有启动守卫
允许 terminal task 启动。共用契约比对可从现有校验中抽取，启动状态判定仍保留。

来源已有 TaskDone 但文件缺失或格式无效时，publication 失败并说明输出尚不具备
消费资格；B 保持未启动。首版不会偷偷修改/重跑 A，或把这样的失败伪装为业务
重试成功。修复用途及新的源执行决策仍属于后续明确的恢复能力。

新增迁移保存两类记录：

- publication 记录绑定 Track、producer attempt、源 Operation、publication
  Operation、输出契约、快照/文件摘要、验证策略及结果。只有对应 Operation 成功
  且证据匹配才能消费；失败留在该 Operation 的诊断中，不重写旧 task done。
- input binding 在 B claim 事务绑定上述来源、快照、用途及固定输入目录；
  源自原 attempt 的恢复保留同一绑定，无法证明原绑定时拒绝启动。准备回执
  另有明确 bound/prepared 状态；身份列不可修改，不能依赖“最新”选择。

记录保留至 Track 删除；首版不做 GC，不能跟随 Worker card/源工作区删除清理
被引用内容。旧 migration 不修改；旧任务不会被默认补成具有文件消费权限。

B 的已有 isolated Operation 创建 owner 标记后，在受保护的输入子目录物化。
重启遇到已存在目录时，只能逐项验证它确实等于冻结绑定；不同目录不能覆盖，
不能因出现目录就伪造准备成功。TaskLaunch 前再次验证输入和原有当前任务授权。
Worker 收到内核提供的相对输入位置与来源说明，不接触宿主机临时路径或凭据。

plan.list 暴露 producer publication 的实际状态、consumer 等待原因及输入绑定
摘要。格式无效、缺文件、源执行未停止、授权撤回及准备冲突都必须能解释，
不从失败推导自动重试权限。更强的业务验收策略不在首版能力范围内。

## 首版验收及后续

真实 Planner 声明两个隔离任务：A 输出 JSON 数组及标签，B 读取内核准备的输入，
核对字段并求和得到 10。实际只产生两个业务 attempt 和一个文件 publication
Operation。外部观察者核对了两个停止证明、publication 与 claim/准备记录、
快照及文件摘要、B 的实际输入字节和结果文件；Planner 没有搬运节点、复制命令
或手工计算交接 hash，仍有正常的报告维护与验收读取。实验实例已停止。

源文件变更/删除、损坏、撤回、准备重放、同合同恢复和失败通知通过生产入口
集成测试验证；不把这些说成真实模型中的故障实验。库的 source-opener 重放和
输入目录身份断言另经有精确预测红集的生产 mutation 验证。

下一步是 F5 的明确修复用途。本首版不包含任意业务验收策略、整树/多文件、
多来源合并、自动业务重试或 GC。Track 删除会释放相关数据库记录，但首版没有
物理快照回收；不能把 metadata 删除说成所有封存字节已删除。


## CR-1501-F4-EVENT（orchestrator 已批准）

为 publication 后失败/成功提供持久化 Planner 唤醒，批准仅扩展
`task.file_publication_settled` 的事件 union、runtime schema 与已有 invalidation 映射。
范围：`fe/core/api/generated/wire.ts` 及真实 generator 必需的事件/OpenAPI 产物，
`fe/core/api/schemas.ts`、`fe/core/events/invalidation-plan.ts` 和对应小型契约测试。
不包含 UI、全局样式、transport 分叉或新的协议引擎。事件身份由 kernel 终结的
publication Operation 提供；live/catch-up 共用既有 SystemContext 管线。
冻结产物的实际更改路径在提交及 PR 中逐条保留 `OWNERSHIP-CHANGE` trailer。
