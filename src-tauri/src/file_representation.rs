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

impl RepresentationLevel {
    pub fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or("metadata").trim() {
            "short" => Self::Short,
            "long" => Self::Long,
            _ => Self::Metadata,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Short => "short",
            Self::Long => "long",
        }
    }
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

    pub fn prune_to_level(&self, level: RepresentationLevel) -> Self {
        match level {
            RepresentationLevel::Metadata => Self {
                metadata: Self::non_empty(self.metadata.as_deref()),
                short: None,
                long: None,
                source: self.source.clone(),
                degraded: self.degraded,
                confidence: self.confidence.clone(),
                keywords: self.keywords.clone(),
            },
            RepresentationLevel::Short => Self {
                metadata: Self::non_empty(self.metadata.as_deref()),
                short: Self::non_empty(self.short.as_deref()),
                long: None,
                source: self.source.clone(),
                degraded: self.degraded,
                confidence: self.confidence.clone(),
                keywords: self.keywords.clone(),
            },
            RepresentationLevel::Long => self.clone(),
        }
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
