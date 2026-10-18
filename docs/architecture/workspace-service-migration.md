# workspace_service 迁入 workspace-runtime（#2666 M1 实施合同）

状态：[track-workspace.md](track-workspace.md) §6 S2a 行中“把 workspace_service 政策经 port 迁入 `workspace-runtime`
（不改行为）”的实施合同。只是设计，不改代码，不声称已实现；实现等级 L2。核实基线为 main `2998dd4ff`。
`server/` 简写 `crates/calm-server/src/`。crate 边界沿用 [workspace-execution.md](workspace-execution.md) §3：`workspace`
是叶子机制库，`workspace-runtime` 是 kernel 侧框架，calm-server 只做路由/存储适配。

## 1. Outcome

1. `workspace-runtime` 拥有 track 工作区的**选择准入、创建/删除顺序、回收与关闭后清理政策**；它只依赖 `workspace`、
   `calm-session`、`calm-types`（IO-free 词汇）与 async 原语，不依赖 calm-server 或 calm-truth。
2. 行为逐字节不变：HTTP 状态与错误文本、Git/FS 副作用及其顺序、选择/冻结时点、7 天 trash、10 分钟 clean、日志级别与
   字段都不变。唯一可观察差异是迁移后日志事件的 tracing target（§4.4）。
3. SQL 不进入 `workspace-runtime`；calm-truth 的查询原样复用，calm-server 现有两处内联查询原样留在其适配器中。
4. 无法以小 port 迁移的模块留在 calm-server，并写明原因（§2.2）。

## 2. 盘点（共 2405 行，其中测试 1631 行）

### 2.1 逐文件：政策与 calm-server 依赖

| 文件（行数） | 政策 | calm-server 依赖 | 调用者 | 处置 |
|---|---|---|---|---|
| `mod.rs`（53）`git_repo_root_for_track_cwd` | cwd 须为绝对路径的 Git 仓库；`rev-parse --show-toplevel` 失败为 BadRequest、spawn 失败为 Internal；根须绝对且 UTF-8 | `error` | `routes/tracks/preparation.rs:209`、`git_candidate/staleness.rs:59`、`operation/workspace_lease/tests.rs:517` | W2 迁入 `selection` |
| `workspace_materialize.rs`（144，测试 178） | 只物化 Managed；attached 准入（绝对/存在/目录/在 Git 工作树内）；`refs/heads/neige` 命名空间冲突与 show-ref 非 0/1 退出码一律拒绝 | `error`、`model::{TrackWorkspace, TrackWorkspaceKind}` | `routes/tracks.rs:1502,2002`、`tracks/create.rs:635`、`tracks/preparation.rs:159`、`routes/areas.rs:26`、`today.rs:500`、`child_track_adapter.rs:236,722,839`、`workspace_lease/worker.rs:253`、3 个集成测试夹具 | W2 迁入 `selection` |
| 同文件 `workspace_key_digest` | 工作区路径的 16 位 sha256 摘要，写入 operation 幂等键 | 无（sha2/hex） | `routes/today.rs:26`、`scheduler/mod.rs:1258` | **留下**（§2.2） |
| `workspace_recycle.rs`（259，测试 197） | kind/system-area 两道守卫；机制拒绝映射为 8 种 `RecycleRefusal`；日志分级；area 批量失败时逆序补偿；成功后去空 area 目录；7 天 trash GC 只记日志；Managed 标记作恢复闸门 | `error`、`model::{AreaKind, Track, TrackWorkspace, TrackWorkspaceKind}` | `routes/tracks.rs:2240–3019`、`routes/areas.rs:514–741`、`harness/mod.rs:194`、`planner_hibernation.rs:238`、`routes/planner_recovery.rs:53`、`tests/cases/track_workspace_recycle.rs:1601` | W1 迁入 `recycle` |
| `track_worktree.rs`（76） | 只有 `worktree` 为 Some 才 ensure；先有界刷新 upstream，再在阻塞线程执行 Git；新 branch 起点 behind→upstream、ahead→HEAD、diverged→拒绝，并记录 upstream；删除从不使调用者失败、只 warn | `error`、`model::Track`、`operation::workspace_lease::{upstream, upstream_fetch}`、`test_seams`（fixtures 暂停点） | `routes/tracks.rs:1517`、`tracks/create.rs:661`、`test_seams.rs:148`、identity 测试 | W3 迁入 `track_worktree`，经 `TrackBranch` |
| `sweep.rs`（67） | 事务内捕获 worktree 路径和 lease 记录的 common dir；提交后先删 worktree，再按 common dir 删 candidate refs；从不失败 | `operation::Tx`、内联 SQL、`git_candidate::refs`、`error` | `operation/workspace_lease/mod.rs:252`、`routes/tracks.rs:2`、`routes/areas.rs:2,582`、identity 测试 | W3：提交后半段迁入 `sweep`，经 `CandidateRefs`；捕获留下 |
| `worktree_clean.rs`（92） | 关闭且无活动 session 的 track（内联 SQL）→ checkout 占用为 Free → 执行全部结算 → `git clean -fdX`；每个 `(track, closed_at)` 每进程一次；10 分钟间隔并跳过启动 tick；失败 warn 并下次重试 | `db::sqlite::checkout_occupancy`、calm-truth `unsettled_executions`、`sqlx::SqlitePool`、`error` | `state.rs:1598`、`test_seams.rs:107` | W4 迁入 `worktree_clean`，经 `ClosedWorktreeFacts` |
| `identity.rs`（83，测试 1256） | 观察当前选择：未观察过的 Managed 未物化→无；按所有权核对 owner；已观察但目录缺失→`WorkspaceRecoveryRequired`；CAS 记录观察 | `db::sqlite::{workspace_observation_snapshot, workspace_observe_tx, begin_immediate_tx, DirectoryObservation, WorkspaceOwnership, SqliteDb}`、`operation::SpawnCtx`、`error` | `workspace_lease/worker.rs:157`、`terminal_adapter/target.rs:61`、`test_seams.rs:149` | **留下**（§2.2） |

