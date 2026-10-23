//! Approval semantics stay in the engine; every objective, including stages, is visible here.
use super::*;

pub(super) const KIND: &str = "objective.kr";

pub(super) fn unit(state: &objective::State) -> Result<DataUnit> {
    let rows = state
        .objectives
        .values()
        .map(|objective| {
            [
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
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), TableScalar::Text(TableText(value.into()))))
            .collect()
        })
        .collect();
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
                columns: [("key", "标识"), ("title", "标题"), ("approval", "细则")]
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
