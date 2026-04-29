import { describe, expect, it } from 'vitest';
import { DEFAULT_PROVIDER_ENDPOINT } from '../../utils/provider-registry';
import {
  applyLoadedProviderModels,
  buildDirtyCredentialsPayload,
  buildProviderSettingsPayload,
  mergeProviderModelOptions,
  normalizeProviders,
  normalizeSearchApi,
  resolveProviderModelValue,
} from './normalizers';

describe('provider manager normalizers', () => {
  it('keeps preset providers and normalizes default provider endpoint', () => {
    const result = normalizeProviders({
      defaultProviderEndpoint: 'missing',
      providerConfigs: {
        custom: { endpoint: 'https://example.test/v1', name: 'Custom', model: 'custom-model' },
      },
    });

    expect(result.providers.some((provider) => provider.endpoint === DEFAULT_PROVIDER_ENDPOINT)).toBe(true);
    expect(result.providers.some((provider) => provider.endpoint === 'https://example.test/v1')).toBe(true);
    expect(result.defaultProviderEndpoint).toBe(result.providers[0].endpoint);
  });

  it('treats any enabled search scope as workflow web search enabled', () => {
    const result = normalizeSearchApi({ searchApi: { scopes: { organizer: true } } });

    expect(result.enabled).toBe(true);
    expect(result.scopes.classify).toBe(true);
    expect(result.scopes.organizer).toBe(true);
  });

  it('serializes provider settings without credentials', () => {
    const payload = buildProviderSettingsPayload(
      [{ name: 'OpenAI', endpoint: DEFAULT_PROVIDER_ENDPOINT, apiKey: 'secret', model: 'gpt-4o-mini' }],
      DEFAULT_PROVIDER_ENDPOINT,
      { provider: 'tavily', enabled: true, scopes: { classify: true, organizer: true } },
    );

    expect(payload.providerConfigs?.[DEFAULT_PROVIDER_ENDPOINT]).toEqual({
      name: 'OpenAI',
      endpoint: DEFAULT_PROVIDER_ENDPOINT,
      model: 'gpt-4o-mini',
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

  it('marks provider models loaded after fallback refresh results', () => {
    const [provider] = applyLoadedProviderModels(
      [{ name: 'OpenAI', endpoint: DEFAULT_PROVIDER_ENDPOINT, apiKey: '', model: 'gpt-4o-mini' }],
      DEFAULT_PROVIDER_ENDPOINT,
      [{ value: 'gpt-4o-mini', label: 'gpt-4o-mini' }],
    );

    expect(provider.modelLoaded).toBe(true);
    expect(provider.model).toBe('gpt-4o-mini');
  });

  it('preserves unknown saved model values in provider options', () => {
    const options = mergeProviderModelOptions(
      DEFAULT_PROVIDER_ENDPOINT,
      'custom-model',
      [{ value: 'gpt-4o-mini', label: 'gpt-4o-mini' }],
    );

    expect(options.map((option) => option.value)).toEqual(['gpt-4o-mini', 'custom-model']);
    expect(resolveProviderModelValue(DEFAULT_PROVIDER_ENDPOINT, 'custom-model', options)).toBe('custom-model');
  });

  it('falls back to the first available model when no model is selected', () => {
    expect(resolveProviderModelValue(
      DEFAULT_PROVIDER_ENDPOINT,
      '',
      [
        { value: 'first-model', label: 'first-model' },
        { value: 'second-model', label: 'second-model' },
      ],
    )).toBe('first-model');
  });
});
