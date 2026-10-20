//! Kernel-owned worker result contracts (#2599). A codex or claude task may declare
//! `result_contract: "<name>"` naming an entry of [`RESULT_CONTRACTS`]. The kernel then refuses
//! a `neige_task_done` whose `result` does not validate, stating the contract's shape; the worker
//! prompt states the same shape, and the Planner receipt leads with the contract's conclusion.
//! Templates only reference names. Adding a contract edits only this registry.

use serde_json::{Map, Value};

use crate::task_recovery::{TASK_CHILD_TRACK_ROUTE, TASK_IN_TRACK_ROUTE};

/// One registered result contract.
pub struct ResultContract {
    /// The name a task declaration's `result_contract` field names.
    pub name: &'static str,
    /// The result's JSON Schema; its `examples` must pass [`Self::validate`].
    pub schema: fn() -> Value,
    /// The shape the worker prompt and every refusal state, so the two cannot diverge.
    pub shape_hint: &'static str,
    /// Every field-level violation of a result, or `Ok` when it matches.
    pub validate: fn(&Value) -> Result<(), Vec<String>>,
    /// One line stating a valid result's conclusion, verdict first.
    pub conclusion: fn(&Value) -> String,
}

/// Every contract a task may declare. A closed set owned by the kernel.
pub static RESULT_CONTRACTS: &[ResultContract] = &[REVIEW_V1, EVALUATION_V1];

/// The registered contract named `name`.
pub fn result_contract(name: &str) -> Option<&'static ResultContract> {
    RESULT_CONTRACTS
        .iter()
        .find(|contract| contract.name == name)
}

impl ResultContract {
    /// The refusal text for a result that does not match.
    pub fn check(&self, result: &Value) -> Result<(), String> {
        (self.validate)(result).map_err(|errors| {
            format!(
                "result does not match result_contract {}: {}. Expected: {}",
                self.name,
                errors.join("; "),
                self.shape_hint
            )
        })
    }

    /// The conclusion line of a result that matches; `None` for one that does not.
    pub fn conclusion_of(&self, result: &Value) -> Option<String> {
        (self.validate)(result)
            .ok()
            .map(|()| (self.conclusion)(result))
    }
}

/// The declaration rules of a task block's `result_contract`: a registered name, on a codex or
/// claude task in the track (only such a worker reports through `neige_task_done`).
pub(crate) fn validate_task_result_contract(map: &Map<String, Value>, errors: &mut Vec<String>) {
    let Some(name) = map.get("result_contract") else {
        return;
    };
    if name.as_str().and_then(result_contract).is_none() {
        let choices: Vec<String> = RESULT_CONTRACTS
            .iter()
            .map(|contract| format!("{:?}", contract.name))
            .collect();
        errors.push(format!(
            "result_contract: must be one of {}",
            choices.join(" | ")
        ));
    }
    if !matches!(
        map.get("kind").and_then(Value::as_str),
        Some("codex" | "claude")
    ) {
        errors.push(
            "result_contract: requires kind \"codex\" or \"claude\"; only a worker reports a \
             result through neige_task_done"
                .into(),
        );
    }
    if map.get("spawn").and_then(Value::as_str) == Some(TASK_CHILD_TRACK_ROUTE) {
        errors.push(format!(
            "result_contract: a {TASK_CHILD_TRACK_ROUTE:?} task completes through its child \
             track; it requires spawn {TASK_IN_TRACK_ROUTE:?}"
        ));
    }
}

const REVIEW_V1: ResultContract = ResultContract {
    name: "review-v1",
    schema: review_v1_schema,
    shape_hint: include_str!("task_result/review-v1.md").trim_ascii_end(),
    validate: validate_review_v1,
    conclusion: review_v1_conclusion,
};

fn review_v1_schema() -> Value {
    serde_json::from_str(include_str!("task_result/review-v1.schema.json"))
        .expect("review-v1.schema.json is valid JSON")
}

