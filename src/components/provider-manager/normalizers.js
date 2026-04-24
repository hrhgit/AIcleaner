import {
  DEFAULT_PROVIDER_ENDPOINT,
  defaultModelByEndpoint,
  PROVIDER_OPTIONS,
} from '../../utils/provider-registry.js';

export function normalizeProviders(settings) {
  const merged = [];
  const byEndpoint = settings?.providerConfigs && typeof settings.providerConfigs === 'object'
    ? settings.providerConfigs
    : {};

  const presetSet = new Set();
  for (const preset of PROVIDER_OPTIONS) {
    presetSet.add(preset.value);
    const config = byEndpoint[preset.value] || {};
    merged.push({
      name: String(config?.name || preset.label),
      endpoint: preset.value,
      apiKey: '',
      model: String(config?.model || defaultModelByEndpoint(preset.value)),
    });
  }

  for (const [key, rawConfig] of Object.entries(byEndpoint)) {
    const endpoint = String(rawConfig?.endpoint || key || '').trim();
    if (!endpoint || presetSet.has(endpoint)) continue;
    merged.push({
      name: String(rawConfig?.name || endpoint),
      endpoint,
      apiKey: '',
      model: String(rawConfig?.model || defaultModelByEndpoint(endpoint)),
    });
  }

  if (!merged.length) {
    merged.push({
      name: 'OpenAI',
      endpoint: DEFAULT_PROVIDER_ENDPOINT,
      apiKey: '',
      model: defaultModelByEndpoint(DEFAULT_PROVIDER_ENDPOINT),
    });
  }

  let defaultProviderEndpoint = String(settings?.defaultProviderEndpoint || '').trim();
  if (!merged.some((item) => item.endpoint === defaultProviderEndpoint)) {
    defaultProviderEndpoint = merged[0].endpoint;
  }

  return { providers: merged, defaultProviderEndpoint };
}

export function normalizeSearchApi(settings) {
  const source = settings?.searchApi && typeof settings.searchApi === 'object'
    ? settings.searchApi
    : {};
  const scopesSource = source?.scopes && typeof source.scopes === 'object'
    ? source.scopes
    : {};
  const workflowEnabled = !!(
    source?.enabled
    || scopesSource.classify
    || scopesSource.organizer
  );

  return {
    provider: 'tavily',
    enabled: workflowEnabled,
    apiKey: '',
    scopes: {
      classify: workflowEnabled,
      organizer: workflowEnabled,
    },
  };
}

