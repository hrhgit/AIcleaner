import type { ProviderModelOption } from '../types';

export const DEFAULT_PROVIDER_ENDPOINT = 'https://api.openai.com/v1';
export const DEFAULT_PROVIDER_MODEL = 'gpt-4o-mini';

export const PROVIDER_OPTIONS = [
  { value: 'https://api.deepseek.com', label: 'DeepSeek' },
  { value: 'https://api.openai.com/v1', label: 'OpenAI' },
  { value: 'https://generativelanguage.googleapis.com/v1beta/openai/', label: 'Google Gemini' },
  { value: 'https://dashscope.aliyuncs.com/compatible-mode/v1', label: 'Qwen (DashScope)' },
  { value: 'https://open.bigmodel.cn/api/paas/v4', label: 'GLM (BigModel)' },
  { value: 'https://api.moonshot.cn/v1', label: 'Kimi (Moonshot)' },
  { value: 'https://api.minimax.io/anthropic/v1', label: 'MiniMax (Anthropic)' },
] as const;

export const PROVIDER_MODELS: Record<string, ProviderModelOption[]> = {
  'https://api.openai.com/v1': [
    { value: 'gpt-4o-mini', label: 'gpt-4o-mini' },
    { value: 'gpt-4o', label: 'gpt-4o' },
    { value: 'gpt-3.5-turbo', label: 'gpt-3.5-turbo' },
  ],
  'https://api.deepseek.com': [
    { value: 'deepseek-chat', label: 'deepseek-chat' },
    { value: 'deepseek-reasoner', label: 'deepseek-reasoner' },
  ],
  'https://dashscope.aliyuncs.com/compatible-mode/v1': [
    { value: 'qwen-plus', label: 'qwen-plus' },
    { value: 'qwen-turbo', label: 'qwen-turbo' },
    { value: 'qwen-max', label: 'qwen-max' },
  ],
  'https://open.bigmodel.cn/api/paas/v4': [
    { value: 'glm-4-flash', label: 'glm-4-flash' },
    { value: 'glm-4', label: 'glm-4' },
  ],
  'https://api.moonshot.cn/v1': [
    { value: 'moonshot-v1-8k', label: 'moonshot-v1-8k' },
    { value: 'moonshot-v1-32k', label: 'moonshot-v1-32k' },
  ],
  'https://generativelanguage.googleapis.com/v1beta/openai/': [
    { value: 'gemini-2.5-flash', label: 'gemini-2.5-flash' },
    { value: 'gemini-2.5-pro', label: 'gemini-2.5-pro' },
    { value: 'gemini-2.0-flash', label: 'gemini-2.0-flash' },
    { value: 'gemini-1.5-pro', label: 'gemini-1.5-pro' },
  ],
  'https://api.minimax.io/anthropic/v1': [
    { value: 'MiniMax-M2.7', label: 'MiniMax-M2.7' },
    { value: 'MiniMax-M2.7-High-Speed', label: 'MiniMax-M2.7-High-Speed' },
    { value: 'MiniMax-M2.5', label: 'MiniMax-M2.5' },
    { value: 'MiniMax-M2.5-High-Speed', label: 'MiniMax-M2.5-High-Speed' },
    { value: 'MiniMax-M2.1', label: 'MiniMax-M2.1' },
    { value: 'MiniMax-M2.1-High-Speed', label: 'MiniMax-M2.1-High-Speed' },
    { value: 'MiniMax-M2', label: 'MiniMax-M2' },
    { value: 'MiniMax-M1-80k', label: 'MiniMax-M1-80k' },
    { value: 'MiniMax-Text-01', label: 'MiniMax-Text-01' },
  ],
};

export function normalizeRemoteModels(models: Array<{ value?: string; label?: string }> = []): ProviderModelOption[] {
  const seen = new Set<string>();
  const normalized: ProviderModelOption[] = [];
  for (const item of models) {
    const value = String(item?.value || '').trim();
    if (!value || seen.has(value)) continue;
    seen.add(value);
    normalized.push({ value, label: String(item?.label || value) });
  }
  return normalized;
}

export function fallbackModelsByEndpoint(endpoint: string): ProviderModelOption[] {
  return normalizeRemoteModels(
    PROVIDER_MODELS[String(endpoint || '').trim()] || [{ value: DEFAULT_PROVIDER_MODEL, label: DEFAULT_PROVIDER_MODEL }],
  );
}

export function defaultModelByEndpoint(endpoint: string): string {
  return fallbackModelsByEndpoint(endpoint)[0]?.value || DEFAULT_PROVIDER_MODEL;
}

export function getProviderLabel(endpoint: string): string {
  return PROVIDER_OPTIONS.find((item) => item.value === endpoint)?.label || String(endpoint || '').trim() || 'N/A';
}

