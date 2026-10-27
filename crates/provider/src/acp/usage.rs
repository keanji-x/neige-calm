//! Context occupancy from the selected ACP wire contract; billing totals are not readings.
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub enum UsageSource {
    SessionUsage,
    XaiNotificationMeta,
}

pub struct UsageTracker {
    source: UsageSource,
    model: Option<String>,
    windows: BTreeMap<String, i64>,
}
impl UsageTracker {
    pub fn new(source: UsageSource) -> Self {
        Self {
            source,
            model: None,
            windows: BTreeMap::new(),
        }
    }

    /// Grok declares ModelInfo._meta.totalContextTokens in session setup answers.
    /// Config-option answers select from that catalog without repeating it.
    pub fn configure(&mut self, response: &Value) {
        if !matches!(self.source, UsageSource::XaiNotificationMeta) {
            return;
        }
        if let Some(models) = response["models"]["availableModels"].as_array() {
            self.windows = models
                .iter()
                .filter_map(|model| {
                    Some((
                        model["modelId"].as_str()?.to_owned(),
                        model["_meta"]["totalContextTokens"]
                            .as_i64()
                            .filter(|size| *size > 0)?,
                    ))
                })
                .collect();
        }
        if let Some(model) = response["models"]["currentModelId"].as_str() {
            self.model = Some(model.to_owned());
        }
        if let Some(options) = response["configOptions"].as_array() {
            if let Some(option) = options.iter().find(|option| option["category"] == "model") {
                self.model = option["currentValue"].as_str().map(str::to_owned);
            }
        }
    }

    pub fn reading(&mut self, params: &Value) -> Option<(i64, i64)> {
        if params["_meta"]["isReplay"] == true {
            return None;
        }
        let update = &params["update"];
        match self.source {
            UsageSource::SessionUsage => {
                if update["sessionUpdate"] != "usage_update" {
                    return None;
                }
                Some((
                    update["used"].as_i64().filter(|used| *used >= 0)?,
                    update["size"].as_i64().filter(|size| *size > 0)?,
                ))
            }
            UsageSource::XaiNotificationMeta => {
                self.configure(update);
                if update["sessionUpdate"] == "current_model_update" {
                    self.model = update["currentModelId"].as_str().map(str::to_owned);
                }
                // Grok's sender uses get_estimated_total_tokens(), not its UsageLedger.
                // Do not pair a fresh count with a previous/guessed model's window.
                let window = *self.windows.get(self.model.as_ref()?)?;
                Some((
                    params["_meta"]["totalTokens"]
                        .as_i64()
                        .filter(|used| *used >= 0)?,
                    window,
                ))
            }
        }
    }
}
