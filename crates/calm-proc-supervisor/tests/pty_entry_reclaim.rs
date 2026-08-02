//! #996: 已退出的 pty `ProcEntry` 必须被回收 —— `ByteRing` 与 pty master fd
//! 不能随终端创建次数线性泄漏 —— 同时不能牺牲 sticky exit 语义：晚到的 attach
//! 仍须得知进程如何结束。

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    AttachRequest, CleanupRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode,
};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

/// 每个终端产生的输出量，足够让"ring 是否被释放"成为可观测事实。
const NOISY_BYTES: usize = 200_000;
const REPLAY_BYTES: usize = 1024 * 1024;

/// 复现：N 个 pty 全部退出后，registry 里的 entry 必须交还 ring 内存与
/// pty master fd。当前 main 上没有任何常规回收路径，于是 8 个终端留下
/// 8 份 1 MiB 级 ring + 8 个常开的 master fd。
#[tokio::test]
async fn exited_pty_entries_release_ring_and_master_fd() {
    let grace = Duration::from_secs(3);
    let supervisor = InProcessProcSupervisor::start_with(grace, 128)
        .await
        .expect("start supervisor");
    let registry = supervisor.registry();

    // 预热一轮，让 tokio / 临时目录相关的 fd 先稳定下来。
    let warm = "pty-reclaim-warmup";
    ensure_noisy_pty(supervisor.sock(), warm).await;
    await_exit(supervisor.sock(), warm).await;

    let fds_before = open_fd_count();

    let procs: Vec<String> = (0..8).map(|i| format!("pty-reclaim-{i}")).collect();
    for proc_id in &procs {
        ensure_noisy_pty(supervisor.sock(), proc_id).await;
    }
    for proc_id in &procs {
        await_exit(supervisor.sock(), proc_id).await;
    }

    // 宽限期内 replay 仍然完整 —— 回收不能早于宽限期。
    let buffered_in_grace: usize = procs
        .iter()
        .map(|proc_id| {
            registry
                .debug_entry_stats(proc_id)
                .expect("entry present during grace")
                .buffered_bytes
        })
        .sum();
    assert!(
        buffered_in_grace > NOISY_BYTES,
        "宽限期内应仍持有 replay 字节，实际 {buffered_in_grace}"
    );

    tokio::time::sleep(grace + Duration::from_secs(2)).await;

    for proc_id in &procs {
        let stats = registry
            .debug_entry_stats(proc_id)
            .expect("tombstone must remain for sticky exit");
        assert_eq!(
            stats.buffered_bytes, 0,
            "{proc_id}: ByteRing 未释放（仍持有 {} 字节）",
            stats.buffered_bytes
        );
        assert!(!stats.pty_master_open, "{proc_id}: pty master fd 未关闭");
        assert!(stats.exited, "{proc_id}: 退出状态应保留");
    }

    let fds_after = open_fd_count();
    assert!(
        fds_after <= fds_before + 3,
        "pty master fd 随终端数线性泄漏：before={fds_before} after={fds_after}（8 个已退出终端）"
    );
}

