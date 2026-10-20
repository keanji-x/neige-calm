//! The standard: the parts of an objective a human approves, and its approval.
//!
//! The standard is the key, mode, checks (roles, kinds, rubrics) and constraints. Title,
//! budget and compact policy are left out, so changing them keeps an approval.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::definition::{CheckDefinition, Mode, ObjectiveDefinition};
use crate::ids::ObjectiveKey;

/// Version of the canonical form below; bump it when that form changes.
const CANONICAL_VERSION: u32 = 1;

/// Stable hash of an objective's standard, `sha256:` followed by lowercase hex.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StandardHash(String);

impl StandardHash {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The canonical form. It is serialized as a struct, so field order is declaration
/// order and does not depend on how JSON maps are ordered.
#[derive(Serialize)]
struct Canonical<'a> {
    version: u32,
    key: &'a ObjectiveKey,
    mode: &'a Mode,
    checks: &'a [CheckDefinition],
    constraints: &'a [String],
}

pub fn standard_hash(objective: &ObjectiveDefinition) -> StandardHash {
    let canonical = Canonical {
        version: CANONICAL_VERSION,
        key: &objective.key,
        mode: &objective.mode,
        checks: &objective.checks,
        constraints: &objective.constraints,
    };
    let bytes = serde_json::to_vec(&canonical).expect("the canonical standard serializes");
    let digest = Sha256::digest(&bytes);
    let mut hex = String::with_capacity(7 + digest.len() * 2);
    hex.push_str("sha256:");
    for byte in digest {
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    StandardHash(hex)
}

/// A human's approval of one exact standard.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub standard_hash: StandardHash,
    /// Who approved, as a reference the host resolves.
    pub approver: String,
}

impl Approval {
    /// Valid only while the objective's standard still hashes to the approved hash.
    pub fn is_valid_for(&self, objective: &ObjectiveDefinition) -> bool {
        self.standard_hash == standard_hash(objective)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{CheckConfig, CommandParams};
    use crate::definition::{Budget, CompactPolicy, Role};
    use crate::ids::CheckId;

    fn objective() -> ObjectiveDefinition {
        ObjectiveDefinition {
            key: ObjectiveKey::new("tests-green"),
            title: "Tests pass".into(),
            mode: Mode::Converge,
            checks: vec![CheckDefinition {
                id: CheckId::new("tests"),
                role: Role::Goal,
                config: CheckConfig::Command(CommandParams {
                    command: "cargo test".into(),
                }),
            }],
            constraints: vec!["keep the public API".into()],
            budget: Budget {
                turns: 4,
                tokens: 10_000,
                wall_secs: 600,
            },
            compact: CompactPolicy::OnClose,
        }
    }

    #[test]
    fn hash_of_a_fixed_standard_is_pinned() {
        // Approvals are stored by hash, so the canonical form must not drift silently. This is
        // the SHA-256 of {"version":1,"key":"tests-green","mode":"converge","checks":[{"id":
        // "tests","role":"goal","config":{"kind":"command","params":{"command":"cargo test"}}}],
        // "constraints":["keep the public API"]} without the line breaks.
        assert_eq!(
            standard_hash(&objective()).as_str(),
            "sha256:7ef26d1f474fc45faa7ccc63b97235c9b40fdbb30d9645486006da5f6384b840"
        );
    }

    #[test]
    fn budget_title_and_compact_policy_are_outside_the_standard() {
        let base = standard_hash(&objective());
        let mut edited = objective();
        edited.title = "Renamed".into();
        edited.budget.turns = 99;
        edited.compact = CompactPolicy::EverySettle;
        assert_eq!(standard_hash(&edited), base);
    }

    #[test]
    fn every_standard_part_changes_the_hash() {
        let base = standard_hash(&objective());
        let edits: Vec<fn(&mut ObjectiveDefinition)> = vec![
            |o| o.key = ObjectiveKey::new("other"),
            |o| o.mode = Mode::Loop { plateau: 2 },
            |o| o.checks[0].role = Role::Ratchet,
            |o| o.checks[0].id = CheckId::new("renamed"),
            |o| {
                o.checks[0].config = CheckConfig::Command(CommandParams {
                    command: "cargo nextest run".into(),
                })
            },
            |o| o.constraints.push("no unsafe".into()),
        ];
        for edit in edits {
            let mut changed = objective();
            edit(&mut changed);
            assert_ne!(standard_hash(&changed), base);
        }
    }

    #[test]
    fn approval_is_valid_only_for_the_approved_standard() {
        let approval = Approval {
            standard_hash: standard_hash(&objective()),
            approver: "user:1".into(),
        };
        let mut objective = objective();
        assert!(approval.is_valid_for(&objective));
        objective.budget.tokens = 1;
        assert!(approval.is_valid_for(&objective));
        objective.constraints.clear();
        assert!(!approval.is_valid_for(&objective));
    }
}
