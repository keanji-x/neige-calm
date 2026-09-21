//! Verification-target vocabulary (#1727 S4 D3): what a task-verify gate verdict was checked
//! against. One serialization is shared by the `task.gate_result` event, the persisted
//! `gate_result_json` wrapper and the Planner read surface (`verification.target`, D8).
//!
//! This module defines the shapes only. The sampler that fills `Sample`, the admission and
//! prepare-time checks and `finalize` live in the kernel (`task_verify_adapter/target.rs`).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What the gate verdict was checked against. Every candidate-bound verdict is exactly one row
/// of the D3 producer × variant table; a verdict recorded before slice 4 reads as `Unbound`.
///
/// `Candidate` carries two checkout samples inline (`clippy::large_enum_variant`): the value is
/// a wire shape built once per verdict and matched by field, and boxing `evidence` would force a
/// nested match on every reader; the persisted observation boxes the whole target instead.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum VerifyTarget {
    /// The attempt is candidate-bound; `evidence` says how the checkout was compared to the candidate.
    Candidate {
        candidate_id: String,
        commit_sha: String,
        lease_id: String,
        evidence: VerifyTargetEvidence,
    },
    /// The attempt is candidate-bound but the gate was admitted before a candidate existed (A10b).
    NoCandidate { reason: NoCandidateReason },
    /// The gate checked nothing against a candidate: legacy lease, pre-upgrade freeze or verdict, terminal task.
    Unbound { reason: UnboundReason },
}

/// Why a gate ran without a candidate to check against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum UnboundReason {
    /// The lease predates kernel delivery (`delivery_policy IS NULL`).
    LegacyLease,
    /// The task-verify op was frozen before slice 4 (`FrozenVerify.target` absent).
    LegacyFrozen,
    /// The verdict was persisted before slice 4 (`gate_result_json.target` absent).
    LegacyVerdict,
    /// Terminal tasks never bind a candidate.
    Terminal,
}

/// The delivery state the gate found instead of a candidate. Each variant carries only facts
/// that exist: no `candidate_id` / `commit_sha` is minted for a delivery that produced none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum NoCandidateReason {
    DeliveryPending { delivery_id: String },
    DeliveryFailed { delivery_id: String },
    DeliveryAbandoned { delivery_id: String },
    NoDeliveryRow,
}

/// How the checkout was compared to the candidate (D3.0): refused before any step ran, verified
/// before and after the steps, or not sampled at all (a sampling command failed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum VerifyTargetEvidence {
    /// The prepare-time sample mismatched; `reasons` is non-empty and no step ran.
    Refused {
        cwd: String,
        before: Sample,
        reasons: Vec<MismatchReason>,
    },
    /// Both samples were taken; `reasons` is empty when the verdict stands and non-empty when
    /// the checkout changed during the gate and every step result is discarded.
    Verified {
        cwd: String,
        before: Sample,
        after: Sample,
        reasons: Vec<MismatchReason>,
    },
    /// A sampling command failed; not a mismatch (unable to check is not a failed check).
    Unsampled { phase: SamplePhase },
}

/// Where sampling failed. `cwd` is absent only in `Prepare` (nothing was frozen yet); the other
/// three phases carry the frozen cwd.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum SamplePhase {
    Prepare { reason: String },
    Finalize { cwd: String, reason: String },
    Compensation { cwd: String, reason: String },
    Reconciliation { cwd: String, last_error: String },
}

/// One D3.0 sample of a checkout: HEAD, the porcelain status lines, and the lease-provenance observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct Sample {
    pub head: String,
    pub dirty: Vec<String>,
    pub provenance: ProvenanceSample,
}

/// The observation line `GIT_LEASE_PROVENANCE_SCRIPT` prints to stderr:
/// `provenance realpath=<path> common_dir=<path> registered=<0|1>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ProvenanceSample {
    pub realpath: String,
    pub common_dir: String,
    pub registered: bool,
}

impl ProvenanceSample {
    /// The prefix every observation record starts with; git's own `warning:` / `fatal:` lines
    /// never do.
    const OBSERVATION_PREFIX: &str = "provenance realpath=";

