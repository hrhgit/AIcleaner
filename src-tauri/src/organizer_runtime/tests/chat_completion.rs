use super::*;

#[test]
fn parse_chat_completion_http_body_extracts_content_and_usage() {
    let raw_body = r#"{
      "choices": [
        {
          "message": {
            "content": "{\"tree\":{\"name\":\"\",\"nodeId\":\"root\",\"children\":[]},\"assignments\":[]}"
          }
        }
      ],
      "usage": {
        "prompt_tokens": 12,
        "completion_tokens": 34,
        "total_tokens": 46
      }
    }"#;
    let parsed = summary::parse_chat_completion_http_body(
        "https://api.openai.com/v1",
        StatusCode::OK,
        raw_body,
    )
    .expect("parse success");
    assert!(parsed.content.contains("\"assignments\":[]"));
    assert_eq!(parsed.usage.prompt, 12);
    assert_eq!(parsed.usage.completion, 34);
    assert_eq!(parsed.usage.total, 46);
}

#[test]
fn parse_chat_completion_http_body_keeps_raw_body_on_decode_error() {
    let raw_body = "<html>upstream gateway error</html>";
    let err = summary::parse_chat_completion_http_body(
        "https://api.openai.com/v1",
        StatusCode::OK,
        raw_body,
    )
    .expect_err("decode error");
    assert!(err.message.contains("error decoding response body"));
    assert!(err.message.contains("upstream gateway error"));
    assert_eq!(err.raw_body, raw_body);
}

#[test]
fn parse_chat_completion_http_body_accepts_tool_calls_without_text() {
    let raw_body = r#"{
      "choices": [
        {
          "message": {
            "content": null,
            "tool_calls": [
              {
                "id": "call_1",
                "type": "function",
                "function": {
                  "name": "submit_classification_batch",
                  "arguments": "{\"baseTreeVersion\":1,\"assignments\":[]}"
                }
              }
            ]
          }
        }
      ],
      "usage": {
        "prompt_tokens": 3,
        "completion_tokens": 5,
        "total_tokens": 8
      }
    }"#;
    let parsed = summary::parse_chat_completion_http_body(
        "https://api.openai.com/v1",
        StatusCode::OK,
        raw_body,
    )
    .expect("parse success");
    assert_eq!(parsed.tool_calls.len(), 1);
    assert_eq!(parsed.tool_calls[0].name, "submit_classification_batch");
    assert_eq!(parsed.usage.total, 8);
}
