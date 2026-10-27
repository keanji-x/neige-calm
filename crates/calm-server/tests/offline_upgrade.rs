#![cfg(target_os = "linux")]

use sha2::{Digest, Sha256};
use sqlx::{Connection, Row, SqliteConnection};
use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::{ffi::OsStringExt, fs::symlink},
    path::Path,
    process::{Command, Output},
};

fn cli(db: &Path, owned: &Path, output: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_offline-audit"));
    command
        .arg(if output.is_some() { "export" } else { "scan" })
        .arg("--backup")
        .arg(db)
        .args([
            "--operator",
            "test-operator",
            "--freeze-evidence",
            "stopped-self-contained-backup",
            "--complete-set-evidence",
            "all-owned-roots-same-window",
        ]);
    if let Some(output) = output {
        command.arg("--output").arg(output);
    }
    command.arg("--owned-root").arg(owned).args([
        "operator-owned-attachments-and-config",
        "same-stopped-window",
    ]);
    command.output().unwrap()
}
fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn digest(value: &serde_json::Value, domain: &str) -> Vec<u8> {
    // Independent protocol oracle: no production digest or row codec.
    fn node(v: &serde_json::Value) -> Vec<u8> {
        let mut out = Vec::new();
        let mut framed = |tag, b: &[u8]| {
            out.push(tag);
            out.extend((b.len() as u64).to_be_bytes());
            out.extend(b);
        };
        match v {
            serde_json::Value::Null => out.push(0),
            serde_json::Value::Bool(b) => out.extend([1, u8::from(*b)]),
            serde_json::Value::Number(n) => framed(2, n.to_string().as_bytes()),
            serde_json::Value::String(s) => framed(3, s.as_bytes()),
            serde_json::Value::Array(a) => {
                out.push(4);
                out.extend((a.len() as u64).to_be_bytes());
                for v in a {
                    out.extend(node(v));
                }
            }
            serde_json::Value::Object(o) => {
                out.push(5);
                out.extend((o.len() as u64).to_be_bytes());
                let mut keys = o.keys().collect::<Vec<_>>();
                keys.sort();
                for k in keys {
                    out.extend(node(&serde_json::Value::String(k.clone())));
                    out.extend(node(&o[k]));
                }
            }
        }
        out
    }
    let mut preimage = b"neige.raw-blocked-audit.v1\0".to_vec();
    preimage.extend((domain.len() as u64).to_be_bytes());
    preimage.extend(domain.as_bytes());
    preimage.extend(node(value));
    Sha256::digest(preimage).to_vec()
}
#[tokio::test]
async fn real_cli_preserves_full_inventory_and_stable_root() {
    let temp = tempfile::tempdir().unwrap();
    let dbdir = temp.path().join("backup");
    let owned = temp.path().join("owned");
    let outputs = temp.path().join("outputs");
    for p in [&dbdir, &owned, &outputs] {
        fs::create_dir(p).unwrap();
    }
    let db = dbdir.join("source.sqlite");
    let mut c = SqliteConnection::connect(&format!("sqlite://{}?mode=rwc", db.display()))
        .await
        .unwrap();
    sqlx::raw_sql(concat!(
        "CREATE TABLE unknown_sentinel(id INTEGER PRIMARY KEY, value); ",
        "INSERT INTO unknown_sentinel VALUES(1,NULL),(2,''),(3,x''),",
        "(4,CAST(x'ff00' AS TEXT)),(5,x'ff00'),(6,42),(7,1.25); ",
        "CREATE TABLE threads(id TEXT, raw TEXT); INSERT INTO threads VALUES('old','{bad'); ",
        "CREATE TABLE acp_submissions(id TEXT); INSERT INTO acp_submissions VALUES('orphan'); ",
        "CREATE TABLE database_identity(id BLOB); INSERT INTO database_identity VALUES(x'0001'); ",
        "CREATE VIEW old_view AS SELECT * FROM threads;"
    ))
    .execute(&mut c)
    .await
    .unwrap();
    let oracle = sqlx::query(
        "SELECT id,typeof(value),hex(CAST(value AS BLOB)) FROM unknown_sentinel ORDER BY id",
    )
    .fetch_all(&mut c)
    .await
    .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM unknown_sentinel")
        .fetch_one(&mut c)
        .await
        .unwrap();
    c.close().await.unwrap();
    for dir in ["bound", "staging"] {
        fs::create_dir(owned.join(dir)).unwrap();
        fs::write(owned.join(dir).join("same-id"), dir).unwrap();
    }
    fs::write(owned.join("orphan.part"), b"\0orphan").unwrap();
    fs::write(
        owned.join("config.json"),
        b"{\"duplicate\":1,\"duplicate\":2}",
    )
    .unwrap();
    let rawname = OsString::from_vec(vec![0xff, b'x']);
    fs::write(owned.join(&rawname), b"nonutf8").unwrap();
    symlink("/unread-target", owned.join("link")).unwrap();
    let before = fs::read(&db).unwrap();
    let scan: serde_json::Value = serde_json::from_slice(&success(cli(&db, &owned, None))).unwrap();
    let a = outputs.join("a");
    let b = outputs.join("b");
    success(cli(&db, &owned, Some(&a)));
    success(cli(&db, &owned, Some(&b)));
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(a.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(scan, manifest);
    assert_eq!(
        fs::read(a.join("root.seal")).unwrap(),
        fs::read(b.join("root.seal")).unwrap()
    );
    assert_eq!(
        fs::read(a.join("root.seal")).unwrap(),
        digest(&manifest, "root")
    );
    assert_eq!(
        manifest["source_sha256"],
        serde_json::json!(Sha256::digest(&before).to_vec())
    );
    let relations = manifest["inventory"]["relations"].as_array().unwrap();
    let relation = relations
        .iter()
        .find(|r| r["name"] == "unknown_sentinel")
        .unwrap();
    assert_eq!(relation["source_count"], count);
    let rows = relation["rows"].as_array().unwrap();
    assert_eq!(rows.len(), count as usize);
    for (row, sql) in rows.iter().zip(oracle) {
        assert_eq!(row["key"][0]["Integer"], sql.get::<i64, _>(0));
        let kind: String = sql.get(1);
        let raw: String = sql.get(2);
        let cell = &row["cells"][1];
        match kind.as_str() {
            "null" => assert_eq!(cell, "Null"),
            "text" | "blob" => {
                let bytes = cell[if kind == "text" { "Text" } else { "Blob" }]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as u8)
                    .collect::<Vec<_>>();
                assert_eq!(hex::encode_upper(bytes), raw);
            }
            "integer" => assert_eq!(cell["Integer"], 42),
            "real" => assert_eq!(cell["RealBits"], 1.25f64.to_bits()),
            _ => panic!("unexpected class"),
        }
    }
    for name in ["threads", "acp_submissions", "database_identity"] {
        assert!(
            relations
                .iter()
                .any(|r| r["name"] == name && r["source_count"] == 1)
        );
    }
    for relation in &manifest["relations"].as_array().unwrap()[..] {
        let raw = relations
            .iter()
            .find(|r| r["name"] == relation["name"])
            .unwrap();
        for (row, hash) in raw["rows"]
            .as_array()
            .unwrap()
            .iter()
            .zip(relation["ordered_rows"].as_array().unwrap())
        {
            assert_eq!(serde_json::json!(digest(row, "row")), *hash);
        }
    }
    let files = manifest["files"].as_array().unwrap();
    assert!(
        files
            .iter()
            .any(|f| f["path"] == serde_json::json!([255, 120]))
    );
    assert_eq!(files.iter().filter(|f| f["kind"] == "Regular").count(), 5);
    assert!(files.iter().any(|f| {
        f["kind"] == "Symlink"
            && f["diagnostics"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("TargetNotRead"))
    }));
    for file in files.iter().filter(|f| f["kind"] == "Regular") {
        let path = OsString::from_vec(
            file["path"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_u64().unwrap() as u8)
                .collect(),
        );
        let bytes = fs::read(owned.join(path)).unwrap();
        assert_eq!(
            fs::read(a.join(file["artifact"].as_str().unwrap())).unwrap(),
            bytes
        );
        assert_eq!(
            file["sha256"],
            serde_json::json!(Sha256::digest(&bytes).to_vec())
        );
    }
    assert!(!cli(&db, &owned, Some(&a)).status.success());
    assert!(
        !cli(&db, &owned, Some(&owned.join("overlap")))
            .status
            .success()
    );
    assert!(
        !cli(&db, &owned, Some(&dbdir.join("overlap")))
            .status
            .success()
    );
    let alias = outputs.join("backup-alias");
    symlink("../backup/source.sqlite", &alias).unwrap();
    let overlap = dbdir.join("aliased-overlap");
    assert!(!cli(&alias, &owned, Some(&overlap)).status.success());
    assert!(!overlap.exists());
    assert!(!overlap.join("root.seal").exists());
    assert_eq!(
        fs::read_link(&alias).unwrap(),
        Path::new("../backup/source.sqlite")
    );
    assert_eq!(fs::read(&db).unwrap(), before);
    assert_eq!(fs::read_dir(&dbdir).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&owned).unwrap().count(), 6);
}
#[tokio::test]
async fn partial_and_unproven_inputs_cannot_publish() {
    let t = tempfile::tempdir().unwrap();
    let dbdir = t.path().join("db");
    let owned = t.path().join("owned");
    let outdir = t.path().join("out");
    for p in [&dbdir, &owned, &outdir] {
        fs::create_dir(p).unwrap();
    }
    let db = dbdir.join("db");
    let out = outdir.join("audit");
    let mut c = SqliteConnection::connect(&format!("sqlite://{}?mode=rwc", db.display()))
        .await
        .unwrap();
    sqlx::raw_sql("CREATE VIRTUAL TABLE unproven USING fts5(content)")
        .execute(&mut c)
        .await
        .unwrap();
    c.close().await.unwrap();
    assert!(!cli(&db, &owned, Some(&out)).status.success());
    assert!(!out.exists());
    fs::write(&db, b"not SQLite").unwrap();
    assert!(!cli(&db, &owned, Some(&out)).status.success());
    assert!(!out.exists());
    assert!(!cli(&db, &owned.join("absent"), Some(&out)).status.success());
    assert!(!out.exists());
    let status = Command::new(env!("CARGO_BIN_EXE_offline-audit"))
        .args([OsStr::new("scan"), OsStr::new("--backup"), db.as_os_str()])
        .output()
        .unwrap();
    assert!(!status.status.success());
}

#[test]
fn explicit_set_proof_and_database_path_are_required() {
    let t = tempfile::tempdir().unwrap();
    let owned = t.path().join("owned");
    fs::create_dir(&owned).unwrap();
    let raw = t.path().join(OsString::from_vec(vec![255]));
    fs::write(&raw, []).unwrap();
    let failure = cli(&raw, &owned, None);
    assert!(!failure.status.success());
    assert!(String::from_utf8_lossy(&failure.stderr).contains("UnsupportedInput"));
    let failure = Command::new(env!("CARGO_BIN_EXE_offline-audit"))
        .args(["scan", "--backup"])
        .arg(t.path().join("absent"))
        .args([
            "--operator",
            "op",
            "--freeze-evidence",
            "stopped",
            "--complete-set-evidence",
            "all-roots-frozen",
        ])
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert!(String::from_utf8_lossy(&failure.stderr).contains("UnsupportedInput"));
}
