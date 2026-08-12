# #985 S6B flake 定向排查

## 现象与复现

- 已知全量门曾以 0.116s 错值失败；隔离、`-p calm-truth`、全量重跑各一次通过。
- 本机不需全量并发即可复现：CI profile 隔离循环前 4 次通过、第 5 次失败（0.119s）；
  default profile 前 2 次通过、第 3 次失败（0.115s）。两次都不是超时，实际诊断为
  `raise tree_task_budget to at least 10`，而测试在第 1524/1530 行硬编码期待 9。
- 最小概率构造：重复
  `cargo nextest run -p calm-truth --locked --profile ci -E
  'test(legacy_member_overage_freezes_new_blocks_across_the_tree)'`；本次 5 次内命中。
- 临时打印生产 writer 写下的 `(id,created_at)` 后，第一次即得到
  `root=7087…6c60, child=5f77…6767`，二者均为 `1786390968342`；SQL 次序是
  child、root，随即以 minimum=10、0.109s 失败。插桩已用 `git checkout -- .` 复原。

## 假设逐项排查

- **临时目录/SQLite 路径**：该测试唯一 repo 是 `SqlxRepo::open("sqlite::memory:")`；`cwd=/tmp`
  只作为 wave 字段落库，路径未被打开。sqlx 每次解析内存 URL 使用进程内新 seqno；nextest 又是
  test-per-process，因此没有同名磁盘文件或跨测试 cache。现有 anchor 注释/门也明确每 repo 隔离。
- **进程全局/env**：测试及 `wave_create_tx`、`wave_tree_term`、`evaluate_schedulability` 路径检索
  `static/OnceLock/LazyLock/once_cell/set_var/remove_var/std::env` 无命中；repo 的两个 cache 是实例字段。
  nextest 的测试进程也不共享 Rust 进程全局。
- **端口/套接字**：上述闭包路径检索 listener/socket/localhost/固定端口无命中；测试只做 SQLite。
- **时间/排序**：命中。`wave_create_tx` 每次调用 `now_ms()`（Unix 毫秒）并生成 UUIDv4；root、child
  可同毫秒。成员 SQL 明确 `ORDER BY w.created_at,w.id`，余数给该全序前缀。现有排序契约三门
  （created_at 主序、同毫秒 id 决胜、SQL 总序）实跑 3/3 通过，排除漏 ORDER BY。
- **1008 格穷举并发**：用 `-j 2 -E` 只选它和目标测试，前 3 轮双绿，第 4 轮目标 0.111s
  minimum=10 失败；网格自身通过且每轮约 5.96s。更关键的是隔离也失败，故它不共享/污染数据，
  不是必要条件；系统负载至多改变两个 writer 调用是否落在同一毫秒的概率。
- **高并发/单包差异**：不是并发专属。一次隔离或 401-test 包通过只是未抽中条件；完整门首次红、
  紧接重跑绿与 UUID/timestamp 概率分支完全一致。

## 根因

测试先造 root(固定占用 5) 与 child(固定占用 3)，但未像同文件其它余数测试那样固定时间。
在 B=4 时两者 share 都为 2，整树冻结。恢复时：

- root 排前：B=9 分成 root=5/child=4，能容纳 5/3 并给 child 一格，minimum=9；
- child 排前：B=9 分成 child=5/root=4，root 仍过额；到 B=10 的 5/5 才恢复，minimum=10。

生产计算返回 10 是正确的；错的是测试无条件写死 9。可复现，非共享状态泄漏。

## 引入时间与修法

- `(created_at,id)` 配额生产路径由 `7eb2a3f9`（2026-08-10）引入；legacy 固定占用/整树冻结及
  本测试函数由 `7ebbfd48` 引入。`minimum==9` 断言由 `d2adea3d` 加入，HEAD `7d8bba23`
  又加了更早触发的文案 `at least 9` 断言。故不是既有旧路径首次暴露：生产策略和 flaky oracle
  都属于 #985 本片；本提交加速暴露了父提交已存在的同一错误假设。
- **最小测试修法**：在 link 后调用已有 helper，固定 root `created_at=1`、child `=2`，再保留
  minimum=9 的精确断言；另可增加固定相同时间且 child id 在前、明确期待 10 的覆盖。不要放宽成
  `{9,10}`，否则精确恢复算法回归会漏掉。
- **生产语义**：安全不变量没有坏：`(created_at,id)` 是持久化全序，单棵树重建稳定、份额和仍为 B，
  返回的 10 也是可执行动作。但同毫秒很常见且 id 是 UUIDv4，所以“谁拿余数”、可用容量和恢复
  minimum 对等价创建序列呈生产可见的随机抽签；按本排查口径，这是容量公平/可预测性的真实生产
  语义问题，不只是测试问题。若该抽签不是明确产品策略，最小完整修法是新增持久化单调
  `quota_order`（事务内分配）并以其排序；只提高时间精度仍可 tie，改用随机 id 作兜底也未解决。
  若产品明确接受 UUID 抽签，则应把它写入设计裁决，生产无需改，只按上法固定测试夹具。

## 交付状态

`git status --short`：`?? docs/_985-s6b-flake-probe.md`
