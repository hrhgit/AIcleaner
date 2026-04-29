use serde_json::{Map, Value};
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub(crate) struct ModelIdMap {
    to_model: HashMap<String, String>,
    to_real: HashMap<String, String>,
}

impl ModelIdMap {
    pub(crate) fn from_values(values: &[&Value]) -> Self {
        let mut ids = Vec::new();
        for value in values {
            collect_model_ids(value, &mut ids);
        }

        let mut map = Self::default();
        for (idx, real) in ids.into_iter().enumerate() {
            let alias = format!("n{}", idx + 1);
            map.to_model.insert(real.clone(), alias.clone());
            map.to_real.insert(alias, real);
        }
        map
    }

    pub(crate) fn compact_value(&self, value: &Value) -> Value {
        self.rewrite_value(value, true)
    }

    pub(crate) fn expand_value(&self, value: &Value) -> Value {
        self.rewrite_value(value, false)
    }

    fn rewrite_value(&self, value: &Value, compact: bool) -> Value {
        match value {
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| self.rewrite_value(item, compact))
                    .collect(),
            ),
            Value::Object(obj) => {
                let mut out = Map::new();
                for (key, field) in obj {
                    out.insert(key.clone(), self.rewrite_field(key, field, compact));
                }
                Value::Object(out)
            }
            _ => value.clone(),
        }
    }

    fn rewrite_field(&self, key: &str, value: &Value, compact: bool) -> Value {
        if is_single_id_field(key) {
            return value
                .as_str()
                .map(|id| Value::String(self.rewrite_id(id, compact)))
                .unwrap_or_else(|| self.rewrite_value(value, compact));
        }

        if is_id_array_field(key) {
            return Value::Array(
                value
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map(|id| Value::String(self.rewrite_id(id, compact)))
                            .unwrap_or_else(|| self.rewrite_value(item, compact))
                    })
                    .collect(),
            );
        }

        self.rewrite_value(value, compact)
    }

    fn rewrite_id(&self, id: &str, compact: bool) -> String {
        let trimmed = id.trim();
        if trimmed.is_empty() || trimmed == "root" {
            return trimmed.to_string();
        }
        if compact {
            self.to_model
                .get(trimmed)
                .cloned()
                .unwrap_or_else(|| trimmed.to_string())
        } else {
            self.to_real
                .get(trimmed)
                .cloned()
                .unwrap_or_else(|| trimmed.to_string())
        }
    }
}

fn collect_model_ids(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_model_ids(item, out);
            }
        }
        Value::Object(obj) => {
            for key in SINGLE_ID_FIELDS {
                if let Some(field) = obj.get(*key) {
                    if let Some(id) = field.as_str().map(str::trim) {
                        push_model_id(id, out);
                    }
                }
            }
            for key in ID_ARRAY_FIELDS {
                if let Some(field) = obj.get(*key) {
                    if let Some(items) = field.as_array() {
                        for item in items {
                            if let Some(id) = item.as_str().map(str::trim) {
                                push_model_id(id, out);
                            }
                        }
                    }
                }
            }
            for (key, field) in obj {
                if is_single_id_field(key) || is_id_array_field(key) {
                    continue;
                }
                collect_model_ids(field, out);
            }
        }
        _ => {}
    }
}

fn push_model_id(id: &str, out: &mut Vec<String>) {
    if id.is_empty() || id == "root" || out.iter().any(|existing| existing == id) {
        return;
    }
    out.push(id.to_string());
}

fn is_single_id_field(key: &str) -> bool {
    SINGLE_ID_FIELDS.contains(&key)
}

fn is_id_array_field(key: &str) -> bool {
    ID_ARRAY_FIELDS.contains(&key)
}

const SINGLE_ID_FIELDS: &[&str] = &[
    "nodeId",
    "categoryId",
    "parentCategoryId",
    "leafNodeId",
    "targetNodeId",
    "sourceCategoryId",
    "targetCategoryId",
    "rootCategoryId",
];

const ID_ARRAY_FIELDS: &[&str] = &["sourceNodeIds", "nodeIds", "categoryIds"];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rewrites_ids_both_directions() {
        let source = json!({
            "nodeId": "root",
            "children": [
                { "nodeId": "real-a", "categoryId": "cat-a", "children": [] },
                { "nodeId": "real-b", "children": [] }
            ]
        });
        let map = ModelIdMap::from_values(&[&source]);
        let compact = map.compact_value(&json!({
            "nodeId": "real-a",
            "categoryIds": ["cat-a"],
            "targetNodeId": "real-b"
        }));
        assert_eq!(compact["nodeId"], "n1");
        assert_eq!(compact["categoryIds"][0], "n2");
        assert_eq!(compact["targetNodeId"], "n3");
        let expanded = map.expand_value(&compact);
        assert_eq!(expanded["nodeId"], "real-a");
        assert_eq!(expanded["categoryIds"][0], "cat-a");
        assert_eq!(expanded["targetNodeId"], "real-b");
    }
}
