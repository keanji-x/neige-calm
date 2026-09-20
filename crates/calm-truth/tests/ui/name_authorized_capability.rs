// `mod events` is private inside `db::sqlite`, so the path does not resolve at all.

fn main() {
    let _forged: Option<calm_truth::db::sqlite::events::gated::Authorized<'_>> = None;
}
