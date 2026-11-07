//! Nonleader socket authority uses real native identities and independently owned children.
use super::*;
use macos_process::{ListenerIdentity, terminate_listener};

fn listener_group() -> (Owned, Owned, Owned) {
    let leader = Owned(
        Command::new("/bin/sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    let member = || {
        Owned(
            Command::new("/bin/sleep")
                .arg("60")
                .process_group(leader.pid())
                .spawn()
                .unwrap(),
        )
    };
    let peer = member();
    let sibling = member();
    (leader, peer, sibling)
}

#[tokio::test]
async fn native_macos_recovery_nonleader_listener_stale_birth_never_signals() {
    let (mut leader, mut peer, mut member) = listener_group();
    let pgid = leader.pid();
    let mut identity = ListenerIdentity::new(peer.pid()).unwrap();
    assert_eq!(identity.process.pgid, pgid);
    // Deliberately stale input; the boot and all children remain real and live.
    identity.process.start_time += 1;
    let result = terminate_listener(identity, Some(pgid), Duration::from_millis(20)).await;
    let survivors = [leader.alive(), peer.alive(), member.alive()];
    // Owned kills and reaps every child even when the assertions below fail.
    drop((leader, peer, member));
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Other);
    assert_eq!(
        survivors, [true; 3],
        "stale peer grants no signal authority"
    );
}

#[tokio::test]
async fn native_macos_recovery_nonleader_listener_reaped_leader_never_signals() {
    let (mut leader, mut peer, mut member) = listener_group();
    let pgid = leader.pid();
    leader.0.kill().unwrap();
    leader.0.wait().unwrap();
    assert!(
        plugin::proc_identity::read_macos_process(pgid)
            .unwrap()
            .is_none()
    );
    let identity = ListenerIdentity::new(peer.pid()).unwrap();
    assert_eq!(identity.process.pgid, pgid);
    let result = terminate_listener(identity, Some(pgid), Duration::from_millis(20)).await;
    let survivors = [peer.alive(), member.alive()];
    drop((leader, peer, member));
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Other);
    assert_eq!(
        survivors, [true; 2],
        "live peer cannot replace an absent leader"
    );
}