### 2.2 留在 calm-server 的部分与原因

- **`identity`**：判定的输入/输出全是 calm-truth 持久化类型（观察快照、`WorkspaceOwnership`、版本 CAS）。若要迁移，
  port 须携带快照与 CAS 合同，或把这三种类型下沉到 `workspace`，二者都超出“最小 port”。它的 1256 行测试要经过
  `SqlxRepo`、`SpawnCtx`、`prepare_worker_lease_as_tx` 与路由幂等键，无法随代码迁移。resource cutover 会以 typed 资源账本
  取代这层观察，届时由该里程碑决定归属。
- **`workspace_key_digest`**：它是 operation 幂等键的格式，已持久化在 operation 行中，属于 operations 层。迁移只会给
  workspace-runtime 增加 sha2/hex 依赖，没有政策收益。W2 中它移到 `workspace_service/mod.rs`，字节不变。
- **sweep 捕获与 clean 候选查询**：两段内联 SQL 原样留在 calm-server 适配器中（calm-server 已有同类内联 SQL，如
  `workspace_lease/mod.rs:253`）。移入 calm-truth 属于独立重构（§6）。
- **upstream 起点政策**（`workspace_lease::upstream`，612 行）属于 lease 政策，被 lease、catch-up 和 publish 共用；本里程碑只经
  `TrackBranch` 调用，不迁移。
- **fixtures 暂停点** `observe`：由 calm-server 按 `feature = "fixtures"` 选择，以 fn 指针传入。

## 3. 目标布局与 port

### 3.1 布局

```
crates/workspace-runtime/src/
  lib.rs            pub mod execution; pub mod error; pub mod selection; pub mod recycle;
                    pub mod track_worktree; pub mod sweep; pub mod worktree_clean;
  error.rs          WorkspaceError
  selection.rs      ← mod.rs::git_repo_root_for_track_cwd + workspace_materialize.rs（不含 key digest）
  selection/tests.rs  ← workspace_materialize/tests.rs
  recycle.rs, recycle/tests.rs     ← workspace_recycle.rs 与其测试
  track_worktree.rs ← track_worktree.rs（TrackBranch、ObservePoint）
  sweep.rs          ← sweep.rs 提交后半段（WorkspaceTrackSweep、CandidateRefs）
  worktree_clean.rs ← worktree_clean.rs（ClosedWorktreeFacts、spawn 循环）
server/workspace_service/
  mod.rs            适配器入口 + workspace_key_digest
  ports.rs          ServerWorkspacePorts：TrackBranch + CandidateRefs + ClosedWorktreeFacts，observe
  sweep.rs          只保留 workspace_track_sweep_for_track_tx（事务内捕获）
  identity.rs(+tests)  不变
```

