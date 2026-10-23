//! Approval semantics stay in the engine; every objective, including stages, is visible here.
use super::*;

pub(super) const KIND: &str = "objective.kr";

pub(super) fn unit(state: &objective::State) -> Result<DataUnit> {
    let rows = state
        .objectives
        .values()
        .map(|objective| {
            let verdict = objective
                .latest_verdict
                .as_ref()
                .map(|v| serde_json::to_value(v.kind))
                .transpose()?
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| "—".into());
            let evaluation = match &objective.evaluation {
                Some(objective::engine::Evaluation::Running { samples, .. }) => {
                    let arrived: usize = samples.values().map(|s| s.len()).sum();
                    let expected: u32 = objective
                        .definition
                        .checks
                        .iter()
                        .map(|c| c.config.samples())
                        .sum();
                    format!("{arrived}/{expected}")
                }
                Some(objective::engine::Evaluation::Ready { .. }) => "待结算".into(),
                None => "—".into(),
            };
            let baseline = objective
                .ledger
                .baseline
                .as_ref()
                .map(|b| b.revision.as_str().chars().take(7).collect::<String>())
                .unwrap_or_else(|| "—".into());
            let ledger = objective.ledger.entries.len().to_string();
            Ok([
                ("key", objective.definition.key.as_str()),
                ("title", objective.definition.title.as_str()),
                (
                    "approval",
                    if objective.is_approved() {
                        "已批准"
                    } else {
                        "未批准"
                    },
                ),
                ("verdict", verdict.as_str()),
                ("evaluation", evaluation.as_str()),
                ("baseline", baseline.as_str()),
                ("ledger", ledger.as_str()),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), TableScalar::Text(TableText(value.into()))))
            .collect())
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(DataUnit {
        snapshot: Snapshot {
            id: KIND.into(),
            observed_at: None,
            produced_at: None,
        },
        cell: Component::Table {
            id: KIND.into(),
            title: "KR".into(),
            table: serde_json::to_value(InlineTable {
                columns: [
                    ("key", "标识"),
                    ("title", "标题"),
                    ("approval", "细则"),
                    ("verdict", "最新判定"),
                    ("evaluation", "进行中评估"),
                    ("baseline", "基线 revision"),
                    ("ledger", "账本条数"),
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
    })
}
