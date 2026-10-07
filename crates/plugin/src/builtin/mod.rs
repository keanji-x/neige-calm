//! Builtin domain implementations and their declarations.
pub mod calendar;
pub mod gitforge;
pub mod tools;

use crate::manifest::Manifest;
use std::sync::LazyLock;
pub enum Binding {
    Calendar,
    Gitforge,
}
pub struct Definition {
    pub binding: Binding,
    pub tool_prompt_directory: &'static str,
    manifest: Manifest,
    optional: bool,
    tools: Vec<tools::NativeToolSpec>,
}
impl Definition {
    pub fn tools(&self) -> &[tools::NativeToolSpec] {
        &self.tools
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn can_disable(&self) -> bool {
        self.optional
    }
}
static CATALOG: LazyLock<Vec<Definition>> = LazyLock::new(|| {
    vec![
        Definition {
            binding: Binding::Gitforge,
            tool_prompt_directory: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/builtin/gitforge/prompts"
            ),
            manifest: Manifest::parse(gitforge::MANIFEST).expect("compiled manifest"),
            optional: true,
            tools: vec![gitforge::publish::descriptor()],
        },
        Definition {
            binding: Binding::Calendar,
            tool_prompt_directory: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/builtin/calendar/prompts"
            ),
            manifest: Manifest::parse(calendar::MANIFEST).expect("compiled manifest"),
            optional: false,
            tools: calendar::tools::descriptors(),
        },
    ]
});
pub fn catalog() -> &'static [Definition] {
    &CATALOG
}
pub fn get(id: &str) -> Option<&'static Definition> {
    catalog().iter().find(|entry| entry.manifest.id == id)
}
pub fn is_reserved(id: &str) -> bool {
    get(id).is_some()
}
pub fn required_owner(template: &str) -> Option<&'static str> {
    catalog()
        .iter()
        .find(|entry| {
            entry
                .manifest
                .templates
                .iter()
                .any(|item| item.id == template)
        })
        .map(|entry| entry.manifest.id.as_str())
}
