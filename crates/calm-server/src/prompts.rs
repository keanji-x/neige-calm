//! Rendering of agent-facing prose fragments that are assembled at runtime
//! (#1635 S1c).
//!
//! The fragments live under `crates/calm-server/prompts/` and are embedded
//! with `include_str!` next to their one use site. Rust keeps the protocol —
//! which branch is taken, which values are bound — and the `.md` keeps the
//! sentences. [`render_named`] is the seam between the two, and it checks
//! the placeholder set in BOTH directions so that a fragment and its call
//! site cannot drift apart silently: a placeholder the code stopped filling
//! and a value the fragment stopped naming are each a hard error, never a
//! stray `{name}` or a silently dropped fact in the agent's input.

use std::fmt;

use crate::error::CalmError;

/// Why a fragment could not be rendered. Every variant is a defect in this
/// binary (fragment and call site disagree), never in a caller's input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RenderError {
    /// The template names `{name}` and the call site supplied no value for it.
    Missing(String),
    /// The call site supplied `name` and the template never names `{name}`.
    Unused(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::Missing(name) => {
                write!(f, "template placeholder {{{name}}} has no value")
            }
            RenderError::Unused(name) => {
                write!(f, "value {name} matches no template placeholder")
            }
        }
    }
}

impl std::error::Error for RenderError {}

impl From<RenderError> for CalmError {
    fn from(error: RenderError) -> Self {
        // `{error:?}` on purpose: the variant name is the fact an operator
        // needs (`Missing("event_id")`).
        CalmError::Internal(format!("prompt fragment mismatch: {error:?}"))
    }
}

/// Substitute `{name}` placeholders in `template` with the matching `values`.
///
/// A placeholder is exactly `{` + `[a-z_]+` + `}`. Any other brace sequence —
/// `{}`, `{"path": …}`, `{ x }`, `{Name}` — is not a placeholder and passes
/// through untouched, which is what lets JSON sit in a template or in a value.
/// Values are inserted verbatim and never re-scanned, so a value that happens
/// to contain `{kind}` stays literal. There is no escape: `{{x}}` is a literal
/// `{` followed by the placeholder `{x}` and a `}`, so a fragment cannot emit
/// a literal `{lowercase}` of its own.
///
/// Errors when the template names a placeholder that `values` does not supply
/// ([`RenderError::Missing`]) or when `values` supplies a name the template
/// never uses ([`RenderError::Unused`]). Both directions are checked so that
/// neither side of the seam can change without the other noticing. A name
/// listed twice in `values` binds its first entry and the second is reported
/// as [`RenderError::Unused`].
pub(crate) fn render_named(template: &str, values: &[(&str, &str)]) -> Result<String, RenderError> {
    let mut out = String::with_capacity(template.len());
    let mut used = vec![false; values.len()];
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        match placeholder_name(after_open) {
            Some(name) => {
                let Some(position) = values.iter().position(|(key, _)| *key == name) else {
                    return Err(RenderError::Missing(name.to_string()));
                };
                used[position] = true;
                out.push_str(values[position].1);
                rest = &after_open[name.len() + 1..];
            }
            None => {
                out.push('{');
                rest = after_open;
            }
        }
    }
    out.push_str(rest);
    if let Some(position) = used.iter().position(|was_used| !was_used) {
        return Err(RenderError::Unused(values[position].0.to_string()));
    }
    Ok(out)
}

/// The placeholder name that starts `after_open` (the text right after a `{`),
/// when that text is `[a-z_]+` followed by `}`.
fn placeholder_name(after_open: &str) -> Option<&str> {
    let end = after_open
        .bytes()
        .position(|byte| !(byte.is_ascii_lowercase() || byte == b'_'))?;
    (end > 0 && after_open.as_bytes()[end] == b'}').then(|| &after_open[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_every_placeholder_and_keeps_the_rest() {
        let rendered =
            render_named("a {one} b {two} c {one}.", &[("one", "1"), ("two", "2")]).unwrap();
        assert_eq!(rendered, "a 1 b 2 c 1.");
    }

    #[test]
    fn a_placeholder_without_a_value_is_missing() {
        assert_eq!(
            render_named("read {kind} and {event_id}", &[("kind", "completed")]),
            Err(RenderError::Missing("event_id".into()))
        );
    }

    #[test]
    fn a_value_without_a_placeholder_is_unused() {
        assert_eq!(
            render_named("read {kind}", &[("kind", "completed"), ("event_id", "7")]),
            Err(RenderError::Unused("event_id".into()))
        );
    }

    #[test]
    fn missing_is_reported_before_unused() {
        // The template's demand comes first: a stray `{name}` in agent input
        // is the more visible defect, so it is the one named.
        assert_eq!(
            render_named("{a}", &[("b", "")]),
            Err(RenderError::Missing("a".into()))
        );
    }

    #[test]
    fn values_are_inserted_verbatim_without_recursive_substitution() {
        let rendered = render_named(
            "events.{kind} require event_id={event_id}",
            &[("kind", "{event_id}"), ("event_id", "42")],
        )
        .unwrap();
        assert_eq!(rendered, "events.{event_id} require event_id=42");
    }

    #[test]
    fn json_braces_in_template_and_values_pass_through() {
        let rendered = render_named(
            "call({\"path\": \"{path}\"}) {} { spaced } {Upper} {digits1} {a-b}",
            &[("path", "{\"nested\": {}}")],
        )
        .unwrap();
        assert_eq!(
            rendered,
            "call({\"path\": \"{\"nested\": {}}\"}) {} { spaced } {Upper} {digits1} {a-b}"
        );
    }

    #[test]
    fn empty_template_renders_empty_and_rejects_any_value() {
        assert_eq!(render_named("", &[]), Ok(String::new()));
        assert_eq!(
            render_named("", &[("x", "1")]),
            Err(RenderError::Unused("x".into()))
        );
    }

    #[test]
    fn double_braces_are_not_an_escape() {
        assert_eq!(render_named("{{x}}", &[("x", "1")]), Ok("{1}".into()));
        assert_eq!(
            render_named("{{x}}", &[]),
            Err(RenderError::Missing("x".into()))
        );
    }

    #[test]
    fn a_duplicate_key_binds_first_and_reports_the_second_unused() {
        assert_eq!(
            render_named("{x}", &[("x", "first"), ("x", "second")]),
            Err(RenderError::Unused("x".into()))
        );
    }

    #[test]
    fn an_unterminated_brace_is_literal() {
        assert_eq!(render_named("a {b", &[]), Ok("a {b".into()));
        assert_eq!(render_named("{", &[]), Ok("{".into()));
        assert_eq!(render_named("}{", &[]), Ok("}{".into()));
    }

    #[test]
    fn render_error_maps_to_internal_and_names_the_variant() {
        let CalmError::Internal(message) = CalmError::from(RenderError::Missing("bogus".into()))
        else {
            panic!("a fragment mismatch is our bug, so it must be Internal");
        };
        assert!(message.contains("Missing(\"bogus\")"), "{message}");
    }
}
