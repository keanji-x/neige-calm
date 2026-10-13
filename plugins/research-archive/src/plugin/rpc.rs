//! The plugin's half of the kernel's stdio channel: newline-delimited
//! JSON-RPC both ways. The kernel calls `initialize` and `tools/call`; the
//! plugin calls the kernel's `neige.*` callbacks and waits for their replies,
//! which arrive on the same stdin the read loop owns.

use std::collections::HashMap;
use std::io::{BufWriter, Stdout, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use serde_json::{Value, json};

/// Callback ids start high, apart from the kernel's own request ids.
const FIRST_CALLBACK_ID: u64 = 1_000;
/// The kernel answers callbacks from memory or SQLite; a slower answer is a
/// wedged host, better logged than blocking the poller.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(15);

/// `stdout` behind a mutex; every outbound call parks a one-shot sender
/// that the read loop completes.
pub(super) struct Rpc {
    out: Mutex<BufWriter<Stdout>>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, mpsc::Sender<Value>>>,
}

impl Rpc {
    pub(super) fn new() -> Self {
        Self {
            out: Mutex::new(BufWriter::new(std::io::stdout())),
            next_id: AtomicU64::new(FIRST_CALLBACK_ID),
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn send(&self, value: &Value) {
        let Ok(mut out) = self.out.lock() else {
            return;
        };
        let mut line = value.to_string();
        line.push('\n');
        let _ = out.write_all(line.as_bytes());
        let _ = out.flush();
    }

    pub(super) fn reply(&self, id: Value, result: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    pub(super) fn error(&self, id: Value, code: i64, message: &str) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }));
    }

    /// Calls a kernel callback and waits for its reply.
    pub(super) fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending
            .lock()
            .map_err(|_| "pending map poisoned".to_string())?
            .insert(id, tx);
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        let outcome = match rx.recv_timeout(CALLBACK_TIMEOUT) {
            Ok(frame) => match frame.get("error") {
                Some(err) => Err(format!("{method} failed: {err}")),
                None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
            },
            Err(_) => Err(format!("{method} timed out after {CALLBACK_TIMEOUT:?}")),
        };
        // Answered, failed or timed out, the id is spent.
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
        outcome
    }

    /// Routes a reply to its waiter; false when nobody waits (a late reply).
    pub(super) fn complete(&self, id: u64, frame: Value) -> bool {
        let sender = self.pending.lock().ok().and_then(|mut p| p.remove(&id));
        match sender {
            Some(tx) => tx.send(frame).is_ok(),
            None => false,
        }
    }
}
