// The green fixture: this one must COMPILE, so the two `compile_fail` cases
// cannot both pass for a reason unrelated to the seam (dependency not wired, crate not building).

fn main() {
    let _entrance = calm_truth::db::sqlite::append_decision_event_in_tx;
    let _batch_entrance = calm_truth::db::sqlite::append_decision_events_in_tx;
}
