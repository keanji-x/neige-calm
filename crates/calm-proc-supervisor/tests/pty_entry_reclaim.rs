//! #996: 已退出的 pty `ProcEntry` 必须被回收 —— `ByteRing`（默认 1 MiB/终端）、
//! broadcast 通道与 pty master/writer fd 不能随终端创建次数线性泄漏。
//!
//! 方案是"到期整条移除"，不是墓碑：
//!
//! * **宽限期内**，entry 完整保留 —— 刚断开的客户端重连仍拿得到最后一屏 replay
//!   与 sticky exit（`replay_and_sticky_exit_survive_within_grace`）。
//! * **宽限期后**，整条 entry 从 registry 移除，资源由 Rust 所有权一次性释放；
//!   证据是 `/proc/self/fd` 的真实计数，不是查某个 `Option`
//!   （`expired_pty_entries_are_removed_and_release_ring_and_fds`）。
//!   此后 attach 得到 `UnknownProc` —— 不丢信息：终端退出状态由 `calm-server`
//!   在退出当时落库（`terminal_set_exit`），数据库才是权威记录。
//! * **slave 仍被持有时不移除**：移除谓词看的是"master 读到过 EOF"
//!   （`eof_reached`），不是"reader 线程结束了"（#993 的 `PtyDrainGate` —— 它
//!   在 read 错误 / panic 上也会落闸）。于是 `portable-pty`
//!   `UnixMasterWriter::drop` 注入的 `\n`+VEOF 绝不会打到活着的孙子进程 stdin
//!   上。两条测试分工：
//!   `entry_survives_while_a_grandchild_holds_the_pty` 锁资源追踪，
//!   `no_eof_is_injected_when_the_pty_reader_dies_without_eof` 锁注入本身
//!   （reader 已死 ⇒ 所有权层不再兜底，断言真能证伪）。

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    AttachRequest, CleanupRequest, ControlErrorKind, ControlMsg, ControlReply, EnsureProcRequest,
    IoMode,
};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

/// 每个终端产生的输出量，足够让"ring 是否被释放"成为可观测事实。
const NOISY_BYTES: usize = 200_000;
const REPLAY_BYTES: usize = 1024 * 1024;

