//! Exact execution stop under the managed inherited-marker contract.
//! The supervisor owns the marker and seals delayed launches. This coordinates
//! cooperative managed descendants; it is not containment against a process
//! deliberately clearing its marker and leaving the original group.
use super::*;
use calm_worker_runtime::execution_process::{capture, stop_marker_pass, stop_pass};

pub(super) async fn stop(registry: &ProcRegistry, proc_id: &str) -> ControlReply {
    stop_with_identity(registry, proc_id, false).await
}

pub(super) async fn stop_known(registry: &ProcRegistry, proc_id: &str) -> ControlReply {
    stop_with_identity(registry, proc_id, true).await
}
async fn stop_with_identity(
    registry: &ProcRegistry,
    proc_id: &str,
    require_known: bool,
) -> ControlReply {
    let mut sealed = registry.executions.lock().await;
    // Seal before scanning. A reply loss still cannot permit a replacement.
    sealed.insert(proc_id.to_owned());
    let entry = match registry.inner.lock() {
        Ok(entries) => entries.get(proc_id).cloned(),
        Err(_) => {
            return ControlReply::Error {
                kind: ControlErrorKind::Internal,
                message: "execution registry unavailable; stop not confirmed".into(),
            };
        }
    };
    if require_known && entry.is_none() {
        return ControlReply::Error {
            kind: ControlErrorKind::WrongState,
            message: "legacy execution identity is unknown; stop not confirmed".into(),
        };
    }
    let artifacts = match entry.as_ref() {
        Some(entry) => match capture(entry.pid as i32) {
            Ok(artifacts) => Some(artifacts),
            Err(e) => {
                return ControlReply::Error {
                    kind: ControlErrorKind::WrongState,
                    message: format!("execution identity unavailable; retained: {e}"),
                };
            }
        },
        None => None,
    };
    let stop = async {
        loop {
            let artifacts = artifacts.clone();
            let marker = proc_id.to_owned();
            let stopped = tokio::task::spawn_blocking(move || match artifacts {
                Some(artifacts) => stop_pass(&artifacts, &marker),
                None => stop_marker_pass(&marker),
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
            if stopped {
                return Ok::<_, String>(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    match tokio::time::timeout(Duration::from_secs(5), stop).await {
        Ok(Ok(())) => ControlReply::Stopped,
        Ok(Err(message)) => ControlReply::Error {
            kind: ControlErrorKind::WrongState,
            message,
        },
        Err(_) => ControlReply::Error {
            kind: ControlErrorKind::WrongState,
            message: "execution stop unconfirmed; retry sealed identity".into(),
        },
    }
}
