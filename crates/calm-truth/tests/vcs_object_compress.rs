//! Exercise the shipped operator executable, never a copied compressor.
use calm_truth::db::sqlite::SqlxRepo;
use std::process::Command;

fn command(url: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vcs-object-compress"))
        .arg(url)
        .args(args)
        .output()
        .unwrap()
}

#[tokio::test]
async fn operator_preview_apply_resume_and_old_schema_refusal() {
    let temp = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", temp.path().join("history.db").display());
    let repo = SqlxRepo::open(&url).await.unwrap();
    let raw = b"durable history\n".repeat(4096);
    let hash = blake3::hash(&[b"calm-track-vcs-v1\0blob\0".as_slice(), &raw].concat())
        .to_hex()
        .to_string();
    sqlx::query("INSERT INTO track_vcs_objects(hash,kind,bytes,created_at) VALUES(?,'blob',?,0)")
        .bind(&hash)
        .bind(&raw)
        .execute(repo.db().writer_pool())
        .await
        .unwrap();
    repo.db().close().await;
    let preview = command(&url, &["--pages", "1"]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let page: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(page["dry_run"], true);
    assert_eq!(page["candidates"], 1);
    assert_eq!(page["converted"], 0);
    let applied = command(&url, &["--apply", "--pages", "1"]);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let page: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(page["dry_run"], false);
    assert_eq!(page["converted"], 1);
    let resumed = command(&url, &["--apply", "--after", &hash]);
    assert!(resumed.status.success());
    let page: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(page["converted"], 0);
    assert_eq!(page["complete"], true);
    let repo = SqlxRepo::open(&url).await.unwrap();
    let row: (String, i64) =
        sqlx::query_as("SELECT encoding,decoded_size FROM track_vcs_objects WHERE hash=?")
            .bind(&hash)
            .fetch_one(repo.db().read_pool())
            .await
            .unwrap();
    assert_eq!(row, ("zlib-v1".into(), raw.len() as i64));
    // Missing the latest migration receipt must not be repaired by this tool.
    sqlx::query(
        "DELETE FROM _sqlx_migrations WHERE version=(SELECT max(version) FROM _sqlx_migrations)",
    )
    .execute(repo.db().writer_pool())
    .await
    .unwrap();
    repo.db().close().await;
    let refused = command(&url, &["--apply"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("deploy its readers first"));
}

#[test]
fn operator_never_creates_a_missing_database() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("missing.db");
    let output = command(&format!("sqlite://{}", path.display()), &["--apply"]);
    assert!(!output.status.success());
    assert!(!path.exists());
}
