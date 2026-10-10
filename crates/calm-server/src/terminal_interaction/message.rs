//! `message` (#2493): one bracketed paste of a fixed kernel header and the Planner's text, then
//! Enter, into a running task worker's agent TUI. The write leg is a kernel-private client that
//! writes as an Observer (`kernel_originated_input`), so it never claims or takes control; its
//! scope re-runs the message rule and the terminal checks when the writer admits the write.
use super::client::InputRole;
use super::operations::cache;
use super::target::{InputRefused, Resolved, refused};
use super::*;
use crate::terminal_renderer::{RendererEntry, WriteShape};
use calm_exec::{MessageDelivery, TuiInput};

/// Cap on the header, its newline and the text (the probe's 7,999-byte paste arrived byte-exact
/// on both TUIs).
pub const MESSAGE_BYTES_MAX: usize = 8000;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// The kernel's first line of every message: the inserted text never starts with the caller's
/// `/` or `!`, and it names the attempt it was written for.
pub fn message_header(attempt_id: &str) -> String {
    format!("[neige] Planner message for attempt {attempt_id}:")
}

/// Invalid message text or arguments: answered `-32602`.
#[derive(Debug)]
pub struct MessageInvalid(pub String);
impl std::fmt::Display for MessageInvalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for MessageInvalid {}

/// Non-empty, and no control character but newline and tab (ESC would end the paste early, CR
/// would submit inside it).
pub fn validate_message_text(text: &str) -> Result<(), MessageInvalid> {
    if text.is_empty() {
        return Err(MessageInvalid("message text is empty".into()));
    }
    match text
        .char_indices()
        .find(|(_, c)| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        Some((at, c)) => Err(MessageInvalid(format!(
            "U+{:04X} at byte {at}; only printable characters, newline and tab",
            c as u32
        ))),
        None => Ok(()),
    }
}

/// The one write of a message: `ESC[200~` header `\n` text `ESC[201~` `\r`.
pub fn encode_message(attempt_id: &str, text: &str) -> Result<Vec<u8>, MessageInvalid> {
    validate_message_text(text)?;
    let header = message_header(attempt_id);
    let body = header.len() + 1 + text.len();
    if body > MESSAGE_BYTES_MAX {
        return Err(MessageInvalid(format!(
            "message is {body} bytes with its header; the cap is {MESSAGE_BYTES_MAX}"
        )));
    }
    let mut bytes = Vec::with_capacity(body + PASTE_START.len() + PASTE_END.len() + 1);
    bytes.extend_from_slice(PASTE_START);
    bytes.extend_from_slice(header.as_bytes());
    bytes.push(b'\n');
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(PASTE_END);
    bytes.push(b'\r');
    Ok(bytes)
}

/// The message rule, before the write and again when the writer admits it: a task worker whose
/// provider declares message delivery, under the shared write rule ([`Resolved::write_refusal`]).
fn message_refusal(resolved: &Resolved, tui: TuiInput, providers: &str) -> Option<InputRefused> {
    if resolved.binding.task.is_none() || tui.message == MessageDelivery::Unsupported {
        return Some(refused(
            "message_unsupported",
            format!(
                "action \"message\" needs a task worker whose agent declares it ({providers}); \
                 terminal {} runs {}. Use \"text\" or \"submit\"",
                resolved.binding.terminal_id,
                resolved.provider.as_db_str()
            ),
        ));
    }
    resolved.write_refusal()
}

/// Readable (the model view captures, as `observable`), not exited, and bracketed paste on: what
/// the write needs from the terminal.
fn terminal_takes_paste(entry: &RendererEntry) -> bool {
    let exited = entry.exit.lock().map_or(true, |exit| exit.is_some());
    !exited
        && entry.handle.model_view.lock().is_ok_and(|view| {
            view.capture(0)
                .is_ok_and(|(frame, _)| frame.input_surface().bracketed_paste())
        })
}

const TERMINAL_UNREADABLE: &str = "the worker's terminal has no live readable view (after a server \
     restart until reattached, #2499), or bracketed paste is off; nothing was sent";

/// How the one write ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Delivered {
    /// Proven before any byte reached the PTY.
    Refused(String),
    Written,
    /// The bytes may have reached the PTY; the acknowledgement was lost.
    Unknown,
}

