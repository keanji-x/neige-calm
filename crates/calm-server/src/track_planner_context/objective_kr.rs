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
            let current = current_text(objective);
            let target = objective
                .definition
                .checks
                .iter()
                .map(|check| format!("{}: {}", check.id, target_text(&check.config)))
                .collect::<Vec<_>>()
                .join("；");
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
                ("current", current.as_str()),
                ("target", target.as_str()),
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
                    ("current", "当前"),
                    ("target", "目标"),
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

// Only format recorded outputs. Comparison and evaluator aggregation belong to the engine.
fn output_text(output: &objective::check::CheckOutput) -> String {
    use objective::check::{CheckOutput, Trend};
    match output {
        CheckOutput::Command(output) => format!("退出码 {}", output.exit_code),
        CheckOutput::Metric(output) => output.value.to_string(),
        CheckOutput::Fact(output) => if output.holds { "成立" } else { "不成立" }.into(),
        CheckOutput::Evaluator(output) => output
            .samples
            .iter()
            .enumerate()
            .map(|(index, sample)| {
                let judgements = sample
                    .judgements
                    .iter()
                    .map(|judgement| {
                        let trend = match judgement.trend {
                            Trend::Worse => "退步",
                            Trend::Same => "持平",
                            Trend::Better => "改善",
                        };
                        format!("{}:{trend}", judgement.item)
                    })
                    .collect::<Vec<_>>()
                    .join("、");
                let findings = &sample.findings;
                format!(
                    "样本{} {judgements} · blocking {}/major {}/minor {}",
                    index + 1,
                    findings.blocking,
                    findings.major,
                    findings.minor
                )
            })
            .collect::<Vec<_>>()
            .join(" / "),
    }
}

fn current_text(objective: &objective::engine::ObjectiveState) -> String {
    let results = match &objective.evaluation {
        Some(objective::engine::Evaluation::Ready { results, .. }) => Some(results),
        _ => objective.ledger.entries.last().map(|entry| &entry.results),
    };
    let baseline = objective.ledger.baseline_results();
    if results.is_none() && baseline.is_none() {
        return "—".into();
    }
    objective
        .definition
        .checks
        .iter()
        .map(|check| {
            let current = results
                .and_then(|results| results.get(&check.id))
                .map(output_text)
                .unwrap_or_else(|| "—".into());
            let baseline = baseline
                .and_then(|results| results.get(&check.id))
                .map(output_text)
                .unwrap_or_else(|| "—".into());
            format!("{}: {current}（基线：{baseline}）", check.id)
        })
        .collect::<Vec<_>>()
        .join("；")
}

fn target_text(config: &objective::check::CheckConfig) -> String {
    use objective::check::{Aggregate, CheckConfig, Direction, Fact};
    match config {
        CheckConfig::Command(_) => "退出码 0".into(),
        CheckConfig::Metric(params) => {
            let direction = match params.direction {
                Direction::Lower => "↓",
                Direction::Higher => "↑",
            };
            let tolerance = params
                .tolerance
                .map(|value| format!("（±{value}）"))
                .unwrap_or_default();
            format!("{direction} 不劣于基线{tolerance}")
        }
        CheckConfig::Evaluator(params) => {
            let aggregate = match params.aggregate {
                Aggregate::Majority => "多数（平票持平）",
                Aggregate::Median => "下中位",
            };
            let rubric = params
                .rubric
                .iter()
                .map(|item| format!("{}: {}", item.id, item.criterion))
                .collect::<Vec<_>>()
                .join("、");
            format!(
                "{rubric} · 趋势：{aggregate}；findings：各严重级别取下中位；通过：有样本且聚合后 blocking 为 0"
            )
        }
        CheckConfig::Fact(params) => match params.fact {
            Fact::TaskAccepted => format!("任务 {} 已被接受", params.subject),
            Fact::PrMerged => format!("PR {} 已合并", params.subject),
            Fact::AskAnswered => format!("问询 {} 已回答", params.subject),
        },
    }
}
