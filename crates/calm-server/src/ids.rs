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
//! Each newtype derives `#[serde(transparent)]` and `#[sqlx(transparent)]`.
//! The first guarantees the JSON wire shape stays a bare string
//! (`"abc123"`, not `{"0":"abc123"}`); the second makes `.bind()` /
//! `query_as` decode against a sqlite `TEXT` column without ceremony.
//! ts-rs picks them up via `#[ts(export)]` and emits the equivalent of
//! `export type CoveId = string;` so the frontend's generated TS keeps
//! working unchanged.
//!
//! ## `ActorId` has zero call sites in PR1
//!
//! It's declared here so PR2/PR3 can wire it into `EventScope` /
//! `enforce_role`. PR1 is pure refactor — the existing
//! `actor::Actor(pub String)` plumbing (which carries the declared
//! `X-Calm-Actor` value through the request stack) is untouched.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Cove identifier. UUID-shaped (32 hex, no dashes) in practice, but the
/// kernel treats the value as opaque; never parses it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, TS)]
#[serde(transparent)]
#[sqlx(transparent)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct CoveId(pub String);

/// Wave identifier. See [`CoveId`] for the opacity contract.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, TS)]
#[serde(transparent)]
#[sqlx(transparent)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct WaveId(pub String);

/// Card identifier. See [`CoveId`] for the opacity contract.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, TS)]
#[serde(transparent)]
#[sqlx(transparent)]
#[ts(export, export_to = "web/src/api/generated-events.ts")]
pub struct CardId(pub String);

/// Semantic identity of an event producer.
///
/// Declared in PR1 for downstream use (`EventScope` in PR2,
/// `enforce_role` in PR3). **Has zero call sites in PR1** — the existing
/// `crate::actor::Actor(pub String)` plumbing carries the declared
/// `X-Calm-Actor` value through the request stack and remains the
/// audit-log truth until PR3 swaps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
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

        // Unit-variant round-trip: serde adjacently-tagged enums encode
        // unit variants with just the `kind` discriminator (no `id`
        // payload). Round-trip is the contract we care about — exact
        // textual shape is locked in by the AiCodex case above.
        let u = ActorId::User;
        let back: ActorId = serde_json::from_str(&serde_json::to_string(&u).unwrap()).unwrap();
        assert_eq!(back, u);
    }
}