    /// Parse the script's observation line. Paths may contain spaces, so the line is split on
    /// the ` registered=` and then the ` common_dir=` marker; `registered` must be exactly `0`
    /// or `1`. A line carrying either marker more than once is ambiguous (a path that contains
    /// the marker — inherent to the printf format) and yields `None` rather than invented
    /// paths: the verdict comes from the exit code, the evidence may be absent but never made up.
    /// Anything else is not an observation and yields `None`.
    pub fn parse_line(line: &str) -> Option<Self> {
        let rest = line
            .trim_end_matches(['\n', '\r'])
            .strip_prefix(Self::OBSERVATION_PREFIX)?;
        if rest.matches(" common_dir=").count() != 1 || rest.matches(" registered=").count() != 1 {
            return None;
        }
        let (paths, registered) = rest.rsplit_once(" registered=")?;
        let registered = match registered {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        let (realpath, common_dir) = paths.rsplit_once(" common_dir=")?;
        Some(Self {
            realpath: realpath.to_owned(),
            common_dir: common_dir.to_owned(),
            registered,
        })
    }

    /// Pick the observation out of the script's stderr. Premise: one invocation of
    /// `GIT_LEASE_PROVENANCE_SCRIPT` prints exactly one record to stderr, writes nothing to
    /// stderr after it, and git's own `warning:` / `fatal:` lines never start with
    /// `provenance realpath=`. So exactly one line carrying that prefix is expected, it must be
    /// the last physical line of stderr, and only then is it handed to `parse_line`; zero such
    /// lines, more than one, or a prefixed line followed by anything yield `None` (fail-closed).
    /// Those are what a path with an embedded newline produces — the printf record breaks into
    /// several physical lines: the tail can spell a second, forged record, or the head can be a
    /// prefixed line that parses on its own with the real `registered=` value pushed onto the
    /// line after it — so neither the last prefixed line nor a prefixed line that is not the last
    /// line must be trusted. PR-B's sampler feeds stderr only — the stdout copy printed on a
    /// mismatch is the same line and is never fed in here. `None` is what the caller records as
    /// `Unsampled`.
    pub fn parse_stderr(stderr: &str) -> Option<Self> {
        let mut prefixed = stderr
            .lines()
            .filter(|line| line.starts_with(Self::OBSERVATION_PREFIX));
        let only = prefixed.next()?;
        if prefixed.next().is_some() {
            return None;
        }
        if stderr.lines().next_back() != Some(only) {
            return None;
        }
        Self::parse_line(only)
    }

    /// The script's own spelling of the observation, for turn text.
    pub fn render(&self) -> String {
        format!(
            "realpath={} common_dir={} registered={}",
            self.realpath,
            self.common_dir,
            u8::from(self.registered)
        )
    }
}

/// Which D3.0 check failed. `reasons` empty means the sample matched the candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum MismatchReason {
    /// The cwd is not the registered lease worktree (script exit 10 or 12).
    Provenance,
    /// HEAD is not the candidate commit.
    Head,
    /// The porcelain status is non-empty.
    Dirty,
}

impl MismatchReason {
    /// The wire spelling, for turn text.
    pub fn wire_str(self) -> &'static str {
        match self {
            MismatchReason::Provenance => "provenance",
            MismatchReason::Head => "head",
            MismatchReason::Dirty => "dirty",
        }
    }
}

