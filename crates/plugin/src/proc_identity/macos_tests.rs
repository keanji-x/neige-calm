use super::*;
use std::{
    os::unix::process::CommandExt,
    process::{Child, Command},
    time::{Duration, Instant},
};

struct Owned(Child);
impl Owned {
    fn spawn(group: i32) -> Self {
        Self(
            Command::new("/bin/sleep")
                .arg("60")
                .process_group(group)
                .spawn()
                .unwrap(),
        )
    }
    fn pid(&self) -> i32 {
        self.0.id() as i32
    }
    fn zombie(&mut self) {
        self.0.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !read_macos_process(self.pid()).unwrap().unwrap().is_zombie {
            assert!(
                Instant::now() < deadline,
                "owned child did not become zombie"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        self.0.wait().expect("reap owned observer test child");
    }
}

fn turnover_after_enumeration(reap: bool) {
    let mut leader = Owned::spawn(0);
    let group = leader.pid();
    let mut member = Owned::spawn(group);
    leader.0.kill().unwrap();
    leader.0.wait().unwrap();
    let mut late = None;
    let observed = observe_group(group, &mut |pids| {
        if late.is_some() {
            return;
        }
        // This barrier runs after the real list syscall, before any per-PID read.
        assert_eq!(pids, [member.pid()]);
        assert!(!read_macos_process(member.pid()).unwrap().unwrap().is_zombie);
        late = Some(Owned::spawn(group));
        let child = read_macos_process(late.as_ref().unwrap().pid())
            .unwrap()
            .unwrap();
        assert_eq!(child.pgid, group);
        assert!(!child.is_zombie);
        member.zombie();
        if reap {
            member.0.wait().unwrap();
            assert!(read_macos_process(member.pid()).unwrap().is_none());
        }
    });
    let child = read_macos_process(late.as_ref().unwrap().pid())
        .unwrap()
        .unwrap();
    assert!(!child.is_zombie);
    eprintln!("turnover reap={reap}: observation={observed:?}, continuously live child={child:?}");
    assert!(
        !observed.is_ok_and(|members| members.iter().all(|m| m.is_zombie)),
        "a stale PID list must not prove a continuously live group empty"
    );
}

#[test]
fn proc_identity_turnover_to_zombie_does_not_prove_empty() {
    turnover_after_enumeration(false);
}

#[test]
fn proc_identity_turnover_to_reaped_does_not_prove_empty() {
    turnover_after_enumeration(true);
}

#[test]
fn proc_identity_stable_zombies_and_reaped_group_are_empty() {
    let mut leader = Owned::spawn(0);
    let group = leader.pid();
    let mut member = Owned::spawn(group);
    leader.0.kill().unwrap();
    leader.0.wait().unwrap();
    member.zombie();
    let observed = observe_macos_group(group).unwrap();
    assert_eq!(observed.len(), 1);
    assert!(observed.iter().all(|m| m.is_zombie));
    member.0.wait().unwrap();
    assert!(observe_macos_group(group).unwrap().is_empty());
}

#[test]
fn proc_identity_zombie_reaped_during_empty_proof_is_unresolved() {
    let mut member = Owned::spawn(0);
    let group = member.pid();
    member.zombie();
    let mut enumerations = 0;
    let observed = observe_group(group, &mut |pids| {
        enumerations += 1;
        if enumerations == 2 {
            // Release the birth identity pin after the proof's list, before its
            // final reads. That list alone cannot certify these numeric PIDs.
            assert_eq!(pids, [member.pid()]);
            member.0.wait().unwrap();
        }
    });
    assert!(
        observed.is_err(),
        "lost zombie identity must invalidate the proof"
    );
    assert!(observe_macos_group(group).unwrap().is_empty());
}
