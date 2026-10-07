//! ACP-declared session configuration. Option ids and values belong to the agent.
use super::Error;
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize)]
pub struct Choice {
    pub value: String,
    pub name: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum ChoiceEntry {
    Choice(Choice),
    Group { group: String, options: Vec<Choice> },
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectOption {
    pub id: String,
    pub category: String,
    pub current_value: String,
    pub options: Vec<ChoiceEntry>,
}
impl SelectOption {
    pub fn choices(&self) -> Vec<&Choice> {
        self.options
            .iter()
            .flat_map(|entry| match entry {
                ChoiceEntry::Choice(choice) => vec![choice],
                ChoiceEntry::Group { options, .. } => options.iter().collect(),
            })
            .collect()
    }
    pub fn accepts(&self, value: &str) -> bool {
        self.choices().iter().any(|choice| choice.value == value)
    }
}
#[derive(Clone, Debug, Default)]
pub struct Configuration {
    pub options: Vec<SelectOption>,
}
impl Configuration {
    pub fn from_response(response: &Value) -> Result<Self, Error> {
        let Some(options) = response.get("configOptions") else {
            return Ok(Self::default());
        };
        let options = options
            .as_array()
            .ok_or(Error::Protocol("invalid configuration options"))?;
        let mut decoded = Vec::new();
        for option in options {
            if !matches!(option["category"].as_str(), Some("model" | "thought_level")) {
                continue;
            }
            if option["type"] != "select" {
                return Err(Error::Protocol("unsupported model configuration type"));
            }
            let option: SelectOption = serde_json::from_value(option.clone())
                .map_err(|_| Error::Protocol("invalid model configuration"))?;
            if option.id.is_empty() || !option.accepts(&option.current_value) {
                return Err(Error::Protocol("configuration default is undeclared"));
            }
            if decoded.iter().any(|prior: &SelectOption| {
                prior.id == option.id || prior.category == option.category
            }) {
                return Err(Error::Protocol("ambiguous configuration option"));
            }
            decoded.push(option);
        }
        Ok(Self { options: decoded })
    }
    pub fn category(&self, category: &str) -> Option<&SelectOption> {
        self.options
            .iter()
            .find(|option| option.category == category)
    }
}
