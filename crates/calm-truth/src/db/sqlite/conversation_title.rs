//! Indexed, bounded candidate batches; only conversations without a visible you advance.
use std::collections::BTreeMap;

use super::SqlxRepo;
use crate::conversation_title::{ConversationTitleInput, ConversationTitleSource};
use crate::error::Result;
use sqlx::{Row, SqliteConnection};

// A per-card batch size, never a cap on how far the first visible input can be found.
const CANDIDATE_BATCH: i64 = 32;
const SOURCES: &str = r#"
SELECT c.id AS card_id, h.id AS item_id, h.params, h.input_segments
FROM json_each(?1) request
JOIN cards c ON c.id = json_extract(request.value, '$.card_id')
LEFT JOIN harness_items h ON h.id IN (
    SELECT candidate.id FROM harness_items candidate
    WHERE candidate.card_id = c.id
      AND candidate.id > json_extract(request.value, '$.after_id')
      AND candidate.method = 'item/completed'
      AND candidate.item_type IN ('userMessage', 'user_message')
    ORDER BY candidate.id LIMIT ?2
)
WHERE c.role IN ('planner', 'assistant')
ORDER BY c.id, h.id
"#;

async fn sources_on_connection(
    connection: &mut SqliteConnection,
    ids: &[String],
) -> Result<Vec<ConversationTitleSource>> {
    let mut active: BTreeMap<String, i64> = ids.iter().map(|id| (id.clone(), 0)).collect();
    let mut sources: BTreeMap<String, ConversationTitleSource> = BTreeMap::new();
    while !active.is_empty() {
        let requests: Vec<_> = active
            .iter()
            .map(
                |(card_id, after_id)| serde_json::json!({"card_id": card_id, "after_id": after_id}),
            )
            .collect();
        let rows = sqlx::query(SOURCES)
            .bind(serde_json::to_string(&requests)?)
            .bind(CANDIDATE_BATCH)
            .fetch_all(&mut *connection)
            .await?;
        let mut progress: BTreeMap<String, (i64, i64)> = BTreeMap::new();
        for row in rows {
            let card_id: String = row.try_get("card_id")?;
            let source =
                sources
                    .entry(card_id.clone())
                    .or_insert_with(|| ConversationTitleSource {
                        card_id: card_id.clone(),
                        input: None,
                    });
            let Some(item_id) = row.try_get::<Option<i64>, _>("item_id")? else {
                continue;
            };
            let (count, last_id) = progress.entry(card_id).or_default();
            *count += 1;
            *last_id = item_id;
            if source.input.is_some() {
                continue;
            }
            let input = match row.try_get::<Option<String>, _>("input_segments")? {
                Some(segments) => {
                    ConversationTitleInput::Structured(serde_json::from_str(&segments)?)
                }
                None => ConversationTitleInput::Legacy(row.try_get("params")?),
            };
            if input.visible_title().is_some() {
                source.input = Some(input);
            }
        }
        active = progress
            .into_iter()
            .filter_map(|(id, (count, last_id))| {
                (count == CANDIDATE_BATCH && sources[&id].input.is_none()).then_some((id, last_id))
            })
            .collect();
    }
    Ok(sources.into_values().collect())
}

pub(super) async fn context(
    repo: &SqlxRepo,
    ids: &[String],
) -> Result<crate::conversation_title::ConversationTitleContext> {
    if ids.is_empty() {
        return Ok(crate::conversation_title::ConversationTitleContext {
            sources: Vec::new(),
            runtimes: Default::default(),
        });
    }
    // Claim writer eligibility before any SELECT: shared-cache readers must not
    // hold a table lock while waiting on an existing writer on another table.
    // This serializes metadata with writers until all local reads commit.
    let mut tx = super::begin_immediate_tx(repo.db()).await?;
    let sources = sources_on_connection(&mut tx, ids).await?;
    let runtimes =
        super::session_projection::runtime_get_projectable_for_cards_on_connection(&mut tx, ids)
            .await?;
    tx.commit().await?;
    Ok(crate::conversation_title::ConversationTitleContext { sources, runtimes })
}

