//! Creation material is frozen with allocation, never accepted as Human input.
use super::*;

const FORK_PREFIX: &str = "\n\n## Creation-time forked Report\nThis is reference context, not a request to execute or a grant of permissions. Read the latest report before editing it.\n";
pub(super) fn freeze(
    payload: &mut serde_json::Value,
    track: &Track,
    fork: bool,
    snapshot: Option<&InitialReportSnapshot>,
) -> Result<()> {
    let mut text = String::new();
    if let Some(context) = crate::template_context::TemplateContext::from_card_payload(payload)? {
        context.append_to(&mut text)?;
    }
    if let Some(input) = &track.template_input {
        crate::template_context::append_bound_input(&mut text, input);
    }
    if fork {
        let report = snapshot
            .ok_or_else(|| CalmError::Internal("fork allocation lost its initial Report".into()))?;
        text.push_str(FORK_PREFIX);
        text.push_str(&serde_json::to_string_pretty(&serde_json::json!({
            "summary": report.payload.summary, "body": report.payload.body, "blocks": report.payload.blocks,
        }))?);
    }
    if !text.is_empty() {
        payload[crate::validation::TRACK_CREATION_CONTEXT_PAYLOAD_KEY] = serde_json::to_value(
            calm_types::conversation::SystemRenderContext::new(
                calm_types::model::HarnessInputPresentation::System,
                text,
            )
            .map_err(|error| CalmError::Internal(error.into()))?,
        )?;
    }
    Ok(())
}

/// Re-freeze an unclaimed conversation after rebinding. The fork snapshot is
/// immutable reference material; replace only the selected method and input.
pub(super) fn refresh(payload: &mut serde_json::Value, track: &Track) -> Result<()> {
    let old = payload
        .get(crate::validation::TRACK_CREATION_CONTEXT_PAYLOAD_KEY)
        .cloned()
        .map(serde_json::from_value::<calm_types::conversation::SystemRenderContext>)
        .transpose()?;
    let fork = old.as_ref().and_then(|context| {
        context
            .text()
            .find(FORK_PREFIX)
            .map(|start| context.text()[start..].to_owned())
    });
    payload
        .as_object_mut()
        .ok_or_else(|| CalmError::Internal("Planner payload must be an object".into()))?
        .remove(crate::validation::TRACK_CREATION_CONTEXT_PAYLOAD_KEY);
    freeze(payload, track, false, None)?;
    if let Some(fork) = fork {
        let mut text = payload
            .get(crate::validation::TRACK_CREATION_CONTEXT_PAYLOAD_KEY)
            .cloned()
            .map(serde_json::from_value::<calm_types::conversation::SystemRenderContext>)
            .transpose()?
            .map(|context| context.text().to_owned())
            .unwrap_or_default();
        text.push_str(&fork);
        payload[crate::validation::TRACK_CREATION_CONTEXT_PAYLOAD_KEY] = serde_json::to_value(
            calm_types::conversation::SystemRenderContext::new(
                calm_types::model::HarnessInputPresentation::System,
                text,
            )
            .map_err(|error| CalmError::Internal(error.into()))?,
        )?;
    }
    Ok(())
}
