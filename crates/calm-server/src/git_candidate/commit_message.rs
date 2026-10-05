//! #2139 R1 — the message of the commit the kernel makes of a completed attempt's checkout.
//!
//! The worker may supply it with `neige_task_done`; the kernel only checks that the bytes can be
//! carried through argv, SQLite and `git commit`, never what the text says (trailers, subject
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

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which message a delivery's commit carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryMessage {
    /// No message was supplied (every non-completed attempt, a done report without one, every row
    /// written before migration 0145): the kernel writes its fixed one-line text.
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
