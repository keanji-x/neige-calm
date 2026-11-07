# #3544：终态历史与原生句柄的生命周期

L2：涉及原生进程所有权和持久化边界。目标是在保留历史的正常终态归还
FD，不借助 Retire、扩大清扫、迁移、资源上限调整或生产重启。

## 所有权与顺序

- legacy renderer 的 attach reader 只在收到真实 `Exited`、完成原有退出
  持久化及回调后，通过既有 `exit_persisted` 通知 control writer 结束。
  writer 在请求边界处理退出，保留进行中写入的 ACK；关闭新入队后，完成
  已接受的 Stop 再释放连接，不把未开始的 Write 回报为成功。registry、最终
  屏幕及 sticky exit 保留。输出断连不产生退出通知。显式 teardown 继续使用
  既有 direct signal 入口，其最终 abort 是 renderer 控制任务的取消边界。
- typed Executor 由各平台原 monitor 收尾：Linux 必须先有 Drained、完成具体
  helper reap，再等待输出收尾并持久化；macOS 维持 original group empty、
  reap、bounded finish 顺序，只有 Complete 才成为 OriginalGroupEmpty。
  成功收尾后移除 Process map 的 native owner，不用终态枚举代替 reap 证据。
  Unknown、不完整原生所有权和持久化失败不授予额外回收或控制权限。
- OutputLog 保留路径、cursor、tail 上限等元数据。输出 reader 结束并完成原有
  sync/结果持久化后关闭 writer；Observe 在日志锁内按需只读打开。历史启动
  只读验证文件及 Complete.final_cursor 长度后关闭，不创建缺失文件，不把
  错误降级为空输出；沿用 O_NOFOLLOW/O_CLOEXEC 私有文件选项，历史
  只读打开使用 O_NONBLOCK 并拒绝非普通文件。
- Observe、收尾、Retire 沿现有 scope gate 序列化；日志锁协调 reader 与
  snapshot。Retire 仍是授权后的删除历史操作。私有源目录继续由
  release_private 的 durably_settled 规则独立管理，不复制其规则。

## 验证与边界

真实 prepare/Launch/Observe 和 renderer ensure 入口重复自然退出，保留历史且
不 Retire，验证 FD 有界、历史重开、tail/cursor、late viewer、活动 peer 和
显式 Stop。Linux 的 reap barrier/OutputOpen、Unknown 测试守住提前释放边界；
macOS 原 group 和输出预算测试交给原生 CI。关键断言只做少量单因素变异。

迭代只运行指定新回归及 monitor/output/control_writer；最终 quick、text、
contract 和重点重放由任务 gate 执行，广域套件由 CI 执行。新增 macOS job
沿 rust_changed 选择，各职责模块独立拒绝空选，不维护逐项名单。
普通新增或重命名模块内测试无需修改 job；新增测试文件只需原 suite 注册。
每个选定模块在该 job 中运行一次；既有 broad suite 不变。新增跨平台
回归在既有 Linux suite 和 macOS job 各运行一次，Linux 专有用例仅前者。


## 首轮 L2 阻塞修复：完整 Stop 的生命周期

`bcd3b968c` 的 writer 在收到 `exit_persisted` 后直接退出，但 WebSocket
Owner 的 `ClientMsg::Kill` 仍由 pump 分别入队 HUP 和两秒后的 KILL。真实
supervisor 允许 leader 退出后经过 50ms drain grace 发布 `Exited`，此时同组
忽略 HUP 的后代仍可能持有 PTY。关闭 writer 会让后一次入队失败。

控制链保持为 `ClientMsg::Kill` → session 的 owner 校验 → `Effect::KillChild`
→ renderer 的 `SupervisorControl::Stop`。停止政策属于 renderer IO owner，
协议层不新增信号、生命周期或重连概念：

- legacy writer 用原控制连接执行整个 HUP → 两秒 → KILL。中途的 Exited、
  pump 结束或 sender 释放都不取消已经接受的操作；leader 退出不替代 group
  退出证明。没有额外 timer task、按 proc_id 重连或失败切换入口。
- writer 的退出分支先关闭 receiver，再排空已接受的 Stop；成功入队与关闭
  竞争的 Stop 不丢失。未开始的 Write/Resize 丢弃，进行中的 Write 仍完成原
  exchange/ACK，原来的 unknown-write fencing 不变。没有待完成 Stop 时，
  自然退出立即释放 UDS，不长期保留空闲控制连接。
