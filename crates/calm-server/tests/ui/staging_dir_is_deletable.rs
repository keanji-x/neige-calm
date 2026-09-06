//! Green fixture. Without it the compile-fail case beside it could be red for a
//! reason that has nothing to do with the seam — a typo, a moved module, a
//! private item — and the guard would pass vacuously.

use calm_server::planner_attachments::gc::remove_staged_file;
use calm_server::planner_attachments::{bound_dir, staging_dir};
use std::path::Path;

fn main() {
    let card = "card".into();
    let staging = staging_dir(Path::new("/tmp"), &card);
    let _ = remove_staged_file(&staging, "x.png");

    // A bound directory is reachable and its path is `pub`. Nothing in the
    // type system stops that `&Path` from reaching a deletion; the fence next
    // door only forbids handing the `BoundDir` itself to `remove_staged_file`.
    let bound = bound_dir(Path::new("/tmp"), &card);
    let _: &Path = bound.path();
}
