use super::*;
use crate::report_read_ledger::{delta, history::probe};
use crate::track_report::ReportBlock;
use calm_truth::db::sqlite::report_knowledge;
use std::time::Instant;

async fn seed(body: &str) -> Pair {
    let p = Pair::new().await;
    mcp(&p, THREAD, "neige_report_read", json!({})).await;
    mcp(
        &p,
        THREAD,
        "neige_report_write",
        json!({"body":body, "message":"seed"}),
    )
    .await;
    p.state
        .harness
        .get(&p.workers[0])
        .unwrap()
        .pause_issuance_for_dev();
    p
}

async fn blocks(p: &Pair) -> Vec<ReportBlock> {
    crate::track_report::resolve_report_for_track(p.fixture.repo.as_ref(), &p.track)
        .await
        .unwrap()
        .2
        .blocks
        .unwrap()
}

async fn edit(p: &Pair, block: &ReportBlock, text: &str) {
    let response = p
        .app
        .clone()
        .oneshot(
            Request::patch(format!(
                "/api/tracks/{}/report/blocks/{}",
                p.track, block.id
            ))
            .header(header::COOKIE, &p.cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"kind":"prose", "markdown":text,
            "payload":null, "ifBlockRev":block.rev})
                .to_string(),
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn suggestion(p: &Pair, worker: usize) -> String {
    delta::for_track(
        p.fixture.repo.as_ref(),
        &p.track.clone().into(),
        &p.workers[worker],
    )
    .await
    .unwrap()
}

async fn report_id(p: &Pair) -> String {
    crate::track_report::resolve_report_for_track(p.fixture.repo.as_ref(), &p.track)
        .await
        .unwrap()
        .1
        .id
        .to_string()
}

async fn knowledge(p: &Pair, worker: usize) -> Option<crate::report_read_ledger::LastRead> {
    report_knowledge::load(
        p.fixture.repo.as_ref(),
        &p.workers[worker],
        &report_id(p).await,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn report_knowledge_delta_large_snapshot_releases_writer_and_freezes_document_and_ledger() {
    let body = (0..38)
        .map(|i| format!("# Section {i}\n\n{}\n\n", "📄".repeat(1200)))
        .collect::<String>();
    let p = seed(&body).await;
    let first = blocks(&p).await.remove(0);
    let edit_started = Instant::now();
    edit(&p, &first, "# Section 0\n\nintermediate\n").await;
    let edit_budget = (edit_started.elapsed() * 4).max(Duration::from_secs(5));
    let before = knowledge(&p, 0).await;
    probe::reset();
    let started = Instant::now();
    let expected = suggestion(&p, 0).await;
    eprintln!(
        "large fixture bytes={} delta_ms={:.3} loads={:?} parses={:?}",
        body.len(),
        started.elapsed().as_secs_f64() * 1000.0,
        probe::counts(),
        probe::parses()
    );
    assert_eq!(
        knowledge(&p, 0).await,
        before,
        "suggestions never certify a read"
    );
    assert_eq!(probe::counts().values().sum::<usize>(), 1);
    assert_eq!(probe::parses().values().sum::<usize>(), 1);

    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("report-delta-render", &p.workers[0], pause.clone());
    let (repo, track, worker) = (
        p.fixture.repo.clone(),
        p.track.clone().into(),
        p.workers[0].clone(),
    );
    let rendering =
        tokio::spawn(async move { delta::for_track(repo.as_ref(), &track, &worker).await });
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    // The real write and the real read/ledger update must finish while rendering is parked.
    let report = report_id(&p).await;
    tokio::time::timeout(
        Duration::from_secs(5),
        p.fixture.repo.db().writer().execute(
            sqlx::query("UPDATE cards SET updated_at=updated_at WHERE id=?").bind(&report),
        ),
    )
    .await
    .expect("parked rendering must release writer admission")
    .unwrap();
    // Large CRDT edits have their own CPU cost. Budget against the real uncontended edit,
    // while the cheap writer probe above isolates admission from that document work.
    let latest = blocks(&p).await.remove(0);
    tokio::time::timeout(edit_budget, edit(&p, &latest, "# Section 0\n\nlatest\n"))
        .await
        .expect("rendering must not hold writer admission");
    tokio::time::timeout(edit_budget, mcp(&p, THREAD, "neige_report_read", json!({})))
        .await
        .expect("ledger may advance while the old suggestion renders");
    assert_ne!(knowledge(&p, 0).await, before);
    let crate::report_read_ledger::HistoryRef::Commit(commit) =
        &before.as_ref().unwrap().blocks[&first.id].history_ref
    else {
        panic!("located history")
    };
    let tree = calm_truth::track_vcs::tree_at(p.fixture.repo.pool(), commit)
        .await
        .unwrap()
        .unwrap();
    let blob = &tree.entries[&format!("cards/{}/.payload.json", report_id(&p).await)].blob_hash;
    p.fixture.repo.db().writer().execute(sqlx::query(
        "UPDATE track_vcs_objects SET bytes=x'00',encoding='raw',decoded_size=NULL WHERE hash=?")
        .bind(blob)).await.unwrap();
    pause.release.notify_one();
    assert_eq!(
        rendering.await.unwrap().unwrap(),
        expected,
        "use captured document AND ledger"
    );
    assert!(suggestion(&p, 0).await.is_empty());
    probe::reset();
    assert!(suggestion(&p, 0).await.is_empty());
    assert!(
        probe::counts().is_empty(),
        "unchanged reports must not prefetch history"
    );
    p.close().await;
}

#[tokio::test]
async fn report_knowledge_delta_history_cache_is_per_exact_commit_and_atomic_call() {
    let p = seed("# One\n\none\n\n# Two\n\ntwo\n").await;
    let original = blocks(&p).await;
    mcp(
        &p,
        "switch-assistant",
        "neige_report_read",
        json!({"blocks":[original[0].id]}),
    )
    .await;
    edit(&p, &original[1], "# Two\n\nsecond baseline\n").await;
    let current = blocks(&p).await;
    mcp(
        &p,
        "switch-assistant",
        "neige_report_read",
        json!({"blocks":[current[1].id]}),
    )
    .await;
    edit(&p, &current[0], "# One\n\none changed\n").await;
    edit(&p, &current[1], "# Two\n\ntwo changed\n").await;
    let known = knowledge(&p, 1).await;
    probe::reset();
    let expected = suggestion(&p, 1).await;
    assert!(
        expected.contains("second baseline") && expected.contains("one changed"),
        "{expected}"
    );
    assert_eq!(probe::counts().len(), 2);
    assert!(probe::counts().values().all(|&n| n == 1));
    assert_eq!(probe::parses().len(), 2);
    assert!(probe::parses().values().all(|&n| n == 1));
    probe::reset();
    let (track, worker) = (p.track.clone().into(), p.workers[1].clone());
    let atomic = crate::db::write_in_tx_typed(p.fixture.repo.as_ref(), move |tx| {
        Box::pin(async move { delta::for_track_tx(tx, &track, &worker).await })
    })
    .await
    .unwrap();
    assert_eq!(atomic, expected);
    assert_eq!(probe::counts().len(), 2);
    assert_eq!(probe::parses().len(), 2);
    assert!(probe::parses().values().all(|&n| n == 1));
    assert_eq!(knowledge(&p, 1).await, known);
    p.close().await;
}

#[tokio::test]
async fn report_knowledge_delta_whole_history_and_changed_blocks_share_decode() {
    let p = seed("# One\n\none\n\n# Two\n\ntwo\n").await;
    mcp(&p, "switch-assistant", "neige_report_read", json!({})).await;
    mcp(
        &p,
        THREAD,
        "neige_report_write",
        json!({
            "body":"# One\n\none changed\n\n# Two\n\ntwo\n\n# Three\n\nadded\n", "message":"extend"
        }),
    )
    .await;
    probe::reset();
    let text = suggestion(&p, 1).await;
    assert!(
        text.contains("\"state\":\"added\"") && text.contains("one changed"),
        "{text}"
    );
    assert_eq!(probe::counts().values().sum::<usize>(), 1);
    assert_eq!(probe::parses().values().sum::<usize>(), 1);
    p.close().await;
}

#[tokio::test]
async fn report_knowledge_delta_cancelled_or_failed_render_releases_connections() {
    let p = seed("# One\n\none\n").await;
    for point in ["report-delta-history", "report-delta-render"] {
        let pause = crate::test_seams::PausePoint::default();
        crate::test_seams::install_pause_for_test(point, &p.workers[0], pause.clone());
        let (repo, track, worker) = (
            p.fixture.repo.clone(),
            p.track.clone().into(),
            p.workers[0].clone(),
        );
        let rendering =
            tokio::spawn(async move { delta::for_track(repo.as_ref(), &track, &worker).await });
        tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
            .await
            .unwrap();
        rendering.abort();
        assert!(rendering.await.unwrap_err().is_cancelled());
    }
    assert_all_read_capacity_released(&p).await;
    let report = report_id(&p).await;
    p.fixture
        .repo
        .db()
        .writer()
        .execute(sqlx::query("UPDATE cards SET body_crdt=x'00' WHERE id=?").bind(&report))
        .await
        .unwrap();
    assert!(
        delta::for_track(
            p.fixture.repo.as_ref(),
            &p.track.clone().into(),
            &p.workers[0]
        )
        .await
        .is_err()
    );
    tokio::time::timeout(
        Duration::from_secs(5),
        p.fixture
            .repo
            .db()
            .writer()
            .execute(sqlx::query("UPDATE cards SET body_crdt=NULL WHERE id=?").bind(&report)),
    )
    .await
    .expect("failure must release transaction and connection")
    .unwrap();
    assert_all_read_capacity_released(&p).await;
    p.close().await;
}

#[tokio::test]
async fn report_knowledge_delta_unavailable_or_ambiguous_history_fails_closed() {
    for case in [
        "missing",
        "wrong_track",
        "malformed",
        "no_blocks",
        "revision",
        "duplicate",
    ] {
        let p = seed("# One\n\none secret baseline\n\n# Two\n\ntwo\n").await;
        let old = blocks(&p).await;
        let report = report_id(&p).await;
        let known = knowledge(&p, 0).await.unwrap();
        let crate::report_read_ledger::HistoryRef::Commit(commit) =
            &known.blocks[&old[0].id].history_ref
        else {
            panic!("located history")
        };
        let tree = calm_truth::track_vcs::tree_at(p.fixture.repo.pool(), commit)
            .await
            .unwrap()
            .unwrap();
        let blob = tree.entries[&format!("cards/{report}/.payload.json")]
            .blob_hash
            .clone();
        edit(&p, &old[0], "# One\n\nnew secret\n").await;
        if case == "missing" {
            p.fixture.repo.db().writer().execute(sqlx::query(
                "UPDATE report_read_ledger_blocks SET history_ref=? WHERE worker_session_id=? AND report_card_id=?")
                .bind(json!({"commit":"absent-history"}).to_string()).bind(&p.workers[0]).bind(&report))
                .await.unwrap();
        } else if case == "wrong_track" {
            let source = p.fixture.repo.track_get(&p.track).await.unwrap().unwrap();
            let other = p
                .fixture
                .repo
                .track_create(crate::model::NewTrack {
                    area_id: source.area_id,
                    title: "other".into(),
                    sort: None,
                    cwd: "/tmp".into(),
                    template_id: None,
                    plugin_scope: None,
                    template_input: None,
                    attach_folder: false,
                    theme: crate::model::RequestTheme::default_dark(),
                })
                .await
                .unwrap();
            p.fixture
                .repo
                .db()
                .writer()
                .execute(
                    sqlx::query("UPDATE track_vcs_commits SET track_id=? WHERE hash=?")
                        .bind(other.id.as_str())
                        .bind(commit),
                )
                .await
                .unwrap();
        } else {
            let (_, _, mut payload) =
                crate::track_report::resolve_report_for_track(p.fixture.repo.as_ref(), &p.track)
                    .await
                    .unwrap();
            payload.doc_rev = known.doc_rev;
            payload.blocks = Some(old.clone());
            match case {
                "no_blocks" => payload.blocks = None,
                "revision" => payload.blocks.as_mut().unwrap()[0].rev += 100,
                "duplicate" => payload.blocks.as_mut().unwrap().push(old[0].clone()),
                _ => {}
            }
            let bytes = if case == "malformed" {
                b"{invalid".to_vec()
            } else {
                serde_json::to_vec(&payload).unwrap()
            };
            p.fixture.repo.db().writer().execute(sqlx::query(
                "UPDATE track_vcs_objects SET bytes=?,encoding='raw',decoded_size=NULL WHERE hash=?")
                .bind(bytes).bind(blob)).await.unwrap();
        }
        probe::reset();
        let text = suggestion(&p, 0).await;
        assert!(text.contains("history unavailable"), "{case}: {text}");
        assert!(
            !text.contains("secret baseline") && !text.contains("new secret"),
            "{case}: {text}"
        );
        assert_eq!(probe::counts().values().sum::<usize>(), 1, "{case}");
        assert_eq!(
            probe::parses().values().sum::<usize>(),
            usize::from(!matches!(case, "missing" | "wrong_track")),
            "{case}"
        );
        p.close().await;
    }
}

#[tokio::test]
async fn report_knowledge_delta_deleted_blocks_and_unknown_tasks_keep_disclosure_rules() {
    let p = seed("# One\n\none\n\n# Two\n\ntwo\n").await;
    mcp(&p, "switch-assistant", "neige_report_read", json!({})).await;
    let old = blocks(&p).await;
    mcp(
        &p,
        THREAD,
        "neige_report_write",
        json!({
            "body":"# Two\n\ntwo\n", "message":"delete first block"
        }),
    )
    .await;
    mcp(&p, THREAD, "neige_report_read", json!({})).await;
    mcp(&p, THREAD, "neige_report_commit", json!({
        "message":"create unknown task",
        "ops":[{"op":"upsert", "kind":"task", "payload":{
            "key":"private-task", "kind":"terminal", "command":"echo task-secret-goal",
            "acceptance":"task-secret-goal", "context":{}, "cwd":"/tmp", "depends_on":[],
            "priority":0, "gate":{"steps":[{"name":"accept", "cmd":"true"}]},
            "declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR, "ready":false
        }}]
    })).await;
    let text = suggestion(&p, 1).await;
    assert!(
        text.contains(&old[0].id) && text.contains("\"current_rev\":null"),
        "{text}"
    );
    assert!(text.contains("unread task"), "{text}");
    assert!(
        !text.contains("task-secret-goal") && !text.contains("private-task"),
        "{text}"
    );
    p.close().await;
}

#[tokio::test]
async fn report_knowledge_delta_wal_read_never_acquires_the_held_writer() {
    let p = seed("# One\n\none\n\n# Two\n\ntwo\n").await;
    let old = blocks(&p).await;
    edit(&p, &old[0], "# One\n\nchanged\n").await;
    let repo = p.fixture.repo.clone();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let writer = tokio::spawn(async move {
        crate::db::write_in_tx_typed(repo.as_ref(), move |_tx| {
            Box::pin(async move {
                entered.send(()).unwrap();
                released.await.unwrap();
                Ok::<_, crate::error::CalmError>(())
            })
        })
        .await
    });
    ready.await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), suggestion(&p, 0)).await;
    release.send(()).unwrap();
    writer.await.unwrap().unwrap();
    let text = result.expect("all Report delta work must complete while writer admission is held");
    assert!(
        text.contains("changed") && text.contains("block_diff"),
        "{text}"
    );
    p.close().await;
}

#[tokio::test]
async fn report_knowledge_delta_wal_history_reads_the_original_snapshot() {
    let p = seed("# One\n\none\n\n# Two\n\ntwo\n").await;
    let old = blocks(&p).await;
    edit(&p, &old[0], "# One\n\nintermediate\n").await;
    let expected = suggestion(&p, 0).await;
    let before = knowledge(&p, 0).await.unwrap();
    let crate::report_read_ledger::HistoryRef::Commit(commit) =
        &before.blocks[&old[0].id].history_ref
    else {
        panic!("located history")
    };
    let tree = calm_truth::track_vcs::tree_at(p.fixture.repo.pool(), commit)
        .await
        .unwrap()
        .unwrap();
    let blob = tree.entries[&format!("cards/{}/.payload.json", report_id(&p).await)]
        .blob_hash
        .clone();
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("report-delta-history", &p.workers[0], pause.clone());
    let (repo, track, worker) = (
        p.fixture.repo.clone(),
        p.track.clone().into(),
        p.workers[0].clone(),
    );
    let reading =
        tokio::spawn(async move { delta::for_track(repo.as_ref(), &track, &worker).await });
    tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
        .await
        .unwrap();
    let latest = blocks(&p).await.remove(0);
    tokio::time::timeout(
        Duration::from_secs(5),
        edit(&p, &latest, "# One\n\nlatest\n"),
    )
    .await
    .expect("WAL read snapshot must allow Report writes");
    tokio::time::timeout(
        Duration::from_secs(5),
        mcp(&p, THREAD, "neige_report_read", json!({})),
    )
    .await
    .expect("WAL snapshot must allow ledger updates");
    p.fixture.repo.db().writer().execute(sqlx::query(
        "UPDATE track_vcs_objects SET bytes=x'00',encoding='raw',decoded_size=NULL WHERE hash=?")
        .bind(blob)).await.unwrap();
    pause.release.notify_one();
    assert_eq!(
        reading.await.unwrap().unwrap(),
        expected,
        "history, ledger and document share one snapshot"
    );
    p.close().await;
}

#[tokio::test]
async fn report_knowledge_delta_many_exact_versions_are_resolved_once() {
    let body = (0..12)
        .map(|i| {
            format!(
                "# Part {i}\n\n{}\n\n",
                format!("original {i}\n").repeat(120)
            )
        })
        .collect::<String>();
    let p = seed(&body).await;
    for i in 0..12 {
        let block = blocks(&p).await.remove(i);
        mcp(
            &p,
            "switch-assistant",
            "neige_report_read",
            json!({"blocks":[block.id]}),
        )
        .await;
        edit(
            &p,
            &block,
            &format!("# Part {i}\n\n{}\n", format!("changed {i}\n").repeat(120)),
        )
        .await;
    }
    let before = knowledge(&p, 1).await.unwrap();
    let versions: std::collections::BTreeSet<_> = before
        .blocks
        .values()
        .map(|block| match &block.history_ref {
            crate::report_read_ledger::HistoryRef::Commit(commit) => commit,
            other => panic!("exact commit required: {other:?}"),
        })
        .collect();
    assert_eq!(versions.len(), 12);
    probe::reset();
    let text = suggestion(&p, 1).await;
    assert!(before.blocks.keys().all(|id| text.contains(id)));
    assert_eq!(probe::counts().len(), 12);
    assert_eq!(probe::parses().len(), 12);
    assert!(probe::counts().values().all(|&count| count == 1));
    assert!(probe::parses().values().all(|&count| count == 1));
    assert_eq!(knowledge(&p, 1).await, Some(before));
    p.close().await;
}

#[tokio::test]
async fn report_knowledge_delta_shared_memory_uses_serialized_snapshot() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            crate::model::NewArea {
                name: "report delta".into(),
                color: "#123456".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(crate::model::NewTrack {
            area_id: area.id,
            title: "memory".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::model::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let report = crate::model::new_id();
    repo.db()
        .writer()
        .execute(
            sqlx::query(
                "INSERT INTO cards \
        (id,track_id,kind,sort,payload,role,deletable,created_at,updated_at) \
        VALUES (?,?,'track-report',-1,?,'reportcard',0,1,1)",
            )
            .bind(&report)
            .bind(track.id.as_str())
            .bind(
                serde_json::to_string(&crate::track_report::TrackReportPayload::new(
                    "",
                    "# One\n\none\n",
                ))
                .unwrap(),
            ),
        )
        .await
        .unwrap();
    assert!(repo.db().try_begin_read_snapshot().await.unwrap().is_none());
    let text = tokio::time::timeout(
        Duration::from_secs(5),
        delta::for_track(&repo, &track.id, "readonly-worker"),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(text.contains("\"state\":\"unread\""), "{text}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM report_read_ledger")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 0, "a suggestion must not certify a read");
    repo.db().close().await;
}

async fn assert_all_read_capacity_released(p: &Pair) {
    let pool = p.fixture.repo.pool();
    let connections = tokio::time::timeout(Duration::from_secs(5), async {
        let mut connections = Vec::new();
        for _ in 0..pool.options().get_max_connections() {
            connections.push(pool.acquire().await.unwrap());
        }
        connections
    })
    .await
    .expect("cancelled or failed snapshots must return every read connection");
    drop(connections);
}
