use super::*;

/// Resolves every HTTP `insert` through the canonical resolver and checks it names the item's
/// own track (and block). Returns how many inserts were checked.
pub(super) async fn assert_inserts_resolve(fx: &Fixture, area: &AreaId, body: &Value) -> usize {
    let pool = fx.repo.pool();
    let listing = area_reports::outlines(&fx.ctx, pool, area.as_str())
        .await
        .unwrap();
    let mut checked = 0;
    for tag in body["tags"].as_array().unwrap() {
        let text = code_text(tag["insert"].as_str().unwrap());
        let name = text
            .strip_prefix("tag:")
            .expect("tag insert is `tag:<tag>`");
        assert_eq!(name, tag["label"].as_str().unwrap());
        let found: Vec<_> = listing
            .iter()
            .filter(|r| r.tags.iter().any(|t| t == name))
            .collect();
        assert_eq!(
            found.len() as u64,
            tag["track_count"].as_u64().unwrap(),
            "{tag}"
        );
        checked += 1;
    }
    for track in body["tracks"].as_array().unwrap() {
        let path = code_text(track["insert"].as_str().unwrap());
        assert!(
            listing.iter().any(|r| r.path == path),
            "insert is in the actual mention producer output"
        );
        assert_eq!(
            Some(resolve_insert(fx, area, &path).await.as_str()),
            track["track_id"].as_str(),
            "{path:?}"
        );
        checked += 1;
    }
    for block in body["blocks"].as_array().unwrap() {
        let text = code_text(block["insert"].as_str().unwrap());
        let (path, id) = text
            .rsplit_once('#')
            .expect("block insert is `<path>#<id>`");
        assert_eq!(id, block["block_id"].as_str().unwrap());
        let target = listing
            .iter()
            .find(|r| r.path == path)
            .expect("mention producer path");
        let report = fx
            .repo
            .cards_by_track(&target.track_id)
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.kind == "track-report")
            .unwrap();
        let blocks = crate::track_report_read::load_report_doc_snapshot(
            fx.repo.as_ref(),
            report.id.as_str(),
        )
        .await
        .unwrap()
        .blocks;
        assert!(
            blocks.iter().any(|b| b.id == id),
            "{text:?}: block {id} is in the report the path resolves to"
        );
        assert_eq!(
            Some(resolve_insert(fx, area, path).await.as_str()),
            block["track_id"].as_str(),
            "{text:?}"
        );
        checked += 1;
    }
    checked
}

#[tokio::test]
async fn every_insert_resolves_through_the_canonical_resolver() {
    let fx = boot().await;
    let [a, b] = fx.areas.clone();
    fx.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE areas SET name = '.a/b 100% ~ `x`#' WHERE id = ?1")
                .bind(a.as_str()),
        )
        .await
        .unwrap();
    let titles = [
        "认证 方案",
        "认证 方案",
        "a/b 100% ~x",
        ".hidden",
        "C# notes",
        "tick `x`",
        "`edge`",
    ];
    let mut tracks = Vec::new();
    for (i, title) in titles.iter().enumerate() {
        let tag = format!("t`{i}");
        tracks.push(
            report(
                &fx,
                &a,
                title,
                TWO_BLOCKS,
                true,
                &[&tag, "共享"],
                100 + i as i64,
            )
            .await,
        );
    }
    tracks.push(report(&fx, &a, "legacy 报告", TWO_BLOCKS, false, &["共享"], 50).await);
    // The same title in another area neither resolves here nor changes this area's names.
    report(&fx, &b, "认证 方案", TWO_BLOCKS, true, &["共享"], 999).await;

    let outlines = area_reports::outlines(&fx.ctx, fx.repo.pool(), a.as_str())
        .await
        .unwrap();
    let expected_ids: Vec<_> = tracks[..tracks.len() - 1]
        .iter()
        .rev()
        .chain(tracks.last())
        .collect();
    let listing: Vec<_> = expected_ids
        .iter()
        .map(|id| {
            outlines
                .iter()
                .find(|r| &r.track_id == *id)
                .unwrap()
                .path
                .clone()
        })
        .collect();
    let body = mentions(&fx, &a, "", None).await;
    let paths: Vec<String> = body["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|track| code_text(track["insert"].as_str().unwrap()))
        .collect();
    assert_eq!(
        paths, listing,
        "tracks retain ranking and use canonical paths"
    );
    assert!(
        paths.iter().any(|p| p.contains('~')),
        "duplicate titles carry `~<id>`: {paths:?}"
    );
    let mut checked = assert_inserts_resolve(&fx, &a, &body).await;
    for track in &tracks {
        let body = mentions(&fx, &a, "", Some(track)).await;
        assert_eq!(body["blocks"].as_array().unwrap().len(), 2, "{body}");
        checked += assert_inserts_resolve(&fx, &a, &body).await;
    }
    let body = mentions(&fx, &a, "goal", None).await;
    assert_eq!(body["blocks"].as_array().unwrap().len(), MAX_PER_GROUP);
    checked += assert_inserts_resolve(&fx, &a, &body).await;
    // q="": 8 tags + 8 tracks + the newest reports' 8 blocks; each of the 8 tracks'
    // recommendations: the tags and tracks again + its 2 blocks; q="goal": 8 blocks and nothing else.
    assert_eq!(checked, 24 + 8 * (16 + 2) + 8);
}