Cargo：workspace-runtime 新增 `calm-types`、`tracing`、`thiserror`（`workspace` 已用的版本）。不加 feature gate；
calm-server 已有的普通依赖与 dev-dependency 保持不变。调用者直接改用 `workspace_runtime::…` 路径，同片删除旧路径，
不留 re-export shim。

### 3.2 错误

```rust
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("bad request: {0}")] BadRequest(String),
    #[error("conflict: {0}")]    Conflict(String),
    #[error("internal: {0}")]    Internal(String),
}
impl From<workspace::error::Error> for WorkspaceError { /* Internal(m) => Internal(m) */ }
// calm-server/src/error.rs：impl From<WorkspaceError> for CalmError，三个变体 1:1，消息原样。
```

Display 与 `CalmError` 同名变体逐字相同。这是必要条件，因为 area 补偿会把错误 `to_string()` 拼进 500 正文
（`recycle_area_workspaces`、`restore_area_recycle_report`）。无 port 的函数返回 `WorkspaceError`。带 port 的函数对 port 的
错误类型 `E: From<WorkspaceError> + From<workspace::error::Error>` 泛型（与 `workspace::worktree::ensure_track_worktree<E>` 同一
惯用法），calm-server 的 `CalmError` 原样穿过，不做往返转换。

### 3.3 Port（最小面）

| Port（定义处） | 方法 | 实现方（calm-server） | 不变量 |
|---|---|---|---|
| `track_worktree::TrackBranch: Clone + Send + Sync + 'static` | `async fn refresh_upstream(&self, repo_root: &Path)`（结果今天即被丢弃）；`fn prepare_branch(&self, repo_root: &Path, branch: &str) -> Result<String, Self::Error>`（阻塞，在 metadata lock 内由叶子回调） | `upstream_fetch::refresh_upstream`；`choose_lease_start` + `diverged_refusal` + `head_upstream` + `set_branch_upstream`（原 `prepare_branch`/`track_worktree_base` 原样迁入适配器） | refresh 先于 `spawn_blocking`；仅新 branch 调 `prepare_branch`；`attached-repo-diverged` 的 409 原样穿过 |
| `sweep::CandidateRefs: Clone + Send + Sync + 'static` | `fn delete_for_track(&self, track_id: &str, git_common_dirs: &[PathBuf])`（在既有 `spawn_blocking` 内调用，不失败） | `git_candidate::refs::delete_candidate_refs_for_track` | 每个 sweep 先删 worktree 再删 refs；整体从不失败 |
| `worktree_clean::ClosedWorktreeFacts: Send + Sync + 'static` | `async fn closed_idle_worktrees(&self) -> Result<Vec<(String, String, i64)>, Self::Error>`；`async fn checkout_free(&self, track: &str) -> Result<bool, Self::Error>`；`async fn executions_settled(&self, track: &str) -> Result<bool, Self::Error>`；`Self::Error: Display` | `CLOSED_TRACK_WORKTREES_SQL` 原文；`checkout_occupancy(.., "") == Free`；`unsettled_executions(Scope::Track).is_empty()`，每次各自 acquire 一个连接 | 检查顺序为 memo → Free → settled，短路与 `?` 位置不变 |
| `ObservePoint = fn(&str, &Path)` | 测试暂停点 | `workspace_service::ports::observe`（fixtures 下调 `test_seams::blocking_pause_point`） | ensure/remove/clean 传同一函数 |

`ServerWorkspacePorts` 是无状态单元结构，`ClosedWorktreeFacts` 持有 `SqlitePool`（`state.rs` 仍传 `read_pool()`）。
`recycle` 与 `selection` 不需要 port：它们只调用 `workspace` 机制、Git 命令和 `calm-types` 词汇。

## 4. 行为等价的证明