/// 回收不得破坏 sticky exit：进程退出很久（远超宽限期）之后新 attach
/// 仍能得知退出状态，并通过 `Gap` 知晓 replay 已不可用。
#[tokio::test]
async fn sticky_exit_survives_entry_reclaim() {
    let supervisor = InProcessProcSupervisor::start_with(Duration::from_millis(200), 128)
        .await
        .expect("start supervisor");
    let proc_id = "pty-sticky-after-reclaim";
    ensure_pty(
        supervisor.sock(),
        proc_id,
        &["-c", "printf abc; exit 7"],
        REPLAY_BYTES,
    )
    .await;
    let exit = await_exit(supervisor.sock(), proc_id).await;
    assert_eq!(exit, (Some(7), false));

    tokio::time::sleep(Duration::from_secs(2)).await;
    let stats = supervisor
        .registry()
        .debug_entry_stats(proc_id)
        .expect("tombstone retained");
    assert!(!stats.pty_master_open, "回收后 master fd 应已关闭");
    assert_eq!(stats.buffered_bytes, 0, "回收后 ring 应已释放");

    let (attached, frames) = attach_and_drain(supervisor.sock(), proc_id).await;
    assert!(!attached.running);
    assert!(attached.replay.is_empty(), "回收后 replay 不再可用，应为空");
    assert!(
        frames
            .iter()
            .any(|frame| matches!(frame, ControlReply::Gap { .. })),
        "回收后 attach 应带 Gap 告知 replay 不可用，实际 {frames:?}"
    );
    let exited = frames
        .iter()
        .find_map(|frame| match frame {
            ControlReply::Exited {
                status, signalled, ..
            } => Some((*status, *signalled)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("退出很久后 attach 仍须拿到 sticky exit，实际 {frames:?}"));
    assert_eq!(exited, (Some(7), false));
}

/// 宽限期内回收不得提前发生：刚断开的客户端重连必须还能看到最后一屏。
#[tokio::test]
async fn replay_survives_within_reclaim_grace() {
    let supervisor = InProcessProcSupervisor::start_with(Duration::from_secs(30), 128)
        .await
        .expect("start supervisor");
    let proc_id = "pty-replay-in-grace";
    ensure_pty(
        supervisor.sock(),
        proc_id,
        &["-c", "printf abc"],
        REPLAY_BYTES,
    )
    .await;
    await_exit(supervisor.sock(), proc_id).await;

    let (attached, frames) = attach_and_drain(supervisor.sock(), proc_id).await;
    assert!(
        contains(&attached.replay, b"abc"),
        "宽限期内 replay 应完好，实际 {:?}",
        attached.replay
    );
    assert!(
        frames
            .iter()
            .any(|frame| matches!(frame, ControlReply::Exited { .. })),
        "应仍收到 sticky exit，实际 {frames:?}"
    );
}

/// `Cleanup` 对 pty 不再整个移除 entry —— 那会让之后的 attach 拿到
/// `UnknownProc`，客户端无从得知进程如何结束。改为立即墓碑化。
#[tokio::test]
async fn cleanup_pty_reclaims_but_keeps_sticky_exit() {
    let supervisor = InProcessProcSupervisor::start_with(Duration::from_secs(60), 128)
        .await
        .expect("start supervisor");
    let proc_id = "pty-cleanup-tombstone";
    ensure_pty(
        supervisor.sock(),
        proc_id,
        &["-c", "printf abc"],
        REPLAY_BYTES,
    )
    .await;
    await_exit(supervisor.sock(), proc_id).await;

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

    let stats = supervisor
        .registry()
        .debug_entry_stats(proc_id)
        .expect("cleanup 后仍须保留墓碑");
    assert!(!stats.pty_master_open, "cleanup 应关闭 pty master fd");
    assert_eq!(stats.buffered_bytes, 0, "cleanup 应释放 ring");

    let (_, frames) = attach_and_drain(supervisor.sock(), proc_id).await;
    assert!(
        frames
            .iter()
            .any(|frame| matches!(frame, ControlReply::Exited { .. })),
        "cleanup 之后 attach 仍须拿到 sticky exit，实际 {frames:?}"
    );
}

/// 墓碑本身也不能无界增长：超过上限后按 FIFO 淘汰最老的墓碑。
#[tokio::test]
async fn tombstones_are_bounded() {
    let supervisor = InProcessProcSupervisor::start_with(Duration::from_millis(200), 2)
        .await
        .expect("start supervisor");
    let procs: Vec<String> = (0..6).map(|i| format!("pty-bounded-{i}")).collect();
    for proc_id in &procs {
        ensure_pty(supervisor.sock(), proc_id, &["-c", "printf abc"], 4096).await;
        await_exit(supervisor.sock(), proc_id).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let count = supervisor.registry().debug_entry_count();
    assert!(
        count <= 2,
        "registry 应被墓碑上限约束，实际 {count} 条 entry"
    );
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
