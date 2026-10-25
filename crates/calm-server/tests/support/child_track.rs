//! Durable candidate fixtures for child result-reference boundary tests.
use calm_server::db::Repo;
use serde_json::{Value, json};

pub async fn seed_candidates(repo: &dyn Repo, track_id: &str, card_id: &str) -> Value {
    let db = repo.sqlite_db().unwrap();
    let mut latest = Value::Null;
    // Insert out of chronological order so insertion order cannot stand in for latest.
    for (id, at) in [("newest", 20_i64), ("older", 10)] {
        sqlx::query("INSERT INTO workspace_leases (lease_id,path,card_id,track_id,state,lease_owner,created_at_ms,updated_at_ms) VALUES(?1,?1,?2,?3,'released','test',1,1)")
            .bind(id).bind(card_id).bind(track_id).execute(db.writer_pool()).await.unwrap();
        sqlx::query("INSERT INTO task_git_deliveries(delivery_id,track_id,producer_attempt_id,card_id,lease_id,ordinal,operation_key,forge_idempotency_key,created_at_ms) VALUES(?1,?2,?1,?3,?1,1,?1,?1,?4)")
            .bind(id).bind(track_id).bind(card_id).bind(at).execute(db.writer_pool()).await.unwrap();
        sqlx::query("INSERT INTO task_candidates(candidate_id,track_id,producer_attempt_id,card_id,lease_id,repo_root,git_common_dir,branch,base_sha,commit_sha,base_is_ancestor,ref_name,created_at_ms) VALUES(?1,?2,?1,?3,?1,'/checkout','/common','topic','base',?1,1,'refs/test',?4)")
            .bind(id).bind(track_id).bind(card_id).bind(at).execute(db.writer_pool()).await.unwrap();
        if id == "newest" {
            latest = json!({"candidate_id":id,"track_id":track_id,"producer_attempt_id":id,
                "card_id":card_id,"lease_id":id,"repo_root":"/checkout","git_common_dir":"/common",
                "branch":"topic","base_sha":"base","commit_sha":id,"base_is_ancestor":true,
                "ref_name":"refs/test","created_at_ms":at});
        }
    }
    latest
}
