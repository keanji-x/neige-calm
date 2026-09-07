//! Startup readiness through RMUX's public typed blocking client. Keep protocol
//! framing in the dependency; never invoke CLI parsing or daemon auto-start.
use rmux_proto::Response;
use std::path::Path;
use std::time::{Duration, Instant};

pub(crate) fn wait(socket: &Path, budget: Duration) -> anyhow::Result<()> {
    let started = Instant::now();
    let mut client = rmux_client::connect(socket)?;
    loop {
        let remaining = budget
            .checked_sub(started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| anyhow::anyhow!("runtime configuration readiness timed out"))?;
        // The public blocking client bounds each RPC with its own socket
        // timeouts. The async caller separately bounds the overall wait; an
        // in-flight read-only RPC can finish after that caller times out.
        match client.daemon_status()? {
            Response::DaemonStatus(status) if !status.config_loading => return Ok(()),
            Response::DaemonStatus(_) => {}
            response => anyhow::bail!("unexpected runtime readiness response: {response:?}"),
        }
        std::thread::sleep(Duration::from_millis(10).min(remaining));
    }
}
