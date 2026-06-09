# Fix brief — terminal_card_endpoint test pin

Test failure (`cargo test -p calm-server --test terminal_card_endpoint post_terminal_card_idempotency_retry_skips_validation_after_wave_delete`): first POST's response has `runtime: None` (runtime row's `status != Running` at serialize time), retry POST's cached operation result re-projects against a now-Running runtime row, so `retry_card != first_card`.

Test intent: verify that after wave delete, retrying with the same `Idempotency-Key` reuses the cached operation result instead of re-validating the (now-missing) wave. The intent does NOT require comparing the new projected `runtime` field.

## Change

In `crates/calm-server/tests/terminal_card_endpoint.rs:532`, replace `assert_eq!(retry_card, first_card);` with an assertion that compares ALL JSON keys EXCEPT `runtime` (which is a runtime-projection live read, not a property of the cached operation).

Implementation: write a small helper at the top of the file:
```rust
fn strip_runtime(mut card: Value) -> Value {
    if let Some(obj) = card.as_object_mut() {
        obj.remove("runtime");
    }
    card
}
```
Then `assert_eq!(strip_runtime(retry_card.clone()), strip_runtime(first_card.clone()));`.

Do not touch any other test or production code.

## Validate
- `PATH=/home/kenji/.cargo/bin:$PATH cargo test -p calm-server --test terminal_card_endpoint post_terminal_card_idempotency_retry_skips_validation_after_wave_delete`
- `PATH=/home/kenji/.cargo/bin:$PATH cargo test -p calm-server --test terminal_card_endpoint` (whole file green)
- `PATH=/home/kenji/.cargo/bin:$PATH cargo clippy --workspace --all-targets -- -D warnings`
