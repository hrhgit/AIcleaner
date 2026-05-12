import { describe, expect, it } from 'vitest';
import { DEFAULT_PROVIDER_ENDPOINT } from '../../utils/provider-registry';
import {
  applyLoadedProviderModels,
  buildDirtyCredentialsPayload,
  buildProviderSettingsPayload,
  mergeProviderModelOptions,
  normalizeProviders,
  normalizeSearchApi,
} from './normalizers';

const baseProvider = {
  name: 'OpenAI',
  endpoint: DEFAULT_PROVIDER_ENDPOINT,
  apiKey: '',
  apiFormat: 'openai' as const,
  model: '',
  thinkingEnabled: false,
  thinkingLevel: 'medium' as const,
  preset: true,
};

describe('provider manager normalizers', () => {
  it('keeps saved providers and normalizes default provider endpoint', () => {
    const result = normalizeProviders({
      defaultProviderEndpoint: 'missing',
      providerConfigs: {
        custom: {
          endpoint: 'https://example.test/v1',
          name: 'Custom',
          apiFormat: 'anthropic',
          model: 'custom-model',
          thinking: { enabled: true, level: 'high' },
        },
      },
    });

    expect(result.providers.some((provider) => provider.endpoint === 'https://example.test/v1')).toBe(true);
    expect(result.defaultProviderEndpoint).toBe(result.providers[0].endpoint);
  });

  it('maps known provider endpoints back to provider template labels', () => {
    const result = normalizeProviders({
      providerConfigs: {
        'https://api.deepseek.com': {
          endpoint: 'https://api.deepseek.com',
          model: 'deepseek-chat',
        },
      },
      defaultProviderEndpoint: 'https://api.deepseek.com',
    });

    expect(result.providers[0]?.name).toBe('DeepSeek');
    expect(result.providers[0]?.apiFormat).toBe('openai');
  });

  it('treats any enabled search scope as workflow web search enabled', () => {
    const result = normalizeSearchApi({ searchApi: { scopes: { organizer: true } } });

    expect(result.enabled).toBe(true);
    expect(result.scopes.classify).toBe(true);
    expect(result.scopes.organizer).toBe(true);
  });

  it('serializes provider settings without credentials', () => {
    const payload = buildProviderSettingsPayload(
      [{ ...baseProvider, model: 'gpt-4o-mini', thinkingEnabled: true, thinkingLevel: 'high' }],
      DEFAULT_PROVIDER_ENDPOINT,
      { provider: 'tavily', enabled: true, scopes: { classify: true, organizer: true } },
    );

    expect(payload.providerConfigs?.[DEFAULT_PROVIDER_ENDPOINT]).toEqual({
      name: 'OpenAI',
      endpoint: DEFAULT_PROVIDER_ENDPOINT,
      apiFormat: 'openai',
      model: 'gpt-4o-mini',
      thinking: {
        enabled: true,
        level: 'high',
      },
    });
    expect(JSON.stringify(payload)).not.toContain('secret');
  });

  it('only includes dirty credentials in save payload', () => {
    expect(buildDirtyCredentialsPayload(
      { a: 'new-a', b: 'new-b' },
      'search',
      { providerSecrets: { b: true }, searchApiKey: false },
    )).toEqual({ providerSecrets: { b: 'new-b' } });
  });

  it('marks provider models loaded and fills first remote model when empty', () => {
    const [provider] = applyLoadedProviderModels(
      [{ ...baseProvider }],
      DEFAULT_PROVIDER_ENDPOINT,
      [{ value: 'gpt-4o-mini', label: 'gpt-4o-mini' }],
    );

    expect(provider.modelLoaded).toBe(true);
    expect(provider.model).toBe('gpt-4o-mini');
  });

  it('preserves unknown saved model values in provider options', () => {
    const options = mergeProviderModelOptions(
      'custom-model',
      [{ value: 'gpt-4o-mini', label: 'gpt-4o-mini' }],
    );

    expect(options.map((option) => option.value)).toEqual(['custom-model', 'gpt-4o-mini']);
  });
});