const EVALUATION_V1: ResultContract = ResultContract {
    name: "evaluation-v1",
    schema: evaluation_v1_schema,
    shape_hint: include_str!("task_result/evaluation-v1.md").trim_ascii_end(),
    validate: validate_evaluation_v1,
    conclusion: evaluation_v1_conclusion,
};

fn evaluation_v1_schema() -> Value {
    serde_json::from_str(include_str!("task_result/evaluation-v1.schema.json"))
        .expect("evaluation-v1.schema.json is valid JSON")
}

/// Summary is kernel receipt vocabulary; the sample's wire semantics belong to objective.
fn evaluation_sample(
    map: &Map<String, Value>,
) -> Result<objective::check::EvaluatorSample, String> {
    let mut sample = map.clone();
    sample.remove("summary");
    let parsed = serde_path_to_error::deserialize(Value::Object(sample))
        .map_err(|error| format!("{}: {}", error.path(), error.inner()))?;
    // Serde structs also accept positional arrays. This contract requires JSON objects;
    // objective remains authoritative for fields, trends, counts and source locations.
    let object = |value: &Value, path: &str| {
        value
            .as_object()
            .ok_or_else(|| format!("{path}: must be a JSON object"))
            .map(|_| ())
    };
    object(&map["findings"], "findings")?;
    for (index, judgement) in map["judgements"]
        .as_array()
        .expect("parsed judgements")
        .iter()
        .enumerate()
    {
        let path = format!("judgements[{index}]");
        object(judgement, &path)?;
        for (index, evidence) in judgement["evidence"]
            .as_array()
            .expect("parsed evidence")
            .iter()
            .enumerate()
        {
            object(evidence, &format!("{path}.evidence[{index}]"))?;
        }
    }
    Ok(parsed)
}

