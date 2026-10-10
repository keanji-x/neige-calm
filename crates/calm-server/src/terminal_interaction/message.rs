//! `message` (#2493): one bracketed paste of a fixed kernel header and the Planner's text, then
//! Enter, into a running task worker's agent TUI. Its writer is a kernel-private client that
//! writes as an Observer (`kernel_originated_input`), so it never claims or takes control; its
//! scope re-runs the message rule and the terminal checks when the writer admits the write. The
//! replay contract and the write leg are typed input's ([`super::write_leg`]). A worker takes a
//! message only from when its provider declares it ready ([`MessageReady`], #2532); typed keys
//! keep their own rule, since the worker watcher answers a startup screen with them.
use super::client::InputRole;
use super::target::{InputRefused, Resolved, refused};
use super::write_leg::{Admission, Delivered, WriteRule};
use super::*;
use crate::terminal_renderer::{RendererEntry, WriteShape};
use calm_exec::{MessageReady, TuiInput};

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

/// The message rule's declaration part: a task worker whose provider declares message delivery,
/// under the shared write rule ([`Resolved::write_refusal`]).
fn message_refusal(resolved: &Resolved, tui: TuiInput, providers: &str) -> Option<InputRefused> {
    if resolved.binding.task.is_none() || !tui.takes_message() {
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

/// The message rule, before the write and again when the writer admits it: the declaration part,
/// then readiness. A provider whose message waits for the task prompt's submission refuses until
/// its hooks reported one for this worker session; the persisted hook survives a restart.
async fn message_check(
    repo: &dyn RouteRepo,
    resolved: &Resolved,
    tui: TuiInput,
    providers: &str,
) -> Result<Option<InputRefused>> {
    if let Some(refusal) = message_refusal(resolved, tui, providers) {
        return Ok(Some(refusal));
    }
    if tui.message_ready() != Some(MessageReady::AfterPromptSubmitted) {
        return Ok(None);
    }
    let started = crate::routes::codex::worker_prompt_submitted(
        repo,
        resolved.provider,
        &resolved.binding.card_id,
        &resolved.binding.worker_session_id,
    )
    .await?;
    let attempt = resolved
        .binding
        .task
        .as_ref()
        .map_or("", |task| task.attempt_id.as_str());
    Ok((!started).then(|| refused("worker_starting", NOT_STARTED.replace("{attempt}", attempt))))
}

/// A running worker whose agent has not submitted its task prompt: Enter may answer a startup
/// screen instead (a folder-trust dialog's "No, exit", #2532).
const NOT_STARTED: &str = "attempt {attempt} is running, but its worker has not started its task \
     yet; its agent may still be on a startup screen, which the worker watcher handles. Read its \
     screen or wait, then send again";

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

impl TerminalInteraction {
    /// The providers whose workers take a message, for the `message_unsupported` text.
    fn message_providers(&self) -> String {
        self.providers
            .tui_inputs()
            .into_iter()
            .filter(|(_, tui)| tui.takes_message())
            .map(|(kind, _)| kind.as_db_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
    /// The message rule (a new write's check), from the provider's declaration.
    pub(super) async fn message_rule(&self, resolved: &Resolved) -> Result<()> {
        match message_check(
            self.repo.as_ref(),
            resolved,
            self.tui_input(resolved)?,
            &self.message_providers(),
        )
        .await?
        {
            Some(refusal) => Err(refusal.into()),
            None => Ok(()),
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
        let key = idempotency_key.to_owned();
        let fingerprint =
            crate::routes::idempotency_key::stable_payload_hash(&json!({"action":action}))?;
        // A replay returns its receipt before the message rule and the terminal check: the worker
        // may have reported or left paste mode since the original write.
        let admitted = match self
            .admit_write(
                identity,
                target,
                &key,
                &fingerprint,
                observation_wait.clone(),
                WriteRule::Message,
            )
            .await?
        {
            Admission::Replayed(replayed) => return Ok(replayed),
            Admission::New(admitted) => admitted,
        };
        let (resolved, client) = (&admitted.resolved, &admitted.client);
        Self::ensure_writable(client)?;
        let entry = match self.renderer.get(&resolved.binding.terminal_id) {
            Some(entry) if terminal_takes_paste(&entry) => entry,
            _ => return Err(refused("terminal_unreadable", TERMINAL_UNREADABLE.into()).into()),
        };
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
        let receipt = |outcome: &Delivered| {
            let mut receipt = json!({"terminal_id":resolved.binding.terminal_id,
                "idempotency_key":idempotency_key,"attempt_id":attempt,
                "application_result":"unverified"});
            match outcome {
                Delivered::Written => {
                    receipt["outcome"] = json!("written");
                    receipt["next"] = json!("read the application result");
                }
                Delivered::Refused(reason) => {
                    receipt["outcome"] = json!("refused");
                    receipt["reason"] = json!(reason);
                    receipt["next"] = json!("nothing was written; show and read again");
                }
                Delivered::Unknown => receipt["outcome"] = json!("unknown"),
            }
            receipt
        };
        let baseline = Self::current_baseline(client);
        let result = match self.deliver(identity, resolved, entry).await {
            Ok(writer) => {
                self.write_once(
                    client,
                    Some(writer),
                    &key,
                    &fingerprint,
                    bytes,
                    WriteShape::Verbatim,
                    receipt,
                )
                .await?
                .1
            }
            // Refused before a connection existed: nothing was reserved or cached.
            Err(reason) => receipt(&Delivered::Refused(reason)),
        };
        Ok(self
            .with_observation(identity, client, result, observation_wait, baseline)
            .await)
    }
    /// The write leg's writer: a kernel-private client that writes once as an Observer. Its scope
    /// admits the physical write only while the message rule and the terminal check still hold.
    /// An `Err` is a refusal proven before any input was sent (closed or refused before
    /// `ServerHello`).
    pub(crate) async fn deliver(
        &self,
        identity: &ToolCallIdentity,
        resolved: &Resolved,
        entry: Arc<RendererEntry>,
    ) -> Result<Arc<Client>, String> {
        let tui = self
            .tui_input(resolved)
            .map_err(|error| error.to_string())?;
        let scope = self.message_scope(identity, &resolved.binding, tui, entry.clone());
        let client = Client::attach(entry, scope, resolved.binding.clone(), InputRole::Kernel)
            .await
            .map_err(|error| error.to_string())?;
        #[cfg(feature = "fixtures")]
        self.run_message_write_seam(&resolved.binding.terminal_id)
            .await;
        Ok(Arc::new(client))
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
        let observe = Self::binding_check(self.repo.clone(), identity, binding, false);
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
                            matches!(
                                message_check(repo.as_ref(), &resolved, tui, &providers).await,
                                Ok(None)
                            ) && terminal_takes_paste(&entry)
                        }
                        Err(_) => false,
                    }
                }) as futures::future::BoxFuture<'static, bool>
            }) as ScopeCheck
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
