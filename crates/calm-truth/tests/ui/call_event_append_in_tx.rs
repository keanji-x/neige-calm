// #1252 S3' PR-B — cross-crate half.
//
// A downstream crate cannot reach the raw appender. `SqlxRepo` itself is
// public; `event_append_in_tx` is a private inherent associated function, so
// naming it from outside the crate fails.

use calm_truth::db::sqlite::SqlxRepo;

fn main() {
    let _appender = SqlxRepo::event_append_in_tx;
}
