## Summary

- Rename dispatcher request events and wire kinds from `*.job_requested` to `*.worker_requested`, with serde aliases for old event payloads and a SQL migration for persisted `events.kind` rows.
- Add additive `Card.runtime: CardRuntimeView` projection while keeping all legacy runtime payload keys (`terminal_id`, `claude_session_id`, `codex_thread_id`, `codex_source`, `codex_thread_status`) intact.
- Regenerate OpenAPI / TypeScript bindings and update frontend runtime event schemas for the new worker-request event names.

Addresses #581 (items 2, 3, 5)

## Testing

- `RUSTC_WRAPPER= cargo fmt --all --check`
- `RUSTC_WRAPPER= cargo clippy --workspace --all-targets -- -D warnings`
- `RUSTC_WRAPPER= cargo check --workspace --all-targets`
- `RUSTC_WRAPPER= cargo test -p calm-server --test runtime_repo`
- `RUSTC_WRAPPER= cargo test -p calm-server codex_worker_requested_serde_round_trip`
- `RUSTC_WRAPPER= cargo test -p calm-server terminal_worker_requested_serde_round_trip`
- `RUSTC_WRAPPER= cargo test -p calm-server dispatcher_filter_matches_push_kinds`
- `RUSTC_WRAPPER= cargo test -p calm-server --test mcp_wave_file runs_index`
- `RUSTC_WRAPPER= cargo test -p calm-server -- migrations`
- `PATH=/home/kenji/.nvm/versions/node/v22.22.2/bin:$PATH npm_config_cache=/tmp/neige-npm-cache RUSTC_WRAPPER= npm run gen:api`
- `PATH=/home/kenji/.nvm/versions/node/v22.22.2/bin:$PATH npm_config_cache=/tmp/neige-npm-cache npm test -- --run`
- `PATH=/home/kenji/.nvm/versions/node/v22.22.2/bin:$PATH npm_config_cache=/tmp/neige-npm-cache npm run typecheck`
- `PATH=/home/kenji/.nvm/versions/node/v22.22.2/bin:$PATH npm_config_cache=/tmp/neige-npm-cache npm run build`

Notes:

- The requested Node path `/home/kenji/.nvm/versions/node/v20.20.2/bin/` was not present in this environment; validation used available Node `v22.22.2`.
- `RUSTC_WRAPPER= cargo test --workspace` was attempted but blocked by sandbox permissions in `calm-proc-supervisor --test attach_race_no_byte_loss` while starting the supervisor (`Operation not permitted`). A focused MCP emit test also hit sandbox Unix-socket bind permissions.