/// 宽限期内 entry **完整**保留：新 attach 既拿得到最后一屏 replay，也拿得到
/// sticky exit。这条锁住"不降级、不半死"——回收只有"还在"和"整条没了"两态。
#[tokio::test]
async fn replay_and_sticky_exit_survive_within_grace() {
    let supervisor = InProcessProcSupervisor::start_with_grace(Duration::from_secs(30))
        .await
        .expect("start supervisor");
    let proc_id = "pty-replay-in-grace";
    ensure_pty(
        supervisor.sock(),
        proc_id,
        &["-c", "printf abc; exit 7"],
        REPLAY_BYTES,
    )
    .await;
    assert_eq!(
        await_exit(supervisor.sock(), proc_id).await,
        (Some(7), false)
    );

    // 进程早就没了，但 entry 在宽限期内必须还在，且 ring 未被动过。
    let stats = supervisor
        .registry()
        .debug_entry_stats(proc_id)
        .expect("宽限期内 entry 必须仍在 registry");
    assert!(stats.exited, "退出状态应已落定");
    assert!(stats.buffered_bytes > 0, "宽限期内 replay 字节不得被释放");

    let (attached, frames) = attach_and_drain(supervisor.sock(), proc_id).await;
    assert!(!attached.running);
    assert!(
        contains(&attached.replay, b"abc"),
        "宽限期内 replay 应完好，实际 {:?}",
        attached.replay
    );
    assert!(
        !frames
            .iter()
            .any(|frame| matches!(frame, ControlReply::Gap { .. })),
        "宽限期内不应有 Gap（replay 完整），实际 {frames:?}"
    );
    let exited = frames
        .iter()
        .find_map(|frame| match frame {
            ControlReply::Exited {
                status, signalled, ..
            } => Some((*status, *signalled)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("宽限期内 attach 必须拿到 sticky exit，实际 {frames:?}"));
    assert_eq!(exited, (Some(7), false));
}

/// 宽限期后整条 entry 被移除，ring 与 fd **真的**回到基线。
///
/// fd 的证据是 `/proc/self/fd` 的计数：supervisor 与测试同进程，8 个 pty 各自
/// 持有 master + reader dup + writer dup，泄漏会直接体现在这个数字上。墓碑方案
/// 下"查 `Option` 是否为 None"是假绿 —— entry 还在，broadcast 通道也还在。
#[tokio::test]
async fn expired_pty_entries_are_removed_and_release_ring_and_fds() {
    let grace = Duration::from_millis(300);
    let supervisor = InProcessProcSupervisor::start_with_grace(grace)
        .await
        .expect("start supervisor");
    let registry = supervisor.registry();

    // 预热一轮，让 tokio / 临时目录相关的 fd 先稳定下来。
    let warm = "pty-reclaim-warmup";
    ensure_noisy_pty(supervisor.sock(), warm).await;
    await_exit(supervisor.sock(), warm).await;
    await_entry_gone(registry, warm).await;

    let fds_before = open_fd_count();
    let entries_before = registry.debug_entry_count();

    let procs: Vec<String> = (0..8).map(|i| format!("pty-reclaim-{i}")).collect();
    for proc_id in &procs {
        ensure_noisy_pty(supervisor.sock(), proc_id).await;
    }
    for proc_id in &procs {
        await_exit(supervisor.sock(), proc_id).await;
    }

    // 宽限期内 ring 仍完好 —— 移除不能早于宽限期。
    let buffered_in_grace: usize = procs
        .iter()
        .map(|proc_id| {
            registry
                .debug_entry_stats(proc_id)
                .expect("宽限期内 entry 必须仍在")
                .buffered_bytes
        })
        .sum();
    assert!(
        buffered_in_grace > NOISY_BYTES,
        "宽限期内应仍持有 replay 字节，实际 {buffered_in_grace}"
    );

    for proc_id in &procs {
        await_entry_gone(registry, proc_id).await;
    }
    assert_eq!(
        registry.debug_entry_count(),
        entries_before,
        "8 条已退出 entry 应全部离开 registry"
    );

    let fds_after = open_fd_count();
    assert!(
        fds_after <= fds_before + 2,
        "pty fd 随终端数线性泄漏：before={fds_before} after={fds_after}（8 个已退出终端）"
    );

    // 移除之后 attach 得到 UnknownProc —— 新契约，退出状态改由 calm-server 侧
    // 的终端行（`terminal_set_exit`）负责，registry 只缓存活着的进程。
    let mut stream = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect attach");
    write_frame(
        &mut stream,
        &ControlMsg::Attach(AttachRequest {
            proc_id: procs[0].clone(),
            from_cursor: Some(0),
            reader_id: "reclaim-test".into(),
        }),
    )
    .await
    .expect("write attach");
    match timeout_read(&mut stream).await {
        ControlReply::Error { kind, .. } => assert_eq!(kind, ControlErrorKind::UnknownProc),
        other => panic!("移除后 attach 应得到 UnknownProc，实际 {other:?}"),
    }
}

/// 孙子进程攥着 slave 时，entry 必须留在 registry 里（`eof_reached` 尚未置位），
/// 而且到期后能被正常收掉 —— 不是永久泄漏。
///
/// **这条测试覆盖的是"资源追踪"，不是"EOF 注入"。** 说清楚为什么：这里 reader
/// 线程还活着（阻塞在 `read()` 上），它自己持有一份 `Arc<ProcEntry>`，所以即便
/// 移除谓词判错、registry 提前撒手，writer 也不会归零，`UnixMasterWriter::drop`
/// 的 `\n`+VEOF 根本不会发生 —— 在这条路径上断言"孙子进程还活着"是空转的。
/// 真正能证伪注入的场景是"reader 已死 + slave 仍被持有"，见下面那条
/// `no_eof_is_injected_when_the_pty_reader_dies_without_eof`。
///
/// 断言分两段：
/// 1. 远超宽限期后，entry 仍在（谓词没有把"进程退出"当成"可以撒手"）；
/// 2. 杀掉孙子进程后 master EOF，entry 随即被清扫掉 —— 第 1 段不是永久泄漏。
#[tokio::test]
async fn entry_survives_while_a_grandchild_holds_the_pty() {
    let grace = Duration::from_millis(300);
    let supervisor = InProcessProcSupervisor::start_with_grace(grace)
        .await
        .expect("start supervisor");
    let temp = tempfile::tempdir().expect("tempdir");
    let pid_file = temp.path().join("grandchild.pid");
    let proc_id = "pty-grandchild-holds-slave";

    ensure_pty(
        supervisor.sock(),
        proc_id,
        &["-c", &grandchild_script(&pid_file)],
        REPLAY_BYTES,
    )
    .await;
    write_stdin(supervisor.sock(), proc_id, b"go\n").await;
    await_exit(supervisor.sock(), proc_id).await;

    let grandchild = read_pid_file(&pid_file);
    assert!(
        process_is_alive(grandchild),
        "这条测试必须走「孙子进程持有 slave」的路径，但 pid {grandchild} 在父进程退出时就没了"
    );

    // 远超宽限期（>10 个清扫周期）。
    tokio::time::sleep(grace * 10).await;

    assert!(
        supervisor.registry().debug_entry_stats(proc_id).is_some(),
        "孙子进程仍持有 pty 时 entry 不得被移除（移除会 drop writer 并注入 \\n+VEOF）"
    );

    // 孙子进程一走，master EOF，reader 结束，下一次清扫收掉 entry：不是泄漏。
    kill_pid(grandchild);
    await_entry_gone(supervisor.registry(), proc_id).await;
}

/// 真正可证伪的那条：**reader 已死，slave 仍被孙子进程持有**。
///
/// 为什么它不空转 —— 这里 reader 线程已经退出，它那份 `Arc<ProcEntry>` 也已归
/// 还，registry 就是最后一个持有者。谓词一旦判错（例如把"reader 结束"
/// `PtyDrainGate::is_drained()` 当成"slave 全关"），清扫器移除 entry → 最后一个
/// `Arc` 归零 → `UnixMasterWriter::drop` 往 master 写 `['\n', VEOF]`
/// （portable-pty-0.9/src/unix.rs:393-405）→ 孙子进程的 `read` 拿到 EOF 退出。
/// 所以这条测试的第一句断言（孙子进程还活着）**直接**观测注入本身，故意排在
/// "entry 还在"之前，免得被前一条断言抢先炸掉。
///
/// 怎么制造"reader 已死但没 EOF"：把 master 置成 `O_NONBLOCK`，reader 的下一次
/// `read()` 拿到 `EAGAIN` 走 `Err(e)` 分支退出（`DrainGuard` 照常落闸，
/// `eof_reached` 保持 false）。这是生产代码里真实存在的那条错误路径，不是给测
/// 试开的旁门。
#[tokio::test]
async fn no_eof_is_injected_when_the_pty_reader_dies_without_eof() {
    let grace = Duration::from_millis(300);
    let supervisor = InProcessProcSupervisor::start_with_grace(grace)
        .await
        .expect("start supervisor");
    let temp = tempfile::tempdir().expect("tempdir");
    let pid_file = temp.path().join("grandchild.pid");
    let proc_id = "pty-reader-dies-without-eof";

    ensure_pty(
        supervisor.sock(),
        proc_id,
        &["-c", &grandchild_script(&pid_file)],
        REPLAY_BYTES,
    )
    .await;
    write_stdin(supervisor.sock(), proc_id, b"go\n").await;
    await_exit(supervisor.sock(), proc_id).await;

    let grandchild = read_pid_file(&pid_file);
    assert!(
        process_is_alive(grandchild),
        "这条测试必须走「孙子进程持有 slave」的路径，但 pid {grandchild} 在父进程退出时就没了"
    );

    // 让 reader 的下一次 read 失败……
    assert!(
        supervisor.registry().debug_force_pty_reader_error(proc_id),
        "故障注入失败：pty master fd 拿不到"
    );
    // ……并把它从阻塞的 read 里叫醒（行规程回显就够，孙子进程也会读走这一行）。
    write_stdin(supervisor.sock(), proc_id, b"wake\n").await;

    // 远超宽限期（>10 个清扫周期）。
    tokio::time::sleep(grace * 10).await;

    assert!(
        process_is_alive(grandchild),
        "孙子进程被 EOF 注入杀死了 —— 回收路径 drop 了 pty writer，\
         说明移除谓词把「reader 结束」误当成了「slave 全关」"
    );
    assert!(
        supervisor.registry().debug_entry_stats(proc_id).is_some(),
        "reader 因 read 错误退出、slave 仍被持有时，entry 不得被移除"
    );

    // 收尾：杀掉孙子进程。注意这条 entry 此后也不会自己消失 —— reader 已死，
    // `eof_reached` 再也置不上，连 `Cleanup` 也只会"排期"而不移除。这是本修法
    // 明确接受的代价：宁可在一条实际不可达的错误路径上留一条 entry（Linux 上
    // slave 全关走的是 `Ok(0)`，不是 `Err`），也不可能踢死一个活着的终端。
    // 整条 entry 最终随 supervisor 一起释放。
    kill_pid(grandchild);
}

/// `Cleanup` 跳过的是**宽限期**，不是安全闸：走的仍是同一个谓词（sticky exit
/// 已落定、master 已 EOF）。
///
/// 断言写成"远早于宽限期就消失"，不是"回复到手的那一瞬间就消失"：`CleanupOk`
/// 的语义是"已排期"（见 `ControlReply::CleanupOk`），安全闸未满足时移除落到后
/// 续某次周期性清扫 —— 而 master EOF 是否已经发生取决于 reader 线程有没有在
/// `PTY_DRAIN_GRACE`(50ms) 内被调度到，高负载 CI 上并不保证。用 600s 宽限期与
/// 20s 轮询上限拉开三个数量级的差距，"跳过宽限期"这件事仍被牢牢锁住，同时不
/// 引入任何时序假红（本 PR 的上游 #993 正是为消除 flake 而做）。
#[tokio::test]
async fn cleanup_removes_the_entry_without_waiting_for_the_grace() {
    let supervisor = InProcessProcSupervisor::start_with_grace(Duration::from_secs(600))
        .await
        .expect("start supervisor");
    let proc_id = "pty-cleanup-removes";
    ensure_pty(
        supervisor.sock(),
        proc_id,
        &["-c", "printf abc"],
        REPLAY_BYTES,
    )
    .await;
    await_exit(supervisor.sock(), proc_id).await;
    assert!(
        supervisor.registry().debug_entry_stats(proc_id).is_some(),
        "600s 宽限期内 entry 本应还在"
    );

    let mut stream = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect cleanup");
    write_frame(
        &mut stream,
        &ControlMsg::Cleanup(CleanupRequest {
            proc_id: proc_id.into(),
        }),
    )
    .await
    .expect("write cleanup");
    match read_frame(&mut stream).await.expect("read cleanup reply") {
        ControlReply::CleanupOk => {}
        other => panic!("unexpected cleanup reply: {other:?}"),
    }

    await_entry_gone(supervisor.registry(), proc_id).await;
}

/// 父进程后台起一个子 shell，用 `exec < /dev/tty` 把 stdin 重新指回 pty slave
/// （POSIX 下非交互 shell 的 `&` 会把 stdin 改成 /dev/null，不重开就读不到注入
/// 的字节，测试会假绿），然后阻塞在 `read` 上。父进程把它的 pid 写进
/// `pid_file` 后退出。
///
/// `trap '' HUP` 是必需的：pty child 是会话首进程，它退出时内核会给整个前台
/// 进程组发 SIGHUP。没有 trap 孙子进程会随父一起死，master 立刻 EOF，这条测试
/// 就悄悄退化成 happy path。`sleep 0.3` 关掉 fork/trap 竞争窗口。
///
/// 于是孙子进程满足两件事：**持有 slave**（master 不会 EOF），且 **stdin 是
/// pty**（任何 `\n`+VEOF 注入都会让它的 `read` 拿到 EOF 从而退出）。它是 EOF
/// 注入的直接探测器。
fn grandchild_script(pid_file: &Path) -> String {
    format!(
        "read x; (trap '' HUP; exec < /dev/tty; while read _; do :; done) & \
         printf '%s' \"$!\" > {}; sleep 0.3; exit 0",
        pid_file.display(),
    )
}

/// True when `pid` names a process that still exists and has not become a
/// zombie —— 僵尸已经关掉了全部 fd，证明不了它还攥着 slave。
fn process_is_alive(pid: i32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return false;
    };
    matches!(rest.split_whitespace().next(), Some(state) if state != "Z" && state != "X")
}

fn read_pid_file(pid_file: &Path) -> i32 {
    let raw = std::fs::read_to_string(pid_file)
        .unwrap_or_else(|e| panic!("read {}: {e}", pid_file.display()));
    raw.trim()
        .parse()
        .unwrap_or_else(|e| panic!("parse pid {raw:?}: {e}"))
}

fn kill_pid(pid: i32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// 轮询到 entry 真的从 registry 消失（或超时失败）。清扫是周期性的，所以断言
/// 必须等它一拍，而不是睡一个魔数。
async fn await_entry_gone(registry: &calm_proc_supervisor::ProcRegistry, proc_id: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if registry.debug_entry_stats(proc_id).is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{proc_id}: 早该被清扫，entry 仍留在 registry");
}

async fn ensure_noisy_pty(sock: &Path, proc_id: &str) {
    ensure_pty(
        sock,
        proc_id,
        &[
            "-c",
            &format!("head -c {NOISY_BYTES} /dev/zero | tr '\\0' 'x'"),
        ],
        REPLAY_BYTES,
    )
    .await;
}

async fn ensure_pty(sock: &Path, proc_id: &str, args: &[&str], replay_bytes: usize) {
    let mut stream = UnixStream::connect(sock).await.expect("connect ensure");
    write_frame(
        &mut stream,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: proc_id.into(),
            program: "/bin/sh".into(),
            args: args.iter().map(|arg| (*arg).into()).collect(),
            envs: Vec::new(),
            cwd: "/tmp".into(),
            ready_timeout_ms: 0,
            io_mode: IoMode::Pty { cols: 80, rows: 24 },
            replay_bytes,
        }),
    )
    .await
    .expect("write ensure");
    match read_frame(&mut stream).await.expect("read spawned") {
        ControlReply::Spawned { .. } => {}
        other => panic!("unexpected ensure reply: {other:?}"),
    }
    match read_frame(&mut stream).await.expect("read ready") {
        ControlReply::Ready => {}
        other => panic!("unexpected ready reply: {other:?}"),
    }
}

