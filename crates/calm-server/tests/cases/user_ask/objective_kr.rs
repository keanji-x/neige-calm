//! KR display through the same production projector as the approval/evaluation tests.
use super::objective_approval::{define, definition, request, setup};
use super::*;
use calm_truth::objective_host::{self, InputSource, ObjectiveInput, PlannerRequestId};

const REV: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NEXT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

async fn table(boot: &Boot) -> Value {
    calm_server::track_planner_context::TrackPlannerContextProjector::new(
        boot.repo.clone(),
        boot.events.clone(),
        boot.ctx.write.clone(),
    )
    .unwrap()
    .recompute_track(boot.track_id.as_str())
    .await
    .unwrap();
    boot.repo
        .overlays_for("track", boot.track_id.as_str())
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.kind == "objective.kr")
        .unwrap()
        .payload["cell"]["table"]
        .clone()
}

async fn planner_event(boot: &Boot, event: objective::Event, id: &str) {
    objective_host::react(
        &boot.repo.sqlite_db().unwrap(),
        ObjectiveInput {
            track_id: boot.track_id.clone(),
            event,
            source: InputSource::Planner {
                principal: calm_types::worker::Principal::Agent {
                    session_id: PLANNER_SESSION_ID.into(),
                    track_id: boot.track_id.clone(),
                    area_id: boot.area_id.clone(),
                },
                request_id: PlannerRequestId::new(id),
            },
        },
    )
    .await
    .unwrap();
}