- typed 的 pre-Spawned 和 live writer 都通过既有 `request_typed_stop`
  提交一次 `Desired::Stop`，沿原事务、ExecutionId 和 drive ticket 规则。
  不使用 legacy 的信号序列或两秒延迟。typed 的 `shutdown_signal` 调用者
  也映射到同一 Stop 队列，不新增并行政策。
- registry teardown 仍通过 direct TERM/KILL 完成原所有权范围清理并取消
  writer。取消同时取消其中的 sleep；旧任务不能在同 terminal id 重建后
  重新连入新执行。supervisor、外部协议和 typed native monitor 无需修改。

扫过所有 `SupervisorControl` 生产者及匹配：pump、broadcaster、state clients、
writer、typed pre-Spawned/live 转发、RendererEntry shutdown。原始 Signal
variant 已无队列调用者，删除而不保留兼容分支。direct teardown 不受此改变。

## 本轮验证范围

- `fd_resources_owner_kill_finishes_after_leader_exit_and_pump_end` 走真实
  registry.ensure、pump 和 supervisor：确认后代在 leader HUP 退出后仍活，
  主动结束 pump，再确认 KILL 结束后代、释放控制 socket，late viewer 仍可
  读取屏幕和 sticky exit。红跑也先用既有 direct 入口清理隔离进程。
- `fd_resources_teardown_cancels_stop_before_same_id_replacement` 在 HUP 后
  teardown，再用相同 terminal id 创建新执行，越过旧升级期限后验证 input
  ACK。原自然退出/活动 peer/历史/输出断连回归保持不变。
- writer 测试固定退出和成功入队同时 ready 的竞争，验证不开始排队 Write、
  不伪造 ACK、完成 Stop；另覆盖 HUP 后 exit/sender 结束、取消无延迟信号、
  observer Kill 被拒绝。既有 admitted Write 与 split write ACK 测试继续重放。
- typed 新测试经真实 WebSocket 覆盖未 Spawned 和 live 两个分支，先分别
  验证 pre-ready 拒绝和实际 InputAck，固定分支事实；观察持久化 Desired::Stop
  并用数据库 trigger 计数确认一个 Kill 只有一次 Stop 事务。
  保持连接超过 legacy 延迟，防止延迟的重复请求漏检。

本轮仍为 L2（控制生命周期与身份隔离）。只改必要 server、测试和本设计；
正式双路评审由 Planner 安排。macOS、Tier1 和广域验证交 CI，本机 Linux
结果不代表 macOS 原生通过。新增 fd_resources 用例保留前缀，自动进入原
macOS job；新增 writer 子模块在原测试模块注册一次，不增加 CI 名单或检查。
每个选定用例在原 macOS/Linux suite 各运行一次（writer/typed 依原 suite），
没有新增永久源码 pin；普通模块内新增/重命名测试无需额外修改检查脚本。

## 退出持久化失败的成功边界

`exit_persisted = true` 必须证明本次退出链中所有必要、已尝试的持久化都
成功，而不是仅证明 reader 跑完了 `Exited` arm。terminal 写入、临时 session
完成和 task hook 各自保留原 owner；reader 只累积这些入口的失败结果，仍按
原顺序广播真实退出、尝试各项回调并唤醒 projector，最后仅在全链成功时
通知 writer。task hook 将已有读写和完成事务的错误返回调用者；任务类型、
Pipe、授权、幂等和作用域判断继续属于 scheduler，合法无操作返回成功。
running-terminal sweep 仍负责原有重试；不增加 reader 重试或新后台任务。

失败 reader 结束并关闭仍为 false 的 watch，`await_exit_persisted` 走
`ReaderGone`，writer 沿既有 pending 分支保留原连接。已接受的 Stop 仍完成
HUP → KILL；既有显式 teardown 保留 TERM grace、无条件 group KILL 和最终
abort。无 repo 或有 repo 的成功退出继续释放 writer，失败不取消真实退出
广播、最终屏幕或历史，也不从 terminal id 重连到另一个执行。

`fd_persistence` 回归走 `new_with_repo`、`ensure`、真实 supervisor 和 SQLite，
仅在启动完成后为目标行安装拒写 trigger，分别拒绝 terminal、session、task
退出写入。无输出 fixture 的退出 wake 证明各回调和失败返回均已发生；检查
数据库、成功通知、writer 接收端及显式 teardown，成功对照验证完整持久化与
自然释放。关键成功判定用一次单因素生产变异验证完整 renderer filter。
新测试沿 `fd_resources_` 前缀加入原 macOS 选择，在原 suite 注册文件一次；
模块内新增或改名只改该测试文件，没有新增脚本名单、CI 步骤或永久源码 pin。
正式 L2 双路重审仍由 Planner 安排，本机不声称 macOS 验证。
