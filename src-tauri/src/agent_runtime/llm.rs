use super::types::{AgentCompletion, AgentLlmError};
use async_trait::async_trait;
use serde_json::Value;

#[async_trait]
pub(crate) trait AgentLlm: Sync {
    async fn complete(
        &self,
        messages: &[Value],
        tools: &[Value],
        trace_key: Option<&str>,
    ) -> Result<AgentCompletion, AgentLlmError>;
}