### 4.1 迁移即移动

每片用 `git diff -M` 审查：迁移文件应被识别为 rename，差异只允许四类替换：模块/导入路径；
`CalmError`→`WorkspaceError` 或泛型 `E`；`crate::model::X`→`calm_types::model::X`；把 calm-server 内部调用换成 port 方法，
调用点和顺序不变。字符串、守卫顺序、`spawn_blocking` 边界、日志宏的级别/消息/字段都不改。超出这四类的改动即为阻断。

### 4.2 测试随代码迁移，断言不改

- `workspace_materialize/tests.rs`、`workspace_recycle/tests.rs` 原样移到 `selection/tests.rs`、`recycle/tests.rs`，只改
  导入；`matches!(err, CalmError::BadRequest(_))` 改为 `WorkspaceError::BadRequest(_)`，判定的变体不变。
- identity 测试、`test_seams`、集成测试夹具中调用被迁函数的地方只改路径。
- 不新增测试、golden、名单或扫描规则。等价证据是：迁移前后各跑一次同一组既有聚焦测试（§5），全部为绿，
  并在 PR 中记录。

### 4.3 新接缝的一次性证据（不永久化）

唯一新增的、承载行为的代码是 `From<WorkspaceError> for CalmError` 与 port 透传。W2 在独占 worktree 内按 AGENTS.md
做一次变异：把 `BadRequest` 映射为 `Internal`，先在 PR 写出完整预测红集（至少包含
`track_workspace_materialize::attaching_a_repo_that_already_has_a_neige_branch_is_refused`），再跑完整 `track_workspace_materialize`
与 `track_workspace_repoint` 过滤器比对，然后恢复并确认转绿。W3 对 `TrackBranch` 透传同样做一次：在 runtime 的 ensure 中
把交给叶子的回调换成绕过 port 的 `workspace::base::resolve_head_base`，同样先写完整预测红集（至少包含
`track_worktree::a_diverged_checkout_fails_the_create_and_makes_no_worktree`），再跑完整 `track_worktree` 过滤器。结果只写进 PR。

### 4.4 日志 target

迁移后事件的 target 从 `calm_server::workspace_service::*` 变为 `workspace_runtime::*`，默认过滤器会因此改变可见性
（`calm-server/src/main.rs:20` 为 `info,calm_server=debug`，`bin/replay.rs:67` 为 `warn,calm_server=info`）。W1 在这两处默认
指令中加入 `workspace_runtime=debug` / `workspace_runtime=info`，使默认可见性不变。用户自设 `RUST_LOG` 并按
calm_server 模块路径过滤时，结果会变化；这是仅影响诊断的差异，须在 PR 中写明。neige-app 的默认指令
（`info,neige_app=debug`）对 info 及以上不受影响，`recycle` 的 debug 事件在那里本来就不显示。

## 5. 有序切片

每片独立可合入，按 [track-workspace.md](track-workspace.md) §6 的公共 gate G 验证：text ratchet、contract、
`scripts/local-rust-gates.sh --quick --base origin/main`（clippy `--workspace --all-targets` 覆盖所有被改路径的测试夹具编译）
各跑一次，再做一次聚焦 replay：

```
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 cargo nextest run --locked --no-tests=fail \
  -p workspace-runtime -p calm-server <下表过滤器> --test-threads 8
```

规模按 `git diff -M --numstat` 计（纯 rename 只计改动行）；括号内是迁移的源行数。全部为 **L2**：涉及回收所有权、
破坏性删除/清理路径（父合同 §6 S1/S2a）。若修复属于机械类，两路评审只看 delta。