impl TerminalInteraction {
    /// The providers whose workers take a message, for the `message_unsupported` text.
    fn message_providers(&self) -> String {
        self.providers
            .tui_inputs()
            .into_iter()
            .filter(|(_, tui)| tui.message != MessageDelivery::Unsupported)
            .map(|(kind, _)| kind.as_db_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
    /// The message rule and the terminal check, decided before any connection or byte.
    fn message_precheck(&self, resolved: &Resolved) -> Result<Arc<RendererEntry>> {
        if let Some(refusal) = message_refusal(
            resolved,
            self.tui_input(resolved)?,
            &self.message_providers(),
        ) {
            return Err(refusal.into());
        }
        match self.renderer.get(&resolved.binding.terminal_id) {
            Some(entry) if terminal_takes_paste(&entry) => Ok(entry),
            _ => Err(refused("terminal_unreadable", TERMINAL_UNREADABLE.into()).into()),
        }
    }
    /// `neige_terminal_input` action `message`: the caller's observation client keeps the
    /// idempotency cache and takes the readback; [`Self::deliver`] is the write leg.
    pub async fn message(
        &self,
        identity: &ToolCallIdentity,
        target: &Target,
        idempotency_key: &str,
        action: Value,
        observation_wait: Option<WaitPlan>,
    ) -> Result<Value> {
        if let Some(wait) = &observation_wait {
            wait.validate()?;
        }
        ensure!(
            !idempotency_key.is_empty() && idempotency_key.len() <= 128,
            "invalid input idempotency_key"
        );
        let text = match action.as_object() {
            Some(object) if object.len() == 2 && object.contains_key("text") => {
                action["text"].as_str()
            }
            _ => None,
        }
        .ok_or_else(|| MessageInvalid("message action accepts only type/text".into()))?;
        validate_message_text(text)?;
        if identity.role != CardRole::Planner {
            return Err(refused(
                "assistant_no_message",
                "action \"message\" is the Planner's; its header names the Planner".into(),
            )
            .into());
        }
        let resolved = Self::resolve_target(self.repo.as_ref(), identity, target).await?;
        if self.renderer.get(&resolved.binding.terminal_id).is_none() {
            // No live entry, so no connection and no receipt to replay: refused as new.
            self.message_precheck(&resolved)?;
            anyhow::bail!("terminal unavailable");
        }
        let client = self.client(identity, &resolved.binding).await?;
        let _serial = {
            let _queued = client.queued_for_serial();
            client.serial.lock().await
        };
        let resolved =
            Self::check_binding(self.repo.as_ref(), identity, &resolved.binding, false).await?;
        let key = idempotency_key.to_owned();
        let fingerprint =
            crate::routes::idempotency_key::stable_payload_hash(&json!({"action":action}))?;
        // A replay returns its receipt before any new-write check: the worker may have reported or
        // left paste mode since the original write.
        if let Some(replayed) = self
            .replay(
                identity,
                &client,
                &key,
                &fingerprint,
                observation_wait.clone(),
            )
            .await?
        {
            return Ok(replayed);
        }
        let entry = self.message_precheck(&resolved)?;
        ensure!(
            Arc::ptr_eq(&entry, &client.entry),
            "terminal generation changed; read again"
        );
        let attempt = resolved
            .binding
            .task
            .as_ref()
            .map(|task| task.attempt_id.clone())
            .ok_or_else(|| anyhow::anyhow!("message target is bound to no task"))?;
        let bytes = encode_message(&attempt, text)?;
        let receipt = |outcome: &str| {
            json!({"terminal_id":resolved.binding.terminal_id,"idempotency_key":idempotency_key,
                "attempt_id":attempt,"outcome":outcome,"application_result":"unverified"})
        };
        let baseline = Self::current_baseline(&client);
        // Cached before the write: a call cancelled mid-write replays as unknown, never writes again.
        cache(&client, &key, &fingerprint, &receipt("unknown")).await;
        let result = match self.deliver(identity, &resolved, entry, bytes).await {
            Delivered::Written => {
                let mut written = receipt("written");
                written["next"] = json!("read the application result");
                written
            }
            Delivered::Refused(reason) => {
                let mut refused = receipt("refused");
                refused["reason"] = json!(reason);
                refused["next"] = json!("nothing was written; show and read again");
                refused
            }
            Delivered::Unknown => receipt("unknown"),
        };
        cache(&client, &key, &fingerprint, &result).await;
        Ok(self
            .with_observation(identity, &client, result, observation_wait, baseline)
            .await)
    }
    /// The write leg: a kernel-private client that writes `bytes` once as an Observer. Its scope
    /// admits the physical write only while the message rule and the terminal check still hold.
    pub(crate) async fn deliver(
        &self,
        identity: &ToolCallIdentity,
        resolved: &Resolved,
        entry: Arc<RendererEntry>,
        bytes: Vec<u8>,
    ) -> Delivered {
        let tui = match self.tui_input(resolved) {
            Ok(tui) => tui,
            Err(error) => return Delivered::Refused(error.to_string()),
        };
        let scope = self.message_scope(identity, &resolved.binding, tui, entry.clone());
        let client =
            match Client::attach(entry, scope, resolved.binding.clone(), InputRole::Kernel).await {
                Ok(client) => client,
                // Closed or refused before `ServerHello`: no input was ever sent.
                Err(error) => return Delivered::Refused(error.to_string()),
            };
        #[cfg(feature = "fixtures")]
        self.run_message_write_seam(&resolved.binding.terminal_id)
            .await;
        let sequence = 1;
        if let Ok(mut state) = client.screen.lock() {
            state.pending = Some(sequence);
        }
        if client
            .send_input(bytes, sequence, WriteShape::Verbatim)
            .await
            .is_err()
        {
            // The pump never received the input.
            return Delivered::Refused("terminal disconnected before the write".into());
        }
        let settled = client
            .wait(
                |state| state.ack >= sequence || state.refused >= sequence,
                Duration::from_secs(7),
            )
            .await;
        let Ok(state) = client.screen.lock() else {
            return Delivered::Unknown;
        };
        match settled {
            Ok(()) if state.ack >= sequence => Delivered::Written,
            Ok(()) => Delivered::Refused(
                state
                    .last_protocol_error
                    .clone()
                    .unwrap_or_else(|| "terminal input refused".into()),
            ),
            Err(_) => Delivered::Unknown,
        }
    }
    /// Observe: the caller's binding. Control (each physical write): the binding, the message
    /// rule and the terminal check, read fresh.
    fn message_scope(
        &self,
        identity: &ToolCallIdentity,
        binding: &Binding,
        tui: TuiInput,
        entry: Arc<RendererEntry>,
    ) -> ClientInputScope {
        let observe = {
            let repo = self.repo.clone();
            let actor = identity.clone();
            let expected = binding.clone();
            Arc::new(move || {
                let (repo, actor, expected) = (repo.clone(), actor.clone(), expected.clone());
                Box::pin(async move {
                    Self::check_binding(repo.as_ref(), &actor, &expected, false)
                        .await
                        .is_ok()
                }) as futures::future::BoxFuture<'static, bool>
            })
                as Arc<dyn Fn() -> futures::future::BoxFuture<'static, bool> + Send + Sync>
        };
        let control = {
            let repo = self.repo.clone();
            let actor = identity.clone();
            let expected = binding.clone();
            let providers = self.message_providers();
            Arc::new(move || {
                let (repo, actor, expected) = (repo.clone(), actor.clone(), expected.clone());
                let (entry, providers) = (entry.clone(), providers.clone());
                Box::pin(async move {
                    match Self::check_binding(repo.as_ref(), &actor, &expected, false).await {
                        Ok(resolved) => {
                            message_refusal(&resolved, tui, &providers).is_none()
                                && terminal_takes_paste(&entry)
                        }
                        Err(_) => false,
                    }
                }) as futures::future::BoxFuture<'static, bool>
            })
                as Arc<dyn Fn() -> futures::future::BoxFuture<'static, bool> + Send + Sync>
        };
        ClientInputScope::Bound { observe, control }
    }
    /// Test seam: runs in [`Self::deliver`] after the kernel client's handshake and before its
    /// input is sent, given the terminal id. Consumed once.
    #[cfg(feature = "fixtures")]
    #[doc(hidden)]
    pub fn set_message_write_seam(&self, seam: ClaimWindowSeam) {
        *self
            .message_write_seam
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(seam);
    }
    #[cfg(feature = "fixtures")]
    async fn run_message_write_seam(&self, terminal_id: &str) {
        let seam = self
            .message_write_seam
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(seam) = seam {
            seam(terminal_id.to_owned()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_encodes_one_bracketed_paste_with_the_kernel_header_and_enter() {
        assert_eq!(
            encode_message("track:k1", "fix the test\n\tthen gate").unwrap(),
            b"\x1b[200~[neige] Planner message for attempt track:k1:\nfix the test\n\tthen gate\x1b[201~\r"
                .to_vec()
        );
    }

    #[test]
    fn message_text_admits_newline_and_tab_only_among_controls() {
        for ok in [
            "a",
            "line\nline",
            "col\tcol",
            "/clear",
            "!rm -rf",
            "ünïcode ✓",
        ] {
            assert!(validate_message_text(ok).is_ok(), "{ok:?}");
        }
        assert_eq!(
            validate_message_text("").unwrap_err().0,
            "message text is empty"
        );
        for (text, at, code) in [
            ("x\x1b[201~y", 1, 0x1b),
            ("a\rb", 1, 0x0d),
            ("\x03", 0, 0x03),
            ("ok\x7f", 2, 0x7f),
            ("é\u{85}", 2, 0x85),
        ] {
            assert_eq!(
                validate_message_text(text).unwrap_err().0,
                format!("U+{code:04X} at byte {at}; only printable characters, newline and tab"),
                "{text:?}"
            );
        }
    }

    #[test]
    fn message_cap_counts_the_header() {
        let attempt = "t:k";
        let room = MESSAGE_BYTES_MAX - message_header(attempt).len() - 1;
        assert!(encode_message(attempt, &"x".repeat(room)).is_ok());
        assert_eq!(
            encode_message(attempt, &"x".repeat(room + 1))
                .unwrap_err()
                .0,
            format!("message is 8001 bytes with its header; the cap is {MESSAGE_BYTES_MAX}")
        );
    }
}
