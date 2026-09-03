# Frontend end-to-end tests

These tests require the real backend stack at `http://127.0.0.1:4041` and Node.js 22. Start the
stack outside this command, then run `npm run e2e` from `fe/`; Playwright starts the Vite frontend
on port 5180. Override the backend with `FE_API_PROXY_TARGET` or the frontend port with
`FE_DEV_PORT` when necessary.

The stack needs a codex app-server it can actually talk to: `track-conversation-create.spec.ts`
requires `POST /api/tracks/{id}/conversations` to return 201, and without one the request dies in
the adapter's daemon preflight — before the card, session and MCP token are minted, so the failure
tells you nothing about whether the mint is correct. Point `CALM_CODEX_HOST_BIN` at the
`osc-probe-child` fixture, which answers the handshake (`initialize` / `thread/start` /
`turn/start`) and nothing else:

```sh
cargo build --release -p calm-server --bin osc-probe-child
# in the stack's .env
CALM_CODEX_HOST_BIN=<repo>/target/release/osc-probe-child
```

This is what `ci.yml`'s `fe e2e` job does. `CALM_CODEX_HOST_BIN=/bin/true` still brings the stack
up, but the conversation-create test will fail against it.
