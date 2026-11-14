//! Node identity, ownership and content-independent parent checks.
use super::*;
use crate::ids::TrackId;

#[tokio::test]
async fn node_identity_uses_card_owner_without_decoding_report_content() {
    use crate::db::RepoSyncDomainRaw;
    use crate::db::sqlite::SqlxRepo;
    use crate::model::{NewArea, NewCard, NewTrack, RequestTheme};
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "nodes".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "nodes".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "track-report".into(),
            sort: None,
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();
    // Corrupt only the stored fixture: node identity must not decode Report content.
    repo.db()
        .writer()
        .execute(
            sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
                .bind(json!({"malformed":true}).to_string())
                .bind(card.id.as_str()),
        )
        .await
        .unwrap();
    let projection_key = format!("{}:dual", track.id);
    repo.card_create(NewCard {
        track_id: track.id.clone(),
        title: None,
        kind: "codex".into(),
        sort: None,
        payload: json!({"idempotency_key":projection_key}),
    })
    .await
    .unwrap();
    let write = WriteContext::new(
        repo.card_role_cache().clone(),
        repo.track_area_cache().clone(),
    );
    let view = TrackFsView::new(&repo, &write);
    assert_eq!(
        view.node_kind(&track, "report.md").await.unwrap(),
        TrackFsNodeKind::File
    );
    assert!(matches!(
        view.cat(&track, "report.md").await,
        Err(TrackFsError::Internal(_))
    ));
    let directory = format!("cards/{}", card.id);
    assert_eq!(
        view.node_kind(&track, &directory).await.unwrap(),
        TrackFsNodeKind::Directory
    );
    for entry in view.ls(&track, Some(&directory)).await.unwrap() {
        assert_eq!(
            view.node_kind(&track, &format!("{directory}/{}", entry.name))
                .await
                .unwrap(),
            TrackFsNodeKind::File
        );
    }
    // Legal task keys can end with a projection suffix. Locate their real parent
    // identity without assuming that the spelling names a projection file.
    for key in ["suffix.json", "suffix.md", "dual.json"] {
        let attempt = format!("{}:{key}", track.id);
        repo.db()
            .writer()
            .execute(
                sqlx::query(
                    "INSERT INTO tasks(
                id, track_id, key, kind, goal, context_json, status,
                declared_by, created_at_ms, updated_at_ms
            ) VALUES (?1, ?2, ?3, 'codex', 'node', '{}', 'pending', 'user', 0, 0)",
                )
                .bind(&attempt)
                .bind(track.id.as_str())
                .bind(key),
            )
            .await
            .unwrap();
        let path = format!("runs/{attempt}");
        if key == "dual.json" {
            let body = view.cat(&track, &path).await.unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&body.content).unwrap()["attempt_id"],
                projection_key
            );
        }
        let node = view.node_kind(&track, &path).await.unwrap();
        assert!(node.is_directory());
        assert_eq!(node.is_file(), key == "dual.json");
        if key != "dual.json" {
            assert!(matches!(
                view.cat(&track, &path).await,
                Err(TrackFsError::PathNotAvailable(_))
            ));
        }
    }
    let mut foreign = track.clone();
    foreign.id = TrackId::from("foreign");
    assert!(matches!(
        view.node_kind(&foreign, &directory).await,
        Err(TrackFsError::Forbidden(_))
    ));
    for path in [
        "cards/missing",
        "cards/missing/.payload.json",
        "runs/missing",
        "runs/missing/result",
        "cards/a/unknown.json",
        "unknown",
        "runs/missing/gates",
    ] {
        assert!(view.node_kind(&track, path).await.is_err(), "{path}");
    }
}
