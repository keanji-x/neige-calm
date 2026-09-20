//! Shared integration-test support: a fake `codex app-server` so `POST /api/tracks` returns 201 without a real codex.

use calm_server::state::CodexClient;

/// Absolute path to the `osc-probe-child` fixture binary, which doubles as a fake `codex app-server`.
pub fn fake_codex_bin() -> String {
    env!("CARGO_BIN_EXE_osc-probe-child").to_string()
}

/// `CodexClient::new_stub()` with `codex_bin` pointed at the fake-codex fixture.
pub fn fake_codex_client() -> CodexClient {
    let mut c = CodexClient::new_stub();
    c.codex_bin = fake_codex_bin();
    c
}
