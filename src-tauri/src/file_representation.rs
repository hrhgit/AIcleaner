use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileRepresentation {
    pub metadata: Option<String>,
    pub short: Option<String>,
    pub long: Option<String>,
    pub source: String,
    pub degraded: bool,
    pub confidence: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepresentationLevel {
    #[default]
    Metadata,
    Short,
    Long,
}

impl FileRepresentation {
    pub fn from_value(value: &Value) -> Self {
        serde_json::from_value::<Self>(value.clone()).unwrap_or_default()
    }

    pub fn to_value(&self) -> Value {
        json!(self)
    }

    pub fn best_text(&self) -> String {
        self.long
            .as_deref()
            .or(self.short.as_deref())
            .or(self.metadata.as_deref())
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    pub fn has_level(&self, level: RepresentationLevel) -> bool {
        match level {
            RepresentationLevel::Metadata => Self::non_empty(self.metadata.as_deref()).is_some(),
            RepresentationLevel::Short => Self::non_empty(self.short.as_deref()).is_some(),
            RepresentationLevel::Long => Self::non_empty(self.long.as_deref()).is_some(),
        }
    }

    fn non_empty(value: Option<&str>) -> Option<String> {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }
}
