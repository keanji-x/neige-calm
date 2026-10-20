//! Forge event validation and pure construction.
use super::*;

pub(super) fn forge_event_spec_needs_json(event_spec: Option<&ForgeEventSpec>) -> bool {
    event_spec
        .map(|event_spec| {
            event_spec
                .fields
                .values()
                .any(|source| matches!(source, FieldSource::JsonField { .. }))
        })
        .unwrap_or(false)
}

fn required_output_fields(event_kind: &str) -> &'static [(&'static str, bool)] {
    match event_kind {
        "forge.pr.merged" => &[("head_sha", true), ("merge_sha", true)],
        _ => &[],
    }
}

#[derive(Clone, Copy)]
enum ForgeFieldType {
    Str,
    U64,
    VecU64,
}

impl ForgeFieldType {
    fn matches_context_value(self, value: &Value) -> bool {
        match self {
            Self::Str => value.is_string(),
            Self::U64 => value.is_u64(),
            Self::VecU64 => value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_u64)),
        }
    }

    fn json_type_name(self) -> &'static str {
        match self {
            Self::Str => "string",
            Self::U64 => "u64",
            Self::VecU64 => "u64 array",
        }
    }
}

/// Required non-kernel payload fields per event kind; `forge.pr.merged` stays on `required_output_fields`.
fn new_kind_required_fields(event_kind: &str) -> &'static [(&'static str, ForgeFieldType)] {
    use ForgeFieldType::*;
    match event_kind {
        "forge.scan.completed" => &[("overlapping_prs", VecU64)],
        "forge.pr.opened" | "forge.pr.published" => &[("pr_number", U64), ("head_sha", Str)],
        "forge.pr.diff.read" => &[
            ("pr_number", U64),
            ("base_sha", Str),
            ("head_sha", Str),
            ("artifact_path", Str),
        ],
        "forge.issue.read" => &[("issue_number", U64), ("artifact_path", Str)],
        "forge.issue.created" => &[("issue_number", U64), ("issue_url", Str)],
        "forge.issue.searched" => &[("artifact_path", Str)],
        "forge.pr.checks" => &[("pr_number", U64), ("conclusion", Str)],
        "forge.ci.evidence.read" => &[
            ("pr_number", U64),
            ("head_sha", Str),
            ("artifact_path", Str),
        ],
        "forge.issue.closed" => &[("issue_number", U64)],
        "worktree.provisioned" | "worktree.removed" => &[("path", Str)],
        "worktree.committed" => &[("commit_sha", Str), ("branch", Str)],
        _ => &[],
    }
}

/// Kernel-authoritative payload fields injected last, which plugin `context`/`event_spec.fields` may not set.
fn kernel_injected_fields(event_kind: &str) -> &'static [&'static str] {
    match event_kind {
        "forge.pr.merged" => &["track_id", "subject"],
        "forge.pr.diff.read"
        | "forge.ci.evidence.read"
        | "forge.issue.read"
        | "forge.issue.searched" => &["track_id", "artifact_path"],
        "worktree.provisioned" | "worktree.committed" | "worktree.removed" => {
            &["track_id", "card_id"]
        }
        _ => &["track_id"],
    }
}

fn field_source_type_name(source: &FieldSource) -> &'static str {
    match source {
        FieldSource::ExitCode => "exit_code",
        FieldSource::JsonField { .. } => "json_field",
    }
}

fn parse_json_stdout_if_needed(
    event_spec: Option<&ForgeEventSpec>,
    stdout: &str,
) -> Result<Option<Value>> {
    if !forge_event_spec_needs_json(event_spec) {
        return Ok(None);
    }
    serde_json::from_str::<Value>(stdout)
        .map(Some)
        .map_err(|e| {
            CalmError::Internal(format!(
                "gate-infra: forge action JSON stdout unparseable: {e}"
            ))
        })
}

