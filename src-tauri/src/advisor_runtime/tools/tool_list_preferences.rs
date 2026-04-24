impl<'a> ToolService<'a> {
    pub(crate) fn list_preferences(&self, session_id: Option<&str>) -> Result<Vec<Value>, String> {
        persist::load_advisor_memories(&self.state.db_path(), session_id)
    }

    pub(crate) fn list_preferences_tool(&self, session_id: Option<&str>) -> Result<Value, String> {
        let rows = self.list_preferences(session_id)?;
        let mut session_preferences = Vec::new();
        let mut global_preferences = Vec::new();
        for row in rows {
            if row.get("scope").and_then(Value::as_str) == Some("global") {
                global_preferences.push(row);
            } else {
                session_preferences.push(row);
            }
        }
        Ok(json!({
            "sessionPreferences": session_preferences,
            "globalPreferences": global_preferences,
        }))
    }
}