async fn write_stdin(sock: &Path, proc_id: &str, bytes: &[u8]) {
    let mut stream = UnixStream::connect(sock).await.expect("connect write");
    write_frame(
        &mut stream,
        &ControlMsg::WriteStdin(calm_session::control::WriteStdinRequest {
            proc_id: proc_id.into(),
            bytes: bytes.to_vec(),
            write_seq: Some(1),
        }),
    )
    .await
    .expect("write stdin");
    match timeout_read(&mut stream).await {
        ControlReply::WriteAck { .. } => {}
        other => panic!("unexpected write reply: {other:?}"),
    }
}

/// 附着直到收到 `Exited`，返回退出状态。
async fn await_exit(sock: &Path, proc_id: &str) -> (Option<i32>, bool) {
    let mut attach = UnixStream::connect(sock).await.expect("connect attach");
    write_frame(
        &mut attach,
        &ControlMsg::Attach(AttachRequest {
            proc_id: proc_id.into(),
            from_cursor: Some(0),
            reader_id: "reclaim-test".into(),
        }),
    )
    .await
    .expect("write attach");
    loop {
        match timeout_read(&mut attach).await {
            ControlReply::Exited {
                status, signalled, ..
            } => return (status, signalled),
            _ => continue,
        }
    }
}

/// 附着一次，返回 `AttachOk` 与随后的全部帧（读到 Exited 或超时为止）。
async fn attach_and_drain(
    sock: &Path,
    proc_id: &str,
) -> (calm_session::control::Attached, Vec<ControlReply>) {
    let mut attach = UnixStream::connect(sock).await.expect("connect attach");
    write_frame(
        &mut attach,
        &ControlMsg::Attach(AttachRequest {
            proc_id: proc_id.into(),
            from_cursor: Some(0),
            reader_id: "reclaim-test".into(),
        }),
    )
    .await
    .expect("write attach");
    let attached = match timeout_read(&mut attach).await {
        ControlReply::AttachOk(attached) => attached,
        other => panic!("expected AttachOk, got {other:?}"),
    };
    let mut frames = Vec::new();
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(3), read_frame(&mut attach)).await;
        match frame {
            Ok(Ok(reply)) => {
                let done = matches!(reply, ControlReply::Exited { .. });
                frames.push(reply);
                if done {
                    break;
                }
            }
            _ => break,
        }
    }
    (attached, frames)
}

async fn timeout_read(stream: &mut UnixStream) -> ControlReply {
    tokio::time::timeout(Duration::from_secs(20), read_frame(stream))
        .await
        .expect("frame timeout")
        .expect("read frame")
}

fn open_fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("read /proc/self/fd")
        .count()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
