import type { ProviderModelOption, ProviderRow } from '../types';

export type ProviderApiFormat = 'openai' | 'anthropic';

export type ProviderTemplate = {
  id: string;
  label: string;
  defaultApiFormat: ProviderApiFormat;
  endpoints: Partial<Record<ProviderApiFormat, string>>;
};

export const DEFAULT_PROVIDER_ENDPOINT = 'https://api.openai.com/v1';

export const PROVIDER_TEMPLATES: ProviderTemplate[] = [
  {
    id: 'deepseek',
    label: 'DeepSeek',
    defaultApiFormat: 'openai',
    endpoints: { openai: 'https://api.deepseek.com' },
  },
  {
    id: 'openai',
    label: 'OpenAI',
    defaultApiFormat: 'openai',
    endpoints: { openai: 'https://api.openai.com/v1' },
  },
  {
    id: 'gemini',
    label: 'Google Gemini',
    defaultApiFormat: 'openai',
    endpoints: { openai: 'https://generativelanguage.googleapis.com/v1beta/openai' },
  },
  {
    id: 'qwen',
    label: 'Qwen (DashScope)',
    defaultApiFormat: 'openai',
    endpoints: { openai: 'https://dashscope.aliyuncs.com/compatible-mode/v1' },
  },
  {
    id: 'glm',
    label: 'GLM (BigModel)',
    defaultApiFormat: 'openai',
    endpoints: { openai: 'https://open.bigmodel.cn/api/paas/v4' },
  },
  {
    id: 'kimi',
    label: 'Kimi (Moonshot)',
    defaultApiFormat: 'openai',
    endpoints: { openai: 'https://api.moonshot.cn/v1' },
  },
  {
    id: 'minimax',
    label: 'MiniMax',
    defaultApiFormat: 'anthropic',
    endpoints: { anthropic: 'https://api.minimax.io/anthropic/v1' },
  },
] as const;

export const PROVIDER_NAME_OPTIONS = PROVIDER_TEMPLATES.map((template) => template.label);

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

export function normalizeProviderApiFormat(value: string | null | undefined): ProviderApiFormat {
  return String(value || '').trim().toLowerCase() === 'anthropic' ? 'anthropic' : 'openai';
}

export function normalizeThinkingLevel(value: string | null | undefined): 'low' | 'medium' | 'high' {
  const normalized = String(value || '').trim().toLowerCase();
  if (normalized === 'low' || normalized === 'high') return normalized;
  return 'medium';
}

export function normalizeProviderEndpoint(endpoint: string, apiFormat: ProviderApiFormat): string {
  const raw = String(endpoint || '').trim();
  if (!raw) return '';
  try {
    const url = new URL(raw);
    const segments = url.pathname.split('/').filter(Boolean);
    if (apiFormat === 'openai') {
      if (segments.at(-2) === 'chat' && segments.at(-1) === 'completions') {
        segments.splice(-2, 2);
      }
    } else {
      if (segments.at(-1) === 'messages') segments.pop();
      if (segments.at(-1) !== 'v1') segments.push('v1');
    }
    url.pathname = segments.length ? `/${segments.join('/')}` : '/';
    url.search = '';
    url.hash = '';
    return url.toString().replace(/\/$/, '');
  } catch {
    return raw.replace(/\/$/, '');
  }
}

export function getProviderTemplateFormats(template: ProviderTemplate): ProviderApiFormat[] {
  const formats: ProviderApiFormat[] = [];
  if (template.endpoints.openai) formats.push('openai');
  if (template.endpoints.anthropic) formats.push('anthropic');
  return formats.length ? formats : [template.defaultApiFormat];
}

export function getProviderTemplateEndpoint(
  template: ProviderTemplate,
  apiFormat: ProviderApiFormat,
): string {
  return template.endpoints[apiFormat] || template.endpoints[template.defaultApiFormat] || '';
}

export function findProviderTemplateByName(name: string): ProviderTemplate | undefined {
  const normalizedName = String(name || '').trim().toLowerCase();
  if (!normalizedName) return undefined;
  return PROVIDER_TEMPLATES.find((template) => template.label.toLowerCase() === normalizedName);
}

export function findProviderTemplateByEndpoint(endpoint: string): ProviderTemplate | undefined {
  const normalizedEndpoint = String(endpoint || '').trim().replace(/\/$/, '');
  if (!normalizedEndpoint) return undefined;
  return PROVIDER_TEMPLATES.find((template) => (
    Object.values(template.endpoints).some((candidate) => candidate === normalizedEndpoint)
  ));
}

export function inferProviderTemplate(name: string, endpoint: string): ProviderTemplate | undefined {
  return findProviderTemplateByName(name) || findProviderTemplateByEndpoint(endpoint);
}

export function getProviderLabel(endpoint: string): string {
  return findProviderTemplateByEndpoint(endpoint)?.label || String(endpoint || '').trim() || 'N/A';
}

export function buildProviderDisplayName(endpoint: string): string {
  const normalized = String(endpoint || '').trim();
  if (!normalized) return 'Custom Provider';
  const template = findProviderTemplateByEndpoint(normalized);
  if (template) return template.label;
  try {
    const url = new URL(normalized);
    const host = url.hostname.replace(/^api\./, '').replace(/^www\./, '');
    const path = url.pathname.split('/').filter(Boolean).slice(0, 2).join(' / ');
    return path ? `${host} (${path})` : host;
  } catch {
    return normalized;
  }
}

export function createCustomProviderRow(id: number): ProviderRow {
  return {
    id: `custom-${id}`,
    name: '',
    endpoint: '',
    apiKey: '',
    apiFormat: 'openai',
    model: '',
    preset: false,
    modelLoaded: false,
  };
}
