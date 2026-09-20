//! Green fixture: without it the compile-fail case beside it could be red for a reason that has nothing to do with the seam.

//! The seam is never called: what is under test is whether it TYPE-CHECKS, and a `pass` fixture is also
//! executed, so building a real descriptor here would make the case depend on a filesystem.

use calm_server::planner_attachments::dir::{Name, StagingFd, unlink_staged};

#[allow(dead_code)]
fn seam(staging: &StagingFd, name: &Name) {
    let _ = unlink_staged(staging, name);
}

fn main() {}