pub(super) fn validate_payload(payload: &ForgeActionPayload) -> Result<()> {
    if payload.track_id.trim().is_empty() {
        return Err(CalmError::BadRequest(
            "forge-action track_id must not be empty".into(),
        ));
    }
    if payload.card_id.trim().is_empty() {
        return Err(CalmError::BadRequest(
            "forge-action card_id must not be empty".into(),
        ));
    }
    if payload.argv.is_empty() {
        return Err(CalmError::BadRequest(
            "forge-action argv must not be empty".into(),
        ));
    }
    if payload.idem_key.trim().is_empty() {
        return Err(CalmError::BadRequest(
            "forge-action idem_key must not be empty".into(),
        ));
    }
    match (
        payload
            .event_spec
            .as_ref()
            .map(|event_spec| event_spec.event_kind.as_str()),
        payload.subject.as_ref(),
    ) {
        (Some("forge.pr.merged"), None) => {
            return Err(CalmError::BadRequest(
                "forge.pr.merged requires subject".into(),
            ));
        }
        (Some("forge.pr.merged"), Some(_)) => {}
        (_, Some(_)) => {
            return Err(CalmError::BadRequest(
                "subject is only valid for forge.pr.merged".into(),
            ));
        }
        (_, None) => {}
    }
    if let Some(event_spec) = payload.event_spec.as_ref() {
        if event_spec.event_kind.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "forge-action event_spec.event_kind must not be empty".into(),
            ));
        }
        if !SUPPORTED_FORGE_EVENT_KINDS.contains(&event_spec.event_kind.as_str()) {
            return Err(CalmError::BadRequest(format!(
                "forge-action event_kind `{}` is not a supported forge event kind",
                event_spec.event_kind
            )));
        }
        for reserved in kernel_injected_fields(&event_spec.event_kind) {
            if payload.context.contains_key(*reserved) || event_spec.fields.contains_key(*reserved)
            {
                return Err(CalmError::BadRequest(format!(
                    "forge event context/output may not set reserved key `{reserved}`"
                )));
            }
        }
        for (field, field_type) in new_kind_required_fields(&event_spec.event_kind) {
            if kernel_injected_fields(&event_spec.event_kind).contains(field) {
                continue;
            }
            if let Some(source) = event_spec.fields.get(*field) {
                if !matches!(source, FieldSource::JsonField { .. }) {
                    return Err(CalmError::BadRequest(format!(
                        "forge-action `{}` field `{field}` must be extracted via a JSON field source, not exit_code",
                        event_spec.event_kind
                    )));
                }
            } else if let Some(value) = payload.context.get(*field) {
                if !field_type.matches_context_value(value) {
                    return Err(CalmError::BadRequest(format!(
                        "forge-action `{}` context field `{field}` must be a {}",
                        event_spec.event_kind,
                        field_type.json_type_name()
                    )));
                }
            } else {
                return Err(CalmError::BadRequest(format!(
                    "forge-action event_spec/context for `{}` must provide field `{field}`",
                    event_spec.event_kind
                )));
            }
        }
        for (field, source) in &event_spec.fields {
            if let FieldSource::JsonField { path } = source
                && !path.is_empty()
                && !path.starts_with('/')
            {
                return Err(CalmError::BadRequest(format!(
                    "forge-action event_spec field `{field}` JsonField path `{path}` must be a valid JSON Pointer (empty string or starting with `/`)"
                )));
            }
        }
        for (field, is_string) in required_output_fields(&event_spec.event_kind) {
            let Some(source) = event_spec.fields.get(*field) else {
                return Err(CalmError::BadRequest(format!(
                    "forge-action event_spec for `{}` must populate field `{}`",
                    event_spec.event_kind, field
                )));
            };
            if *is_string && !matches!(source, FieldSource::JsonField { .. }) {
                return Err(CalmError::BadRequest(format!(
                    "forge-action `{}` field `{}` must be a JSON string source, not {}",
                    event_spec.event_kind,
                    field,
                    field_source_type_name(source)
                )));
            }
        }
    }
    if payload.cwd_lease.as_os_str().is_empty() {
        return Err(CalmError::BadRequest(
            "forge-action cwd_lease must not be empty".into(),
        ));
    }
    if !payload.cwd_lease.is_absolute() {
        return Err(CalmError::BadRequest(
            "forge-action cwd_lease must be absolute".into(),
        ));
    }
    if payload.result_path.as_os_str().is_empty() {
        return Err(CalmError::BadRequest(
            "forge-action result_path must not be empty".into(),
        ));
    }
    if !payload.result_path.is_absolute() {
        return Err(CalmError::BadRequest(
            "forge-action result_path must be absolute".into(),
        ));
    }
    if payload.deadline_ms <= 0 {
        return Err(CalmError::BadRequest(
            "forge-action deadline_ms must be positive".into(),
        ));
    }
    if let Some(probe) = payload.probe.as_ref()
        && probe.probe_argv.is_empty()
    {
        return Err(CalmError::BadRequest(
            "forge-action probe.probe_argv must not be empty".into(),
        ));
    }
    if let Some(probe) = payload.probe.as_ref() {
        if let Some(output_probe_argv) = probe.output_probe_argv.as_ref()
            && output_probe_argv.is_empty()
        {
            return Err(CalmError::BadRequest(
                "forge-action probe.output_probe_argv must not be empty".into(),
            ));
        }
        if forge_event_spec_needs_json(payload.event_spec.as_ref())
            && probe.output_probe_argv.is_none()
        {
            return Err(CalmError::BadRequest(
                "forge-action probe.output_probe_argv must be present when event_spec uses JsonField"
                    .into(),
            ));
        }
    }
    Ok(())
}

