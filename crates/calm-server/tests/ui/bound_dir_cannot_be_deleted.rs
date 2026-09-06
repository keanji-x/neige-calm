//! One statement: there is no conversion from `BoundDir` into `StagingDir`,
//! so `remove_staged_file` — which takes only the latter — cannot be reached by
//! passing it a bound directory.
//!
//! Adding `impl From<BoundDir> for StagingDir` makes this file compile, which
//! is exactly the mutation this case exists to catch.
//!
//! It is not a proof that `bound/` is never deleted. `BoundDir::path()` is
//! `pub`, so `std::fs::remove_dir_all(bound_dir(root, &card).path())` compiles
//! and this fence stays green — the fixture beside it does the first half of
//! exactly that. Keeping `bound/` undeleted is a property of the call sites in
//! `planner_attachments`, not of the type system.

use calm_server::planner_attachments::gc::remove_staged_file;
use calm_server::planner_attachments::{StagingDir, bound_dir};
use std::path::Path;

fn main() {
    let card = "card".into();
    let bound = bound_dir(Path::new("/tmp"), &card);
    let _ = remove_staged_file(&StagingDir::from(bound), "x.png");
}