/// `reasons` joined for turn text: `provenance, head, dirty`.
pub fn render_reasons(reasons: &[MismatchReason]) -> String {
    reasons
        .iter()
        .map(|reason| reason.wire_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn sample(head: &str, dirty: &[&str], registered: bool) -> Sample {
        Sample {
            head: head.into(),
            dirty: dirty.iter().map(|path| (*path).to_owned()).collect(),
            provenance: ProvenanceSample {
                realpath: "/leases/l-1".into(),
                common_dir: "/repo/.git".into(),
                registered,
            },
        }
    }

    fn sample_json(head: &str, dirty: &[&str], registered: bool) -> Value {
        json!({
            "head": head,
            "dirty": dirty,
            "provenance": {
                "realpath": "/leases/l-1",
                "common_dir": "/repo/.git",
                "registered": registered,
            },
        })
    }

    fn round_trip(target: &VerifyTarget, wire: Value) {
        assert_eq!(serde_json::to_value(target).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<VerifyTarget>(wire).unwrap(),
            *target
        );
    }

    /// Every variant round-trips and serializes to the D8 wire shape byte for byte: `kind` tags,
    /// field names, `cwd` absent only in `Unsampled{Prepare}`.
    #[test]
    fn verify_target_wire_shape() {
        let candidate = |evidence: VerifyTargetEvidence| VerifyTarget::Candidate {
            candidate_id: "c-1".into(),
            commit_sha: "a".repeat(40),
            lease_id: "l-1".into(),
            evidence,
        };
        let candidate_json = |evidence: Value| {
            json!({
                "kind": "candidate",
                "candidate_id": "c-1",
                "commit_sha": "a".repeat(40),
                "lease_id": "l-1",
                "evidence": evidence,
            })
        };

        round_trip(
            &candidate(VerifyTargetEvidence::Refused {
                cwd: "/leases/l-1".into(),
                before: sample(&"b".repeat(40), &[" M src/lib.rs"], true),
                reasons: vec![MismatchReason::Head, MismatchReason::Dirty],
            }),
            candidate_json(json!({
                "kind": "refused",
                "cwd": "/leases/l-1",
                "before": sample_json(&"b".repeat(40), &[" M src/lib.rs"], true),
                "reasons": ["head", "dirty"],
            })),
        );
        round_trip(
            &candidate(VerifyTargetEvidence::Verified {
                cwd: "/leases/l-1".into(),
                before: sample(&"a".repeat(40), &[], true),
                after: sample(&"a".repeat(40), &[], true),
                reasons: vec![],
            }),
            candidate_json(json!({
                "kind": "verified",
                "cwd": "/leases/l-1",
                "before": sample_json(&"a".repeat(40), &[], true),
                "after": sample_json(&"a".repeat(40), &[], true),
                "reasons": [],
            })),
        );
        round_trip(
            &candidate(VerifyTargetEvidence::Verified {
                cwd: "/leases/l-1".into(),
                before: sample(&"a".repeat(40), &[], true),
                after: sample(&"a".repeat(40), &["?? out.txt"], false),
                reasons: vec![MismatchReason::Provenance, MismatchReason::Dirty],
            }),
            candidate_json(json!({
                "kind": "verified",
                "cwd": "/leases/l-1",
                "before": sample_json(&"a".repeat(40), &[], true),
                "after": sample_json(&"a".repeat(40), &["?? out.txt"], false),
                "reasons": ["provenance", "dirty"],
            })),
        );

        // `Unsampled`: every phase; `Prepare` serializes without a `cwd` key.
        let prepare = candidate(VerifyTargetEvidence::Unsampled {
            phase: SamplePhase::Prepare {
                reason: "git rev-parse exited 128".into(),
            },
        });
        let prepare_json = candidate_json(json!({
            "kind": "unsampled",
            "phase": {"kind": "prepare", "reason": "git rev-parse exited 128"},
        }));
        round_trip(&prepare, prepare_json.clone());
        let phase = &prepare_json["evidence"]["phase"];
        assert!(
            phase.as_object().unwrap().get("cwd").is_none(),
            "Prepare carries no cwd: {phase}"
        );
        for (phase, wire) in [
            (
                SamplePhase::Finalize {
                    cwd: "/leases/l-1".into(),
                    reason: "status exited 1".into(),
                },
                json!({"kind": "finalize", "cwd": "/leases/l-1", "reason": "status exited 1"}),
            ),
            (
                SamplePhase::Compensation {
                    cwd: "/leases/l-1".into(),
                    reason: "spawn failed".into(),
                },
                json!({"kind": "compensation", "cwd": "/leases/l-1", "reason": "spawn failed"}),
            ),
            (
                SamplePhase::Reconciliation {
                    cwd: "/leases/l-1".into(),
                    last_error: "parked_deadline".into(),
                },
                json!({"kind": "reconciliation", "cwd": "/leases/l-1", "last_error": "parked_deadline"}),
            ),
        ] {
            round_trip(
                &candidate(VerifyTargetEvidence::Unsampled { phase }),
                candidate_json(json!({"kind": "unsampled", "phase": wire})),
            );
        }

        // `NoCandidate`: the four delivery states.
        for (reason, wire) in [
            (
                NoCandidateReason::DeliveryPending {
                    delivery_id: "d-1".into(),
                },
                json!({"kind": "delivery_pending", "delivery_id": "d-1"}),
            ),
            (
                NoCandidateReason::DeliveryFailed {
                    delivery_id: "d-1".into(),
                },
                json!({"kind": "delivery_failed", "delivery_id": "d-1"}),
            ),
            (
                NoCandidateReason::DeliveryAbandoned {
                    delivery_id: "d-1".into(),
                },
                json!({"kind": "delivery_abandoned", "delivery_id": "d-1"}),
            ),
            (
                NoCandidateReason::NoDeliveryRow,
                json!({"kind": "no_delivery_row"}),
            ),
        ] {
            round_trip(
                &VerifyTarget::NoCandidate { reason },
                json!({"kind": "no_candidate", "reason": wire}),
            );
        }

        // `Unbound`: the four reasons, including the serde-default targets of D12 (b)(c).
        for (reason, wire) in [
            (UnboundReason::LegacyLease, "legacy_lease"),
            (UnboundReason::LegacyFrozen, "legacy_frozen"),
            (UnboundReason::LegacyVerdict, "legacy_verdict"),
            (UnboundReason::Terminal, "terminal"),
        ] {
            round_trip(
                &VerifyTarget::Unbound { reason },
                json!({"kind": "unbound", "reason": wire}),
            );
        }

        // Unknown keys are rejected on every level.
        assert!(
            serde_json::from_value::<VerifyTarget>(json!({
                "kind": "unbound", "reason": "terminal", "extra": 1
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<VerifyTarget>(candidate_json(json!({
                "kind": "unsampled",
                "phase": {"kind": "prepare", "reason": "r", "cwd": "/x"},
            })))
            .is_err(),
            "Prepare has no cwd"
        );
        assert_eq!(
            render_reasons(&[
                MismatchReason::Provenance,
                MismatchReason::Head,
                MismatchReason::Dirty
            ]),
            "provenance, head, dirty"
        );
    }

    /// The script line as `printf` writes it: registered 0 / 1, paths with spaces, trailing newline.
    #[test]
    fn provenance_sample_parses_the_script_line() {
        assert_eq!(
            ProvenanceSample::parse_line(
                "provenance realpath=/leases/l-1 common_dir=/repo/.git registered=1\n"
            ),
            Some(ProvenanceSample {
                realpath: "/leases/l-1".into(),
                common_dir: "/repo/.git".into(),
                registered: true,
            })
        );
        assert_eq!(
            ProvenanceSample::parse_line(
                "provenance realpath=/tmp/other clone common_dir=/tmp/other clone/.git registered=0"
            ),
            Some(ProvenanceSample {
                realpath: "/tmp/other clone".into(),
                common_dir: "/tmp/other clone/.git".into(),
                registered: false,
            })
        );
        let parsed = ProvenanceSample::parse_line(
            "provenance realpath=/a b/c common_dir=/a b/c/.git registered=1",
        )
        .unwrap();
        assert_eq!(parsed.realpath, "/a b/c");
        assert_eq!(parsed.common_dir, "/a b/c/.git");
        assert_eq!(
            parsed.render(),
            "realpath=/a b/c common_dir=/a b/c/.git registered=1"
        );
        // Not observations: wrong prefix, a registered value outside {0, 1}, a missing marker.
        assert_eq!(
            ProvenanceSample::parse_line("fatal: not a git repository"),
            None
        );
        assert_eq!(
            ProvenanceSample::parse_line(
                "provenance realpath=/x common_dir=/x/.git registered=yes"
            ),
            None
        );
        assert_eq!(
            ProvenanceSample::parse_line("provenance realpath=/x registered=1"),
            None
        );
    }

    /// A path that itself contains a ` common_dir=` / ` registered=` marker makes the printf
    /// record ambiguous; the parser refuses it rather than splitting invented paths (fail-closed:
    /// the mismatch verdict comes from the exit code, the evidence may be absent but never made up).
    #[test]
    fn provenance_sample_refuses_an_ambiguous_record() {
        assert_eq!(
            ProvenanceSample::parse_line(
                "provenance realpath=/w common_dir=/repo common_dir=x/.git registered=1"
            ),
            None
        );
        assert_eq!(
            ProvenanceSample::parse_line(
                "provenance realpath=/w common_dir=/r registered=0 registered=1"
            ),
            None
        );
        assert_eq!(
            ProvenanceSample::parse_line(
                "provenance realpath=/w registered=0 common_dir=/r registered=1"
            ),
            None
        );
    }

    /// The sampler reads stderr as a whole: git's own `warning:` lines may precede the one
    /// observation record and are skipped; a second `provenance realpath=` line means the
    /// premise (one record per invocation) does not hold and nothing is picked — neither the
    /// first nor the last. The script writes nothing to stderr after the record, so a record
    /// followed by another line is not the script's record either.
    #[test]
    fn provenance_sample_refuses_more_than_one_observation_line() {
        let stderr = "warning: refname 'HEAD' is ambiguous.\n\
                      provenance realpath=/leases/l-1 common_dir=/repo/.git registered=1\n";
        assert_eq!(
            ProvenanceSample::parse_stderr(stderr),
            Some(ProvenanceSample {
                realpath: "/leases/l-1".into(),
                common_dir: "/repo/.git".into(),
                registered: true,
            })
        );
        let two = "provenance realpath=/stale common_dir=/stale/.git registered=0\n\
                   provenance realpath=/leases/l-1 common_dir=/repo/.git registered=1\n\
                   warning: trailing noise\n";
        assert_eq!(ProvenanceSample::parse_stderr(two), None);
        let trailing = "provenance realpath=/leases/l-1 common_dir=/repo/.git registered=1\n\
                        warning: trailing noise\n";
        assert_eq!(ProvenanceSample::parse_stderr(trailing), None);
        assert_eq!(
            ProvenanceSample::parse_stderr("fatal: not a git repository\n"),
            None
        );
        assert_eq!(ProvenanceSample::parse_stderr(""), None);
    }

    /// stderr exactly as the script's
    /// `printf 'provenance realpath=%s common_dir=%s registered=%s\n' "$rp" "$cd_" "$reg"`
    /// spells it for the given values.
    fn script_stderr(realpath: &str, common_dir: &str, registered: &str) -> String {
        format!("provenance realpath={realpath} common_dir={common_dir} registered={registered}\n")
    }

    /// A newline inside a path splits the printf record into several physical lines. The tail
    /// can spell a second, forged record (the old "take the last parsable line" rule returned
    /// `realpath=/fake` for the first case); the head is a prefixed line missing a marker; or
    /// the head is a prefixed line that parses on its own — `common_dir` carrying
    /// ` registered=0\n` pushes the real `registered=1` onto the next line (the "exactly one
    /// prefixed line" rule alone read `common_dir=/r/.git registered=false` there, while the
    /// script exits 0). All four yield `None` — the verdict comes from the exit code, the
    /// evidence is never made up.
    #[test]
    fn provenance_sample_refuses_newline_paths() {
        let forged = script_stderr("/w\nprovenance realpath=/fake", "/r/.git", "0");
        assert_eq!(
            forged,
            "provenance realpath=/w\nprovenance realpath=/fake common_dir=/r/.git registered=0\n"
        );
        assert_eq!(ProvenanceSample::parse_stderr(&forged), None);

        let realpath_split = script_stderr("/w\nfoo", "/r/.git", "1");
        assert_eq!(
            realpath_split,
            "provenance realpath=/w\nfoo common_dir=/r/.git registered=1\n"
        );
        assert_eq!(ProvenanceSample::parse_stderr(&realpath_split), None);

        let common_dir_split = script_stderr("/w", "/r/.git\nx", "1");
        assert_eq!(
            common_dir_split,
            "provenance realpath=/w common_dir=/r/.git\nx registered=1\n"
        );
        assert_eq!(ProvenanceSample::parse_stderr(&common_dir_split), None);

        let head_parses_alone = script_stderr("/w", "/r/.git registered=0\nx", "1");
        assert_eq!(
            head_parses_alone,
            "provenance realpath=/w common_dir=/r/.git registered=0\nx registered=1\n"
        );
        assert_eq!(
            ProvenanceSample::parse_line("provenance realpath=/w common_dir=/r/.git registered=0"),
            Some(ProvenanceSample {
                realpath: "/w".into(),
                common_dir: "/r/.git".into(),
                registered: false,
            })
        );
        assert_eq!(ProvenanceSample::parse_stderr(&head_parses_alone), None);

        // The same helper spells an ordinary record, which still parses.
        let plain = script_stderr("/leases/l-1", "/repo/.git", "1");
        assert_eq!(
            ProvenanceSample::parse_stderr(&plain).map(|sample| sample.render()),
            Some("realpath=/leases/l-1 common_dir=/repo/.git registered=1".into())
        );
    }
}
