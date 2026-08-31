# Wave 工作区

状态：r3.5，实现中。S0 已合入 #1158；S1 已合入 #1163；下一片是 S2。

## 前提

新 FE 尚未上生产。S2 起不再为旧客户端新增字段别名、缺键默认或其他过渡层；破坏性的 API/DTO 变更可以一步到位。S1 已存在的 wire `cwd` 别名保留。

服务端仍必须迁移和回放已经持久化的数据。FE 是否上线不改变这项责任。

## 产品契约

- Neige 在 `$HOME/neige-workspaces` 下管理默认工作区。
- Cove 只是命名空间；每个 wave 拥有独立仓库。
- 创建 wave 时默认分配工作区，不要求用户先选目录。
- 工作区在产生工作前可更换；开始工作后永久冻结。
- 用户仓库可以附加，但 Neige 永不初始化、移动或删除它。
- 子 wave 必须拥有独立工作区，不能继承父 wave 的路径。

## 数据模型

```rust
enum WaveWorkspaceKind {
    Managed,
    Attached,
}

struct WaveWorkspace {
    kind: WaveWorkspaceKind,
    path: String,
    frozen_at: Option<i64>,
}
```

`waves.workspace_path` 是路径的唯一存储。wire 上的 `cwd` 从它派生，不是第二真源。

`Managed` 表示服务端创建、独占且可回收的目录；`Attached` 表示用户已有仓库。这个类型决定删除权限，不能由路径猜测。

除 system cove 的 Today/launchpad wave 外，`frozen_at` 单调：一旦有值，`kind` 和 `path` 永不再变。用户 cove 中的 attached wave 创建时即冻结；launchpad 路径由内核维护，保持未冻结且不接受用户 PATCH。

## 托管工作区

配置：

```text
--workspace-root / CALM_WORKSPACE_ROOT
默认：$HOME/neige-workspaces
```

布局：

```text
<root>/<cove_id>/<wave_id>/
<root>/<cove_id>/<wave_id>/.claude/worktrees/<wave>/<card>
```

目录使用稳定 ID，不使用标题 slug。托管路径不写入 `cove_folders`；该表只维护 attached 路径的 cove 归属。

### 物化

创建 managed wave 时：

1. 创建一个不存在或为空的目标目录；非空目录直接失败。
2. 使用隔离的 Git 配置初始化仓库，避免全局 template、hook 和签名设置影响结果。
3. 创建一个固定身份的空初始提交，使首次 `git worktree add` 可用。
4. 在 `.git/info/exclude` 中排除 `.claude/worktrees/`。
5. 任一步失败都让 wave 创建返回错误，不能留下一个稍后才以 `spawn-failed` 暴露的坏 wave。

所有 wave 创建入口必须走同一物化契约，包括普通 REST、workflow/template、cove chat、Today/launchpad 和 child wave。这个集合不应长期靠调用点清单维持，应收敛到统一创建边界。

Attached 创建只做校验：绝对路径、目录存在、是 Git 仓库，并完成 `cove_folders` 的唯一归属检查。

## 更换与冻结

允许：

- managed → managed
- managed → attached

拒绝：

- attached → 任意目标
- 已冻结 wave 的任何变更
- system cove wave 的用户 PATCH

不可重锚的持久 cwd 消费者出现前必须冻结工作区，包括首次 workspace lease、terminal 持久化、wave 离开 Draft、child wave 创建。冻结应位于真正的底层写入口，不能依赖上层调用点枚举。

更换未冻结的 managed 工作区时，旧仓库必须同时满足：

```text
git status --porcelain --ignored 为空
git rev-list --count --all == 1
git worktree list 只有主工作区
```

变更流程：

1. 在数据库事务内 park 或 supersede harness runtime，阻止新 turn 获得旧路径。
2. 在移动前重新检查仓库仍为空。
3. 断言旧路径属于 workspace root。
4. 将旧目录 rename 到 `<root>/.trash/`；跨文件系统移动失败时中止，不做 copy + delete。
5. 物化或校验新目录，更新唯一存储字段。
6. 用新路径启动新的 harness thread。

SQLite 事务不能隔离文件系统写入，因此“事务内检查一次”不足以关闭检查与 rename 之间的竞态。

## 生命周期

- 删除 wave：只回收 managed 目录，并先验证路径位于 workspace root。
- 删除 cove：回收其所有 managed wave 目录和空的 cove 目录。
- attached 路径永不移动、初始化或删除。
- 归档不回收；长期磁盘回收需要独立策略。
- 子 wave 创建时分配独立 managed 仓库并立即冻结。
- fork/template 复制报告，不继承源 wave 的工作区。

回收必须通过单一受控入口；任何直接递归删除都不得接受未经类型和根目录校验的路径。

## Cove 对话与执行器

没有 attached folder 的 cove 仍可创建对话；它使用默认 managed 工作区。已有 folder claim 的 cove 可以继续采用 attached 语义。

Managed 仓库保证 Codex 能获得 Git 工作区。Claude 当前不读取 wave workspace，迁移到同等 worktree 租约是独立工作，不属于本设计。

Managed 仓库默认没有 remote；需要操作真实代码仓库时，用户应在创建时 attached，或在冻结前从 managed 切换到 attached。

## 交付顺序

1. S0：失败原因进入 task 状态与前端。
2. S1：typed workspace、迁移、唯一存储、wire 类型。
3. S2：托管根、物化、所有创建入口默认 managed。
4. S4：子 wave 独立分配并冻结。
5. S5：安全回收和根目录断言。
6. S3：工作区更换、冻结、harness 重锚定、FE attached 入口。
7. S6：terminal 默认落在 wave 工作区。

S4 必须不晚于 S5；S3 必须复用 S5 的路径安全边界。

## 验收重点

- 新 managed wave 能直接创建第一个 worktree。
- 物化失败时 create 返回非 2xx，且不留下可见 wave。
- 删除 attached wave 不触碰用户仓库。
- 子 wave 路径与父 wave 不同；删除子 wave 后父仓库仍可用。
- 全局 Git 签名、模板和 hook 配置不会影响物化。
- 工作区有普通文件、ignored worker 产出、其他分支提交、stash、活 worktree 或 terminal 时，更换均被拒绝。
- 活跃 turn 与更换并发时不能在 trash 中继续产生产出。
- 任意回收或 rename 传入 workspace root 外路径时失败。
- GET wave 与 Today/launchpad 路径均只从 `workspace_path` 读取并派生 wire `cwd`。
