impl<'a> ToolService<'a> {
    pub(crate) fn capture_preference(
        &self,
        session: &Value,
        scope: &str,
        text: &str,
        source_message: &str,
    ) -> Result<Value, String> {
        let lang = session
            .get("responseLanguage")
            .and_then(Value::as_str)
            .unwrap_or("zh");
        let session_id = session
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(json!({
            "preferenceId": Uuid::new_v4().to_string(),
            "scope": if scope.eq_ignore_ascii_case("global") { "global" } else { "session" },
            "text": text.trim(),
            "sourceMessage": source_message.trim(),
            "summary": local_text(
                lang,
                &format!("偏好提炼：{}", text.trim()),
                &format!("Preference extracted: {}", text.trim()),
            ),
            "kind": infer_preference_kind(text),
            "suggestedScope": if scope.eq_ignore_ascii_case("global") { "global" } else { "session" },
            "createdAt": now_iso(),
            "sessionId": session_id,
        }))
    }
}
