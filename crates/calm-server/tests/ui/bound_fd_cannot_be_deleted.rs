//! One statement: there is no conversion from `BoundFd` into `StagingFd`, so
//! `unlink_staged` — which takes only the latter — cannot be reached by
//! passing it a bound directory.
//!
//! Adding `impl From<BoundFd> for StagingFd` makes this file compile, which is
//! exactly the mutation this case exists to catch.
//!
//! # What changed, and why this fence now says more than it used to
//!
//! Its predecessor guarded `BoundDir`/`StagingDir` and carried a paragraph
//! admitting how little that proved: `BoundDir::path()` was `pub`, so
//! `std::fs::remove_dir_all(bound_dir(root, &card).path())` compiled anywhere
//! and the fence stayed green. The green fixture beside it deliberately did
//! the first half of exactly that, to keep the admission honest.
//!
//! `BoundFd` exposes no path, no descriptor and no conversion, and every
//! filesystem operation in the module goes through `planner_attachments::dir`,
//! which has no path-taking entry point. So the escape hatch that paragraph
//! described is gone — see `dir`'s module docs for the grep that keeps it
//! gone.

use calm_server::planner_attachments::dir::{BoundFd, Name, StagingFd, unlink_staged};

#[allow(dead_code)]
fn seam(bound: BoundFd, name: &Name) {
    let _ = unlink_staged(&StagingFd::from(bound), name);
}

fn main() {}
