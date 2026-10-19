//! Opaque identifiers. Each is a distinct type so an objective key cannot be passed
//! where a check id or a revision is expected.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! name_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

name_type!(
    /// Stable key of one objective, unique within a [`crate::engine::State`].
    ObjectiveKey
);
name_type!(
    /// Id of one check, unique within its objective.
    CheckId
);
name_type!(
    /// A workspace revision the host can evaluate, such as a commit SHA.
    Revision
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_a_bare_string() {
        let key = ObjectiveKey::new("latency");
        assert_eq!(serde_json::to_string(&key).unwrap(), "\"latency\"");
        let back: ObjectiveKey = serde_json::from_str("\"latency\"").unwrap();
        assert_eq!(back, key);
        assert_eq!(key.to_string(), "latency");
    }
}
