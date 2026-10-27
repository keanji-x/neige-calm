use super::*;
use sha2::{Digest, Sha256};
use sqlx::{Connection, SqliteConnection};

#[tokio::test]
async fn manifest_binds_all_evidence_and_rejects_duplicate_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("backup");
    let mut c = SqliteConnection::connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .unwrap();
    sqlx::raw_sql("CREATE TABLE unknown(id INTEGER PRIMARY KEY, raw); INSERT INTO unknown VALUES(1,NULL),(2,''),(3,x''),(4,x'ff');").execute(&mut c).await.unwrap();
    c.close().await.unwrap();
    let audit = OfflineSnapshot::open(FrozenBackup {
        path,
        operator: "operator".into(),
        freeze_evidence: "stopped".into(),
    })
    .await
    .unwrap()
    .inventory()
    .await
    .unwrap();
    let source = SourceDeclaration {
        path: b"/owned".to_vec(),
        ownership: "all-owned".into(),
        freeze_evidence: "same-window".into(),
    };
    let file = FileEvidence {
        source: 0,
        path: b"orphan.part".to_vec(),
        kind: "Regular".into(),
        mode: 0o100600,
        length: 0,
        sha256: Some(Sha256::digest([]).into()),
        artifact: Some("file-0-0".into()),
        diagnostics: vec!["UninterpretedOwnership".into()],
    };
    let manifest = audit
        .manifest(
            b"/backup".to_vec(),
            "complete-set",
            vec![source.clone()],
            vec![file.clone()],
        )
        .unwrap();
    let root = manifest.root().unwrap();
    let original = serde_json::to_value(&manifest).unwrap();
    // Each independently meaningful field must affect the root.
    for (pointer, replacement) in [
        ("/inventory/encoding", serde_json::json!("UTF-16le")),
        ("/inventory/schema/0/0", serde_json::json!("Null")),
        ("/relations/0/count", serde_json::json!(999)),
        ("/relations/0/ordered_rows/0/0", serde_json::json!(256)),
        (
            "/inventory/relations/0/rows/0/cells/0",
            serde_json::json!({"Blob":[]}),
        ),
        ("/files/0/path", serde_json::json!([255])),
        ("/files/0/sha256/0", serde_json::json!(256)),
        ("/files/0/kind", serde_json::json!("Other")),
        ("/source_sha256/0", serde_json::json!(256)),
        ("/sources/0/ownership", serde_json::json!("different")),
        ("/coverage", serde_json::json!("partial")),
        ("/blocked_reason", serde_json::json!("missing")),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert_ne!(
            root,
            evidence_digest("root", &changed).unwrap(),
            "{pointer}"
        );
    }
    let cells = [
        RawCell::Null,
        RawCell::Text(vec![]),
        RawCell::Blob(vec![]),
        RawCell::Integer(0),
        RawCell::RealBits(0),
        RawCell::DerivedNotStored,
    ];
    let hashes = cells
        .iter()
        .map(|c| evidence_digest("row", c).unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(hashes.len(), cells.len());
    assert_ne!(
        evidence_digest("root", &cells).unwrap(),
        evidence_digest("row", &cells).unwrap()
    );
    assert!(
        audit
            .manifest(
                b"/backup".to_vec(),
                "set",
                vec![source],
                vec![file.clone(), file]
            )
            .is_err()
    );
}
