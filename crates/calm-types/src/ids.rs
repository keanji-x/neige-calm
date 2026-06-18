//! Typed identifier newtypes — `CoveId` / `WaveId` / `CardId` — plus the
//! `ActorId` semantic enum.
//!
//! ## Why typed ids
//!
//! Pre-#136 the kernel passed bare `String`s everywhere. That made it
//! mechanically impossible to tell a wave id from a card id at the type
//! level, and every new write-path layer had to re-document the implicit
//! contract. The typed newtypes here are the foundation for the
//! "Wave-as-Actor" chain (#136): later PRs introduce `EventScope`,
//! `ActorId::AiSpec(CardId)`, the dispatcher, and the role gate — all of
//! which need the compiler to enforce "this is a card id, not a wave id".
//!
//! ## Wire/storage compatibility
//!
//! Each newtype derives `#[serde(transparent)]`, which guarantees the JSON
//! wire shape stays a bare string (`"abc123"`, not `{"0":"abc123"}`).
//! ts-rs picks them up via `#[ts(export)]` and emits the equivalent of
//! `export type CoveId = string;` so the frontend's generated TS keeps
//! working unchanged.
//!
//! ## #679 PR1 note — no sqlx here
//!
//! These types used to also derive `#[sqlx(transparent)]` while they lived
//! in calm-server. calm-types is sqlx-free by design (compile firewall), so
//! DB binds use `.as_str()` and row decodes go through calm-server's
//! `db::rows` wrappers (`#[sqlx(try_from = "String")]` + the blanket
//! `TryFrom<String>` each newtype gets via its `From<String>` impl). The
//! stored TEXT shape is unchanged.
//!
//! ## #679 PR1 note — `ActorId` is frozen
//!
//! `ActorId` is card-shaped (`AiSpec(CardId)` / `AiCodex(CardId)` /
//! `AiClaude(CardId)`) and lives in the persisted event log. Evolving it to
//! session identity is issue #679 hard-problem 1, owned by #770 (HP1). PR1
//! moves the definition verbatim — the TS-bindings byte gate pins the shape.
//!
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utoipa::ToSchema;

use crate::worker::WorkerSessionId;

/// Cove identifier. UUID-shaped (32 hex, no dashes) in practice, but the
/// kernel treats the value as opaque; never parses it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, TS)]
#[serde(transparent)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct CoveId(pub String);

/// Wave identifier. See [`CoveId`] for the opacity contract.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, TS)]
#[serde(transparent)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct WaveId(pub String);

/// Card identifier. See [`CoveId`] for the opacity contract.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, TS)]
#[serde(transparent)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct CardId(pub String);

/// Semantic identity of an event producer.
///
/// Declared in PR1 for downstream use (`EventScope` in PR2,
/// `enforce_role` in PR3). **Has zero call sites in PR1** — the existing
/// `crate::actor::Actor(pub String)` plumbing carries the declared
/// `X-Calm-Actor` value through the request stack and remains the
/// audit-log truth until PR3 swaps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, TS)]
#[serde(tag = "kind", content = "id")]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub enum ActorId {
    User,
    Kernel,
    KernelDispatcher,
    Plugin(String),
    AiSpec(CardId),
    AiCodex(CardId),
    AiClaude(CardId),
    #[schema(value_type = String)]
    AiSpecSession(WorkerSessionId),
    #[schema(value_type = String)]
    AiCodexSession(WorkerSessionId),
    #[schema(value_type = String)]
    AiClaudeSession(WorkerSessionId),
}

impl std::fmt::Display for ActorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => f.write_str("user"),
            Self::Kernel => f.write_str("kernel"),
            Self::KernelDispatcher => f.write_str("kernel-dispatcher"),
            Self::Plugin(id) => write!(f, "plugin:{id}"),
            Self::AiSpec(id) if id.as_str().is_empty() => f.write_str("ai:spec"),
            Self::AiSpec(id) => write!(f, "ai:spec:{}", id.as_str()),
            Self::AiCodex(id) if id.as_str().is_empty() => f.write_str("ai:codex"),
            Self::AiCodex(id) => write!(f, "ai:codex:{}", id.as_str()),
            Self::AiClaude(id) if id.as_str().is_empty() => f.write_str("ai:claude"),
            Self::AiClaude(id) => write!(f, "ai:claude:{}", id.as_str()),
            Self::AiSpecSession(id) if id.as_str().is_empty() => f.write_str("ai:spec-session"),
            Self::AiSpecSession(id) => write!(f, "ai:spec-session:{}", id.as_str()),
            Self::AiCodexSession(id) if id.as_str().is_empty() => f.write_str("ai:codex-session"),
            Self::AiCodexSession(id) => write!(f, "ai:codex-session:{}", id.as_str()),
            Self::AiClaudeSession(id) if id.as_str().is_empty() => f.write_str("ai:claude-session"),
            Self::AiClaudeSession(id) => write!(f, "ai:claude-session:{}", id.as_str()),
        }
    }
}

