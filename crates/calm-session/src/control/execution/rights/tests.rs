use super::*;
use std::{io::Read, os::fd::AsFd};
use tokio::io::AsyncWriteExt;

fn frame(value: &str) -> Vec<u8> {
    let payload = bincode::serde::encode_to_vec(value, bincode::config::standard()).unwrap();
    let mut bytes = Vec::new();
    bytes.extend(crate::FRAME_MAGIC);
    bytes.extend(crate::FRAME_VERSION.to_be_bytes());
    bytes.extend((payload.len() as u32).to_be_bytes());
    bytes.extend(payload);
    bytes
}
#[tokio::test]
async fn execution_ancillary_fragmentation_and_coalesced_frames_preserve_rights_boundaries() {
    let (mut sender, receiver) = UnixStream::pair().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::File::open(temp.path()).unwrap();
    let bytes = frame(&"x".repeat(65536));
    let task = tokio::spawn(async move {
        sender.writable().await.unwrap();
        assert_eq!(send_once(&sender, &bytes[..1], &[root.as_fd()]).unwrap(), 1);
        for part in bytes[1..].chunks(37) {
            sender.write_all(part).await.unwrap();
        }
        send(&sender, &"next", &[]).await.unwrap();
    });
    let (value, files) = receive::<String>(&receiver).await.unwrap();
    assert_eq!(value.len(), 65536);
    assert_eq!(files.len(), 1);
    let flags = unsafe { libc::fcntl(files[0].as_raw_fd(), libc::F_GETFD) };
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    assert!(
        std::fs::File::from(files.into_iter().next().unwrap())
            .metadata()
            .unwrap()
            .is_dir()
    );
    let (next, files) = receive::<String>(&receiver).await.unwrap();
    assert_eq!(next, "next");
    assert!(files.is_empty());
    task.await.unwrap();
}
#[tokio::test]
async fn execution_ancillary_truncation_closes_all_received_descriptors() {
    let (sender, receiver) = UnixStream::pair().unwrap();
    let (passed, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();
    sender.writable().await.unwrap();
    let files = (0..MAX_RIGHTS + 1)
        .map(|_| passed.as_fd())
        .collect::<Vec<_>>();
    let bytes = frame("truncated");
    assert_eq!(send_once(&sender, &bytes[..1], &files).unwrap(), 1);
    assert_eq!(
        send_once(&sender, &bytes[1..], &[]).unwrap(),
        bytes.len() - 1
    );
    drop(files);
    drop(passed);
    assert!(receive::<String>(&receiver).await.is_err());
    let mut byte = [0];
    assert_eq!(
        peer.read(&mut byte).unwrap(),
        0,
        "a rejected received descriptor leaked"
    );
}
#[tokio::test]
async fn execution_ancillary_rights_in_middle_of_frame_are_rejected_and_closed() {
    let (sender, receiver) = UnixStream::pair().unwrap();
    let (passed, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();
    let bytes = frame("misplaced");
    sender.writable().await.unwrap();
    assert_eq!(send_once(&sender, &bytes[..1], &[]).unwrap(), 1);
    assert_eq!(
        send_once(&sender, &bytes[1..2], &[passed.as_fd()]).unwrap(),
        1
    );
    drop(passed);
    assert!(receive::<String>(&receiver).await.is_err());
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
}
#[tokio::test]
async fn execution_ancillary_peer_crash_mid_frame_closes_transferred_objects() {
    let (sender, receiver) = UnixStream::pair().unwrap();
    let (passed, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap();
    sender.writable().await.unwrap();
    assert_eq!(
        send_once(&sender, &frame("crash")[..1], &[passed.as_fd()]).unwrap(),
        1
    );
    drop(passed);
    drop(sender);
    assert!(receive::<String>(&receiver).await.is_err());
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
}
