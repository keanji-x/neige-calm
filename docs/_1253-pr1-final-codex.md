## BLOCKER

- **[读] 机制断言消除了空绿，但仍是概率测试，并把纯测试状态带进了生产路径。** [today.rs:165](/tmp/wt1253f/crates/calm-server/src/routes/today.rs:165)、[today_launchpad.rs:1279](/tmp/wt1253f/crates/calm-server/tests/cases/today_launchpad.rs:1279)

  `Relaxed` 对“两个 future 完成后读取纯计数”本身足够，不会丢增量；问题是作用域和调度。`tokio::join!` 没有保证两个请求都在写入前读到 `None`：若 A 在 B 首次读取前完成 mint，`attempts == 1`，正确实现也会 RED。20/20 只能说明本机时序稳定，不能建立保证。

  同时，delta load 不是测试隔离。普通多线程 `cargo test --test domain_api_suite` 会让本文件其他大量 `ensure()` 用例在两次 load 之间增加同一全局原子，产生 `attempts > 2` 或 `retries > 1`。nextest 的每测试一进程只保护 CI 这一种 runner；代码内没有锁、reset 或实例级作用域。

  两个 `pub static` 没有生产消费者，却让生产首次 mint 执行测试观测指令；“测试必须运行生产指令”的论证也不完整，因为该观测指令本身已经改变被观测的调度。更便宜且确定的 carrier 是沿用 [waves.rs:101](/tmp/wt1253f/crates/calm-server/src/routes/waves.rs:101) 的 fixtures-only、按 DB/App 标识作用域的 barrier：在读到 `None` 后、mint 前 rendezvous。届时“两请求成功 + 唯一行”即可真实绑定 retry call site，生产构建为零指令、测试也不靠时运。

## MAJOR

无

## MINOR

- **[跑+读] 历史语义仍在两个契约位置残留。** [today.rs:217](/tmp/wt1253f/crates/calm-server/src/routes/today.rs:217)、[openapi.json:2826](/tmp/wt1253f/fe/core/api/generated/openapi.json:2826)、[_design-1253.md:249](/tmp/wt1253f/_design-1253.md:249)

  200 响应仍描述为 “whether its report has been written”，冻结设计仍说“汇总跑过一次之后空态永不回来”。具体反例：先修改、再把 `summary/body` 逐字恢复 canonical，实际返回 `false`，空态会回来。属性说明、README、`today.ts` 及生成 OpenAPI 的属性描述已正确；遗漏的是端点响应描述和 r11 设计正文。

## 结论

1 BLOCKER、0 MAJOR、1 MINOR；当前不建议合入，需先把竞态测试改成确定、作用域隔离的 carrier。

读码确认：

- 双向 mutation claim 与已提交断言结构一致：顺序 await 时 `attempts == 1` 使新机制断言 RED；删除新断言后，原状态/单例断言 GREEN。
- `LAUNCHPAD_UNIQUE` 的 clippy `dead_code` carrier 注释准确；`SYSTEM_COVE_UNIQUE` 的行为 carrier 在竞态实际发生时真实，但目前没有确定性地制造竞态。
- 两个并发 `POST .../ensure`、收窄后的 null 注释和 `aria-controls` 均准确。
- `wave_report.rs`、`routes/today.rs` 字段正文、`today.ts`、README 与 OpenAPI 属性说明一致。

实际运行了只读机械检查：`git diff --check`、全仓语义 `rg`、两份 OpenAPI SHA/逐字比较、merge 函数名集合 union、父文件差分及 combined diff。merge spot-check 未见丢失或复活：测试文件与 feature parent 逐字相同，main parent 到该文件仅为纯新增合并；两份 OpenAPI 完全相同并同时含两侧变更。未运行 Rust/FE 编译或测试。