// Executor outputs enter through the host's authorized IntentJob seam. This does not
// execute commands or implement an executor; the production engine combines the samples.
async fn measure(boot: &Boot, revision: &str, changed: bool) {
    planner_event(
        boot,
        objective::Event::Requested {
            objective: objective::ids::ObjectiveKey::new("kr"),
            revision: objective::ids::Revision::new(revision),
        },
        revision,
    )
    .await;
    let db = boot.repo.sqlite_db().unwrap();
    for job in objective_host::unsettled_intents_for_track(&db, &boot.track_id)
        .await
        .unwrap()
    {
        let objective::Intent::RunCheck {
            objective,
            evaluation,
            check,
            revision,
            sample,
        } = job.intent
        else {
            continue;
        };
        let output = match check.as_str() {
            "cmd" => json!({"kind":"command","output":{"exit_code":if changed {7} else {0}}}),
            "lower" | "higher" => {
                json!({"kind":"metric","output":{"value":if changed {3.5} else {5.0}}})
            }
            "majority" | "median" => {
                json!({"kind":"evaluator","output":{"judgements":[{"item":"quality","trend":if changed {"worse"} else {"same"},"evidence":[{"path":"src/lib.rs","line":1}]}],"findings":{"blocking":0,"major":if changed {1} else {0},"minor":0}}})
            }
            _ => json!({"kind":"fact","output":{"holds":!changed}}),
        };
        let mut tx = calm_server::db::sqlite::begin_immediate_tx(&db)
            .await
            .unwrap();
        objective_host::intent_mark_dispatched_tx(&mut tx, &job.id)
            .await
            .unwrap();
        objective_host::react_tx(
            &mut tx,
            &ObjectiveInput {
                track_id: boot.track_id.clone(),
                source: InputSource::IntentJob { intent: job.id },
                event: objective::Event::CheckFinished {
                    objective,
                    evaluation,
                    check,
                    revision,
                    sample,
                    output: serde_json::from_value(output).unwrap(),
                },
            },
            1,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
}

async fn fixture(measurement: bool) -> Boot {
    let (boot, block) = setup().await;
    let mut d = definition();
    d["mode"] = json!({"loop":{"plateau":10}});
    let configs = [
        ("cmd", json!({"kind":"command","params":{"command":"true"}})),
        (
            "lower",
            json!({"kind":"metric","params":{"command":"bench","direction":"lower","tolerance":0.2}}),
        ),
        (
            "higher",
            json!({"kind":"metric","params":{"command":"bench","direction":"higher"}}),
        ),
        (
            "majority",
            json!({"kind":"evaluator","params":{"rubric":[{"id":"quality","criterion":"Quality"}],"samples":1,"aggregate":"majority"}}),
        ),
        (
            "median",
            json!({"kind":"evaluator","params":{"rubric":[{"id":"quality","criterion":"Quality"}],"samples":1,"aggregate":"median"}}),
        ),
        (
            "task",
            json!({"kind":"fact","params":{"fact":"task_accepted","subject":"finish"}}),
        ),
        (
            "pr",
            json!({"kind":"fact","params":{"fact":"pr_merged","subject":"#42"}}),
        ),
        (
            "ask",
            json!({"kind":"fact","params":{"fact":"ask_answered","subject":"123"}}),
        ),
    ];
    d["checks"] = configs
        .into_iter()
        .filter(|(id, _)| !measurement || !matches!(*id, "higher" | "median"))
        .map(|(id, config)| json!({"id":id,"role":"ratchet","config":config}))
        .collect();
    define(&boot, Some(block), d).await;
    if measurement {
        let id = request(&boot).await["ask_id"].as_i64().unwrap();
        assert_eq!(
            answer(&boot, id, json!([{"option":0}])).await.0,
            StatusCode::NO_CONTENT
        );
    }
    boot
}

#[tokio::test]
async fn objective_kr_no_data_and_targets_for_every_check_kind() {
    let boot = fixture(false).await;
    let table = table(&boot).await;
    for (key, label) in [("current", "当前"), ("target", "目标")] {
        assert!(
            table["columns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["key"] == key && c["label"] == label)
        );
    }
    assert_eq!(table["rows"][0]["current"], "—");
    assert_eq!(
        table["rows"][0]["target"],
        concat!(
            "cmd: 退出码 0；lower: ↓ 不劣于基线（±0.2）；higher: ↑ 不劣于基线；",
            "majority: rubric 1 项 · 多数；median: rubric 1 项 · 中位；",
            "task: 任务 finish 已被接受；pr: PR #42 已合并；ask: 问询 123 已回答"
        )
    );
}

const BASELINE: &str = concat!(
    "cmd: 退出码 0（基线：退出码 0）；",
    "lower: 5（基线：5）；",
    "majority: 样本1 quality:持平 · blocking 0/major 0/minor 0（基线：样本1 quality:持平 · blocking 0/major 0/minor 0）；",
    "task: 成立（基线：成立）；",
    "pr: 成立（基线：成立）；",
    "ask: 成立（基线：成立）",
);
const CHANGED: &str = concat!(
    "cmd: 退出码 7（基线：退出码 0）；",
    "lower: 3.5（基线：5）；",
    "majority: 样本1 quality:退步 · blocking 0/major 1/minor 0（基线：样本1 quality:持平 · blocking 0/major 0/minor 0）；",
    "task: 不成立（基线：成立）；",
    "pr: 不成立（基线：成立）；",
    "ask: 不成立（基线：成立）",
);

async fn keep_first(boot: &Boot) {
    measure(boot, REV, false).await;
    let current = table(boot).await["rows"][0]["current"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(current.contains("cmd: 退出码 0（基线：—）"));
    assert!(current.contains("lower: 5（基线：—）"));
    planner_event(
        boot,
        objective::Event::Settled {
            objective: objective::ids::ObjectiveKey::new("kr"),
            evaluation: objective::ids::EvaluationId::new(0),
            decision: objective::ledger::Decision::Keep,
            note: "baseline".into(),
        },
        "keep",
    )
    .await;
}

#[tokio::test]
async fn objective_kr_ready_overrides_ledger_and_shows_baseline() {
    let boot = fixture(true).await;
    keep_first(&boot).await;
    measure(&boot, NEXT, true).await;
    let before = objective_host::load_state(&boot.repo.sqlite_db().unwrap(), &boot.track_id)
        .await
        .unwrap();
    let table = table(&boot).await;
    assert_eq!(table["rows"][0]["current"], CHANGED);
    assert_eq!(table["rows"][0]["verdict"], "regression");
    assert_eq!(
        objective_host::load_state(&boot.repo.sqlite_db().unwrap(), &boot.track_id)
            .await
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn objective_kr_latest_ledger_including_discard_and_running_fallback() {
    let boot = fixture(true).await;
    keep_first(&boot).await;
    assert_eq!(table(&boot).await["rows"][0]["current"], BASELINE);
    measure(&boot, NEXT, true).await;
    planner_event(
        &boot,
        objective::Event::Settled {
            objective: objective::ids::ObjectiveKey::new("kr"),
            evaluation: objective::ids::EvaluationId::new(1),
            decision: objective::ledger::Decision::Discard,
            note: "discard".into(),
        },
        "discard",
    )
    .await;
    assert_eq!(table(&boot).await["rows"][0]["current"], CHANGED);
    planner_event(
        &boot,
        objective::Event::Requested {
            objective: objective::ids::ObjectiveKey::new("kr"),
            revision: objective::ids::Revision::new("cccccccccccccccccccccccccccccccccccccccc"),
        },
        "running",
    )
    .await;
    assert_eq!(table(&boot).await["rows"][0]["current"], CHANGED);
}
