//! #2139 R1 — the message of the commit the kernel makes of a completed attempt's checkout.
//!
//! The worker supplies it with a gate run or `neige_task_done`; the kernel checks that the bytes
//! can be carried through argv, SQLite and `git commit`, never what the text says (trailers, subject
//! form and sign-offs are the repository's policy, judged by its own CI).

/// The most bytes (UTF-8) a worker-supplied commit message may have; migration 0145's CHECK
/// carries the same bound.
pub const COMMIT_MESSAGE_MAX_BYTES: usize = 16_384;

/// A worker-supplied commit message that passed [`CommitMessage::parse`]: non-blank, at most
/// [`COMMIT_MESSAGE_MAX_BYTES`], no NUL and no control character other than tab, LF and CR. The
/// text is kept verbatim; git's own `-m` cleanup normalises whitespace at commit time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitMessage(String);

impl CommitMessage {
    /// Parse the optional tool argument without assigning policy to an omitted message.
    pub fn from_tool_arg(args: &serde_json::Value) -> Result<Option<Self>, String> {
        match args.get("commit_message") {
            None => Ok(None),
            Some(serde_json::Value::String(text)) => Self::parse(text).map(Some),
            Some(_) => Err("commit_message must be a string".into()),
        }
    }

    /// The one validator, for the tool argument and for the stored row alike. `Err` names the
    /// rule the text breaks.
    pub fn parse(text: &str) -> Result<Self, String> {
        for c in text.chars() {
            if c == '\0' {
                return Err("commit_message has a NUL byte".into());
            }
            if c.is_ascii_control() && !matches!(c, '\t' | '\n' | '\r') {
                return Err(format!(
                    "commit_message has the control character U+{:04X}",
                    c as u32
                ));
            }
        }
        if text.trim().is_empty() {
            return Err("commit_message is empty".into());
        }
        if text.len() > COMMIT_MESSAGE_MAX_BYTES {
            return Err(format!(
                "commit_message is {} bytes; the limit is {COMMIT_MESSAGE_MAX_BYTES}",
                text.len()
            ));
        }
        Ok(Self(text.to_string()))
    }

    /// Equality after git's default `-m` whitespace cleanup (comments are retained).
    pub(crate) fn same_commit_text(&self, other: &Self) -> bool {
        fn cleaned(text: &str) -> String {
            let lines: Vec<_> = text
                .lines()
                .map(|line| line.trim_end_matches([' ', '\t', '\r']))
                .collect();
            let mut result = Vec::new();
            for line in lines {
                if line.is_empty()
                    && result
                        .last()
                        .is_none_or(|previous: &&str| previous.is_empty())
                {
                    continue;
                }
                result.push(line);
            }
            while result.last().is_some_and(|line| line.is_empty()) {
                result.pop();
            }
            result.join("\n")
        }
        cleaned(self.as_str()) == cleaned(other.as_str())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which message a delivery's commit carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryMessage {
    /// A non-completed outcome uses the kernel's fixed one-line text. Historical rows may
    /// also decode to this variant; a completed delivery cannot be created without a message.
    Kernel,
    /// The completed attempt's worker supplied this message; it is committed verbatim.
    Worker(CommitMessage),
}

impl DeliveryMessage {
    /// The `task_git_deliveries.commit_message` column value.
    pub(crate) fn as_column(&self) -> Option<&str> {
        match self {
            DeliveryMessage::Kernel => None,
            DeliveryMessage::Worker(message) => Some(message.as_str()),
        }
    }
}

/// The attempt's durable message: its delivery, otherwise its latest checkpointed run.
/// The run owner decodes its typed payload and commit evidence under the caller's write lock.
pub(crate) async fn attempt_commit_message_tx(
    tx: &mut crate::operation::Tx<'_>,
    attempt_id: &str,
) -> crate::error::Result<Option<CommitMessage>> {
    let text: Option<String> = sqlx::query_scalar(
        "SELECT commit_message FROM task_git_deliveries \
         WHERE producer_attempt_id = ?1 AND commit_message IS NOT NULL \
         ORDER BY ordinal DESC LIMIT 1",
    )
    .bind(attempt_id)
    .fetch_optional(&mut **tx)
    .await?;
    match text {
        Some(text) => CommitMessage::parse(&text)
            .map(Some)
            .map_err(crate::error::CalmError::Internal),
        None => crate::operation::task_gate_run::checkpointed_message_tx(tx, attempt_id).await,
    }
}
