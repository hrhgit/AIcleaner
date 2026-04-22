pub(crate) fn provider_secret_key(endpoint: &str) -> String {
    format!("provider:{}:apiKey", endpoint.trim())
}

pub(crate) fn default_model_for_endpoint(endpoint: &str) -> &'static str {
    match endpoint.trim() {
        "https://api.deepseek.com" => "deepseek-chat",
        "https://generativelanguage.googleapis.com/v1beta/openai/" => "gemini-2.5-flash",
        "https://dashscope.aliyuncs.com/compatible-mode/v1" => "qwen-plus",
        "https://open.bigmodel.cn/api/paas/v4" => "glm-4-flash",
        "https://api.moonshot.cn/v1" => "moonshot-v1-8k",
        "https://api.minimax.io/anthropic/v1"
        | "https://api.minimaxi.com/anthropic/v1"
        | "https://api.minimax.io/anthropic"
        | "https://api.minimaxi.com/anthropic" => "MiniMax-M2.7",
        _ => "gpt-4o-mini",
    }
}