#[cfg(test)]
mod tests {
    use super::super::session_conversation_tests::card;
    use super::*;
    use crate::db::{RepoOutOfDomain, RepoRead};
    use serde_json::json;

    pub(super) async fn insert(
        repo: &SqlxRepo,
        card: &str,
        track: &str,
        segments: Option<serde_json::Value>,
        text: &str,
    ) -> i64 {
        repo.harness_item_insert(
            "retired",
            card,
            track,
            "thread",
            None,
            None,
            Some("userMessage"),
            "item/completed",
            &json!({"item":{"content":[{"text":text}]}}).to_string(),
            segments.map(|s| s.to_string()).as_deref(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn conversation_title_shared_writer_admission_preserves_read_capacity() {
        use std::future::{Future, poll_fn};
        use std::task::Poll;
        use std::time::Duration;
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let (card, _) = card(&repo).await;
        // Occupy the shared admission lease without taking an SQLite lock:
        // BEGIN on another pool would succeed, so SQLite contention cannot mask a bypass.
        let holder = repo.db().writer_pool().acquire().await.unwrap();
        let readers = repo.db().read_pool();
        let mut held = Vec::new();
        for _ in 0..readers.options().get_max_connections() {
            held.push(readers.acquire().await.unwrap());
        }
        drop(held.pop());
        tokio::time::timeout(Duration::from_secs(10), async {
            while readers.num_idle() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let ids = [card];
        let title = repo.conversation_title_context(&ids);
        tokio::pin!(title);
        poll_fn(|cx| {
            assert!(title.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let preserved = readers.num_idle() == 1;
        if preserved {
            let count: i64 = tokio::time::timeout(
                Duration::from_secs(10),
                sqlx::query_scalar("SELECT count(*) FROM settings").fetch_one(readers),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(count, 0);
        }
        // Release before asserting so a broken implementation can cleanly unwind.
        drop(holder);
        tokio::time::timeout(Duration::from_secs(10), title)
            .await
            .unwrap()
            .unwrap();
        drop(held);
        assert!(
            preserved,
            "title must queue on the shared writer without consuming read capacity"
        );
    }

    #[tokio::test]
    async fn conversation_title_indexed_candidates_isolated_and_replaceable() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let (a, track) = card(&repo).await;
        let (b, other_track) = card(&repo).await;
        let (empty, _) = card(&repo).await;
        // The source query intentionally selects conversation roles, independent of provider.
        sqlx::query("UPDATE cards SET role='assistant' WHERE id IN (?1,?2,?3)")
            .bind(&a)
            .bind(&b)
            .bind(&empty)
            .execute(repo.db().writer_pool())
            .await
            .unwrap();
        insert(
            &repo,
            &a,
            &track,
            Some(json!([{"presentation":"system","text":"system-only","attachments":[]}])),
            "not a user",
        )
        .await;
        let first = insert(&repo, &a, &track, Some(json!([
            {"presentation":"system","text":"context","attachments":[]},
            {"presentation":"user","text":"User says:\nOriginal first\nsecond","attachments":[]},
            {"presentation":"user","text":"later in same batch","attachments":[]}
        ])), "flattened provider words are not authoritative").await;
        for _ in 0..305 {
            insert(&repo, &a, &track, None, "latest page title").await;
        }
        insert(&repo, &b, &other_track, None, "User says:\nOther card").await;
        let values = repo
            .conversation_title_context(&[a.clone(), b.clone(), empty.clone(), "missing".into()])
            .await
            .unwrap()
            .sources;
        assert_eq!(values.len(), 3);
        let title = |id: &str| {
            values
                .iter()
                .find(|s| s.card_id == id)
                .unwrap()
                .input
                .as_ref()
                .and_then(|i| i.title())
        };
        assert_eq!(title(&a), Some("Original first".into()));
        assert_eq!(title(&b), Some("Other card".into()));
        for provider in ["codex", "claude", "opencode"] {
            sqlx::query("UPDATE cards SET payload=?1 WHERE id=?2")
                .bind(json!({"assistant_provider":provider}).to_string())
                .bind(&b)
                .execute(repo.db().writer_pool())
                .await
                .unwrap();
            assert_eq!(
                repo.conversation_title_context(std::slice::from_ref(&b))
                    .await
                    .unwrap()
                    .sources[0]
                    .input
                    .as_ref()
                    .unwrap()
                    .title(),
                Some("Other card".into())
            );
        }
        assert!(
            values
                .iter()
                .find(|s| s.card_id == empty)
                .unwrap()
                .input
                .is_none()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM worker_sessions")
                .fetch_one(repo.db().read_pool())
                .await
                .unwrap(),
            0
        );
        let plan = sqlx::query(&format!("EXPLAIN QUERY PLAN {SOURCES}"))
            .bind(serde_json::json!([{ "card_id": a, "after_id": 0 }, { "card_id": b, "after_id": 0 }]).to_string())
            .bind(CANDIDATE_BATCH)
            .fetch_all(repo.db().read_pool())
            .await
            .unwrap();
        let details: Vec<String> = plan.iter().map(|row| row.get("detail")).collect();
        assert!(
            details
                .iter()
                .any(|line| line.contains("SEARCH candidate USING INDEX")
                    && line.contains("card_id=?")
                    && line.contains("id>?")),
            "{details:?}"
        );
        assert!(
            !details
                .iter()
                .any(|line| line.contains("SCAN candidate") || line.starts_with("SCAN h")),
            "{details:?}"
        );
        let mut tx = repo.db().writer_pool().begin().await.unwrap();
        assert_eq!(
            super::super::transcript_delete_thread_suffix_tx(
                &mut tx,
                &a,
                "thread",
                first - 1,
                first,
            )
            .await
            .unwrap(),
            1
        );
        tx.commit().await.unwrap();
        let replaced = repo.conversation_title_context(&[a]).await.unwrap().sources;
        assert_eq!(
            replaced[0].input.as_ref().unwrap().title(),
            Some("latest page title".into())
        );
    }

    #[tokio::test]
    async fn conversation_title_invisible_records_and_system_do_not_block() {
        let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
        let (a, track) = card(&repo).await;
        sqlx::query("UPDATE cards SET role='planner' WHERE id=?1")
            .bind(&a)
            .execute(repo.db().writer_pool())
            .await
            .unwrap();
        insert(
            &repo,
            &a,
            &track,
            Some(json!([{"presentation":"system","text":"context","attachments":[]}])),
            "system",
        )
        .await;
        insert(
            &repo,
            &a,
            &track,
            Some(json!([{"presentation":"user","text":"","attachments":[]}])),
            "",
        )
        .await;
        // Continue across multiple bounded batches, with no silent history cutoff.
        for _ in 0..97 {
            insert(
                &repo,
                &a,
                &track,
                None,
                "## Track state changes since your last turn\nchanged\n\n---\n\nUser says:\n  ",
            )
            .await;
        }
        insert(
            &repo,
            &a,
            &track,
            Some(json!([
                {"presentation":"user","text":"User says:\n  ","attachments":[]},
                {"presentation":"user","text":"later input","attachments":[]},
                {"presentation":"user","text":"later segment","attachments":[]}
            ])),
            "flattened",
        )
        .await;
        insert(&repo, &a, &track, None, "later row").await;
        let batch = sqlx::query(SOURCES)
            .bind(json!([{"card_id":a,"after_id":0}]).to_string())
            .bind(CANDIDATE_BATCH)
            .fetch_all(repo.db().read_pool())
            .await
            .unwrap();
        assert_eq!(batch.len(), CANDIDATE_BATCH as usize);
        let value = repo.conversation_title_context(&[a]).await.unwrap().sources;
        assert!(value[0].input.is_some());
        assert_eq!(
            value[0].input.as_ref().unwrap().title(),
            Some("later input".into())
        );
    }
}

#[cfg(test)]
mod snapshot_tests;
