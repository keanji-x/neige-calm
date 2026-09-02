## BLOCKER

无。

## MAJOR

- **并发测试仍未证明实际进入 system-cove 冲突分支。** [`today_launchpad.rs:1273`](/tmp/wt1253rev2/crates/calm-server/tests/cases/today_launchpad.rs:1273) 仅用 `tokio::join!` 启动两个请求，随后只断言两者成功及数据库单例结果；没有 barrier、mint-attempt 计数或故障注入证明两个 `cove_get_system()` 都在任一写入前返回 `None`。具体假绿场景：第一个请求复用已有连接并完成整个 ensure，第二个连接建立较慢、随后读到 `Some`；即使唯一约束 retry arm 已损坏，两次请求仍成功且所有现有断言成立。磁盘 WAL 消除了读者必然阻塞，但没有建立调度顺序。当前版本该用例实跑 10/10 通过，说明未见红向 flaky；这不能证明回归时会红。对应的“both read None”注释 [`today_launchpad.rs:1239`](/tmp/wt1253rev2/crates/calm-server/tests/cases/today_launchpad.rs:1239) 仍宽于测试载体。

## MINOR

- **Round 1 的字段语义文档修复仍有残留矛盾。** [`today.rs:64`](/tmp/wt1253rev2/crates/calm-server/src/routes/today.rs:64) 及其生成的 OpenAPI 仍把字段概括成“has anyone ever written it”，而同一注释后文和实现说明它只描述当前内容；[`README.md:60`](/tmp/wt1253rev2/fe/web/src/features/today/README.md:60) 更明确声称它“never flips back”。具体场景：报告曾被编辑，随后 `summary` 与 `body` 恢复成 canonical 初始值；实现返回 `false`，但这些文档承诺历史性的 `true`，可能误导 PR2 将其当成“汇总曾运行”的持久标记。

- **system-cove arm 的新注释用了错误的调用载体。** [`today.rs:482`](/tmp/wt1253rev2/crates/calm-server/src/routes/today.rs:482) 写成“两次冷 page load”会同时进入 mint；但页面加载按 INV-TODAYDOC-001 只调用只读 resolve，真正能触发该 race 的是两个并发 `POST .../ensure`。具体场景：同时打开两个全新 Today 页面不会执行该 arm，与注释所述相反。arm 本身的可达性判断是正确的，只是调用场景表述错误。

## 结论

暂不建议确认通过：生产修复基本成立，但并发回归门禁仍是概率性的，留有明确假绿路径。

其余指定项确认如下：

- 三态分支有效：`isError` 优先，`data === undefined` 才为空白帧，只有已有 detail 数据且无 query error 才能进入解码文案；服务端错误原文和 Retry 均存在。`report_has_noninitial_content=false` 时 detail 不发请求，对其他 query consumer 没有行为影响。
- per-case seeding 有效：INV-003 使用 `seeded`，`hung`/500/不可解码用例分别观察真实状态。删除 [`public.tsx:395`](/tmp/wt1253rev2/fe/web/src/features/today/public.tsx:395) 的服务端字段判断，会让 seeded canonical payload 渲染四个标题，现有断言会红；因只读要求，未实际应用 mutation。
- SQLite 字符串测试通过真实 `SqlxRepo::open` migrations 和真实 sqlx constraint errors；生产两点分别使用 `SYSTEM_COVE_UNIQUE`、`LAUNCHPAD_UNIQUE`，测试也读取同一常量。生产 `today.rs` 中没有残留字面量调用。改回字面量会让常量在非测试构建中 unused，并被 CI 的 `-D warnings` 拒绝。`waves.purpose` 注释也明确承认只覆盖字符串、不覆盖 arm 可达性。
- 状态条折叠态有固定高度：5 条时显示 5 行且无按钮；6 条时显示 5 行和 `+1 more waiting`；0 条时整个 section 为 `null`。溢出只有一个 disclosure，展开后所有 waiting waves 可达。
- merge import 精确等于 main 的列表加 `todayLaunchpadQueryOptions`；#1276 删除的四个 settings/template consumers 均未复活，本分支新增查询也未丢失。

运行验证：同一 HEAD `5a09bc76` 的已安装依赖工作树上，前端聚焦测试 3 文件、36 tests 全绿；真实 SQLite 常量测试通过；并发用例连续 10 次通过。静态符号/调用点核查按 `navi` 导航流程完成。
