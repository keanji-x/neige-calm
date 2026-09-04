// #1252 S3' PR-B — cross-crate half.
//
// A downstream crate cannot name the capability type. `mod events` is private
// inside `db::sqlite`, so the path does not resolve at all; `gated` and
// `Authorized` are never reached.

fn main() {
    let _forged: Option<calm_truth::db::sqlite::events::gated::Authorized<'_>> = None;
}