| # | 结果 | 改动与规模 | 聚焦过滤器 |
|---|---|---|---|
| W1 | `error` + `recycle` 迁入；calm-server `From<WorkspaceError>`；两处默认 EnvFilter | Cargo/lib/error 约 50；回收调用者 tracks/areas/harness/planner_hibernation/planner_recovery 与一个集成测试常量约 40；（迁移 456） | `recycle::tests`、`track_workspace_recycle` |
| W2 | `selection` 迁入（attached 准入、物化分派、repo 根解析）；key digest 移到 `workspace_service/mod.rs` | 调用者 preparation/tracks/create/areas/today/scheduler/child_track_adapter/worker/staleness，lease 测试，identity 测试，3 个集成测试夹具，约 60；（迁移约 360） | `selection::tests`、`track_workspace_materialize`、`track_workspace_repoint`、`today_launchpad`、`workspace_identity`、`workspace_lease::tests`、`child_track_adapter`、`candidate_upstream`、`planner_attachments_rest` |
| W3 | `track_worktree` + `sweep` 提交后半段迁入；`TrackBranch`、`CandidateRefs`、`ObservePoint`；calm-server `ports.rs` | port 约 40，适配器约 60，调用者 tracks/create/areas/workspace_lease/test_seams 与 identity 测试约 30，`upstream_fetch.rs:6` 文档指针；（迁移约 80，`prepare_branch` 留在适配器） | `track_worktree`（含 `track_metadata_concurrency` 锁序、diverged、删除）、`track_workspace_recycle`、`workspace_identity` |
| W4 | `worktree_clean` 迁入；`ClosedWorktreeFacts` 与 spawn 循环 | port 约 20，适配器约 50，`state.rs`/`test_seams` 约 6；（迁移 92） | `track_worktree::a_closed`、`worktree_clean_skips_until_settled` |

依赖：W1 引入 `error` 与依赖，最先合入；W2 只依赖 W1；W3 依赖 W1 并新建 `ports.rs`；W4 依赖 W3（复用 `ports.rs` 与 `observe`）。
每片的 PR 记录迁移前后的同组测试结果、`git diff -M` 摘要，以及 W2/W3 的变异证据。

## 6. 非目标

- resource cutover（typed 资源、registration 生命周期、RuntimeSafety、Legacy 拒绝）、borrowed/Legacy adoption、resource
  task-admission cutover；`WorkspaceRef`/`LaunchHandle`/`resolve`。
- 隔离等级与 backend 选择（worktree / Linux COW view、namespace、sandbox）、COW mediator、S3 交付证据与 7/90 typed 保留期。
- 迁移 `identity`、`workspace_lease`（含 upstream 政策）、`git_candidate`；把内联 SQL 移入 calm-truth；合并 calm-server 与
  `workspace` 两份 `RecycleRefusal`（现存重复，作为 follow-up issue 记录）。
- 任何重命名之外的 API 改动、错误文本/日志文本改动、schema/migration/OpenAPI/wire 改动。

## 7. AGENTS.md 架构五点与维护成本

- **抽象边界**：选择、生命周期顺序与回收政策归 `workspace-runtime`；FS/Git 机制仍在 `workspace`；SQL 与事务在 calm-truth
  或 calm-server 适配器；lease 的 upstream 政策与 candidate refs 仍由各自 owner 拥有，只经 port 调用。workspace-runtime 不 import
  calm-server/calm-truth，由 Cargo 依赖表保证（不另建扫描）。
- **重复逻辑**：移动而非复制，同片删除旧实现，不留 shim；port 实现只是对原函数的一次调用。既有的 `RecycleRefusal`
  双份不在本次扩大，记为 follow-up。
- **硬编码假设**：`refs/heads/neige` 是 Neige 自有的分支命名空间协议，随 owner 迁移；`TRASH_RETENTION_MS` 与 10 分钟间隔
  原样保留（typed 设置属 S3）。不按模板、插件或应用身份分支。
- **外部合同事实**：Git 仓库根只取 `rev-parse --show-toplevel` 的输出，show-ref 只按退出码判断；`git` 的本地化文本不参与
  判定，这些都不变；不新增路径推导。
- **永久成本**：没有新测试、golden、名单或阈值。新增 3 个 port trait（共 6 个方法）、1 个 fn 类型、1 个错误枚举，以及
  workspace-runtime 的 3 个依赖；calm-types 会进入 calm-proc-supervisor 的 dev 构建。以后在被迁模块中新增或重命名测试，
  只改 `workspace-runtime` 内的测试文件；新增调用者不改任何列表；以后 lease/candidate 政策迁移时只换 §3.3 的实现方，不碰调用者。
  CI 中每个迁移测试仍只跑一次（从 calm-server lib 移到 workspace-runtime），总数不变。
