impl<'a> ToolService<'a> {
    pub(crate) fn rollback_plan(
        &self,
        session_id: &str,
        job_id: &str,
    ) -> Result<(Value, Value), String> {
        let mut job = persist::load_advisor_plan_job(&self.state.db_path(), job_id)?
            .ok_or_else(|| "plan job not found".to_string())?;
        ensure_session_owned(&job, session_id, "plan job")?;
        let rollback_result = rollback_plan_job(&job)?;
        if let Some(obj) = job.as_object_mut() {
            obj.insert(
                "status".to_string(),
                Value::String("rolled_back".to_string()),
            );
            obj.insert("rollbackResult".to_string(), rollback_result.clone());
            obj.insert("updatedAt".to_string(), Value::String(now_iso()));
        }
        persist::save_advisor_plan_job(&self.state.db_path(), &job)?;
        Ok((job, rollback_result))
    }
}