// ---------------------------------------------------------------------------
// Boilerplate conversions for each newtype. We generate the From / Display /
// AsRef impls via a tiny macro so adding a future id (e.g. PluginId in some
// later wave) is one `newtype_id_impls!(PluginId);` line.
// ---------------------------------------------------------------------------

macro_rules! newtype_id_impls {
    ($Ty:ident) => {
        impl $Ty {
            /// Borrow the underlying string slice. Convenience over the
            /// `AsRef<str>` impl for sites that already type the variable
            /// as the newtype.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $Ty {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $Ty {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }

        impl std::fmt::Display for $Ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }

        impl AsRef<str> for $Ty {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

newtype_id_impls!(CoveId);
newtype_id_impls!(WaveId);
newtype_id_impls!(CardId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shape_is_transparent_string() {
        // The whole point of `#[serde(transparent)]` is that the wire shape
        // stays a bare string — no `{"0": "..."}` wrapper from the tuple
        // struct's default Serialize. The frontend's generated TS treats
        // these as string aliases; a change here would break the wire
        // contract silently.
        let id = CardId("abc123".to_string());
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""abc123""#);
        let back: CardId = serde_json::from_str(r#""abc123""#).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn from_str_and_string_round_trip() {
        let a = WaveId::from("w-1");
        let b: WaveId = "w-1".to_string().into();
        assert_eq!(a, b);
        assert_eq!(a.as_ref(), "w-1");
        assert_eq!(format!("{a}"), "w-1");
    }

    #[test]
    fn actor_id_tagged_serialization() {
        // `#[serde(tag = "kind", content = "id")]` is what later PRs will
        // pin the audit-log shape against. Lock the encoding down here so a
        // future serde attribute change can't silently break the wire.
        let a = ActorId::AiCodex(CardId::from("card-7"));
        let s = serde_json::to_string(&a).unwrap();
        assert_eq!(s, r#"{"kind":"AiCodex","id":"card-7"}"#);

        let claude = ActorId::AiClaude(CardId::from("card-8"));
        let s = serde_json::to_string(&claude).unwrap();
        assert_eq!(s, r#"{"kind":"AiClaude","id":"card-8"}"#);
        let back: ActorId = serde_json::from_str(&s).unwrap();
        assert_eq!(back, claude);

        let codex_session = ActorId::AiCodexSession(WorkerSessionId::from("sess-9"));
        let s = serde_json::to_string(&codex_session).unwrap();
        assert_eq!(s, r#"{"kind":"AiCodexSession","id":"sess-9"}"#);
        let back: ActorId = serde_json::from_str(&s).unwrap();
        assert_eq!(back, codex_session);

        let spec_session = ActorId::AiSpecSession(WorkerSessionId::from("sess-9"));
        let s = serde_json::to_string(&spec_session).unwrap();
        assert_eq!(s, r#"{"kind":"AiSpecSession","id":"sess-9"}"#);
        let back: ActorId = serde_json::from_str(&s).unwrap();
        assert_eq!(back, spec_session);

        let claude_session = ActorId::AiClaudeSession(WorkerSessionId::from("sess-9"));
        let s = serde_json::to_string(&claude_session).unwrap();
        assert_eq!(s, r#"{"kind":"AiClaudeSession","id":"sess-9"}"#);
        let back: ActorId = serde_json::from_str(&s).unwrap();
        assert_eq!(back, claude_session);

        // Unit-variant round-trip: serde adjacently-tagged enums encode
        // unit variants with just the `kind` discriminator (no `id`
        // payload). Round-trip is the contract we care about — exact
        // textual shape is locked in by the AiCodex case above.
        let u = ActorId::User;
        let back: ActorId = serde_json::from_str(&serde_json::to_string(&u).unwrap()).unwrap();
        assert_eq!(back, u);
    }

    #[test]
    fn actor_id_display_is_commit_author_label() {
        assert_eq!(ActorId::User.to_string(), "user");
        assert_eq!(ActorId::Kernel.to_string(), "kernel");
        assert_eq!(
            ActorId::AiSpec(CardId::from("card-7")).to_string(),
            "ai:spec:card-7"
        );
        assert_eq!(ActorId::AiCodex(CardId::from("")).to_string(), "ai:codex");
        assert_eq!(
            ActorId::AiSpecSession(WorkerSessionId::from("sess-7")).to_string(),
            "ai:spec-session:sess-7"
        );
        assert_eq!(
            ActorId::AiCodexSession(WorkerSessionId::from("sess-8")).to_string(),
            "ai:codex-session:sess-8"
        );
        assert_eq!(
            ActorId::AiClaudeSession(WorkerSessionId::from("")).to_string(),
            "ai:claude-session"
        );
    }

    #[test]
    fn actor_id_decodes_both_card_and_session_forms() {
        let card: ActorId = serde_json::from_str(r#"{"kind":"AiCodex","id":"card-7"}"#).unwrap();
        assert_eq!(card, ActorId::AiCodex(CardId::from("card-7")));

        let session: ActorId =
            serde_json::from_str(r#"{"kind":"AiCodexSession","id":"sess-7"}"#).unwrap();
        assert_eq!(
            session,
            ActorId::AiCodexSession(WorkerSessionId::from("sess-7"))
        );
    }
}
