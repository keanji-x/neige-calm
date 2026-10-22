//! Budget semantics come from the engine; this module only builds a native table.
use super::*;
use calm_truth::objective_host;
use objective::engine::Status;

pub(super) const KIND: &str = "objective.budget";

pub(super) async fn build(pool: &sqlx::SqlitePool, track: &str) -> Result<Option<DataUnit>> {
    let enabled: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM track_capabilities WHERE track_id=? AND capability=?)",
    )
    .bind(track)
    .bind(calm_types::track_capability::TrackCapability::Objectives.as_db_str())
    .fetch_one(pool)
    .await?;
    if !enabled {
        return Ok(None);
    }
    let (_, state) = objective_host::read_state(pool, &TrackId::from(track.to_owned())).await?;
    let rows = state
        .objectives
        .values()
        .map(|objective| {
            let used = objective.used(&state.usage);
            let definition = &objective.definition;
            let status = match objective.status {
                Status::Open => "进行中",
                Status::Done { .. } => "已完成",
                Status::Closed { .. } => "已关闭",
            };
            [
                (
                    "key",
                    TableScalar::Text(TableText(definition.key.as_str().into())),
                ),
                (
                    "title",
                    TableScalar::Text(TableText(definition.title.clone())),
                ),
                ("status", TableScalar::Text(TableText(status.into()))),
                (
                    "budget",
                    TableScalar::Number(definition.budget.tokens as f64),
                ),
                ("used", TableScalar::Number(used.context_tokens as f64)),
                ("turns", TableScalar::Number(used.turns as f64)),
                (
                    "exceeded",
                    TableScalar::Text(TableText(
                        if definition.budget.exhausted(&used).is_empty() {
                            "否"
                        } else {
                            "是"
                        }
                        .into(),
                    )),
                ),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect()
        })
        .collect();
    Ok(Some(DataUnit {
        snapshot: Snapshot {
            id: KIND.into(),
            observed_at: None,
            produced_at: None,
        },
        cell: Component::Table {
            id: KIND.into(),
            title: "阶段预算".into(),
            table: serde_json::to_value(InlineTable {
                columns: [
                    ("key", "标识"),
                    ("title", "标题"),
                    ("status", "状态"),
                    ("budget", "预算"),
                    ("used", "已用"),
                    ("turns", "回合"),
                    ("exceeded", "超预算"),
                ]
                .into_iter()
                .map(|(key, label)| TableColumn {
                    key: key.into(),
                    label: label.into(),
                    align: None,
                })
                .collect(),
                rows,
                caption: None,
                highlight: None,
            })?,
        },
    }))
}
