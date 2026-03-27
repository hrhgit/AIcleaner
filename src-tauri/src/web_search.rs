use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

const TAVILY_SEARCH_URL: &str = "https://api.tavily.com/search";
const DEFAULT_MAX_RESULTS: u64 = 5;
const MAX_QUERY_LENGTH: usize = 240;

#[derive(Clone, Debug)]
pub struct WebSearchRequest {
    pub query: String,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct WebSearchTrace {
    pub query: String,
    pub reason: String,
    pub answer: Option<String>,
    pub response_time: Option<String>,
    pub request_id: Option<String>,
    pub results: Vec<Value>,
}

fn normalize_query(value: &str) -> String {
    let compact = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    if compact.len() <= MAX_QUERY_LENGTH {
        compact
    } else {
        compact.chars().take(MAX_QUERY_LENGTH).collect()
    }
}

fn extract_string(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
        .to_string()
}

pub fn parse_web_search_request(value: &Value) -> Option<WebSearchRequest> {
    let action = extract_string(value.get("action")).to_ascii_lowercase();
    let nested = value.get("webSearch").and_then(Value::as_object);
    let query = if let Some(search) = nested {
        extract_string(search.get("query"))
    } else {
        extract_string(value.get("query"))
    };
    if query.is_empty() {
        return None;
    }

    let explicit_request = matches!(
        action.as_str(),
        "web_search" | "search" | "request_web_search" | "need_web_search"
    ) || nested.is_some();
    if !explicit_request {
        return None;
    }

    let reason = if let Some(search) = nested {
        extract_string(search.get("reason"))
    } else {
        extract_string(value.get("reason"))
    };

    Some(WebSearchRequest {
        query: normalize_query(&query),
        reason,
    })
}

pub async fn tavily_search(
    api_key: &str,
    request: &WebSearchRequest,
) -> Result<WebSearchTrace, String> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err("web search api key is empty".to_string());
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(TAVILY_SEARCH_URL)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {key}"))
        .json(&json!({
            "query": request.query,
            "topic": "general",
            "search_depth": "basic",
            "max_results": DEFAULT_MAX_RESULTS,
            "include_answer": false,
            "include_raw_content": false,
            "include_images": false,
            "include_favicon": false
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = response.status();
    let body: Value = response.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let message = extract_string(body.get("detail"));
        if !message.is_empty() {
            return Err(message);
        }

        let fallback = extract_string(body.pointer("/error/message"));
        return Err(if fallback.is_empty() {
            "web search request failed".to_string()
        } else {
            fallback
        });
    }

    Ok(WebSearchTrace {
        query: request.query.clone(),
        reason: request.reason.clone(),
        answer: body
            .get("answer")
            .and_then(Value::as_str)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        response_time: body
            .get("response_time")
            .and_then(|value| {
                value
                    .as_str()
                    .map(|text| text.trim().to_string())
                    .or_else(|| value.as_f64().map(|number| number.to_string()))
            })
            .filter(|value| !value.is_empty()),
        request_id: body
            .get("request_id")
            .and_then(Value::as_str)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        results: body
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    })
}

pub fn format_web_search_context(trace: &WebSearchTrace, response_language: &str) -> String {
    let zh = response_language
        .trim()
        .to_ascii_lowercase()
        .starts_with("zh");
    let mut lines = if zh {
        vec![
            "联网搜索结果（仅供参考，请结合本地文件信息判断）".to_string(),
            format!("查询: {}", trace.query),
        ]
    } else {
        vec![
            "Web search results (reference only; still judge from local file evidence)."
                .to_string(),
            format!("Query: {}", trace.query),
        ]
    };

    if !trace.reason.trim().is_empty() {
        lines.push(if zh {
            format!("请求原因: {}", trace.reason)
        } else {
            format!("Request reason: {}", trace.reason)
        });
    }

    if let Some(answer) = trace
        .answer
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(if zh {
            format!("简答: {answer}")
        } else {
            format!("Answer: {answer}")
        });
    }

    if trace.results.is_empty() {
        lines.push(if zh {
            "无搜索结果。".to_string()
        } else {
            "No search results.".to_string()
        });
        return lines.join("\n");
    }

    lines.push(if zh {
        "结果:".to_string()
    } else {
        "Results:".to_string()
    });
    for (index, item) in trace.results.iter().enumerate() {
        let title = extract_string(item.get("title"));
        let url = extract_string(item.get("url"));
        let content = extract_string(item.get("content"));
        let score = item
            .get("score")
            .and_then(Value::as_f64)
            .map(|value| format!("{value:.3}"))
            .unwrap_or_default();

        lines.push(if title.is_empty() {
            if zh {
                format!("{}. 未命名结果", index + 1)
            } else {
                format!("{}. Untitled result", index + 1)
            }
        } else {
            format!("{}. {}", index + 1, title)
        });
        if !url.is_empty() {
            lines.push(format!("   URL: {url}"));
        }
        if !score.is_empty() {
            lines.push(format!("   Score: {score}"));
        }
        if !content.is_empty() {
            lines.push(format!("   Snippet: {content}"));
        }
    }

    lines.join("\n")
}

pub fn web_search_trace_to_value(trace: &WebSearchTrace) -> Value {
    json!({
        "request": {
            "query": trace.query,
            "reason": trace.reason,
        },
        "answer": trace.answer,
        "responseTime": trace.response_time,
        "requestId": trace.request_id,
        "results": trace.results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_top_level_web_search_request() {
        let value = json!({
            "action": "web_search",
            "query": "what is electron appdata folder",
            "reason": "Need vendor context"
        });
        let request = parse_web_search_request(&value).expect("request");
        assert_eq!(request.query, "what is electron appdata folder");
        assert_eq!(request.reason, "Need vendor context");
    }

    #[test]
    fn parse_nested_web_search_request() {
        let value = json!({
            "webSearch": {
                "query": "obsidian base file format",
                "reason": "Need product context"
            }
        });
        let request = parse_web_search_request(&value).expect("request");
        assert_eq!(request.query, "obsidian base file format");
        assert_eq!(request.reason, "Need product context");
    }
}
