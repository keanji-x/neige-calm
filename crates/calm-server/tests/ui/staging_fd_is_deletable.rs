//! Green fixture. Without it the compile-fail case beside it could be red for a
//! reason that has nothing to do with the seam — a typo, a moved module, a
//! private item — and the guard would pass vacuously.
//!
//! It deletes through a `StagingFd`, which is the one thing that is allowed,
//! and it does not try to reach a path out of either handle: there is no
//! accessor to try.

//! The seam lives in a function that is never called: what is under test is
//! whether it TYPE-CHECKS, and a `pass` fixture is also executed, so building
//! a real descriptor here would make the case depend on a filesystem it has no
//! business needing.

use calm_server::planner_attachments::dir::{Name, StagingFd, unlink_staged};

#[allow(dead_code)]
fn seam(staging: &StagingFd, name: &Name) {
    let _ = unlink_staged(staging, name);
}

fn main() {}
