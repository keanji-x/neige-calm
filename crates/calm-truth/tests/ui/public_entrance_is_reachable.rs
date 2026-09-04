// #1252 S3' PR-B — the green fixture of the cross-crate half.
//
// Without this case the two `compile_fail` cases could both be passing for a
// reason that has nothing to do with the seam (dependency not wired, crate not
// building at all). This one must COMPILE: the public entrance is exactly as
// reachable from downstream as it was before the seam existed.

fn main() {
    let _entrance = calm_truth::db::sqlite::append_decision_event_in_tx;
    let _batch_entrance = calm_truth::db::sqlite::append_decision_events_in_tx;
}