fn validate_evaluation_v1(result: &Value) -> Result<(), Vec<String>> {
    let Some(map) = result.as_object() else {
        return Err(vec![format!(
            "result: must be a JSON object, got {}",
            json_type(result)
        )]);
    };
    let mut errors = Vec::new();
    text(map, "", "summary", MAX_SUMMARY_BYTES, true, &mut errors);
    if let Err(error) = evaluation_sample(map) {
        errors.push(error);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn evaluation_v1_conclusion(result: &Value) -> String {
    use objective::check::Trend;
    let sample = evaluation_sample(result.as_object().expect("validated object"))
        .expect("validated evaluator sample");
    let count = |trend| {
        sample
            .judgements
            .iter()
            .filter(|j| j.trend == trend)
            .count()
    };
    format!(
        "evaluation: {} better / {} same / {} worse; findings {}/{}/{}",
        count(Trend::Better),
        count(Trend::Same),
        count(Trend::Worse),
        sample.findings.blocking,
        sample.findings.major,
        sample.findings.minor
    )
}

const REVIEW_V1_FIELDS: &[&str] = &[
    "verdict",
    "summary",
    "blocking_findings",
    "non_blocking",
    "details",
];
const FINDING_FIELDS: &[&str] = &["title", "evidence", "location", "acceptance"];
const MAX_FINDINGS: usize = 32;
const MAX_SUMMARY_BYTES: usize = 2048;
const MAX_FINDING_TEXT_BYTES: usize = 4096;

/// `{verdict, summary, blocking_findings, non_blocking?, details?}`, closed; `verdict` is `pass`
/// exactly when `blocking_findings` is empty.
fn validate_review_v1(result: &Value) -> Result<(), Vec<String>> {
    let Some(map) = result.as_object() else {
        return Err(vec![format!(
            "result: must be a JSON object, got {}",
            json_type(result)
        )]);
    };
    let mut errors = Vec::new();
    reject_unknown("", map, REVIEW_V1_FIELDS, &mut errors);
    let verdict = match map.get("verdict").and_then(Value::as_str) {
        Some(verdict @ ("pass" | "blocking")) => Some(verdict),
        _ => {
            errors.push("verdict: required; must be \"pass\" | \"blocking\"".into());
            None
        }
    };
    text(map, "", "summary", MAX_SUMMARY_BYTES, true, &mut errors);
    let blocking = findings(map, "blocking_findings", true, &mut errors);
    findings(map, "non_blocking", false, &mut errors);
    match (verdict, blocking) {
        (Some("pass"), Some(count)) if count > 0 => {
            errors.push("verdict: \"pass\" requires blocking_findings to be empty".into())
        }
        (Some("blocking"), Some(0)) => errors
            .push("verdict: \"blocking\" requires at least one entry in blocking_findings".into()),
        _ => {}
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// The finding count of a well-formed list, `None` when the list itself is malformed or absent.
fn findings(
    map: &Map<String, Value>,
    field: &str,
    required: bool,
    errors: &mut Vec<String>,
) -> Option<usize> {
    match map.get(field) {
        None if !required => None,
        Some(Value::Null) if !required => {
            errors.push(format!("{field}: must be an array of findings or omitted"));
            None
        }
        Some(Value::Array(items)) => {
            if items.len() > MAX_FINDINGS {
                errors.push(format!(
                    "{field}: too many findings ({}), limit is {MAX_FINDINGS}",
                    items.len()
                ));
            }
            for (index, item) in items.iter().enumerate() {
                let path = format!("{field}[{index}]");
                let Some(finding) = item.as_object() else {
                    errors.push(format!(
                        "{path}: must be an object {{title, evidence, location?, acceptance?}}"
                    ));
                    continue;
                };
                let prefix = format!("{path}.");
                reject_unknown(&prefix, finding, FINDING_FIELDS, errors);
                for (field, required) in [
                    ("title", true),
                    ("evidence", true),
                    ("location", false),
                    ("acceptance", false),
                ] {
                    text(
                        finding,
                        &prefix,
                        field,
                        MAX_FINDING_TEXT_BYTES,
                        required,
                        errors,
                    );
                }
            }
            Some(items.len())
        }
        _ => {
            errors.push(format!("{field}: required array of findings"));
            None
        }
    }
}

fn reject_unknown(
    prefix: &str,
    map: &Map<String, Value>,
    allowed: &[&str],
    errors: &mut Vec<String>,
) {
    for key in map.keys().filter(|key| !allowed.contains(&key.as_str())) {
        errors.push(format!("{prefix}{key}: unknown field"));
    }
}

/// A non-blank string of at most `limit` bytes; an absent optional field passes.
fn text(
    map: &Map<String, Value>,
    prefix: &str,
    field: &str,
    limit: usize,
    required: bool,
    errors: &mut Vec<String>,
) {
    match map.get(field) {
        None if !required => {}
        Some(Value::String(text)) if !text.trim().is_empty() => {
            if text.len() > limit {
                errors.push(format!(
                    "{prefix}{field}: too long ({} bytes), limit is {limit}",
                    text.len()
                ));
            }
        }
        _ if required => errors.push(format!("{prefix}{field}: required non-blank string")),
        _ => errors.push(format!("{prefix}{field}: must be a non-blank string")),
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `pass: <summary>` or `blocking: <n> blocking findings: "<title>"; .... <summary>`. Titles are
/// plain text; the receipt escapes the whole line once.
fn review_v1_conclusion(result: &Value) -> String {
    let summary = result["summary"].as_str().unwrap_or_default();
    let titles: Vec<String> = result["blocking_findings"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|finding| format!("\"{}\"", finding["title"].as_str().unwrap_or_default()))
        .collect();
    match titles.len() {
        0 => format!(
            "{}: {summary}",
            result["verdict"].as_str().unwrap_or("pass")
        ),
        count => format!(
            "blocking: {count} blocking finding{}: {}. {summary}",
            if count == 1 { "" } else { "s" },
            titles.join("; ")
        ),
    }
}

#[cfg(test)]
mod tests;
