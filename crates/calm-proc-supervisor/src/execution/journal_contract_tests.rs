//! Fixtures serialized by unmodified production Journal/workspace/session types at
//! d1349241374542564ff87342b3017ecae5b53d3c: journal 4, receipt 4, wire 9.
//! Numeric process/directory facts are fixture data, never live signal authority.
use super::*;
use calm_session::control::execution::{MacProcessIdentity, OutputEnd};
use workspace::execution::objects::PlatformIdentity;

const LEGACY_JOURNAL: &str = include_str!("fixtures/linux-v4.execution.json");
const LEGACY_REGISTRATION: &str = include_str!("fixtures/linux-v4.registration.json");

fn legacy() -> Journal {
    serde_json::from_str(LEGACY_JOURNAL).unwrap()
}
fn mac_receipt(record: &Journal) -> ExecutionReceipt {
    let mut receipt = record.phase.receipt().unwrap().clone();
    receipt.process = NativeProcessIdentity::MacOwnedGroup(MacProcessIdentity {
        pid: 12345,
        birth_seconds: 123456,
        birth_microseconds: 789,
        nonce: receipt.process.nonce().clone(),
    });
    receipt.scope_mechanism = ScopeMechanismIdentity::MacOwnedGroup {
        protocol_version: 1,
    };
    receipt
}

#[test]
fn native_macos_reads_frozen_linux_journal_and_registration_without_backfills() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("journal")).unwrap();
    std::fs::write(store.path().join("legacy.execution"), LEGACY_JOURNAL).unwrap();
    std::fs::write(
        store.path().join("legacy.registration"),
        LEGACY_REGISTRATION,
    )
    .unwrap();
    let mut records = store.load().unwrap();
    assert_eq!(records.len(), 1);
    let record = records.pop().unwrap();
    record.validate().unwrap();
    assert_eq!(record.version, 4);
    assert!(record.durably_settled());
    assert_eq!(
        serde_json::to_value(&record).unwrap(),
        serde_json::from_str::<serde_json::Value>(LEGACY_JOURNAL).unwrap()
    );
    let receipts = store.registrations().unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].contract, 4);
    assert_eq!(
        receipts[0].transfer.shape.mount_namespace,
        PlatformIdentity::Linux(4026531841)
    );
    assert_eq!(
        receipts[0].transfer.shape.objects[0].mount,
        PlatformIdentity::Linux(17)
    );
    assert_eq!(
        serde_json::to_value(&receipts[0]).unwrap(),
        serde_json::from_str::<serde_json::Value>(LEGACY_REGISTRATION).unwrap()
    );
    let mut missing: serde_json::Value = serde_json::from_str(LEGACY_JOURNAL).unwrap();
    missing["phase"]["Drained"]["receipt"]["process"]
        .as_object_mut()
        .unwrap()
        .remove("start_time_ticks");
    assert!(serde_json::from_value::<Journal>(missing).is_err());
}

#[test]
fn native_macos_original_group_cannot_masquerade_as_linux_drain_proof() {
    let mut record = legacy();
    record.version = JOURNAL_VERSION;
    let receipt = mac_receipt(&record);
    let Phase::Drained { proof, .. } = &mut record.phase else {
        unreachable!()
    };
    proof.helper_process = receipt.process.clone();
    proof.mechanism = receipt.scope_mechanism.clone();
    let proof = proof.clone();
    record.phase = Phase::Drained {
        receipt: receipt.clone(),
        status: Some(0),
        proof,
    };
    assert!(
        record
            .validate()
            .unwrap_err()
            .to_string()
            .contains("drain proof")
    );
    record.phase = Phase::OriginalGroupEmpty {
        receipt,
        status: Some(0),
    };
    record.validate().unwrap();
    record.version = 4;
    assert!(
        record.validate().is_err(),
        "old journals cannot claim native facts"
    );
}

#[test]
fn native_macos_journal_incomplete_output_never_releases_private_resources() {
    let mut record = legacy();
    record.version = JOURNAL_VERSION;
    record.phase = Phase::OriginalGroupEmpty {
        receipt: mac_receipt(&record),
        status: Some(0),
    };
    record.validate().unwrap();
    for output in [
        OutputEnd::Open,
        OutputEnd::Incomplete {
            reason: "reader failure".into(),
        },
    ] {
        record.output_end = output;
        assert!(!record.durably_settled());
    }
    record.output_end = OutputEnd::Complete { final_cursor: 0 };
    assert!(record.durably_settled());
    record.pending_persistence = true;
    assert!(!record.durably_settled());
}

#[test]
fn w_owned_old_journal_keeps_explicit_only() {
    use calm_session::control::execution::{
        AmbientEnvironment, AmbientProxyPolicy, SpecificationVersion,
    };
    for version in [4, 5] {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("journal")).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(LEGACY_JOURNAL).unwrap();
        value["version"] = version.into();
        std::fs::write(
            store.path().join("old.execution"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let mut record = store.load().unwrap().pop().unwrap();
        record.validate().unwrap();
        assert_eq!(
            record.request.specification.ambient_environment,
            AmbientEnvironment::None
        );
        assert_eq!(
            record.request.specification.version,
            SpecificationVersion::ExplicitOnlyV1
        );
        record.request.specification.ambient_environment = AmbientEnvironment::SupervisorAllowed {
            proxy_policy: AmbientProxyPolicy::RejectSecret,
        };
        assert!(record.validate().is_err());
    }
}
