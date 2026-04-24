impl<'a> ToolService<'a> {
    pub(crate) fn execute_plan(
        &self,
        session_id: &str,
        job_id: &str,
    ) -> Result<(Value, Value, Value), String> {
        let mut job = persist::load_advisor_plan_job(&self.state.db_path(), job_id)?
            .ok_or_else(|| "plan job not found".to_string())?;
        ensure_session_owned(&job, session_id, "plan job")?;
        let (result, rollback) = execute_plan_job(&job)?;
        if let Some(obj) = job.as_object_mut() {
            obj.insert("status".to_string(), Value::String("executed".to_string()));
            obj.insert("result".to_string(), result.clone());
            obj.insert("rollback".to_string(), rollback.clone());
            obj.insert("updatedAt".to_string(), Value::String(now_iso()));
        }
        persist::save_advisor_plan_job(&self.state.db_path(), &job)?;
        Ok((job, result, rollback))
    }

    pub(crate) fn execute_plan_by_preview_id(
        &self,
        session_id: &str,
        preview_id: &str,
    ) -> Result<(Value, Value, Value), String> {
        self.execute_plan(session_id, preview_id)
            .map_err(|_| "当前预览不存在或已过期，请先重新生成 preview，再执行。".to_string())
    }
}
