//! 已退出的 pty `ProcEntry` 必须被回收：宽限期内整条保留（replay + sticky exit），宽限期后整条移除；slave 仍被持有时不移除。

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
    assert!(stats.exit_recorded, "退出状态应已落定");
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

/// fd 的证据是 `/proc/self/fd` 的计数：supervisor 与测试同进程，泄漏会直接体现在这个数字上。
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

/// 覆盖的是"资源追踪"，不是"EOF 注入"：reader 线程还活着并持有一份 `Arc<ProcEntry>`，writer 不会归零；真正能证伪注入的是下面 reader 已死的那条。
/// 第 3 段：entry 一走，被钉住的 leader 僵尸也被收掉、pin 计数归零。
#[tokio::test]
async fn entry_removal_reaps_even_when_the_grandchild_holds_the_pty() {
    let grace = Duration::from_millis(300);
    let supervisor = InProcessProcSupervisor::start_with_grace(grace)
        .await
        .expect("start supervisor");
    let temp = tempfile::tempdir().expect("tempdir");
    let pid_file = temp.path().join("grandchild.pid");
    let proc_id = "pty-grandchild-holds-slave";

    let leader = ensure_pty(
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
    // 建立步：entry 被保留期间，leader 必须是一个仍属于本进程的僵尸，否则第 3 段可能假绿。
    assert_eq!(
        proc_state(leader).as_deref(),
        Some("Z"),
        "entry 仍被保留时，leader {leader} 必须仍是本进程未收尸的僵尸 —— \
         waiter 用 waitid(.., WNOWAIT) 只观测不收尸"
    );

    // 孙子进程一走，master EOF，reader 结束，下一次清扫收掉 entry：不是泄漏。
    kill_pid(grandchild);
    await_entry_gone(supervisor.registry(), proc_id).await;

    // 轮询而不是即时断言：registry 撒手的那一瞬，reader 线程与清扫器的 doomed vec 还各持一份 Arc。
    assert!(
        poll_until(Duration::from_secs(5), || {
            supervisor.registry().debug_pin_count() == 0
                && proc_state(leader).as_deref() != Some("Z")
        })
        .await,
        "entry 被回收后 leader {leader} 的僵尸仍未被收：pin_count = {}, state = {:?}",
        supervisor.registry().debug_pin_count(),
        proc_state(leader)
    );
}

/// `/proc/<pid>/stat` 的 state 字段（第 3 个）。`comm` 可能含空格与括号，
/// 唯一安全的切分点是**最后一个** `')'`。
fn proc_state(pid: i32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().next().map(str::to_owned)
}

/// 必须 `async` + `tokio::time::sleep`：current-thread runtime 下阻塞这根线程会把 serve 循环（含清扫器）一并冻住。
async fn poll_until(budget: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + budget;
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

/// 真正可证伪的那条：reader 已死，slave 仍被孙子进程持有 —— registry 是最后一个 `Arc` 持有者，谓词判错就会 drop writer 并注入 `\n`+VEOF。
/// 第一句断言（孙子进程还活着）直接观测注入本身，故意排在"entry 还在"之前。
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

    // 收尾。这条 entry 此后不会自己消失（reader 已死，`eof_reached` 再也置不上），随 supervisor 一起释放。
    kill_pid(grandchild);
}

/// `Cleanup` 跳过的是宽限期，不是安全闸。
/// 断言写成"远早于宽限期就消失"而非"回复到手就消失"：`CleanupOk` 只是"已排期"，master EOF 取决于 reader 是否在 `PTY_DRAIN_GRACE` 内被调度到。
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

/// 后台子 shell 用 `exec < /dev/tty` 把 stdin 重新指回 pty slave（`&` 会把 stdin 改成 /dev/null），然后阻塞在 `read` 上；父进程写下它的 pid 后退出。
/// `trap '' HUP` 是必需的：会话首进程退出时内核给前台进程组发 SIGHUP，没有 trap 孙子进程会随父一起死，测试退化成 happy path。
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

/// 轮询到 entry 真的从 registry 消失；清扫是周期性的，断言必须等它一拍。
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

/// 返回 `Spawned.pid`（= pty leader 的 pid）。
async fn ensure_pty(sock: &Path, proc_id: &str, args: &[&str], replay_bytes: usize) -> i32 {
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
    let pid = match read_frame(&mut stream).await.expect("read spawned") {
        ControlReply::Spawned { pid } => pid as i32,
        other => panic!("unexpected ensure reply: {other:?}"),
    };
    match read_frame(&mut stream).await.expect("read ready") {
        ControlReply::Ready => {}
        other => panic!("unexpected ready reply: {other:?}"),
    }
    pid
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