pub(super) fn build_forge_event(
    frozen: &FrozenForge,
    exit_code: i32,
    stdout: &str,
) -> std::result::Result<(Option<Event>, Value), ForgeEventBuildError> {
    if exit_code != 0 {
        return Err(ForgeEventBuildError::ActionFailed {
            reason: format!("forge action exited with code {exit_code}"),
        });
    }
    let Some(event_spec) = frozen.event_spec.as_ref() else {
        return Ok((
            None,
            json!({
                "exit_code": exit_code,
                "event_kind": Value::Null,
                "event": Value::Null,
            }),
        ));
    };
    let json_stdout = parse_json_stdout_if_needed(Some(event_spec), stdout).map_err(|e| {
        ForgeEventBuildError::ExtractionFailed {
            reason: e.to_string(),
        }
    })?;
    let mut payload = frozen.context.clone();
    for (key, value) in event_spec
        .extract_payload(exit_code, json_stdout.as_ref())
        .map_err(|e| ForgeEventBuildError::ExtractionFailed {
            reason: format!("gate-infra: forge event extraction failed: {e}"),
        })?
    {
        payload.insert(key, value);
    }
    for field in kernel_injected_fields(&event_spec.event_kind) {
        if payload.contains_key(*field) {
            return Err(ForgeEventBuildError::ExtractionFailed {
                reason: format!("forge event context/output may not set reserved key `{field}`"),
            });
        }
    }
    for field in kernel_injected_fields(&event_spec.event_kind) {
        match *field {
            "track_id" => {
                payload.insert(
                    "track_id".into(),
                    serde_json::to_value(TrackId::from(frozen.track_id.clone())).map_err(|e| {
                        ForgeEventBuildError::ExtractionFailed {
                            reason: format!(
                                "gate-infra: forge event context serialize failed: {e}"
                            ),
                        }
                    })?,
                );
            }
            "subject" if event_spec.event_kind == "forge.pr.merged" => {
                if let Some(subject) = frozen.subject.as_ref() {
                    payload.insert(
                        "subject".into(),
                        serde_json::to_value(subject).map_err(|e| {
                            ForgeEventBuildError::ExtractionFailed {
                                reason: format!(
                                    "gate-infra: forge event subject serialize failed: {e}"
                                ),
                            }
                        })?,
                    );
                }
            }
            "card_id" => {
                payload.insert(
                    "card_id".into(),
                    serde_json::to_value(CardId::from(frozen.card_id.clone())).map_err(|e| {
                        ForgeEventBuildError::ExtractionFailed {
                            reason: format!(
                                "gate-infra: forge event card_id serialize failed: {e}"
                            ),
                        }
                    })?,
                );
            }
            "artifact_path" if is_artifact_bearing_forge_event_kind(&event_spec.event_kind) => {
                payload.insert(
                    "artifact_path".into(),
                    Value::String(frozen.result_path.display().to_string()),
                );
            }
            _ => {}
        }
    }
    let payload_value = Value::Object(payload);
    let event = Event::from_kind_and_payload(&event_spec.event_kind, payload_value.clone())
        .map_err(|e| ForgeEventBuildError::ExtractionFailed {
            reason: format!("gate-infra: forge event deserialize failed: {e}"),
        })?;
    let mut result = Map::new();
    result.insert("exit_code".into(), json!(exit_code));
    let event_kind = event_spec.event_kind.as_str();
    result.insert("event_kind".into(), Value::String(event_kind.to_owned()));
    result.insert("event".into(), payload_value);
    if matches!(event_kind, "forge.issue.read" | "forge.issue.searched") {
        result.insert("stdout".into(), Value::String(stdout.to_owned()));
    }
    Ok((Some(event), Value::Object(result)))
}
