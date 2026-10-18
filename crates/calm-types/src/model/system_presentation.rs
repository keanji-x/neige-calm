//! A System display cannot acquire human presentation. The owning enum remains
//! the only variant list used by serde, schema and generated TypeScript.
use super::HarnessInputPresentation;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(try_from = "HarnessInputPresentation")]
#[ts(
    export,
    export_to = "fe/core/api/generated/wire.ts",
    type = "Exclude<HarnessInputPresentation, 'user'>"
)]
pub struct SystemInputPresentation(HarnessInputPresentation);
impl TryFrom<HarnessInputPresentation> for SystemInputPresentation {
    type Error = &'static str;
    fn try_from(value: HarnessInputPresentation) -> Result<Self, Self::Error> {
        if value == HarnessInputPresentation::User {
            return Err("System input cannot have User presentation");
        }
        Ok(Self(value))
    }
}
impl SystemInputPresentation {
    pub fn get(self) -> HarnessInputPresentation {
        self.0
    }
}
impl utoipa::PartialSchema for SystemInputPresentation {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::{RefOr, schema::Schema};
        let mut schema = <HarnessInputPresentation as utoipa::PartialSchema>::schema();
        let RefOr::T(Schema::Object(object)) = &mut schema else {
            unreachable!("presentation owner is a string enum")
        };
        object
            .enum_values
            .as_mut()
            .expect("presentation enum values")
            .retain(|value| value != "user");
        schema
    }
}
impl utoipa::ToSchema for SystemInputPresentation {}
