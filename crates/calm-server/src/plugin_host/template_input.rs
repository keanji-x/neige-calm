//! Plugin validation contracts and server HTTP error mapping.
pub use plugin::template_input::*;

/// A field violation answers 400 with its `field`; a whole-instance one is a plain `bad_request`.
impl From<plugin::template_input::InstanceViolation> for crate::error::CalmError {
    fn from(violation: plugin::template_input::InstanceViolation) -> Self {
        match violation {
            plugin::template_input::InstanceViolation::Whole(sentence) => {
                crate::error::CalmError::BadRequest(sentence)
            }
            plugin::template_input::InstanceViolation::Field { field, reason } => {
                crate::error::CalmError::InvalidField { field, reason }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A one-key violation becomes the 400 that names its field; a whole-instance one stays a plain 400.
    #[test]
    fn a_violation_maps_to_the_error_its_shape_says() {
        use crate::error::CalmError;
        let field: CalmError = InstanceViolation::Field {
            field: "config.retries".into(),
            reason: "expected type `integer`".into(),
        }
        .into();
        assert!(matches!(
            &field,
            CalmError::InvalidField { field, reason }
                if field == "config.retries" && reason == "expected type `integer`"
        ));
        let whole: CalmError =
            InstanceViolation::Whole("config: expected a JSON object".into()).into();
        assert!(
            matches!(&whole, CalmError::BadRequest(m) if m == "config: expected a JSON object")
        );
    }
}