#[tokio::test]
async fn mentions_name_all_siblings_before_report_filtering() {
    let fx = boot().await;
    let [area, other] = &fx.areas;
    // A user Area with no Reports still participates in global name disambiguation.
    fx.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE areas SET name = 'same' WHERE id IN (?1, ?2)")
                .bind(area.as_str())
                .bind(other.as_str()),
        )
        .await
        .unwrap();
    // Enough non-report siblings to cross directory and mention page caps.
    for i in 0..55 {
        let id = report(&fx, area, &format!("filler-{i}"), TWO_BLOCKS, false, &[], 0).await;
        fx.repo
            .db()
            .writer()
            .execute(sqlx::query("DELETE FROM cards WHERE track_id = ?1").bind(id))
            .await
            .unwrap();
    }
    for (title, hidden) in [("same", false), ("hidden sibling", true), ("", false)] {
        let track = report(&fx, area, title, TWO_BLOCKS, true, &["only"], 1).await;
        let sibling = report(&fx, area, title, TWO_BLOCKS, true, &[], 2).await;
        if hidden {
            fx.repo
                .db()
                .writer()
                .execute(
                    sqlx::query("UPDATE tracks SET purpose = ?1 WHERE id = ?2")
                        .bind(crate::AREA_CHAT_PURPOSE)
                        .bind(&sibling),
                )
                .await
                .unwrap();
        } else {
            fx.repo
                .db()
                .writer()
                .execute(sqlx::query("DELETE FROM cards WHERE track_id = ?1").bind(&sibling))
                .await
                .unwrap();
        }
        let body = mentions(&fx, area, title, Some(&track)).await;
        let candidate = body["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["track_id"] == track)
            .unwrap();
        assert_eq!(
            code_text(candidate["insert"].as_str().unwrap()),
            format!(
                "/same~{}/{title}~{}/report.md",
                &area.as_str()[..8],
                &track[..8]
            )
        );
        assert_inserts_resolve(&fx, area, &body).await;
    }
    // Empty Area names also remain discoverable through their canonical ID suffix.
    fx.repo
        .db()
        .writer()
        .execute(
            sqlx::query("UPDATE areas SET name = '' WHERE id IN (?1, ?2)")
                .bind(area.as_str())
                .bind(other.as_str()),
        )
        .await
        .unwrap();
    let body = mentions(&fx, area, "same", None).await;
    assert!(
        code_text(body["tracks"][0]["insert"].as_str().unwrap())
            .starts_with(&format!("/~{}/", &area.as_str()[..8]))
    );
    assert_inserts_resolve(&fx, area, &body).await;
}

#[tokio::test]
async fn system_area_offers_no_named_reports_blocks_or_tags() {
    let fx = boot().await;
    let area = &fx.areas[0];
    let own = report(&fx, area, "system own", TWO_BLOCKS, true, &["secret"], 1).await;
    report(
        &fx,
        area,
        "system sibling",
        TWO_BLOCKS,
        true,
        &["secret"],
        2,
    )
    .await;
    fx.repo
        .db()
        .writer()
        .execute(sqlx::query("UPDATE areas SET kind = 'system' WHERE id = ?1").bind(area.as_str()))
        .await
        .unwrap();
    for query in ["", "secret", "system", "goal"] {
        assert_eq!(
            mentions(&fx, area, query, Some(&own)).await,
            json!({"tags": [], "tracks": [], "blocks": []})
        );
    }
    // Own Report reads retain their original capability, independently of name discovery.
    let cards = fx.repo.cards_by_track(&own).await.unwrap();
    let card = cards.iter().find(|c| c.kind == "track-report").unwrap();
    assert_eq!(
        crate::track_report_read::load_report_doc_snapshot(fx.repo.as_ref(), card.id.as_str())
            .await
            .unwrap()
            .body,
        TWO_BLOCKS
    );
}

async fn resolve_insert(fx: &Fixture, area: &AreaId, path: &str) -> String {
    use crate::mcp_server::registry::ToolCallIdentity;
    let bound = fx
        .repo
        .tracks_by_area(area.as_str())
        .await
        .unwrap()
        .remove(0);
    let identity = ToolCallIdentity {
        card_id: "mention-reader".into(),
        role: crate::model::CardRole::Planner,
        provider: calm_types::runtime::AgentProvider::Codex,
        session_id: "mentions".into(),
        track_id: Some(bound.id.to_string()),
        area_id: area.to_string(),
        thread_id: "mentions".into(),
    };
    let scope = crate::report_paths::resolve(&fx.ctx, &identity, &bound, path)
        .await
        .unwrap();
    let crate::report_paths::Scope::Report(resolved_area, track) = scope else {
        panic!("not a Report: {path}")
    };
    assert_eq!(resolved_area, area.as_str());
    let target = fx.repo.track_get(&track).await.unwrap().unwrap();
    let (card, _) = crate::mcp_server::tools::track_report::load_report_for_track(&fx.ctx, &target)
        .await
        .unwrap();
    crate::track_report_read::load_report_doc_snapshot(fx.repo.as_ref(), card.id.as_str())
        .await
        .unwrap();
    track
}
