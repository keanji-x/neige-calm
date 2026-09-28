use super::store::{self, Applied};
use super::{MAX_TAG_CHARS, MAX_TAGS_PER_REPORT, normalize_tag};
use crate::db::sqlite::SqlxRepo;
use crate::db::{RepoSyncDomainRaw, ServerRepoReadExt};
use crate::error::CalmError;
use crate::model::{NewArea, NewTrack, RequestTheme};

/// A track with a report card whose `updated_at` is 1, far behind any real clock.
async fn fixture() -> (SqlxRepo, String) {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = repo
        .area_create(NewArea {
            name: "report-tags".into(),
            color: "#123456".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track_id = track(&repo, &area.id).await;
    (repo, track_id)
}

async fn track(repo: &SqlxRepo, area_id: &crate::ids::AreaId) -> String {
    let track = repo
        .track_create(NewTrack {
            area_id: area_id.clone(),
            title: "tags".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO cards (id, track_id, kind, sort, payload, role, deletable, created_at, updated_at) \
         VALUES (?1, ?2, 'track-report', -1, '{\"schemaVersion\":4,\"summary\":\"\",\"body\":\"\"}', \
                 'reportcard', 0, 1, 1)",
    )
    .bind(format!("report-{}", track.id.as_str()))
    .bind(track.id.as_str())
    .execute(repo.pool())
    .await
    .unwrap();
    track.id.as_str().to_string()
}

fn tags(list: &[&str]) -> Vec<String> {
    list.iter().map(|tag| tag.to_string()).collect()
}

async fn apply(
    repo: &SqlxRepo,
    track_id: &str,
    add: &[&str],
    remove: &[&str],
) -> Result<Applied, CalmError> {
    let mut tx = repo.pool().begin().await.unwrap();
    let applied = store::apply_tx(&mut tx, track_id, &tags(add), &tags(remove)).await?;
    tx.commit().await.unwrap();
    Ok(applied)
}

async fn report_updated_at(repo: &SqlxRepo, track_id: &str) -> i64 {
    sqlx::query_scalar("SELECT updated_at FROM cards WHERE track_id = ?1 AND kind = 'track-report'")
        .bind(track_id)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn add_keeps_insertion_order_and_is_idempotent() {
    let (repo, track) = fixture().await;
    let applied = apply(&repo, &track, &["认证", "架构"], &[]).await.unwrap();
    assert_eq!(applied.tags, tags(&["认证", "架构"]));
    let applied = apply(&repo, &track, &["排障"], &[]).await.unwrap();
    assert_eq!(
        applied.tags,
        tags(&["认证", "架构", "排障"]),
        "not lexicographic"
    );

    let again = apply(&repo, &track, &["认证", "排障", "认证"], &[])
        .await
        .unwrap();
    assert_eq!(again.tags, tags(&["认证", "架构", "排障"]));
    assert!(
        again.touched_report.is_none(),
        "a no-op add changes nothing"
    );
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM report_tags WHERE track_id = ?1")
        .bind(&track)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(rows, 3, "no duplicate rows");
}

#[tokio::test]
async fn remove_drops_only_the_named_tag_and_absent_is_a_no_op() {
    let (repo, track) = fixture().await;
    apply(&repo, &track, &["a", "b", "c"], &[]).await.unwrap();
    let applied = apply(&repo, &track, &[], &["b"]).await.unwrap();
    assert_eq!(applied.tags, tags(&["a", "c"]));
    let absent = apply(&repo, &track, &[], &["zzz"]).await.unwrap();
    assert_eq!(absent.tags, tags(&["a", "c"]));
    assert!(absent.touched_report.is_none());
    // Re-adding a removed tag appends it; the survivors keep their order.
    let applied = apply(&repo, &track, &["b"], &[]).await.unwrap();
    assert_eq!(applied.tags, tags(&["a", "c", "b"]));
    // Adds apply before removes in one call.
    let applied = apply(&repo, &track, &["d"], &["a", "d"]).await.unwrap();
    assert_eq!(applied.tags, tags(&["c", "b"]));
}

#[tokio::test]
async fn a_change_bumps_the_report_updated_at_and_a_no_op_does_not() {
    let (repo, track) = fixture().await;
    assert_eq!(report_updated_at(&repo, &track).await, 1);
    let applied = apply(&repo, &track, &["x"], &[]).await.unwrap();
    let bumped = report_updated_at(&repo, &track).await;
    assert!(bumped > 1, "tag add bumps the report card: {bumped}");
    assert_eq!(applied.touched_report.unwrap().updated_at, bumped);

    sqlx::query("UPDATE cards SET updated_at = 1 WHERE track_id = ?1")
        .bind(&track)
        .execute(repo.pool())
        .await
        .unwrap();
    apply(&repo, &track, &["x"], &["absent"]).await.unwrap();
    assert_eq!(report_updated_at(&repo, &track).await, 1, "no-op call");
    apply(&repo, &track, &[], &["x"]).await.unwrap();
    assert!(
        report_updated_at(&repo, &track).await > 1,
        "tag remove bumps"
    );
}

#[tokio::test]
async fn tracks_do_not_share_tags_and_deleting_a_track_cascades_its_rows() {
    let (repo, track) = fixture().await;
    let area_id = repo.track_get(&track).await.unwrap().unwrap().area_id;
    let other = self::track(&repo, &area_id).await;
    apply(&repo, &track, &["mine"], &[]).await.unwrap();
    apply(&repo, &other, &["theirs"], &[]).await.unwrap();
    assert_eq!(
        store::list(repo.pool(), &track).await.unwrap(),
        tags(&["mine"])
    );
    assert_eq!(
        store::list(repo.pool(), &other).await.unwrap(),
        tags(&["theirs"])
    );

    sqlx::query("DELETE FROM tracks WHERE id = ?1")
        .bind(&track)
        .execute(repo.pool())
        .await
        .unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM report_tags WHERE track_id = ?1")
        .bind(&track)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(rows, 0, "FK cascade removed the deleted track's tags");
    assert_eq!(
        store::list(repo.pool(), &other).await.unwrap(),
        tags(&["theirs"])
    );
}

#[tokio::test]
async fn an_untagged_report_lists_empty_and_the_count_bound_rolls_back() {
    let (repo, track) = fixture().await;
    assert!(store::list(repo.pool(), &track).await.unwrap().is_empty());
    let many: Vec<String> = (0..=MAX_TAGS_PER_REPORT).map(|i| format!("t{i}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let err = apply(&repo, &track, &many, &[]).await.unwrap_err();
    assert!(
        matches!(&err, CalmError::BadRequest(m) if m.contains("at most 32 tags")),
        "{err:?}"
    );
    assert!(store::list(repo.pool(), &track).await.unwrap().is_empty());
    assert_eq!(report_updated_at(&repo, &track).await, 1);
}

#[tokio::test]
async fn a_track_without_a_report_card_is_refused() {
    let (repo, track) = fixture().await;
    sqlx::query("DELETE FROM cards WHERE track_id = ?1")
        .bind(&track)
        .execute(repo.pool())
        .await
        .unwrap();
    let err = apply(&repo, &track, &["x"], &[]).await.unwrap_err();
    assert!(
        matches!(&err, CalmError::BadRequest(m) if m.contains("no report card")),
        "{err:?}"
    );
}

#[test]
fn normalize_trims_and_refuses_separators() {
    assert_eq!(normalize_tag("  认证 ").unwrap(), "认证");
    assert_eq!(normalize_tag("c++/rust-2024").unwrap(), "c++/rust-2024");
    for bad in ["", "   ", "a b", "a,b", "a\tb", "a\u{3000}b", "a\u{7}"] {
        assert!(normalize_tag(bad).is_err(), "{bad:?}");
    }
    assert!(normalize_tag(&"字".repeat(MAX_TAG_CHARS)).is_ok());
    assert!(normalize_tag(&"字".repeat(MAX_TAG_CHARS + 1)).is_err());
}
