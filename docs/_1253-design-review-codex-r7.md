- Stuck + re-point 的 409 窗口还需限定“派生卡不存在”。若 Stuck 已留下 `deletable:false` 卡，`card_get` 会绕过 create，转入 dormant 恢复，不会再比较旧 payload hash。
- hard-fire 绕过去抖、bootstrap-only turn 都正确；但合并条件是“同一次可发起的 drain 中仍共同排队”，不只限于同一 50ms tick。状态阻塞时跨多个 tick 也会合并。

## 总判

**r8：不 ship。** 当前 r9 已修复上述 BLOCKER；裸 key 方案本身在代码上成立。
tokens used
183,864
评审对象锁定为 r8（`8630d7c9`）；评审期间工作区已被外部推进至 r9，我未修改文件。

## BLOCKER

- r8 §6 的 PR2 切片仍要求“掺 workspace digest”，并写“三次触发 → 三行”。这与 D5 的裸 key、INV-011 及 INV-010 的四行断言直接冲突，按切片实施会重新派生第二张卡。r8 因此未真正收敛。

## MAJOR

无。

MAJOR-2、3、4 均已关闭：纯计数 DTO 有静态长度界；`report_has_noninitial_content` 语义及 canonical 正例正确；dormant 的 start 重提交能以 `reset_harness_items:false` 保留 transcript，INV-002 也已覆盖。

## MINOR

- `SpecHarnessStartOperationPayload` 实际有 12 个字段，不是六个；还包括 `goal`、两个控制位、`profile`、`create_card`、`first_message_sha256`。其中最后一个是第三个潜在变量，但已可由逐字节静态 bootstrap 管住，因此裸 key 结论仍成立。[payload 定义](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/operation/spec_harness_start_adapter.rs:216)
- 固定 kernel actor 可行，但必须直接构造 `ActorId::Kernel`；`Actor("kernel").to_actor_id()` 会降为 `User`。[转换实现](/mnt/data2/kenji/neige-calm/.claude/worktrees/1253-design/crates/calm-server/src/actor.rs:94)
- Stuck + re-point 的 409 窗口还需限定“派生卡不存在”。若 Stuck 已留下 `deletable:false` 卡，`card_get` 会绕过 create，转入 dormant 恢复，不会再比较旧 payload hash。
- hard-fire 绕过去抖、bootstrap-only turn 都正确；但合并条件是“同一次可发起的 drain 中仍共同排队”，不只限于同一 50ms tick。状态阻塞时跨多个 tick 也会合并。

## 总判

**r8：不 ship。** 当前 r9 已修复上述 BLOCKER；裸 key 方案本身在代码上成立。
