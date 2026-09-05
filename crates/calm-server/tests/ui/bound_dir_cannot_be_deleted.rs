//! `bound/` has no deletion path. This is the statement a source-text scan
//! cannot make: it would have to prove that no `PathBuf` derived from
//! `bound_dir` ever reaches a `remove_file`, across every present and future
//! call site. The type system makes it structural instead — there is no
//! conversion from `BoundDir` into `StagingDir`, and `remove_staged_file`
//! accepts only the latter.
//!
//! Adding `impl From<BoundDir> for StagingDir` makes this file compile, which
//! is exactly the mutation this case exists to catch.

use calm_server::planner_attachments::gc::remove_staged_file;
use calm_server::planner_attachments::{StagingDir, bound_dir};
use std::path::Path;

fn main() {
    let card = "card".into();
    let bound = bound_dir(Path::new("/tmp"), &card);
    let _ = remove_staged_file(&StagingDir::from(bound), "x.png");
}
